//! The publication of a land (FG-8, design §11.5 steps 5 to 7): the branch
//! push, the pull request, the push to the default branch, the confirmation
//! of an uncertain write, and attempt 2. Every remote write goes through the
//! gated start of `fleet_git_writer.rs` (the per-land lock, the intent, the
//! authority check, then the spawn). Every transaction here carries the fence
//! rule of the store. An outcome that cannot be confirmed stays open:
//! `reconciling = 1`, never "assume failed".
//!
//! - [`branch_job`] and [`default_job`] run inside the writer (decision 15);
//! - `land_pr.rs` runs step 6 on the land task (a forge write is not a Git write);
//! - [`drive`] picks the next action from the stored row, so a run of an
//!   earlier process continues the same way (`land_reconcile.rs`).

use std::path::PathBuf;
use std::sync::Arc;

use fleetdeck_proto::fleet_git::{LandMode, LandPhase, LandState, code};

use super::fleet_git_writer::{Gate, Gated, LandLocks, LandTimeouts, WriteJob, gated_write};
use super::land::{Shared, land_text};
use super::policy_hub::PolicyHub;
use crate::store::Store;
use crate::store::land_publish::{
    BranchStored, OutcomeChange, Pending, PendingChange, PushUpsert, ReconcileChange, Step,
    StepEnd, WRITE_APPLIED, WRITE_NOT_STARTED, WRITE_REJECTED, WRITE_UNKNOWN,
};
use crate::store::land_runs::{
    LandRunRow, PENDING_BRANCH_PUSH, PENDING_DEFAULT_PUSH, PENDING_PR_CREATE,
};
use crate::workspace::GitError;
use crate::workspace::GitOptions;
use crate::workspace::fleet_git::{
    LandPush, OptionsProvider, PushDestination, PushEnd, PushFailure, PushOutcome, RemoteTip,
    backoff_delay, effective_push_url, fetch_default, is_ancestor, local_tip, redact, remote_tip,
};

/// What the publication steps need from a station.
#[derive(Clone)]
pub(super) struct Ctx {
    pub(super) store: Store,
    pub(super) policy: Arc<PolicyHub>,
    pub(super) options: OptionsProvider,
    pub(super) locks: Arc<LandLocks>,
    pub(super) timeouts: LandTimeouts,
}

impl Shared {
    pub(super) fn ctx(&self) -> Ctx {
        Ctx {
            store: self.store.clone(),
            policy: Arc::clone(&self.policy),
            options: Arc::clone(&self.options),
            locks: Arc::clone(&self.locks),
            timeouts: self.timeouts,
        }
    }
}

/// The home folder of a run and the data of its session.
struct Home {
    folder: PathBuf,
    slug: String,
    clone_url: String,
}

impl Ctx {
    fn git(&self) -> GitOptions {
        (self.options)()
    }

    /// The run, when it is still `running`.
    pub(super) async fn running(&self, land_id: &str) -> Option<LandRunRow> {
        match self.store.land_run(land_id.to_owned()).await {
            Ok(Some(row)) if row.state == LandState::Running => Some(row),
            Ok(_) => None,
            Err(e) => {
                tracing::error!(land = %land_id, error = %e, "a land run is unreadable");
                None
            }
        }
    }

    /// One transaction on a run. `false`: dropped (fenced, ended, or a store
    /// error).
    pub(super) async fn step(&self, land_id: &str, step: Step) -> bool {
        match self.store.land_step(land_id.to_owned(), step).await {
            Ok(done) => done,
            Err(e) => {
                tracing::error!(land = %land_id, error = %e, "a land step was not stored");
                false
            }
        }
    }

    async fn home(&self, row: &LandRunRow) -> Option<Home> {
        let project = self
            .store
            .git_project(row.plan.project_id.clone())
            .await
            .ok()??;
        let session = self
            .store
            .git_session(row.session_id.clone())
            .await
            .ok()??;
        Some(Home {
            folder: PathBuf::from(
                project
                    .canonical_path
                    .as_ref()
                    .unwrap_or(&project.home_path),
            ),
            slug: session.slug?,
            clone_url: project.view.clone_url.clone(),
        })
    }

