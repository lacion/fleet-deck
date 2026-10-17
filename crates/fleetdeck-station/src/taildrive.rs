//! The Taildrive report of this station and the exposure check of its shares
//! (design §15.2, §15.4, FG-12, O36, O37).
//!
//! The station runs `tailscale drive list` and `tailscale status --json` at
//! start, every 10 minutes, and before a use when the report is older than
//! 60 s. Fleet Deck only reads Taildrive. A share is `exposed` when its path
//! is, contains or is inside a protected path, or when a bounded metadata
//! walk finds a protected or deny-listed entry below it (B5). A share that
//! the check cannot rule out is `unknown`, and an `unknown` share is never an
//! artifact share (O37). The walk reads names and metadata only.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use fleetdeck_core::tailscale::{TailscaleCli, TailscaleError};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{
    Exposure, ExposureState, SharePathState, TaildriveReport, TaildriveShare, TaildriveState,
};
use fleetdeck_proto::member_deny::MemberDeny;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};

use crate::fleet_git_runtime::FleetGitRuntime;
use crate::manager::{Policy, SessionManager};
use crate::store::Store;
use crate::store::artifacts_state::ExposureInputs;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// How often the report is made again (design §15.2).
pub const REFRESH_EVERY: Duration = Duration::from_secs(10 * 60);
/// A report older than this is made again before a use (design §15.5).
pub const MAX_AGE_MS: i64 = 60_000;
/// The reason of every share before the first snapshot (design §15.4).
pub const NO_HOMES_YET: &str = "Fleet Deck does not know the project homes yet.";
/// The reason of a failed `tailscale status --json` (decision C3).
const STATUS_FAILED: &str = "tailscale status failed";

/// The inputs of the report that the station fixes at its start (decision
/// C2). `allowed_roots` and `mounts` are the policy values at the start.
#[derive(Debug, Clone)]
pub struct TaildriveSource {
    pub cli: TailscaleCli,
    /// The station data directory (`DataDir`).
    pub data_dir: PathBuf,
    /// `$HOME`.
    pub home: Option<PathBuf>,
    /// `fleetdeck_proto::member_deny` plus `member_deny` of `station.toml`.
    pub member_deny: MemberDeny,
    /// `CLAUDE_CONFIG_DIR` or `CODEX_HOME` of each provider instance.
    pub instance_homes: Vec<(ProviderKind, PathBuf)>,
    /// The allowed roots: a deny pattern is also tested relative to a root
    /// that holds the share.
    pub allowed_roots: Vec<PathBuf>,
    /// `[taildrive_mounts]`: `<machine>/<share>` to the mount path.
    pub mounts: BTreeMap<String, PathBuf>,
}

/// The limits of the bounded walk (design §15.4, B5).
#[derive(Debug, Clone)]
pub struct WalkLimits {
    pub depth: usize,
    pub entries: usize,
    pub time: Duration,
}

impl Default for WalkLimits {
    fn default() -> Self {
        Self {
            depth: 8,
            entries: 20_000,
            time: Duration::from_secs(2),
        }
    }
}

/// What `tailscale drive list` and `tailscale status --json` gave, before the
/// exposure check.
#[derive(Debug, Clone)]
pub struct Listing {
    pub state: TaildriveState,
    pub reason: Option<String>,
    pub machine: Option<String>,
    /// `CurrentTailnet.Name`.
    pub tailnet: Option<String>,
    pub shares: Vec<TaildriveShare>,
}

fn unavailable(reason: &str) -> (TaildriveState, Option<String>) {
    (TaildriveState::Unavailable, Some(reason.to_owned()))
}

