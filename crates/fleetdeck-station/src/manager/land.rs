//! The land flow of a station (FG-8, design §11.4, §11.5, O23 to O26): the
//! admission decision of `session.land`, the engine that runs the admitted
//! runs, and the texts of their ends.
//!
//! - [`decide`] runs in the command transaction (the `submit` split, like the
//!   trash): the checks of §11.1, the pinned plan (§11.3), then the run row,
//!   the session hold and the `land` effect, all in one transaction.
//! - [`LandEngine`] polls the `land_runs` table (decision Q2, H3): a run in
//!   phase `admitted` that no task runs yet gets a task (`land_run.rs`, steps
//!   1 to 4 of §11.5). The `land` effect itself is `Confirmed` at once.
//! - `land_temp_cleanup.rs` removes the temporary folders (§11.6).
//! - `land_publish.rs` runs steps 5 to 7 (the writer, the forge), `land_reconcile.rs`
//!   recovers the runs of an earlier process, `land_abandon.rs` answers
//!   `fleet_git.land_abandon` and `fleet_git.land_status`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_proto::command::{CommandOrigin, CommandResult, SessionCommand};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::fleet_git::{
    HomeCheckState, LandMode, LandPlan, LandState, LocalRemotes, code, remote_identity,
};
use fleetdeck_proto::{Event, RejectCode, SessionId};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use super::decide::{reject, result};
use super::fleet_control;
use super::fleet_git_writer::{FleetGitWriter, LandLocks, LandTimeouts};
use super::policy::Policy;
use super::policy_hub::PolicyHub;
use super::trash::{custody, pending_shell, sharing_refusal, shell_in, shell_refusal};
use crate::coordinator::Bindings;
use crate::integrations::Integrations;
use crate::shell::Shells;
use crate::store::git_state;
use crate::store::land_runs::{self, LandRunRow, NewLandRun};
use crate::store::session_holds::{self, KIND_LAND};
use crate::store::{DecideError, Decision, EffectPayload, NewEvent, Store};
use crate::workspace::fleet_git::OptionsProvider;

// (FG-9) The source side of a move. Attached here because `manager/mod.rs`
// and `decide.rs` are full.
#[path = "move_session.rs"]
pub(super) mod move_session;

/// How often the engine looks for an admitted run when nothing wakes it.
#[cfg(not(test))]
const POLL: Duration = Duration::from_secs(1);
/// The poll, short in the tests.
#[cfg(test)]
const POLL: Duration = Duration::from_millis(50);
/// The longest `land_id`, in bytes (decision Q5).
const MAX_LAND_ID: usize = 128;
/// The longest check command, in characters (the setup command rule).
const MAX_CHECK_CMD: usize = 4096;
/// The longest pull request title, in characters.
const MAX_TITLE: usize = 256;
/// The longest pull request body, in bytes.
const MAX_BODY: usize = 64 * 1024;

/// The refusal of a decision: `InvalidState` with `<code>: <text>` (the
/// convention of design §4.3 and `decide_fleet_git.rs`).
fn refuse(code: &str, text: &str) -> DecideError {
    reject(RejectCode::InvalidState, format!("{code}: {text}"))
}

/// The refusal of an input that is not valid.
fn invalid(text: &str) -> DecideError {
    reject(
        RejectCode::InvalidCommand,
        format!("invalid_params: {text}"),
    )
}

/// The refusal of a land that reaches `decide::decide` (a direct call has no
/// shell list).
pub(super) fn refuse_direct() -> DecideError {
    reject(
        RejectCode::Unsupported,
        "the session manager decides a land with the shells of the station",
    )
}

const PLAN_MISMATCH: &str = "The land plan does not match this session.";
const NOT_READY: &str = "land_not_ready";