    /// The end of a run without a landing: the pending write is cleared; the
    /// text has the publication sentence of design §11.10.
    async fn end(&self, row: &LandRunRow, code: Option<&str>, first: &str, state: LandState) {
        let fresh = self
            .running(&row.land_id)
            .await
            .unwrap_or_else(|| row.clone());
        let step = Step {
            pending: PendingChange::Clear,
            reconcile: ReconcileChange::Clear,
            end: Some(StepEnd {
                state,
                code: code.map(str::to_owned),
                text: Some(land_text(&fresh, first, None)),
            }),
            ..Step::default()
        };
        self.step(&row.land_id, step).await;
    }

    pub(super) async fn fail(&self, row: &LandRunRow, code: &str, first: &str) {
        self.end(row, Some(code), first, LandState::Failed).await;
    }

    /// The read could not reach the remote or the forge: the run waits with
    /// `reconciling = 1` (design §11.5, M1). The FG-2 backoff gives the time.
    pub(super) async fn wait(&self, row: &LandRunRow) {
        let attempts = row.reconcile_attempts.saturating_add(1);
        let delay = i64::try_from(backoff_delay(attempts).as_millis()).unwrap_or(i64::MAX);
        let step = Step {
            reconcile: ReconcileChange::Set {
                attempts,
                next_at: self.store.now_ms().saturating_add(delay),
            },
            event: !row.reconciling,
            ..Step::default()
        };
        self.step(&row.land_id, step).await;
    }
}

/// Which push of step 5 or 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Branch,
    Default,
}

impl Which {
    fn action(self) -> &'static str {
        match self {
            Self::Branch => PENDING_BRANCH_PUSH,
            Self::Default => PENDING_DEFAULT_PUSH,
        }
    }

    fn intent(self) -> LandPhase {
        match self {
            Self::Branch => LandPhase::BranchPushIntent,
            Self::Default => LandPhase::DefaultPushIntent,
        }
    }

    /// The phase a fresh write starts from.
    fn start_phase(self) -> LandPhase {
        match self {
            Self::Branch => LandPhase::Checked,
            Self::Default => LandPhase::BranchPushed,
        }
    }
}

/// The operands of a write (the stored `pending_*` columns).
#[derive(Debug, Clone)]
struct Ops {
    url: String,
    ref_name: String,
    oid: String,
    lease: String,
    expect: String,
}

fn ops_of(row: &LandRunRow) -> Ops {
    Ops {
        url: row.pending_url.clone().unwrap_or_default(),
        ref_name: row.pending_ref.clone().unwrap_or_default(),
        oid: row.pending_oid.clone().unwrap_or_default(),
        lease: row.pending_lease.clone().unwrap_or_default(),
        expect: row.pending_expect.clone().unwrap_or_default(),
    }
}

/// How the last write ended, when it did not apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Rejected,
    Unknown,
}

enum Wrote {
    /// Applied, or the run was dropped: nothing more to do in this job.
    Done,
    Failed {
        kind: Kind,
        failure: PushFailure,
        ops: Ops,
    },
}

/// The push of a land that did not start (a spawn error is an unknown
/// outcome that the read decides).
fn spawn_failed(e: &GitError) -> PushEnd {
    PushEnd {
        outcome: PushOutcome::Unknown,
        failure: PushFailure {
            code: code::PUSH_FAILED,
            text: format!("Push failed: {}", redact(&e.to_string())),
        },
    }
}

struct Publish<'a> {
    ctx: &'a Ctx,
    which: Which,
    home: Home,
    row: LandRunRow,
    ops: Option<Ops>,
    kind: Kind,
    failure: Option<PushFailure>,
    retried: bool,
}