/// The state and the reason of a failed `drive list` (design §15.2).
fn drive_failure(e: &TailscaleError) -> (TaildriveState, Option<String>) {
    match e {
        TailscaleError::NotInstalled => unavailable("tailscale is not installed"),
        TailscaleError::Timeout => unavailable("tailscale drive list took more than 3 s"),
        TailscaleError::TooLarge | TailscaleError::Parse(_) => (
            TaildriveState::Unknown,
            Some("Fleet Deck cannot read the share list".to_owned()),
        ),
        TailscaleError::Exit(n) => unavailable(&format!("tailscale drive list failed (exit {n})")),
        TailscaleError::NotRunning | TailscaleError::Signal | TailscaleError::Spawn(_) => {
            unavailable("tailscale drive list failed")
        }
    }
}

/// The first label of the DNS name, lower case (design §15.5 item 1).
fn machine_of(dns_name: &str) -> Option<String> {
    let label = dns_name.split('.').next().unwrap_or_default();
    (!label.is_empty()).then(|| label.to_lowercase())
}

/// Runs `tailscale drive list` and `tailscale status --json`.
pub async fn list(source: &TaildriveSource) -> Listing {
    let (drive, status) = tokio::join!(source.cli.drive_list(), source.cli.status());
    let (machine, tailnet) = match &status {
        Ok(s) => (machine_of(&s.this.dns_name), s.current_tailnet.clone()),
        Err(_) => (None, None),
    };
    let (state, reason, shares) = match drive {
        Err(e) => {
            let (state, reason) = drive_failure(&e);
            (state, reason, Vec::new())
        }
        Ok(_) if status.is_err() => (
            TaildriveState::Unknown,
            Some(STATUS_FAILED.to_owned()),
            Vec::new(),
        ),
        Ok(shares) => (TaildriveState::Ok, None, shares),
    };
    Listing {
        state,
        reason,
        machine,
        tailnet,
        shares,
    }
}

// ---------------------------------------------------------------- protected

/// The order of the kinds in a share (design §15.2).
const ORDER: [Exposure; 9] = [
    Exposure::ProjectHome,
    Exposure::Worktree,
    Exposure::DataDir,
    Exposure::ClaudeHome,
    Exposure::CodexHome,
    Exposure::Ssh,
    Exposure::Gnupg,
    Exposure::Aws,
    Exposure::MemberDeny,
];

fn sorted(kinds: &mut Vec<Exposure>) {
    kinds.sort_by_key(|k| ORDER.iter().position(|o| o == k).unwrap_or(ORDER.len()));
    kinds.dedup();
}

/// The protected paths of this station, canonical and existing (design
/// §15.4). A protected path that does not exist is skipped.
#[derive(Debug, Default)]
pub struct Protected {
    items: Vec<(Exposure, PathBuf, (u64, u64))>,
    /// The canonical homes, worktrees and allowed roots: a deny pattern is
    /// also tested relative to each of them that holds the share.
    bases: Vec<PathBuf>,
}

impl Protected {
    #[must_use]
    pub fn build(source: &TaildriveSource, inputs: &ExposureInputs) -> Self {
        let mut p = Self::default();
        let list = |paths: &[String], kind, p: &mut Self| {
            for path in paths {
                p.add(kind, Path::new(path), true);
            }
        };
        list(&inputs.homes, Exposure::ProjectHome, &mut p);
        list(&inputs.worktrees, Exposure::Worktree, &mut p);
        p.add(Exposure::DataDir, &source.data_dir, false);
        if let Some(home) = &source.home {
            p.add(Exposure::ClaudeHome, &home.join(".claude"), false);
            p.add(Exposure::CodexHome, &home.join(".codex"), false);
            p.add(Exposure::Ssh, &home.join(".ssh"), false);
            p.add(Exposure::Gnupg, &home.join(".gnupg"), false);
            p.add(Exposure::Aws, &home.join(".aws"), false);
        }
        for (kind, path) in &source.instance_homes {
            match kind {
                ProviderKind::Claude => p.add(Exposure::ClaudeHome, path, false),
                ProviderKind::Codex => p.add(Exposure::CodexHome, path, false),
                ProviderKind::Unknown => {}
            }
        }
        for root in &source.allowed_roots {
            if let Ok(canon) = std::fs::canonicalize(root) {
                p.bases.push(canon);
            }
        }
        p
    }

