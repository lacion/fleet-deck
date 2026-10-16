//! The list of the native sessions of a station (B7, design §2.1, §2.2).
//!
//! `station.native_sessions` walks the provider homes of the configured
//! instances at a fixed depth, reads a head window and a tail window of the
//! newest transcripts, applies the identity and trust rules, and joins the
//! result with the station's own sessions. It runs under the shared history
//! permit with one deadline and a total budget. Nothing is stored. No text of
//! a transcript is logged.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use fleetdeck_proto::ProviderInstanceId;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::HistoryKind;
use fleetdeck_proto::native::{
    HomeState, ImportState, NativeHome, NativeSession, NativeSessionList, NativeSessionsQuery,
    TranscriptPin,
};
use rustix::io::Errno;

pub use super::browse_preview::Budget;
use super::browse_preview::{
    Cand, Gauge, Halt, Walk, invalid, mtime_ms, open_cand, run_job, scan, walk_home,
};
use super::lines::{Line, ReadError};
use super::locate::{self, Trust, resolve_home};
use super::parse::{self, Parsed, cut_chars};
use super::trust::{Identity, Mounts, PathCheck, trusted_home, trusted_path};
use super::{HistoryError, Limits, env_for};
use crate::manager::Policy;
use crate::store::Store;
use crate::store::native_browse::{BrowseFact, ImportBlock};
use crate::workspace::PrivatePaths;
use crate::workspace::files::open_root;

/// The default and the largest `limit` of a list.
const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 200;
/// The head window of a transcript (identity, first prompt, title).
pub(super) const HEAD_BYTES: u64 = 256 * 1024;
/// The tail window of a transcript in a list (identity, title).
const TAIL_BYTES: u64 = 64 * 1024;
/// The longest `first_prompt`, in characters.
const PROMPT_MAX: usize = 200;

// ---------------------------------------------------------------- homes

/// The homes of the configured instances, before the filesystem is asked.
struct HomePlan {
    kind: ProviderKind,
    instances: Vec<ProviderInstanceId>,
    /// The absolute home of the launch environment; `None` when it is
    /// missing or relative.
    raw: Option<PathBuf>,
}

fn plan_homes(policy: &Policy) -> Vec<HomePlan> {
    let mut plans: Vec<HomePlan> = Vec::new();
    for (instance, kind) in policy.instances() {
        if *kind == ProviderKind::Unknown {
            continue;
        }
        let env = env_for(Some(policy), instance);
        let raw = resolve_home(&env, *kind).filter(|h| h.is_absolute());
        match plans.iter_mut().find(|p| p.kind == *kind && p.raw == raw) {
            Some(plan) => plan.instances.push(instance.clone()),
            None => plans.push(HomePlan {
                kind: *kind,
                instances: vec![instance.clone()],
                raw,
            }),
        }
    }
    plans
}

/// One home, resolved against the filesystem.
pub(super) struct Home {
    kind: ProviderKind,
    instances: Vec<ProviderInstanceId>,
    state: HomeState,
    /// The canonical home (the resolved path when it does not exist).
    canon: Option<PathBuf>,
    /// False when the home does not exist.
    exists: bool,
    /// False when the home is on a filesystem or mount that cannot be
    /// verified.
    verified: bool,
    root: Option<OwnedFd>,
}

fn unavailable(reason: &str) -> HomeState {
    HomeState::Unavailable {
        reason: reason.to_owned(),
    }
}

/// One plan, resolved against the filesystem (B7 PRE-PR r1: a home resolves
/// through links that only the station user or root own).
struct Resolved {
    canon: Option<PathBuf>,
    state: HomeState,
    exists: bool,
    verified: bool,
}