/// The branch push job (design §11.5 step 5), inside the writer.
pub(super) async fn branch_job(ctx: &Ctx, land_id: &str) {
    publish(ctx, land_id, Which::Branch).await;
}

/// The default push job (design §11.5 step 7), inside the writer.
pub(super) async fn default_job(ctx: &Ctx, land_id: &str) {
    publish(ctx, land_id, Which::Default).await;
}

async fn publish(ctx: &Ctx, land_id: &str, which: Which) {
    let Some(row) = ctx.running(land_id).await else {
        return;
    };
    let recovery = row.pending_action.as_deref() == Some(which.action());
    if !recovery && (row.pending_action.is_some() || row.phase != which.start_phase()) {
        return;
    }
    let Some(home) = ctx.home(&row).await else {
        if recovery {
            ctx.wait(&row).await;
        } else {
            ctx.fail(
                &row,
                code::NOT_A_HOME,
                "This folder is not a verified home of the project.",
            )
            .await;
        }
        return;
    };
    let ops = recovery.then(|| ops_of(&row));
    let mut job = Publish {
        ctx,
        which,
        home,
        row,
        ops,
        kind: Kind::Unknown,
        failure: None,
        retried: false,
    };
    job.run().await;
}

impl Publish<'_> {
    fn id(&self) -> &str {
        &self.row.land_id
    }

    fn default_ref(&self) -> String {
        self.row
            .default_ref
            .clone()
            .unwrap_or_else(|| "the default branch".to_owned())
    }

    async fn run(&mut self) {
        let mut read_first = self.ops.is_some();
        loop {
            let Some(row) = self.ctx.running(&self.row.land_id).await else {
                return;
            };
            self.row = row;
            if read_first && !self.read().await {
                return;
            }
            match self.write().await {
                Wrote::Done => return,
                Wrote::Failed { kind, failure, ops } => {
                    self.kind = kind;
                    self.failure = Some(failure);
                    self.ops = Some(ops);
                    read_first = true;
                }
            }
        }
    }

    /// The reconciliation read (design §11.5). `true`: write again.
    async fn read(&mut self) -> bool {
        let Some(ops) = self.ops.clone() else {
            return false;
        };
        let options = self.ctx.git();
        let tip = remote_tip(
            &options,
            &self.home.folder,
            &ops.url,
            &ops.ref_name,
            self.ctx.timeouts.read,
        )
        .await;
        let RemoteTip::Answered(tip) = tip else {
            self.ctx.wait(&self.row).await;
            return false;
        };
        if tip.as_deref() == Some(ops.oid.as_str()) {
            self.success(&ops, OutcomeChange::Keep).await;
            return false;
        }
        let base = match self.which {
            Which::Branch => &ops.lease,
            Which::Default => &ops.expect,
        };
        let at_base = match &tip {
            Some(t) => t == base,
            None => self.which == Which::Branch && base.is_empty(),
        };
        if at_base {
            // Confirmed not applied. A rejected write ends; an unknown one
            // pushes again once.
            if self.kind == Kind::Rejected || self.retried {
                self.push_failed().await;
                return false;
            }
            self.retried = true;
            return true;
        }
        match self.which {
            Which::Branch => {
                let first = format!(
                    "The branch {} changed on {} during the land.",
                    self.row.plan.branch_ref, self.row.plan.branch_target.target
                );
                self.ctx.fail(&self.row, code::BRANCH_MOVED, &first).await;
                false
            }
            Which::Default => {
                self.default_moved(&ops).await;
                false
            }
        }
    }

    /// The default branch moved: landed when `R` is below the new tip, else
    /// attempt 2, else `remote_moved` (design §11.5 step 7).
    async fn default_moved(&self, ops: &Ops) {
        let plan = &self.row.plan;
        let options = self.ctx.git();
        let default_ref = self.default_ref();
        if !fetch_default(&options, &self.home.folder, &plan.fetch_remote).await {
            self.ctx.wait(&self.row).await;
            return;
        }
        let tracking = format!("refs/remotes/{}/{default_ref}", plan.fetch_remote);
        let below = is_ancestor(
            &options,
            &self.home.folder,
            &ops.oid,
            &tracking,
            self.ctx.timeouts.read,
        )
        .await;
        match below {
            Some(true) => self.success(ops, OutcomeChange::Keep).await,
            Some(false) if self.row.attempt == 1 => {
                let step = Step {
                    attempt: Some(2),
                    phase: Some(LandPhase::Admitted),
                    pending: PendingChange::Clear,
                    reconcile: ReconcileChange::Clear,
                    event: true,
                    ..Step::default()
                };
                self.ctx.step(self.id(), step).await;
            }
            Some(false) => {
                let first = format!(
                    "{}/{default_ref} moved twice during the land. Land again.",
                    plan.fetch_remote
                );
                self.ctx
                    .end(&self.row, None, &first, LandState::RemoteMoved)
                    .await;
            }
            None => self.ctx.wait(&self.row).await,
        }
    }

    /// The end by a push that was confirmed not applied.
    async fn push_failed(&self) {
        let failure = self.failure.clone().unwrap_or(PushFailure {
            code: code::PUSH_FAILED,
            text: "Push failed: the remote did not take the commit.".to_owned(),
        });
        self.ctx.fail(&self.row, failure.code, &failure.text).await;
    }

    /// The success of a write that is known to have reached the remote.
    async fn success(&self, ops: &Ops, outcome: OutcomeChange) {
        match self.which {
            Which::Branch => {
                self.branch_success(ops, outcome).await;
            }
            Which::Default => {
                let step = Step {
                    pending: PendingChange::Clear,
                    reconcile: ReconcileChange::Clear,
                    outcome,
                    default_published_oid: Some(ops.oid.clone()),
                    end: Some(StepEnd {
                        state: LandState::Landed,
                        code: None,
                        text: None,
                    }),
                    ..Step::default()
                };
                self.ctx.step(self.id(), step).await;
            }
        }
    }

    async fn branch_success(&self, ops: &Ops, outcome: OutcomeChange) -> BranchStored {
        let plan = &self.row.plan;
        let options = self.ctx.git();
        let tip = local_tip(
            &options,
            &self.home.folder,
            &plan.branch_ref,
            self.ctx.timeouts.read,
        )
        .await;
        // A tip that cannot be read counts as "does not contain R": the push
        // row stops (design §11.5 step 5; the safe side).
        let tip_contains = match &tip {
            Some(tip) => {
                is_ancestor(
                    &options,
                    &self.home.folder,
                    &ops.oid,
                    tip,
                    self.ctx.timeouts.read,
                )
                .await
                    == Some(true)
            }
            None => false,
        };
        let mut published = self.row.clone();
        published.branch_published_oid = Some(ops.oid.clone());
        let conflict = format!(
            "The branch {} changed on {} during the land.",
            plan.branch_ref, plan.branch_target.target
        );
        let up = PushUpsert {
            session_id: self.row.session_id.clone(),
            target: plan.branch_target.target.clone(),
            target_dest: plan.branch_target.dest.clone(),
            publish_ref: plan.publish_ref.clone(),
            oid: ops.oid.clone(),
            tip,
            tip_contains,
        };
        match self
            .ctx
            .store
            .land_branch_applied(
                self.row.land_id.clone(),
                outcome,
                up,
                land_text(&published, &conflict, None),
            )
            .await
        {
            Ok(stored) => stored,
            Err(e) => {
                tracing::error!(land = %self.id(), error = %e, "a branch success was not stored");
                BranchStored::Dropped
            }
        }
    }

    /// The destination of the write (design §3.8).
    fn destination(&self) -> (PushDestination, String) {
        let plan = &self.row.plan;
        let remote = PushDestination::Remote {
            name: plan.fetch_remote.clone(),
            project: plan.remote.clone(),
            clone_url: self.home.clone_url.clone(),
        };
        match self.which {
            Which::Default => (remote, plan.remote.dest.clone()),
            Which::Branch if plan.branch_target.target == plan.fetch_remote => {
                (remote, plan.branch_target.dest.clone())
            }
            Which::Branch => (
                PushDestination::Wip {
                    url: plan.branch_target.target.clone(),
                },
                plan.branch_target.dest.clone(),
            ),
        }
    }

    /// One write through the gated start. The preparation (the destination
    /// check) comes first, without the lock.
    async fn write(&self) -> Wrote {
        let row = &self.row;
        let options = self.ctx.git();
        let (destination, dest) = self.destination();
        let allow_local = self.ctx.policy.current().allow_local_remotes();
        let verified = effective_push_url(
            &options,
            &self.home.folder,
            &destination,
            &dest,
            allow_local,
        )
        .await;
        let verified = match verified {
            Ok(v) if self.ops.as_ref().is_none_or(|o| o.url == v.url) => v,
            Ok(_) => {
                let text = format!(
                    "The remote {} of the home no longer points to {}.",
                    row.plan.fetch_remote, row.plan.remote.repo
                );
                return self.not_started(code::REMOTE_MISMATCH, &text).await;
            }
            Err(refusal) => return self.not_started(refusal.code, &refusal.text).await,
        };
        let default_ref = self.default_ref();
        let ops = self.ops.clone().unwrap_or_else(|| Ops {
            url: verified.url.clone(),
            ref_name: match self.which {
                Which::Branch => format!("refs/heads/{}", row.plan.publish_ref),
                Which::Default => format!("refs/heads/{default_ref}"),
            },
            oid: row.rebased_oid.clone().unwrap_or_default(),
            lease: row.branch_lease.clone().unwrap_or_default(),
            expect: row.base_oid.clone().unwrap_or_default(),
        });
        let push = match self.which {
            Which::Branch => LandPush::branch(
                &options,
                &self.home.folder,
                self.ctx.timeouts.push,
                &ops.url,
                &self.home.slug,
                &row.plan.publish_ref,
                &ops.oid,
                &ops.lease,
            ),
            Which::Default => LandPush::default(
                &options,
                &self.home.folder,
                self.ctx.timeouts.push,
                &ops.url,
                &default_ref,
                &ops.oid,
            ),
        };
        let push = match push {
            Ok(push) => push,
            Err(e) => {
                let text = format!("The land failed: Fleet Git refused to push this ref: {e}");
                return self.not_started(code::INTERNAL, &text).await;
            }
        };
        let intent = if self.ops.is_none() {
            Step {
                phase: Some(self.which.intent()),
                pending: PendingChange::Set(Pending {
                    action: self.which.action(),
                    attempt: row.attempt,
                    url: ops.url.clone(),
                    ref_name: ops.ref_name.clone(),
                    oid: ops.oid.clone(),
                    lease: (self.which == Which::Branch).then(|| ops.lease.clone()),
                    expect: (self.which == Which::Default).then(|| ops.expect.clone()),
                }),
                outcome: OutcomeChange::Clear,
                require: crate::store::land_publish::Require::NoPending,
                event: true,
                ..Step::default()
            }
        } else {
            Step {
                outcome: OutcomeChange::Clear,
                require: crate::store::land_publish::Require::Pending,
                ..Step::default()
            }
        };
        let gate = Gate {
            store: &self.ctx.store,
            policy: &self.ctx.policy,
            locks: &self.ctx.locks,
            run: row,
        };
        let result = gated_write(
            &gate,
            intent,
            || {
                let spawned = push.spawn();
                async move {
                    match spawned {
                        Ok(running) => running.finish().await,
                        Err(e) => spawn_failed(&e),
                    }
                }
            },
            |end| self.record(&ops, end),
        )
        .await;
        match result {
            Gated::Written(done) => done,
            Gated::Stopped | Gated::Refused => Wrote::Done,
        }
    }

    /// The end of a write that never started (a refused destination, a
    /// refused ref): the pending write is cleared.
    async fn not_started(&self, code: &str, text: &str) -> Wrote {
        let fresh = self
            .ctx
            .running(self.id())
            .await
            .unwrap_or_else(|| self.row.clone());
        let step = Step {
            pending: PendingChange::Clear,
            reconcile: ReconcileChange::Clear,
            outcome: OutcomeChange::Set(WRITE_NOT_STARTED),
            end: Some(StepEnd {
                state: LandState::Failed,
                code: Some(code.to_owned()),
                text: Some(land_text(&fresh, text, None)),
            }),
            ..Step::default()
        };
        self.ctx.step(self.id(), step).await;
        Wrote::Done
    }

    /// Stores the outcome of the write, with the lock held (design §11.5,
    /// the gated start, step 6).
    async fn record(&self, ops: &Ops, end: PushEnd) -> Wrote {
        let kind = match end.outcome {
            PushOutcome::Applied => {
                self.success(ops, OutcomeChange::Set(WRITE_APPLIED)).await;
                return Wrote::Done;
            }
            PushOutcome::Rejected(_) => Kind::Rejected,
            PushOutcome::Unknown => Kind::Unknown,
        };
        let value = match kind {
            Kind::Rejected => WRITE_REJECTED,
            Kind::Unknown => WRITE_UNKNOWN,
        };
        if let Err(e) = self
            .ctx
            .store
            .land_write_outcome(self.row.land_id.clone(), value)
            .await
        {
            tracing::error!(land = %self.id(), error = %e, "a write outcome was not stored");
        }
        Wrote::Failed {
            kind,
            failure: end.failure,
            ops: ops.clone(),
        }
    }
}