    fn add(&mut self, kind: Exposure, path: &Path, base: bool) {
        let Ok(canon) = std::fs::canonicalize(path) else {
            return;
        };
        let Ok(meta) = std::fs::metadata(&canon) else {
            return;
        };
        if base {
            self.bases.push(canon.clone());
        }
        self.items.push((kind, canon, (meta.dev(), meta.ino())));
    }

    /// The kinds of the protected paths that `canonical` is, contains or is
    /// inside (compared by path components).
    #[must_use]
    pub fn kinds_of(&self, canonical: &Path) -> Vec<Exposure> {
        let mut kinds: Vec<Exposure> = self
            .items
            .iter()
            .filter(|(_, p, _)| canonical.starts_with(p) || p.starts_with(canonical))
            .map(|(k, _, _)| *k)
            .collect();
        sorted(&mut kinds);
        kinds
    }

    fn kinds_of_id(&self, id: (u64, u64)) -> Vec<Exposure> {
        self.items
            .iter()
            .filter(|(_, _, i)| *i == id)
            .map(|(k, _, _)| *k)
            .collect()
    }

    /// The paths of `share` relative to each home, worktree or allowed root
    /// that holds it.
    fn prefixes(&self, share: &Path) -> Vec<String> {
        self.bases
            .iter()
            .filter_map(|b| share.strip_prefix(b).ok())
            .map(|rel| rel.to_string_lossy().into_owned())
            .filter(|rel| !rel.is_empty())
            .collect()
    }
}

/// True when a component of `canonical` matches a name pattern of the deny
/// list.
fn deny_component(deny: &MemberDeny, canonical: &Path) -> bool {
    canonical.components().any(|c| match c {
        Component::Normal(name) => name.to_str().is_some_and(|n| deny.denies(n)),
        _ => false,
    })
}

// -------------------------------------------------------------------- walk

/// The result of the walk of one share.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WalkResult {
    pub exposed: Vec<Exposure>,
    /// Why the walk is not complete (a limit, a link to a folder, a folder
    /// that cannot be read).
    pub incomplete: Option<String>,
}

impl WalkResult {
    fn incomplete(&mut self, why: &str) {
        self.incomplete.get_or_insert_with(|| why.to_owned());
    }
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// The bounded metadata walk of a share (design §15.4, B5). `root` is the
/// canonical share folder. Names and metadata only: no file is opened, and no
/// link is followed.
#[must_use]
pub fn walk_share(
    root: &Path,
    protected: &Protected,
    deny: &MemberDeny,
    limits: &WalkLimits,
) -> WalkResult {
    let mut out = WalkResult::default();
    let prefixes = protected.prefixes(root);
    let started = Instant::now();
    let Ok(base) = rustix::fs::open(root, dir_flags(), Mode::empty()) else {
        out.incomplete("a folder that cannot be read");
        return out;
    };
    let mut stack = vec![(String::new(), 0_usize)];
    let mut seen = 0_usize;
    while let Some((rel, depth)) = stack.pop() {
        let segments: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
        let dir = match crate::workspace::files::descend(&base, &segments) {
            Ok(dir) => dir,
            Err(rustix::io::Errno::NOENT) => continue,
            Err(_) => {
                out.incomplete("a folder that cannot be read");
                continue;
            }
        };
        let Ok(entries) = Dir::read_from(&dir) else {
            out.incomplete("a folder that cannot be read");
            continue;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                out.incomplete("a folder that cannot be read");
                break;
            };
            let raw = entry.file_name();
            if raw == c"." || raw == c".." {
                continue;
            }
            let name = raw.to_string_lossy().into_owned();
            seen += 1;
            if seen > limits.entries {
                out.incomplete("more than 20 000 entries");
                return finish(out);
            }
            if started.elapsed() >= limits.time {
                out.incomplete("the check took more than 2 s");
                return finish(out);
            }
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let stat = match rustix::fs::statat(&dir, raw, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                Err(rustix::io::Errno::NOENT) => continue,
                Err(_) => {
                    out.incomplete("a folder that cannot be read");
                    continue;
                }
            };
            if deny.denies(&path) || prefixes.iter().any(|p| deny.denies(&format!("{p}/{path}"))) {
                out.exposed.push(Exposure::MemberDeny);
            }
            out.exposed.extend(protected.kinds_of_id(id_of(&stat)));
            match FileType::from_raw_mode(stat.st_mode) {
                FileType::Symlink => link_entry(root, &path, protected, &mut out),
                FileType::Directory if depth + 1 > limits.depth => {
                    out.incomplete("deeper than 8 folders");
                }
                FileType::Directory => stack.push((path, depth + 1)),
                _ => {}
            }
        }
    }
    finish(out)
}

