//! The automatic cleanup of Fleet Git (FG-3, design §6.3, O12, O13).
//!
//! One task per station: 5 minutes after the start, then every 15 minutes, it
//! looks at each worktree session of a project with `cleanup = merged`. A
//! worktree goes to the trash only when every condition of O13 holds. The
//! checks run in the order of the design and stop at the first false one; that
//! one is recorded (`session_git.cleanup_state`, the event `session.cleanup`).
//! The trash itself is the existing trash flow (restorable): the loop calls
//! [`trash::decide_cleanup`], the effect moves the folder. A branch is never
//! deleted.
//!
//! The loop pauses while a deck link without `fleet_git.v1` is up
//! ([`Store::fleet_git_paused`], design §3.3).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use fleetdeck_proto::command::{CommandEnvelope, CommandOrigin, ReceiptState, SessionCommand};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::fleet_git::{
    CleanupMode, CleanupState, GitProjectView, HomeCheckState, PushMode,
};
use fleetdeck_proto::{CommandId, RejectCode, SessionId};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::policy_hub::PolicyHub;
use super::trash;
use crate::shell::Shells;
use crate::store::git_cleanup::CleanupRow;
use crate::store::{Caller, Store, StoreError};
use crate::workspace::fleet_git::{OptionsProvider, PushDestination, effective_push_url, redact};
use crate::workspace::trash::worktree_is_clean;
use crate::workspace::{Git, GitOptions};

/// The wait after the station start (design §6.3).
pub const FIRST_DELAY: Duration = Duration::from_secs(5 * 60);
/// The wait between two runs (design §6.3).
pub const INTERVAL: Duration = Duration::from_secs(15 * 60);

/// The time limit of the fetch (design §2.4).
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// The time limit of a local read (design §2.4).
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// The time limit of the merge proof (design §2.4).
const MERGED_TIMEOUT: Duration = Duration::from_secs(20);
/// The most bytes of the answer of a local read (design §2.4).
const SMALL_CAP: usize = 4 * 1024;
/// The most bytes of stderr that the fetch keeps (design §2.4).
const FETCH_STDERR: usize = 2 * 1024;
/// One hour in milliseconds.
const HOUR_MS: i64 = 3_600_000;
/// How long the station accepts the trash command that the loop makes.
const COMMAND_WINDOW_MS: i64 = 60_000;

/// What the loop needs.
#[derive(Clone)]
pub struct CleanupCtx {
    pub store: Store,
    pub policy: Arc<PolicyHub>,
    pub shells: Arc<Shells>,
    pub options: OptionsProvider,
}

/// What one run decided for one session. `Trashed` means that the trash
/// decision was accepted; the effect moves the folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupOutcome {
    pub session_id: SessionId,
    pub state: CleanupState,
    pub reason: Option<String>,
}

/// The first false condition: the state and the reason to record.
type Verdict = (CleanupState, Option<String>);

/// The result of the fetch of one repository in one run: `Err` is the reason
/// of `kept`.
type Fetches = HashMap<(String, String), Result<(), String>>;

/// Starts the cleanup task. It ends when `shutdown` becomes `true`.
#[must_use]
pub fn start(
    store: Store,
    policy: Arc<PolicyHub>,
    shells: Arc<Shells>,
    options: OptionsProvider,
    shutdown: watch::Receiver<bool>,
) -> Vec<JoinHandle<()>> {
    let ctx = CleanupCtx {
        store,
        policy,
        shells,
        options,
    };
    vec![tokio::spawn(run(ctx, shutdown))]
}

/// Waits `time`, or returns `true` at the shutdown.
async fn nap(shutdown: &mut watch::Receiver<bool>, time: Duration) -> bool {
    tokio::select! {
        () = tokio::time::sleep(time) => {}
        _ = shutdown.wait_for(|stop| *stop) => return true,
    }
    *shutdown.borrow()
}

async fn run(ctx: CleanupCtx, mut shutdown: watch::Receiver<bool>) {
    let mut wait = FIRST_DELAY;
    loop {
        if nap(&mut shutdown, wait).await {
            return;
        }
        wait = INTERVAL;
        if ctx.store.fleet_git_paused().load(Ordering::Acquire) {
            continue;
        }
        run_once(&ctx, ctx.store.now_ms()).await;
    }
}