fn resolve_plan(plan: &HomePlan, uid: u32, mounts: &Mounts) -> Resolved {
    let resolved = |canon: &PathBuf, state, exists, verified| Resolved {
        canon: Some(canon.clone()),
        state,
        exists,
        verified,
    };
    let Some(raw) = &plan.raw else {
        return Resolved {
            canon: None,
            state: unavailable("home_not_absolute"),
            exists: false,
            verified: false,
        };
    };
    match trusted_home(raw, uid, mounts) {
        Ok(path) => resolved(&path, HomeState::Ok, true, true),
        Err(PathCheck::Untrusted("missing")) => resolved(raw, HomeState::Ok, false, false),
        Err(PathCheck::Untrusted(_)) => {
            resolved(raw, unavailable("home_not_trusted"), false, false)
        }
        // The station makes no claim about this home: it is resolved as
        // before and marked not verified.
        Err(PathCheck::Unverifiable(_)) => match std::fs::canonicalize(raw) {
            // The later check of today: a canonical path that fails it is
            // not trusted.
            Ok(canon) => match trusted_path(&canon, uid, mounts) {
                Err(PathCheck::Untrusted(_)) => {
                    resolved(raw, unavailable("home_not_trusted"), false, false)
                }
                Ok(()) | Err(PathCheck::Unverifiable(_)) => {
                    resolved(&canon, HomeState::Ok, true, false)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                resolved(raw, HomeState::Ok, false, false)
            }
            Err(_) => resolved(raw, unavailable("not_readable"), false, false),
        },
    }
}

/// Resolves each home through trusted links, merges homes with the same
/// canonical path, keeps those of `only`, and opens each one.
fn open_homes(
    plans: Vec<HomePlan>,
    only: Option<&ProviderInstanceId>,
    uid: u32,
    mounts: &Mounts,
) -> Vec<Home> {
    let mut homes: Vec<Home> = Vec::new();
    for plan in plans {
        let Resolved {
            canon,
            state,
            exists,
            verified,
        } = resolve_plan(&plan, uid, mounts);
        let same = homes
            .iter_mut()
            .find(|h| h.kind == plan.kind && h.canon == canon && h.state == state);
        match same {
            Some(home) => home.instances.extend(plan.instances),
            None => homes.push(Home {
                kind: plan.kind,
                instances: plan.instances,
                state,
                canon,
                exists,
                verified,
                root: None,
            }),
        }
    }
    if let Some(only) = only {
        homes.retain(|h| h.instances.contains(only));
    }
    for home in &mut homes {
        let (HomeState::Ok, Some(canon), true) = (&home.state, &home.canon, home.exists) else {
            // No home, a home that is not usable, or one that does not
            // exist: nothing to walk.
            continue;
        };
        match open_root(canon) {
            Ok(fd) => home.root = Some(fd),
            Err(Errno::NOENT) => {}
            Err(_) => home.state = unavailable("not_readable"),
        }
    }
    homes
}

/// What the windows of a transcript say.
struct Inspected {
    found: super::trust::Found,
    first_prompt: Option<String>,
}

fn first_prompt(kind: ProviderKind, line: &[u8]) -> Option<String> {
    match parse::parse_line(kind, line) {
        Parsed::Known(record) => record
            .items
            .into_iter()
            .find(|i| i.kind == HistoryKind::User && !i.text.trim().is_empty())
            .map(|i| cut_chars(&i.text, PROMPT_MAX)),
        _ => None,
    }
}

/// Runs the identity rules on the head and the tail window. `None` is a
/// contradiction.
fn inspect(
    file: &File,
    cand: &Cand,
    size: u64,
    limits: &Limits,
    deadline: Instant,
) -> Result<Option<Inspected>, ReadError> {
    let head_end = size.min(HEAD_BYTES);
    let tail_start = head_end.max(size.saturating_sub(TAIL_BYTES));
    let dir_name = (cand.kind == ProviderKind::Claude)
        .then(|| cand.dirs.get(1).map(String::as_str))
        .flatten();
    let mut identity = Identity::new(cand.kind, &cand.id, dir_name);
    let mut prompt: Option<String> = None;
    let mut bad = false;
    scan(
        file,
        0,
        head_end,
        limits,
        deadline,
        |offset, line| match line {
            Line::TooLong => {
                bad = offset == 0 && cand.kind == ProviderKind::Codex;
                !bad
            }
            Line::Record(bytes) => {
                if identity.record(bytes).is_err() {
                    bad = true;
                    return false;
                }
                if prompt.is_none() {
                    prompt = first_prompt(cand.kind, bytes);
                }
                true
            }
        },
    )?;
    if !bad && tail_start < size {
        scan(file, tail_start, size, limits, deadline, |_, line| {
            if let Line::Record(bytes) = line
                && identity.record(bytes).is_err()
            {
                bad = true;
            }
            !bad
        })?;
    }
    if bad {
        return Ok(None);
    }
    Ok(identity.finish().ok().map(|found| Inspected {
        found,
        first_prompt: prompt,
    }))
}

// ------------------------------------------------------------ candidates

/// A session of the list; `open` when the store decides `in_use`, `held`,
/// `execution_unknown` or `ok`.
pub(super) struct Draft {
    home: usize,
    session: NativeSession,
    open: bool,
}

/// What the worker needs to read a candidate.
struct Env<'a> {
    policy: &'a Policy,
    private: &'a PrivatePaths,
    uid: u32,
    limits: &'a Limits,
    mounts: &'a Mounts,
    deadline: Instant,
}