/// The device and the inode of a `statat`.
#[allow(clippy::unnecessary_cast, clippy::cast_sign_loss)]
fn id_of(stat: &rustix::fs::Stat) -> (u64, u64) {
    (stat.st_dev as u64, stat.st_ino as u64)
}

fn finish(mut out: WalkResult) -> WalkResult {
    sorted(&mut out.exposed);
    out
}

/// A symbolic link below the share: the target is resolved, not walked.
fn link_entry(root: &Path, rel: &str, protected: &Protected, out: &mut WalkResult) {
    let Ok(target) = std::fs::canonicalize(root.join(rel)) else {
        return;
    };
    out.exposed.extend(protected.kinds_of(&target));
    if target.is_dir() {
        out.incomplete("a link to a folder");
    }
}

// ---------------------------------------------------------------- the check

fn unknown(why: &str) -> (ExposureState, Vec<Exposure>, Option<String>) {
    (ExposureState::Unknown, Vec::new(), Some(why.to_owned()))
}

/// The exposure of one share: the union over its candidates when it is
/// ambiguous (design §15.4).
fn check_share(
    share: &TaildriveShare,
    protected: &Protected,
    source: &TaildriveSource,
    limits: &WalkLimits,
) -> (ExposureState, Vec<Exposure>, Option<String>) {
    let paths: Vec<&String> = match share.path_state {
        SharePathState::Missing => return (ExposureState::Clear, Vec::new(), None),
        SharePathState::Ok => vec![&share.path],
        SharePathState::Ambiguous => share.candidates.iter().collect(),
        SharePathState::Unknown => return unknown("the path of the share is not known"),
    };
    let mut result = WalkResult::default();
    for path in paths {
        let canon = match std::fs::canonicalize(path) {
            Ok(canon) => canon,
            // A candidate that does not exist cannot expose a path now.
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && share.path_state == SharePathState::Ambiguous =>
            {
                continue;
            }
            Err(_) => {
                result.incomplete("the path of the share cannot be resolved");
                continue;
            }
        };
        result.exposed.extend(protected.kinds_of(&canon));
        if deny_component(&source.member_deny, &canon) {
            result.exposed.push(Exposure::MemberDeny);
        }
        if canon.is_dir() {
            let walked = walk_share(&canon, protected, &source.member_deny, limits);
            result.exposed.extend(walked.exposed);
            if let Some(why) = walked.incomplete {
                result.incomplete(&why);
            }
        }
    }
    sorted(&mut result.exposed);
    if !result.exposed.is_empty() {
        (ExposureState::Exposed, result.exposed, None)
    } else if let Some(why) = result.incomplete {
        (ExposureState::Unknown, Vec::new(), Some(why))
    } else {
        (ExposureState::Clear, Vec::new(), None)
    }
}