fn land_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_LAND_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// The input caps of the plan (decision Q5).
fn check_caps(land_id: &str, plan: &LandPlan) -> Result<(), DecideError> {
    if !land_id_ok(land_id) {
        return Err(invalid("The land id is not valid."));
    }
    if let Some(cmd) = &plan.check_cmd
        && (cmd.is_empty() || cmd.chars().count() > MAX_CHECK_CMD || cmd.contains('\0'))
    {
        return Err(invalid("The check command is not valid."));
    }
    if plan.pr_title.chars().count() > MAX_TITLE || plan.pr_body.len() > MAX_BODY {
        return Err(invalid("The pull request text is too long."));
    }
    Ok(())
}

/// The session must be ended, or idle (design §11.1).
pub(super) fn idle_or_ended(summary: &fleetdeck_proto::SessionSummary) -> bool {
    let ready = match summary.state {
        SessionState::Exited | SessionState::Lost => true,
        SessionState::Running => {
            summary.current_run.is_none()
                && summary.current_turn.is_none()
                && summary.open_requests.is_empty()
        }
        _ => false,
    };
    ready && summary.terminal.is_none()
}

/// Decides `session.land` (design §11.4 step 1 to 3). The session manager
/// calls it with the shell list of the station.
///
/// # Errors
/// A stored rejection (`<code>: <text>`), or a store error.
pub(super) fn decide(
    d: &mut Decision<'_>,
    policy: &Policy,
    bindings: &Bindings,
    origin: CommandOrigin,
    shells: Option<&Shells>,
    command: SessionCommand,
) -> Result<CommandResult, DecideError> {
    // (FG-9) `submit` routes `MoveOut` here because it needs the shells.
    if matches!(command, SessionCommand::MoveOut { .. }) {
        return move_session::decide_out(d, policy, bindings, origin, shells, command);
    }
    if !matches!(origin, CommandOrigin::Board | CommandOrigin::Coordinator) {
        return Err(reject(
            RejectCode::Forbidden,
            "only the owner's board or a coordinator lands a session",
        ));
    }
    fleet_control::check(d, policy, bindings, &command)?;
    let SessionCommand::Land {
        session_id,
        land_id,
        plan,
    } = command
    else {
        return Err(refuse_direct());
    };
    let plan = *plan;
    check_caps(&land_id, &plan)?;
    let conn = d.connection();
    if crate::store::land_fences::is_fenced(conn, &land_id)? {
        return Err(refuse(
            code::LAND_FENCED,
            "This land was stopped before; it does not run again.",
        ));
    }
    if land_runs::get(conn, &land_id)?.is_some() {
        return Err(invalid("This land id exists already."));
    }
    // (FG-8) A land or a move holds its session.
    session_holds::check_free(conn, &session_id)?;
    let summary = d
        .summary(&session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    let stored = summary.worktree.clone().ok_or_else(|| {
        reject(
            RejectCode::InvalidState,
            "the session has no worktree of its own",
        )
    })?;
    if !idle_or_ended(&summary) {
        return Err(refuse(
            NOT_READY,
            "The session is busy. Wait until it is idle or ended.",
        ));
    }
    plan_checks(d, &session_id, &plan)?;
    worktree_checks(d, policy, shells, &session_id, &stored)?;
    if plan.check_cmd.is_some() && !policy.allow_check_cmd() {
        return Err(refuse(
            code::CHECK_DISABLED,
            "This station does not run check commands (allow_check_cmd = false).",
        ));
    }
    admit(d, &session_id, &land_id, plan)?;
    Ok(result(&session_id, None))
}

/// The row checks and the plan checks (design §11.1, §11.3).
fn plan_checks(
    d: &Decision<'_>,
    session_id: &SessionId,
    plan: &LandPlan,
) -> Result<(), DecideError> {
    let conn = d.connection();
    let not_ready = || refuse(NOT_READY, "This session has no Fleet Git branch to land.");
    let facts = land_runs::session_facts(conn, session_id)?.ok_or_else(not_ready)?;
    if facts.kind != "worktree" || !facts.created_branch || facts.publish_ref.is_none() {
        return Err(not_ready());
    }
    let project = git_state::project_row(conn, &facts.project_id)?;
    let Some(project) = project.filter(|p| p.check_state == HomeCheckState::Verified) else {
        return Err(refuse(NOT_READY, "The home folder is not verified."));
    };
    if plan.home_generation != project.home_generation {
        return Err(refuse(
            code::STALE_GENERATION,
            "The project home changed. Land again.",
        ));
    }
    let slug_ok = facts
        .slug
        .as_deref()
        .zip(facts.publish_ref.as_deref())
        .is_some_and(|(slug, publish)| publish.starts_with(&format!("fd/{slug}/")));
    if plan.project_id != facts.project_id
        || Some(plan.branch_ref.as_str()) != facts.branch.as_deref()
        || Some(plan.publish_ref.as_str()) != facts.publish_ref.as_deref()
        || !slug_ok
    {
        return Err(invalid(PLAN_MISMATCH));
    }
    if plan.mode == LandMode::Unknown {
        return Err(refuse(
            code::UNSUPPORTED_MODE,
            "This station does not know the land mode. Update the station.",
        ));
    }
    if plan.mode == LandMode::Pr {
        let Some(forge) = &plan.forge else {
            return Err(invalid("A pull request land needs a forge."));
        };
        let identity = remote_identity(&plan.branch_target.dest, LocalRemotes::Allow)
            .map_err(|_| invalid(PLAN_MISMATCH))?;
        let wanted = format!("{}/{}", forge.host, forge.repo).to_lowercase();
        if identity.repo.to_lowercase() != wanted {
            return Err(refuse(
                code::LAND_PR_NEEDS_FORGE_BRANCH,
                &format!(
                    "Pull request mode needs the branch on {}/{}, but pushes go to {}. \
                     Choose Merge in the project settings, or clear the WIP remote.",
                    forge.host, forge.repo, identity.repo
                ),
            ));
        }
    }
    if facts
        .push_dest
        .as_deref()
        .is_some_and(|dest| dest != plan.branch_target.dest)
    {
        return Err(refuse(
            code::REMOTE_MISMATCH,
            "The push destination changed since the land was requested.",
        ));
    }
    Ok(())
}

/// Every existing worktree safety check of the trash decision that applies to
/// a land (design §11.4 step 1); the folder checks do not, because a land
/// never moves the worktree.
pub(super) fn worktree_checks(
    d: &Decision<'_>,
    policy: &Policy,
    shells: Option<&Shells>,
    session_id: &SessionId,
    stored: &str,
) -> Result<(), DecideError> {
    if d.unsettled_effects(session_id)? > 0 {
        return Err(reject(
            RejectCode::InvalidState,
            "the session has work that is not done yet",
        ));
    }
    if d.open_worktree_op(session_id)?.is_some() {
        return Err(reject(
            RejectCode::InvalidState,
            "the worktree is being moved; wait",
        ));
    }
    if let Some(other) = d.open_revert_over(stored)? {
        return Err(reject(
            RejectCode::InvalidState,
            format!(
                "a revert of session `{}` holds this worktree",
                other.revert.session_id
            ),
        ));
    }
    custody(d, stored)?;
    sharing_refusal(d, stored, session_id)?;
    if shell_in(shells, stored) {
        return Err(shell_refusal());
    }
    pending_shell(d, policy, stored)
}

/// Design §11.4 steps 2 and 3: the run row, the hold, the `land` effect and
/// the first `session.land` event.
fn admit(
    d: &mut Decision<'_>,
    session_id: &SessionId,
    land_id: &str,
    plan: LandPlan,
) -> Result<(), DecideError> {
    let now = d.now();
    let new = NewLandRun {
        land_id: land_id.to_owned(),
        command_id: d.command_id().as_str().to_owned(),
        session_id: session_id.clone(),
        plan,
        authority: d.authority().cloned(),
    };
    land_runs::insert(d.connection(), &new, now)?;
    session_holds::insert(d.connection(), session_id, KIND_LAND, land_id, now)?;
    let row = land_runs::get(d.connection(), land_id)?
        .ok_or_else(|| reject(RejectCode::InvalidState, "the land run was not stored"))?;
    d.append(
        session_id,
        NewEvent::from(Event::SessionLand(Box::new(land_runs::info_of(&row)))),
    )?;
    d.enqueue(
        session_id,
        EffectPayload::Land {
            land_id: land_id.to_owned(),
            authority: None,
        },
    )?;
    Ok(())
}

/// Gives the integration service of the moment (a publish can replace it).
pub type IntegrationsProvider = Arc<dyn Fn() -> Arc<Integrations> + Send + Sync>;

/// What the land tasks need besides the store and the policy.
pub struct Deps {
    /// The one writer of the station (design §5.5).
    pub writer: FleetGitWriter,
    /// The per-land locks that the writer and the abandonment share.
    pub locks: Arc<LandLocks>,
    pub integrations: IntegrationsProvider,
    pub timeouts: LandTimeouts,
}

/// What the tasks of a station share.
pub(super) struct Shared {
    pub(super) store: Store,
    pub(super) policy: Arc<PolicyHub>,
    pub(super) options: OptionsProvider,
    pub(super) shutdown: watch::Receiver<bool>,
    /// Pinged after each phase change: the cleanup of the temporary folders
    /// runs (design §11.6).
    pub(super) cleanup: Arc<Notify>,
    pub(super) writer: FleetGitWriter,
    pub(super) locks: Arc<LandLocks>,
    pub(super) integrations: IntegrationsProvider,
    pub(super) timeouts: LandTimeouts,
}

/// The land engine of a station (decision Q2, H3). It runs the admitted
/// runs, one task for each.
#[derive(Clone)]
pub struct LandEngine {
    wake: Arc<Notify>,
    pub(super) store: Store,
    pub(super) locks: Arc<LandLocks>,
    pub(super) timeouts: LandTimeouts,
}

impl std::fmt::Debug for LandEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LandEngine").finish_non_exhaustive()
    }
}