/// The session of a candidate with the facts of its open file; the import
/// state starts at `not_trusted` with no text.
fn skeleton(home: &Home, cand: &Cand, meta: Option<&std::fs::Metadata>) -> NativeSession {
    let (dev, ino, size, modified_at) = meta
        .map_or((cand.dev, cand.ino, cand.size, cand.mtime_ms), |m| {
            (m.dev(), m.ino(), m.len(), mtime_ms(m))
        });
    NativeSession {
        provider: cand.kind,
        native_id: cand.id.clone(),
        instances: Vec::new(),
        pin: TranscriptPin {
            home: home
                .canon
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            dev,
            ino,
        },
        cwd: None,
        title: None,
        first_prompt: None,
        modified_at,
        size,
        parent_native_id: None,
        known: None,
        import: ImportState::NotTrusted,
        verified: false,
    }
}

/// The checks of a `cwd` that do not need the store: the state that applies
/// first, and whether the path is verified.
fn cwd_state(env: &Env<'_>, cwd: &str) -> (Option<ImportState>, bool) {
    let Ok(canon) = env.policy.workspace(cwd) else {
        return (Some(ImportState::OutsideRoots), false);
    };
    let verified = match trusted_path(Path::new(cwd), env.uid, env.mounts) {
        Ok(()) => true,
        Err(PathCheck::Unverifiable(_)) => false,
        Err(PathCheck::Untrusted(_)) => return (Some(ImportState::PathNotTrusted), false),
    };
    if canon.as_os_str() != std::ffi::OsStr::new(cwd) {
        return (Some(ImportState::CwdMismatch), verified);
    }
    (None, verified)
}

fn read_candidate(
    env: &Env<'_>,
    gauge: &mut Gauge<'_>,
    home: &Home,
    cand: &Cand,
    duplicate: bool,
) -> Result<Option<Draft>, Halt> {
    let draft = |session| {
        Some(Draft {
            home: cand.home,
            session,
            open: false,
        })
    };
    let Some(root) = home.root.as_ref() else {
        return Ok(None);
    };
    let file = match open_cand(root, cand) {
        Ok(file) => file,
        Err(Errno::NOENT) => return Ok(None),
        Err(_) => return Ok(draft(skeleton(home, cand, None))),
    };
    let Ok(meta) = file.metadata() else {
        return Ok(draft(skeleton(home, cand, None)));
    };
    let mut session = skeleton(home, cand, Some(&meta));
    let trust = Trust {
        private: env.private,
        uid: env.uid,
        limits: env.limits,
        deadline: env.deadline,
    };
    if locate::check_meta(&file, &trust, u64::MAX).is_err() {
        return Ok(draft(session));
    }
    let Some(canon) = home.canon.as_deref() else {
        return Ok(draft(session));
    };
    let mut verified = home.verified;
    let path: PathBuf = canon
        .join(cand.dirs.iter().collect::<PathBuf>())
        .join(&cand.name);
    match trusted_path(&path, env.uid, env.mounts) {
        Ok(()) => {}
        Err(PathCheck::Unverifiable(_)) => verified = false,
        Err(PathCheck::Untrusted(_)) => return Ok(draft(session)),
    }
    if duplicate {
        return Ok(draft(session));
    }
    let size = meta.len();
    let head_end = size.min(HEAD_BYTES);
    let tail_start = head_end.max(size.saturating_sub(TAIL_BYTES));
    gauge.bytes(head_end + (size - tail_start))?;
    let inspected = match inspect(&file, cand, size, env.limits, env.deadline) {
        Ok(Some(inspected)) => inspected,
        Ok(None) => return Ok(draft(session)),
        Err(ReadError::Deadline) => return Err(Halt::Timed),
        Err(ReadError::Io(_)) => return Ok(draft(session)),
    };
    let found = inspected.found;
    session.title = found.title;
    session.first_prompt = inspected.first_prompt;
    session.parent_native_id = found.parent;
    session.cwd = Some(found.cwd.clone());
    let mut open = false;
    let (state, cwd_verified) = if session.parent_native_id.is_some() {
        (Some(ImportState::SubagentThread), false)
    } else if size > env.limits.file_max {
        (Some(ImportState::TooLarge), false)
    } else {
        cwd_state(env, &found.cwd)
    };
    match state {
        Some(state) => session.import = state,
        None => {
            session.import = ImportState::Ok;
            open = true;
        }
    }
    session.verified = verified && cwd_verified;
    Ok(Some(Draft {
        home: cand.home,
        session,
        open,
    }))
}