/// What [`drive`] decided.
pub(super) enum Next {
    /// Nothing more to do in this task.
    Done,
    /// Attempt 2: steps 1 to 4 run again.
    Again(Box<LandRunRow>),
}

/// The fields that tell whether an action moved the run.
fn mark(row: &LandRunRow) -> (LandPhase, LandState, u32, Option<String>, bool) {
    (
        row.phase,
        row.state,
        row.attempt,
        row.pending_action.clone(),
        row.reconciling,
    )
}

/// Drives a run from its stored state (design §11.5 steps 5 to 7 and the
/// reconciliation table): the pending write first, else the next step of the
/// phase. It stops when the run ended, waits for a reconciliation, or an
/// action changed nothing.
pub(super) async fn drive(shared: &Shared, mut row: LandRunRow) -> Next {
    loop {
        let before = mark(&row);
        let id = row.land_id.clone();
        let wrote = match (row.pending_action.as_deref(), row.phase) {
            (Some(PENDING_BRANCH_PUSH), _) | (None, LandPhase::Checked) => {
                shared
                    .writer
                    .run(WriteJob::LandBranch {
                        land_id: id.clone(),
                    })
                    .await
            }
            (Some(PENDING_DEFAULT_PUSH), _) => {
                shared
                    .writer
                    .run(WriteJob::LandDefault {
                        land_id: id.clone(),
                    })
                    .await
            }
            (Some(PENDING_PR_CREATE), _) => {
                super::land_pr::pr_step(shared, &id).await;
                true
            }
            (None, LandPhase::BranchPushed) if row.plan.mode == LandMode::Pr => {
                super::land_pr::pr_step(shared, &id).await;
                true
            }
            (None, LandPhase::BranchPushed) => {
                shared
                    .writer
                    .run(WriteJob::LandDefault {
                        land_id: id.clone(),
                    })
                    .await
            }
            (None, LandPhase::Admitted) if row.attempt == 2 => return Next::Again(Box::new(row)),
            _ => return Next::Done,
        };
        if !wrote {
            return Next::Done;
        }
        let Some(next) = shared.ctx().running(&id).await else {
            return Next::Done;
        };
        // A run that waits for its next reconciliation is left to the poll.
        if mark(&next) == before || (next.reconciling && next.pending_action.is_some()) {
            return Next::Done;
        }
        row = next;
    }
}