/// One run of the loop (no waiting): every worktree session is checked, and
/// its state is recorded. Returns what was decided for each session that was
/// checked; a session that is skipped has no outcome (its project is gone,
/// has `cleanup = off`, or has no verified home, or its folder is gone).
pub(crate) async fn run_once(ctx: &CleanupCtx, now_ms: i64) -> Vec<CleanupOutcome> {
    let rows = match ctx.store.git_cleanup_rows().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "the cleanup rows are unreadable");
            return Vec::new();
        }
    };
    let mut fetches = Fetches::new();
    let mut outcomes = Vec::new();
    for row in rows {
        let session_id = row.session_id.clone();
        match check(ctx, &mut fetches, now_ms, &row).await {
            Ok(Some((state, reason))) => {
                if state != CleanupState::Trashed {
                    let recorded = ctx
                        .store
                        .git_cleanup_record(session_id.clone(), state, reason.clone())
                        .await;
                    if let Err(e) = recorded {
                        tracing::warn!(session = %session_id, error = %e, "a cleanup state was not recorded");
                    }
                }
                outcomes.push(CleanupOutcome {
                    session_id,
                    state,
                    reason,
                });
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(session = %session_id, error = %e, "a cleanup check failed");
            }
        }
    }
    outcomes
}

fn kept(reason: impl Into<String>) -> Result<Option<Verdict>, StoreError> {
    Ok(Some((CleanupState::Kept, Some(reason.into()))))
}

/// A Git that runs a local read in `dir`: the user configuration, no
/// confinement, the time limit `timeout`.
fn read_git(options: &GitOptions, dir: &Path, timeout: Duration) -> Git {
    Git::new(
        dir,
        GitOptions {
            timeout,
            isolated: false,
            confine: None,
            ..options.clone()
        },
    )
}

/// Runs `git <args>`: `(exit code, stdout as text)`. An error is the text of
/// a Git that did not run, timed out or printed too much.
async fn read(git: &Git, args: &[&str]) -> Result<(i32, String), String> {
    let (code, out, cut) = git
        .bounded(args, SMALL_CAP)
        .await
        .map_err(|e| e.to_string())?;
    if cut {
        return Err("Git printed too much text".to_owned());
    }
    Ok((code, String::from_utf8_lossy(&out).trim().to_owned()))
}

fn is_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The commit that `rev` names in `git`'s repository; `None` when it is not
/// a commit.
async fn commit_of(git: &Git, rev: &str) -> Result<Option<String>, String> {
    let probe = format!("{rev}^{{commit}}");
    match read(
        git,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &probe,
        ],
    )
    .await?
    {
        (0, out) if is_oid(&out) => Ok(Some(out)),
        (0 | 1, _) => Ok(None),
        (code, _) => Err(format!("git rev-parse failed ({code})")),
    }
}

/// Checks one session (design §6.3, the eight conditions in order). `None`:
/// the session is skipped, with no state.
async fn check(
    ctx: &CleanupCtx,
    fetches: &mut Fetches,
    now_ms: i64,
    row: &CleanupRow,
) -> Result<Option<Verdict>, StoreError> {
    let Some(project) = ctx.store.git_project(row.project_id.clone()).await? else {
        return Ok(None);
    };
    let view = &project.view;
    if view.settings.cleanup != CleanupMode::Merged
        || project.check_state != HomeCheckState::Verified
    {
        return Ok(None);
    }
    let Some(summary) = ctx.store.summary(row.session_id.clone()).await? else {
        return Ok(None);
    };
    // 1. ended: at least `cleanup_after_hours` since the latest end.
    let ended = matches!(summary.state, SessionState::Exited | SessionState::Lost);
    let wait = i64::from(view.settings.cleanup_after_hours).saturating_mul(HOUR_MS);
    match row.ended_at.filter(|_| ended) {
        Some(at) if now_ms >= at.saturating_add(wait) => {}
        Some(at) => {
            let due = at.saturating_add(wait);
            return Ok(Some((CleanupState::Waiting, Some(due.to_string()))));
        }
        // A live session has nothing to wait for; one that was marked before
        // it ran again goes back to `waiting`.
        None if row.cleanup_state.is_some() => {
            return Ok(Some((CleanupState::Waiting, None)));
        }
        None => return Ok(None),
    }
    // 2. not held: the holds of a land or a move arrive with phase 3.
    let Some(worktree) = summary.worktree.as_deref() else {
        return Ok(None);
    };
    if !Path::new(worktree).is_dir() {
        return Ok(None);
    }
    let options = (ctx.options)();
    // 3. on its branch, at the recorded tip.
    let tip = match branch_proof(&options, row, worktree).await {
        Ok(Ok(tip)) => tip,
        Ok(Err(reason)) => return kept(reason),
        Err(_) => return kept(UNKNOWN),
    };
    // 4. fetched: one fetch of the remote for each repository in a run.
    if let Err(reason) = fetched(ctx, fetches, &options, row, view).await {
        return kept(reason);
    }
    // 5. merged into the fetched `<remote>/<default>`.
    match merged(&options, row, view, &tip).await {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => return kept(reason),
        Err(_) => return kept(UNKNOWN),
    }
    // 6. clean: no change, no untracked file that is not ignored.
    let scan = GitOptions {
        timeout: READ_TIMEOUT,
        isolated: true,
        confine: None,
        ..options.clone()
    };
    match worktree_is_clean(&scan, Path::new(worktree)).await {
        Ok(true) => {}
        Ok(false) => return kept("dirty"),
        Err(_) => return kept(UNKNOWN),
    }
    // 7. pushed (only with push on).
    if view.settings.push != PushMode::Off && !pushed(row, &tip) {
        return kept("not_pushed");
    }
    // 8. the checks of the trash flow, and the trash itself.
    trash_it(ctx, now_ms, row, &tip).await
}