impl LandEngine {
    /// Wakes the poll: a run was admitted.
    pub fn wake(&self) {
        self.wake.notify_one();
    }
}

/// Starts the engine and the cleanup of the temporary folders. The tasks end
/// when `shutdown` becomes `true`.
#[must_use]
pub fn start(
    store: Store,
    policy: Arc<PolicyHub>,
    options: OptionsProvider,
    shutdown: watch::Receiver<bool>,
    deps: Deps,
) -> (LandEngine, Vec<JoinHandle<()>>) {
    let wake = Arc::new(Notify::new());
    let cleanup = Arc::new(Notify::new());
    let started_at = store.now_ms();
    let shared = Arc::new(Shared {
        store: store.clone(),
        policy,
        options: Arc::clone(&options),
        shutdown: shutdown.clone(),
        cleanup: Arc::clone(&cleanup),
        writer: deps.writer,
        locks: Arc::clone(&deps.locks),
        integrations: deps.integrations,
        timeouts: deps.timeouts,
    });
    let engine = LandEngine {
        wake: Arc::clone(&wake),
        store: store.clone(),
        locks: deps.locks,
        timeouts: deps.timeouts,
    };
    let handles = vec![
        tokio::spawn(poll(Arc::clone(&shared), wake, started_at)),
        tokio::spawn(super::land_temp_cleanup::run(store, cleanup, shutdown)),
    ];
    (engine, handles)
}