// ------------------------------------------------------------- the list

struct Scanned {
    homes: Vec<Home>,
    drafts: Vec<Draft>,
    truncated: bool,
    timed_out: bool,
}

struct ListJob {
    plans: Vec<HomePlan>,
    only: Option<ProviderInstanceId>,
    limit: usize,
    policy: Arc<Policy>,
    private: Arc<PrivatePaths>,
    uid: u32,
    limits: Limits,
    budget: Budget,
    mounts: Option<Mounts>,
}

fn list_blocking(job: ListJob, deadline: Instant, stop: &AtomicBool) -> Scanned {
    let mounts = job.mounts.unwrap_or_else(Mounts::load);
    let mut homes = open_homes(job.plans, job.only.as_ref(), job.uid, &mounts);
    let mut gauge = Gauge::new(&job.budget, deadline, stop);
    let mut cands: Vec<Cand> = Vec::new();
    let (mut truncated, mut timed_out) = (false, false);
    for (index, home) in homes.iter_mut().enumerate() {
        let Some(root) = home.root.as_ref() else {
            continue;
        };
        let before = cands.len();
        let walk = Walk {
            kind: home.kind,
            only: None,
        };
        match walk_home(root, &mut gauge, &walk, index, &mut cands) {
            Ok(()) => {}
            Err(Halt::Unreadable) => {
                cands.truncate(before);
                home.state = unavailable("not_readable");
            }
            Err(Halt::Truncated) => {
                truncated = true;
                break;
            }
            Err(Halt::Timed) => {
                timed_out = true;
                break;
            }
        }
    }
    // A Codex id in two or more files: every copy is not trusted.
    let mut copies: HashMap<(usize, &str), usize> = HashMap::new();
    for cand in &cands {
        if cand.kind == ProviderKind::Codex {
            *copies.entry((cand.home, cand.id.as_str())).or_default() += 1;
        }
    }
    let duplicates: Vec<bool> = cands
        .iter()
        .map(|c| copies.get(&(c.home, c.id.as_str())).is_some_and(|n| *n > 1))
        .collect();
    drop(copies);
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|a, b| {
        let (a, b) = (&cands[*a], &cands[*b]);
        b.mtime_ms.cmp(&a.mtime_ms).then_with(|| a.id.cmp(&b.id))
    });
    if order.len() > job.limit {
        truncated = true;
        order.truncate(job.limit);
    }
    let env = Env {
        policy: &job.policy,
        private: &job.private,
        uid: job.uid,
        limits: &job.limits,
        mounts: &mounts,
        deadline,
    };
    let mut drafts = Vec::new();
    if !timed_out {
        for index in order {
            let cand = &cands[index];
            match read_candidate(&env, &mut gauge, &homes[cand.home], cand, duplicates[index]) {
                Ok(Some(draft)) => drafts.push(draft),
                Ok(None) => {}
                Err(Halt::Timed) => {
                    timed_out = true;
                    break;
                }
                Err(_) => {
                    truncated = true;
                    break;
                }
            }
        }
    }
    Scanned {
        homes,
        drafts,
        truncated,
        timed_out,
    }
}