/// The reason of `kept` when the scan, a read or the merge proof could not
/// answer.
const UNKNOWN: &str = "scan_unknown";

/// Item 3: in the session worktree, `HEAD` is the recorded branch (a detached
/// HEAD is `detached`, another branch is `branch_changed`), and its tip
/// equals the tip of the branch in the home repository. The inner `Err` is
/// the reason of `kept`; the outer `Err` is a Git that could not answer.
async fn branch_proof(
    options: &GitOptions,
    row: &CleanupRow,
    worktree: &str,
) -> Result<Result<String, String>, String> {
    let here = read_git(options, Path::new(worktree), READ_TIMEOUT);
    let wanted = format!("refs/heads/{}", row.branch);
    match read(&here, &["symbolic-ref", "--quiet", "HEAD"]).await? {
        (0, out) if out == wanted => {}
        (0, _) => return Ok(Err("branch_changed".to_owned())),
        (1, _) => return Ok(Err("detached".to_owned())),
        (code, _) => return Err(format!("git symbolic-ref failed ({code})")),
    }
    let Some(tip) = commit_of(&here, "HEAD").await? else {
        return Ok(Err("detached".to_owned()));
    };
    let home = read_git(options, Path::new(&row.repo_root), READ_TIMEOUT);
    if commit_of(&home, &wanted).await?.as_deref() != Some(tip.as_str()) {
        return Ok(Err("branch_changed".to_owned()));
    }
    Ok(Ok(tip))
}

/// Item 4: the destination check of the remote (design §3.8) and one fetch.
/// A repository is fetched once in a run; the answer is kept for the other
/// sessions. The `Err` is the reason of `kept`.
async fn fetched(
    ctx: &CleanupCtx,
    fetches: &mut Fetches,
    options: &GitOptions,
    row: &CleanupRow,
    view: &GitProjectView,
) -> Result<(), String> {
    let remote = view.settings.push_remote.clone();
    let key = (row.repo_root.clone(), remote.clone());
    if let Some(done) = fetches.get(&key) {
        return done.clone();
    }
    let done = fetch(ctx, options, &row.repo_root, &remote, view).await;
    fetches.insert(key, done.clone());
    done
}

async fn fetch(
    ctx: &CleanupCtx,
    options: &GitOptions,
    repo: &str,
    remote: &str,
    view: &GitProjectView,
) -> Result<(), String> {
    let destination = PushDestination::Remote {
        name: remote.to_owned(),
        project: view.remote.clone(),
        clone_url: view.clone_url.clone(),
    };
    let allow_local = ctx.policy.current().allow_local_remotes();
    effective_push_url(
        options,
        Path::new(repo),
        &destination,
        &view.remote.dest,
        allow_local,
    )
    .await
    .map_err(|refusal| match refusal.code {
        fleetdeck_proto::fleet_git::code::GIT_TOO_OLD => refusal.code.to_owned(),
        other => format!("{other}: {}", refusal.text),
    })?;
    let net = Git::new(
        repo,
        GitOptions {
            timeout: FETCH_TIMEOUT,
            isolated: false,
            ..options.clone()
        },
    );
    let args = [
        "-c",
        "protocol.ext.allow=never",
        "fetch",
        "--prune",
        "--no-tags",
        "--no-recurse-submodules",
        "--no-write-fetch-head",
        "--",
        remote,
    ];
    match net.status_stderr(&args, &[], FETCH_STDERR).await {
        Ok((0, _)) => Ok(()),
        Ok((_, stderr)) => {
            tracing::info!(repo, "the cleanup fetch failed: {}", redact(&stderr));
            Err("fetch_failed".to_owned())
        }
        Err(e) => {
            tracing::info!(repo, "the cleanup fetch failed: {}", redact(&e.to_string()));
            Err("fetch_failed".to_owned())
        }
    }
}