/// The report for `listing` (design §15.2, §15.4). It reads the file system:
/// call it from a blocking context. `inventory_revision` is set by the
/// caller.
#[must_use]
pub fn build_report(
    source: &TaildriveSource,
    listing: &Listing,
    inputs: &ExposureInputs,
    limits: &WalkLimits,
    now_ms: i64,
) -> TaildriveReport {
    let protected = Protected::build(source, inputs);
    let mut shares = listing.shares.clone();
    for share in &mut shares {
        let (state, exposed, reason) = if inputs.snapshot_seen {
            check_share(share, &protected, source, limits)
        } else {
            unknown(NO_HOMES_YET)
        };
        share.exposure_state = state;
        share.exposed = exposed;
        share.unknown_reason = reason;
    }
    TaildriveReport {
        state: listing.state,
        reason: listing.reason.clone(),
        machine: listing.machine.clone(),
        shares,
        checked_at: now_ms,
        inventory_revision: 0,
    }
}

/// Which kinds a local path is, contains or is inside (design §15.5 item 2):
/// the protected paths only; no deny pattern and no walk. A path that cannot
/// be resolved is not a protected path.
pub async fn path_exposure(
    source: &TaildriveSource,
    store: &Store,
    path: &Path,
) -> Result<Vec<Exposure>, crate::store::StoreError> {
    let inputs = store.exposure_inputs().await?;
    let (source, path) = (source.clone(), path.to_path_buf());
    let kinds = tokio::task::spawn_blocking(move || {
        let canon = std::fs::canonicalize(&path).unwrap_or(path);
        Protected::build(&source, &inputs).kinds_of(&canon)
    })
    .await
    .unwrap_or_default();
    Ok(kinds)
}

// ----------------------------------------------------------------- runtime

fn ms_to_u64(ms: i64) -> u64 {
    u64::try_from(ms).unwrap_or(0)
}

async fn publish(
    runtime: &FleetGitRuntime,
    store: &Store,
    source: std::sync::Arc<TaildriveSource>,
    listing: Listing,
    limits: WalkLimits,
) -> Option<TaildriveReport> {
    let inputs = match store.exposure_inputs().await {
        Ok(inputs) => inputs,
        Err(e) => {
            tracing::warn!(error = %e, "the Taildrive exposure inputs were not read");
            return None;
        }
    };
    let now = store.now_ms();
    let (src, lst) = (source, listing.clone());
    let mut report =
        tokio::task::spawn_blocking(move || build_report(&src, &lst, &inputs, &limits, now))
            .await
            .map_err(|e| tracing::warn!(error = %e, "the Taildrive check failed"))
            .ok()?;
    Some(runtime.taildrive_publish(listing, &mut report, ms_to_u64(now)))
}

/// Runs `drive list` and `status --json` again, checks the exposure and
/// stores the report. `None` without a source.
pub async fn refresh(runtime: &FleetGitRuntime, store: &Store) -> Option<TaildriveReport> {
    refresh_with(runtime, store, WalkLimits::default()).await
}

/// [`refresh`] with the walk limits (tests).
pub async fn refresh_with(
    runtime: &FleetGitRuntime,
    store: &Store,
    limits: WalkLimits,
) -> Option<TaildriveReport> {
    let source = runtime.taildrive_source()?;
    let _gate = runtime.taildrive_gate().lock().await;
    run_locked(runtime, store, source, limits).await
}

/// The refresh while the caller holds the gate.
async fn run_locked(
    runtime: &FleetGitRuntime,
    store: &Store,
    source: std::sync::Arc<TaildriveSource>,
    limits: WalkLimits,
) -> Option<TaildriveReport> {
    let listing = list(&source).await;
    publish(runtime, store, source, listing, limits).await
}

/// A stored snapshot makes a new report: the exposure is checked again, the
/// revision grows, and `drive list` does not run again (decision C10).
pub async fn recompute(runtime: &FleetGitRuntime, store: &Store) -> Option<TaildriveReport> {
    let source = runtime.taildrive_source()?;
    let _gate = runtime.taildrive_gate().lock().await;
    let listing = runtime.taildrive_listing()?;
    publish(runtime, store, source, listing, WalkLimits::default()).await
}