/// `station.native_sessions`.
///
/// # Errors
/// [`HistoryError`] with `invalid_params`, `busy` or `internal`.
pub async fn native_sessions(
    store: &Store,
    policy: &Arc<Policy>,
    private: &Arc<PrivatePaths>,
    query: NativeSessionsQuery,
    budget: Budget,
) -> Result<NativeSessionList, HistoryError> {
    run_list(store, policy, private, query, budget, None).await
}

/// [`native_sessions`] with a mount table of the caller (tests).
pub(super) async fn run_list(
    store: &Store,
    policy: &Arc<Policy>,
    private: &Arc<PrivatePaths>,
    query: NativeSessionsQuery,
    budget: Budget,
    mounts: Option<Mounts>,
) -> Result<NativeSessionList, HistoryError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(invalid(&format!("limit must be 1 to {MAX_LIMIT}")));
    }
    if let Some(instance) = &query.provider_instance
        && !policy.instances().contains_key(instance)
    {
        return Err(invalid("unknown provider instance"));
    }
    let history = store.history();
    let job = ListJob {
        plans: plan_homes(policy),
        only: query.provider_instance,
        limit: limit as usize,
        policy: Arc::clone(policy),
        private: Arc::clone(private),
        uid: history.uid(),
        limits: history.limits().clone(),
        budget: budget.clone(),
        mounts,
    };
    let scanned = run_job(history, &budget, move |deadline, stop| {
        list_blocking(job, deadline, stop)
    })
    .await?;
    let keys: Vec<(ProviderKind, String)> = scanned
        .drafts
        .iter()
        .map(|d| (d.session.provider, d.session.native_id.clone()))
        .collect();
    let facts = store
        .native_browse_facts(keys)
        .await
        .map_err(super::internal)?;
    Ok(assemble(scanned, &facts, history.limits().answer_max))
}

/// Joins the drafts with the store facts and cuts the answer to the cap.
fn assemble(
    scanned: Scanned,
    facts: &HashMap<(ProviderKind, String), BrowseFact>,
    answer_max: usize,
) -> NativeSessionList {
    let Scanned {
        homes,
        drafts,
        mut truncated,
        timed_out,
    } = scanned;
    let mut sessions: Vec<NativeSession> = Vec::new();
    for draft in drafts {
        let mut session = draft.session;
        session.instances = homes[draft.home].instances.clone();
        if let Some(fact) = facts.get(&(session.provider, session.native_id.clone())) {
            session.known = fact.known.clone();
            if draft.open {
                session.import = match &fact.block {
                    Some(ImportBlock::InUse(id)) => ImportState::InUse {
                        session_id: id.clone(),
                    },
                    Some(ImportBlock::Held(id)) => ImportState::Held {
                        session_id: id.clone(),
                    },
                    Some(ImportBlock::ExecutionUnknown) => ImportState::ExecutionUnknown,
                    None => ImportState::Ok,
                };
            }
        }
        sessions.push(session);
    }
    sessions.sort_by(|a, b| {
        b.modified_at
            .cmp(&a.modified_at)
            .then_with(|| a.native_id.cmp(&b.native_id))
    });
    let homes: Vec<NativeHome> = homes
        .into_iter()
        .map(|h| NativeHome {
            provider: h.kind,
            instances: h.instances,
            state: h.state,
        })
        .collect();
    let mut list = NativeSessionList {
        homes,
        sessions: Vec::new(),
        truncated,
        timed_out,
    };
    let base = serde_json::to_vec(&list).map_or(0, |v| v.len());
    let mut total = base;
    let mut keep = 0;
    for session in &sessions {
        total += serde_json::to_vec(session).map_or(0, |v| v.len()) + 1;
        if total > answer_max {
            break;
        }
        keep += 1;
    }
    if keep < sessions.len() {
        truncated = true;
        sessions.truncate(keep);
    }
    list.sessions = sessions;
    list.truncated = truncated;
    // The exact check on the whole answer; normally it drops nothing.
    while serde_json::to_vec(&list).map_or(0, |v| v.len()) > answer_max && !list.sessions.is_empty()
    {
        list.sessions.pop();
        list.truncated = true;
    }
    list
}