/// What a task does with a run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Task {
    /// Steps 1 to 4, then the publication.
    Run,
    /// The recovery of a run of an earlier process, or a run that waits for
    /// its next reconciliation (design §11.5).
    Reconcile,
}

/// The poll. A run in phase `admitted` that this process admitted and that
/// has no task gets a run task. The runs that an earlier process left are
/// recovered once at the start, in the first pass (`land_reconcile`); after
/// that a run with `reconciling = 1` gets a task when its `next_reconcile_at`
/// has come.
async fn poll(shared: Arc<Shared>, wake: Arc<Notify>, started_at: i64) {
    let active: Arc<Mutex<HashSet<String>>> = Arc::default();
    let mut shutdown = shared.shutdown.clone();
    let mut booted = false;
    loop {
        match shared.store.land_runs_running().await {
            Ok(rows) => {
                let now = shared.store.now_ms();
                for row in rows {
                    if is_new(&row, started_at) {
                        spawn_run(&shared, &active, row, Task::Run);
                    } else if !booted || reconcile_due(&row, now) {
                        spawn_run(&shared, &active, row, Task::Reconcile);
                    }
                }
                booted = true;
            }
            Err(e) => tracing::warn!(error = %e, "the land runs are unreadable"),
        }
        tokio::select! {
            () = wake.notified() => {}
            () = tokio::time::sleep(POLL) => {}
            _ = shutdown.wait_for(|stop| *stop) => return,
        }
    }
}