/// Item 5: `tip` is an ancestor of `<remote>/<default>`. The default branch is
/// the `default_branch` setting, else the target of `refs/remotes/<remote>/HEAD`,
/// else `main`, else `master`; the first whose `<remote>/<name>` is a commit
/// wins. Only the ancestor proof counts: a squash merge is `not_merged`.
async fn merged(
    options: &GitOptions,
    row: &CleanupRow,
    view: &GitProjectView,
    tip: &str,
) -> Result<Result<(), String>, String> {
    let remote = &view.settings.push_remote;
    let home = read_git(options, Path::new(&row.repo_root), READ_TIMEOUT);
    let mut names: Vec<String> = Vec::new();
    names.extend(view.settings.default_branch.clone());
    let head = format!("refs/remotes/{remote}/HEAD");
    if let (0, target) = read(&home, &["symbolic-ref", "--quiet", &head]).await? {
        let prefix = format!("refs/remotes/{remote}/");
        names.extend(target.strip_prefix(&prefix).map(str::to_owned));
    }
    names.push("main".to_owned());
    names.push("master".to_owned());
    let mut default = None;
    for name in names.iter().filter(|n| !n.is_empty()) {
        if let Some(oid) = commit_of(&home, &format!("{remote}/{name}")).await? {
            default = Some(oid);
            break;
        }
    }
    let Some(default) = default else {
        return Ok(Err("no_default_branch".to_owned()));
    };
    let proof = read_git(options, Path::new(&row.repo_root), MERGED_TIMEOUT);
    match read(&proof, &["merge-base", "--is-ancestor", tip, &default]).await? {
        (0, _) => Ok(Ok(())),
        (1, _) => Ok(Err("not_merged".to_owned())),
        (code, _) => Err(format!("git merge-base failed ({code})")),
    }
}

/// Item 7: the push row says the last commit is pushed: `pushed`, with the
/// last pushed commit and the newest seen tip both equal to `tip`. A branch
/// that Fleet Git did not make has no push row, so it is never pushed.
fn pushed(row: &CleanupRow, tip: &str) -> bool {
    row.created_branch
        && row.push.as_ref().is_some_and(|p| {
            p.state == "pushed"
                && p.last_pushed_oid.as_deref() == Some(tip)
                && p.desired_oid.as_deref() == Some(tip)
        })
}

/// Item 8: the trash decision with the expectation `(refs/heads/<branch>,
/// tip)`. A refusal keeps the session with `trash_refused: <text>`.
async fn trash_it(
    ctx: &CleanupCtx,
    now_ms: i64,
    row: &CleanupRow,
    tip: &str,
) -> Result<Option<Verdict>, StoreError> {
    let session_id = row.session_id.clone();
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from(format!("cleanup-{session_id}-{now_ms}")),
        target_incarnation: None,
        retry_until: ctx.store.now_ms().saturating_add(COMMAND_WINDOW_MS),
        origin: CommandOrigin::Local,
        command: SessionCommand::WorktreeTrash {
            session_id: session_id.clone(),
        },
    };
    let policy = ctx.policy.current();
    let shells = Arc::clone(&ctx.shells);
    let expect = (format!("refs/heads/{}", row.branch), tip.to_owned());
    let sid = session_id.clone();
    let decided = ctx
        .store
        .decide(envelope, Caller::local(), move |d| {
            trash::decide_cleanup(d, &policy, Some(&shells), &sid, expect)?;
            Ok(super::decide::result(&sid, None))
        })
        .await?;
    if decided.receipt.state == ReceiptState::Accepted {
        return Ok(Some((CleanupState::Trashed, None)));
    }
    let (code, message) = decided
        .receipt
        .reject
        .map_or((RejectCode::InvalidState, String::new()), |r| {
            (r.code, r.message)
        });
    tracing::info!(session = %session_id, ?code, "the cleanup trash was refused");
    let mut text = redact(&message);
    if text.is_empty() {
        text = "the trash was refused".to_owned();
    }
    kept(format!("trash_refused: {text}"))
}

#[cfg(test)]
#[path = "fleet_git_cleanup_tests.rs"]
mod tests;