/// The newest report when it is at most 60 s old.
fn current(runtime: &FleetGitRuntime, store: &Store) -> Option<TaildriveReport> {
    runtime
        .taildrive_report()
        .filter(|r| store.now_ms().saturating_sub(r.checked_at) <= MAX_AGE_MS)
}

/// The newest report, made again first when it is older than 60 s or when
/// there is none (design §15.5). `None` without a source.
pub async fn fresh(runtime: &FleetGitRuntime, store: &Store) -> Option<TaildriveReport> {
    let source = runtime.taildrive_source()?;
    if let Some(report) = current(runtime, store) {
        return Some(report);
    }
    let _gate = runtime.taildrive_gate().lock().await;
    // A caller that waited for the gate uses the report that the first one made.
    if let Some(report) = current(runtime, store) {
        return Some(report);
    }
    run_locked(runtime, store, source, WalkLimits::default()).await
}

/// The inputs of the Taildrive report from the policy and the process.
fn source_of(policy: &Policy, tailscale_bin: Option<&Path>, data_dir: &Path) -> TaildriveSource {
    let instance_homes = policy
        .instances()
        .iter()
        .filter_map(|(id, kind)| {
            let env = policy.setup_env(id)?;
            Some((*kind, crate::history::locate::resolve_home(env, *kind)?))
        })
        .collect();
    TaildriveSource {
        cli: fleetdeck_core::tailscale::TailscaleCli::locate(tailscale_bin),
        data_dir: data_dir.to_path_buf(),
        home: fleetdeck_core::paths::Env::from_process().home,
        member_deny: policy.member_deny().clone(),
        instance_homes,
        allowed_roots: policy.allowed_roots().to_vec(),
        mounts: policy.taildrive_mounts().clone(),
    }
}

/// Waits `every`, or until the shutdown; false when the station stops.
async fn pause(every: Duration, shutdown: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        () = tokio::time::sleep(every) => true,
        changed = shutdown.changed() => changed.is_ok() && !*shutdown.borrow(),
    }
}

/// Starts the Taildrive report (at start, then every 10 minutes) and the
/// refresh of the authorizations of the live sessions (every 5 minutes).
/// `tailscale_bin` is the setting of `station.toml`, used once (decision C2).
/// The source is set before this returns, so a start never misses it.
pub async fn start(
    runtime: &Arc<FleetGitRuntime>,
    manager: &Arc<SessionManager>,
    config: &fleetdeck_core::config::StationConfig,
    data_dir: &Path,
    shutdown: &watch::Receiver<bool>,
) -> Vec<JoinHandle<()>> {
    let (policy, data) = (manager.policy(), data_dir.to_path_buf());
    let config_bin = config.tailscale_bin.clone();
    let made =
        tokio::task::spawn_blocking(move || source_of(&policy, config_bin.as_deref(), &data)).await;
    match made {
        Ok(source) => runtime.set_taildrive_source(source),
        Err(e) => {
            tracing::warn!(error = %e, "the Taildrive source was not made");
            return Vec::new();
        }
    }
    let report = {
        let (runtime, manager, mut rx) =
            (Arc::clone(runtime), Arc::clone(manager), shutdown.clone());
        tokio::spawn(async move {
            loop {
                refresh(&runtime, manager.store()).await;
                if !pause(REFRESH_EVERY, &mut rx).await {
                    return;
                }
            }
        })
    };
    let sessions = {
        let (runtime, manager, mut rx) =
            (Arc::clone(runtime), Arc::clone(manager), shutdown.clone());
        tokio::spawn(async move {
            while pause(crate::artifact_share::SESSION_REFRESH_EVERY, &mut rx).await {
                crate::artifact_share::refresh_sessions(&runtime, manager.store()).await;
            }
        })
    };
    vec![report, sessions]
}

#[cfg(test)]
#[path = "taildrive_tests.rs"]
mod tests;