/// A run that this process admitted and that no step has touched.
fn is_new(row: &LandRunRow, started_at: i64) -> bool {
    row.phase == fleetdeck_proto::fleet_git::LandPhase::Admitted
        && row.state == LandState::Running
        && row.created_at >= started_at
        && row.attempt == 1
}

/// A run that waits for its reconciliation and whose time has come.
fn reconcile_due(row: &LandRunRow, now: i64) -> bool {
    row.reconciling && row.next_reconcile_at.is_some_and(|at| at <= now)
}

fn spawn_run(
    shared: &Arc<Shared>,
    active: &Arc<Mutex<HashSet<String>>>,
    row: LandRunRow,
    task: Task,
) {
    let first = active
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(row.land_id.clone());
    if !first {
        return;
    }
    let (shared, active) = (Arc::clone(shared), Arc::clone(active));
    tokio::spawn(async move {
        let land_id = row.land_id.clone();
        match task {
            Task::Run => super::land_run::run(&shared, row).await,
            Task::Reconcile => super::land_reconcile::run(&shared, row).await,
        }
        shared.locks.release(&land_id);
        active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&land_id);
        shared.cleanup.notify_one();
    });
}

/// The first seven characters of an object id (design §11.10).
pub(super) fn oid7(oid: &str) -> &str {
    oid.get(..7).unwrap_or(oid)
}

/// The publication sentence that ends every text of a land that did not land
/// (design §11.10, decision Q11): "Nothing was pushed." while no branch or
/// default push is recorded, else the sentence for the published branch.
pub(super) fn publication_sentence(row: &LandRunRow) -> String {
    match &row.branch_published_oid {
        Some(oid) if row.default_published_oid.is_none() => format!(
            "The rebased branch {} is on {} ({}); {} did not change.",
            row.plan.branch_ref,
            row.plan.branch_target.target,
            oid7(oid),
            row.default_ref.as_deref().unwrap_or("the default branch"),
        ),
        _ => "Nothing was pushed.".to_owned(),
    }
}

/// The text of a terminal land that did not land: `<first sentence>`, the
/// optional tail, then the publication sentence (decision Q11).
pub(super) fn land_text(row: &LandRunRow, first: &str, tail: Option<&str>) -> String {
    let mut text = first.to_owned();
    if let Some(tail) = tail.filter(|t| !t.is_empty()) {
        text.push('\n');
        text.push_str(tail);
    }
    text.push('\n');
    text.push_str(&publication_sentence(row));
    text
}

#[cfg(test)]
#[path = "land_rig.rs"]
mod rig;

#[cfg(test)]
#[path = "land_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "land_run_tests.rs"]
mod run_tests;

#[cfg(test)]
#[path = "land_rig_stage.rs"]
mod stage;

#[cfg(test)]
#[path = "land_publish_tests.rs"]
mod publish_tests;

#[cfg(test)]
#[path = "land_reconcile_tests.rs"]
mod reconcile_tests;

#[cfg(test)]
#[path = "land_abandon_tests.rs"]
mod abandon_tests;

#[cfg(test)]
#[path = "land_gate_tests.rs"]
mod gate_tests;
