//! The way back after a failed update (design §3.7 steps 12 to 17) and the
//! recovery at the station start (§3.7.2).
//!
//! [`recover`] needs no session manager: it runs between `Store::open` and
//! `SessionManager::start`, when nothing can start a provider.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::config::ProviderUpdatesConfig;
use fleetdeck_core::{ChildEnv, SharedIds, new_id};
use fleetdeck_proto::cli_update::{InstallMethod, UpdateOutcome, UpdateTrigger, reason};
use fleetdeck_proto::event::ProviderKind;

use super::activate::{clean_stale, remove_path, rollback_dir};
use super::detect::{Tool, detect};
use super::run::{
    Engine, Flight, HOLD_PHASE, Installer, Job, JournalSink, ProviderTarget, STAGE_PHASE, Staged,
    channel_text, end_of, new_attempt,
};
use super::stage::{Ctx, Failure, probe_env, run_fence};
use crate::store::Store;
use crate::store::cli_update_journal::{
    JournalAction, JournalPhase, QuarantineAction, RecoveryRecord,
};
use crate::store::cli_updates::AttemptEnd;

/// The limit of the whole recovery (§3.7.2 step 2).
pub const RECOVER_LIMIT: Duration = Duration::from_secs(5 * 60);

fn name_of(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Claude => "Claude",
        _ => "Codex",
    }
}

/// The command that reinstalls version `from` by hand (§3.6.2).
#[must_use]
pub fn hint(kind: ProviderKind, method: InstallMethod, from: &str) -> String {
    match (kind, method) {
        (ProviderKind::Claude, _) => format!("claude install {from}"),
        (_, InstallMethod::Bun) => format!("bun add -g @openai/codex@{from}"),
        _ => format!("npm install -g @openai/codex@{from}"),
    }
}

/// The quarantine text of `rollback_failed` (§3.6.2).
#[must_use]
pub fn rollback_text(kind: ProviderKind, method: InstallMethod, from: &str) -> String {
    format!(
        "{name} failed after an update and the way back failed. Reinstall it on this station: {hint}",
        name = name_of(kind),
        hint = hint(kind, method, from)
    )
}

/// The quarantine text of `recovery_pending` (§3.8.1).
#[must_use]
pub fn pending_text(kind: ProviderKind, method: InstallMethod, from: &str) -> String {
    format!(
        "the station could not finish an interrupted {name} update. Restart the station; if this stays, reinstall {name}: {hint}",
        name = name_of(kind),
        hint = hint(kind, method, from)
    )
}

/// The quarantine text of `process_left` (§3.8.1).
#[must_use]
pub fn left_text(kind: ProviderKind) -> String {
    format!(
        "a process of a {} update did not stop. Restart the station.",
        name_of(kind)
    )
}

/// The folders of a stale stage, rollback copy and probe folder that no
/// journal row and no `rollback_failed` quarantine row names (§3.6
/// "Clean-up"). `keep` holds the ids that stay.
pub fn sweep(station_dir: &Path, tool: &Tool, keep: &[String]) {
    let base = station_dir.join("cli-update");
    clean_stale(&base.join("rollback"), "", keep);
    clean_stale(&base.join("probe-cwd"), "", keep);
    match tool {
        Tool::Native { versions, .. } => clean_stale(versions, ".fd-stage-", keep),
        Tool::Npm { modules, .. } | Tool::Bun { modules, .. } => {
            if let Some(parent) = modules.parent() {
                clean_stale(&parent.join(".fd-cli-update"), "", keep);
            }
        }
        Tool::None => {}
    }
}

/// The ids that [`sweep`] keeps: each journal row (attempt and marker) and
/// each `rollback_failed` quarantine row.
pub async fn keep_ids(store: &Store) -> Vec<String> {
    let mut keep = Vec::new();
    if let Ok(rows) = store.cli_update_journals().await {
        for j in rows {
            keep.push(j.attempt_id);
            keep.push(j.marker);
        }
    }
    if let Ok(rows) = store.cli_update_quarantines().await {
        keep.extend(
            rows.into_iter()
                .filter(|q| q.reason == reason::ROLLBACK_FAILED)
                .map(|q| q.attempt_id),
        );
    }
    keep
}

/// What [`recover`] needs. No session manager, no deck.
pub struct Recovery<'a> {
    pub store: &'a Store,
    pub installer: &'a dyn Installer,
    pub targets: &'a [ProviderTarget],
    pub station_dir: &'a Path,
    pub curl: &'a Path,
    pub ids: &'a SharedIds,
    pub cfg: &'a ProviderUpdatesConfig,
    /// [`RECOVER_LIMIT`] in production.
    pub limit: Duration,
}

/// §3.7.2: finishes the attempts that a stop interrupted. It runs between
/// `Store::open` and `SessionManager::start`, also when updates are off.
/// A row that is not done keeps its journal row and its quarantine row;
/// the next station start tries again.
pub async fn recover(r: &Recovery<'_>) {
    if tokio::time::timeout(r.limit, r.run()).await.is_err() {
        tracing::warn!("the update recovery reached its limit; the next station start tries again");
    }
    let keep = keep_ids(r.store).await;
    for t in r.targets {
        if let Ok(install) = detect(t.kind, &t.binary, &t.env) {
            sweep(r.station_dir, &install.tool, &keep);
        }
    }
}

impl Recovery<'_> {
    async fn run(&self) {
        if let Err(e) = self.store.cli_update_interrupt_orphans().await {
            tracing::warn!(error = %e, "the update recovery cannot read the attempts");
        }
        let rows = match self.store.cli_update_journals().await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "the update recovery cannot read the journal");
                return;
            }
        };
        for j in rows {
            self.one(j).await;
        }
    }

    async fn finish(&self, id: &str, end: AttemptEnd, j: JournalAction, q: QuarantineAction) {
        if let Err(e) = self.store.cli_update_finish(id.to_owned(), end, j, q).await {
            tracing::warn!(error = %e, "the update recovery result is not written");
        }
    }

    /// Step 1a: a process of the attempt stays. The journal row stays.
    async fn leave(&self, j: &RecoveryRecord) {
        let text = left_text(j.provider);
        let end = end_of(
            UpdateOutcome::RollbackFailed,
            Some(reason::PROCESS_LEFT),
            None,
        );
        let q = QuarantineAction::Set {
            reason: reason::PROCESS_LEFT,
            text,
        };
        self.finish(&j.attempt_id, end, JournalAction::Keep, q)
            .await;
    }

    fn clean(&self, j: &RecoveryRecord, successor: &str) {
        remove_path(&j.stage_root);
        remove_path(&rollback_dir(self.station_dir, &j.marker));
        let probe = self.station_dir.join("cli-update/probe-cwd");
        remove_path(&probe.join(&j.marker));
        remove_path(&probe.join(successor));
    }

    async fn one(&self, j: RecoveryRecord) {
        let kind = j.provider;
        let sink = JournalSink {
            store: self.store.clone(),
            kind,
        };
        let ctx = Ctx {
            attempt_id: &j.attempt_id,
            marker: &j.marker,
            station_dir: self.station_dir,
            sink: &sink,
            curl: self.curl,
        };
        // a
        if run_fence(&ctx, j.installer_pgid).await.is_err() {
            return self.leave(&j).await;
        }
        // b
        if j.phase == JournalPhase::Staging {
            let end = end_of(UpdateOutcome::Interrupted, None, Some(&j.from_version));
            self.finish(
                &j.attempt_id,
                end,
                JournalAction::Delete,
                QuarantineAction::Keep,
            )
            .await;
            self.clean(&j, &j.attempt_id);
            return;
        }
        // c
        let successor = new_id::<String>(self.ids.as_ref());
        let attempt = new_attempt(
            &successor,
            kind,
            UpdateTrigger::Recovery,
            j.method,
            &channel_text(kind, self.cfg),
            Some(&j.target_version),
            Some(&j.from_version),
        );
        let text = pending_text(kind, j.method, &j.from_version);
        let rec = match self
            .store
            .cli_update_recover_begin(kind, attempt, text)
            .await
        {
            Ok(rec) => rec,
            Err(e) => {
                tracing::warn!(error = %e, "the update recovery does not start");
                return;
            }
        };
        // d
        let ctx = Ctx {
            attempt_id: &successor,
            marker: &rec.marker,
            station_dir: self.station_dir,
            sink: &sink,
            curl: self.curl,
        };
        let restored = self.restore(&ctx, &rec).await;
        match restored {
            Ok(()) => {
                let end = end_of(UpdateOutcome::RolledBack, None, Some(&rec.from_version));
                self.finish(
                    &successor,
                    end,
                    JournalAction::Delete,
                    QuarantineAction::Clear,
                )
                .await;
                self.clean(&rec, &successor);
            }
            Err(Failure::ProcessLeft) => {
                let mut left = rec;
                left.attempt_id = successor;
                self.leave(&left).await;
            }
            Err(_) => {
                let text = rollback_text(kind, rec.method, &rec.from_version);
                let end = end_of(
                    UpdateOutcome::RollbackFailed,
                    Some(reason::ROLLBACK_FAILED),
                    None,
                );
                let q = QuarantineAction::Set {
                    reason: reason::ROLLBACK_FAILED,
                    text,
                };
                self.finish(&successor, end, JournalAction::Delete, q).await;
            }
        }
    }

    async fn restore(&self, ctx: &Ctx<'_>, rec: &RecoveryRecord) -> Result<(), Failure> {
        let failed = Failure::Failed(reason::ROLLBACK_FAILED);
        let t = self
            .targets
            .iter()
            .find(|t| t.kind == rec.provider)
            .ok_or(failed)?;
        let install = detect(rec.provider, &t.binary, &t.env).map_err(|_| failed)?;
        let env = probe_env(&t.env, &rec.marker).ok_or(failed)?;
        self.installer.restore(ctx, rec, &install.tool, &env).await
    }
}

impl Engine {
    /// Steps 12 to 17, in the task of a started attempt.
    #[allow(clippy::too_many_lines)]
    pub(super) async fn execute(self: &Arc<Self>, flight: Flight, job: Job) {
        let _flight = flight;
        let sink = JournalSink {
            store: self.store.clone(),
            kind: job.kind,
        };
        let ctx = Ctx {
            attempt_id: &job.id,
            marker: &job.id,
            station_dir: &self.station_dir,
            sink: &sink,
            curl: &self.curl,
        };
        let Some(env) = probe_env(&job.instance, &job.id) else {
            self.end_early(&job, UpdateOutcome::Failed, reason::INSTALL_FAILED)
                .await;
            return;
        };
        // 12
        let staged = tokio::time::timeout(
            STAGE_PHASE,
            self.installer
                .stage(&ctx, &job.install, &job.verified, &job.instance),
        )
        .await;
        let staged = match staged {
            Ok(Ok(s)) => s,
            Ok(Err(Failure::ProcessLeft)) => return self.process_left(&job).await,
            Ok(Err(f)) => return self.end_failure(&job, f).await,
            Err(_) => {
                if self.fence_job(&ctx, job.kind).await.is_err() {
                    return self.process_left(&job).await;
                }
                return self
                    .end_early(&job, UpdateOutcome::Failed, reason::TIMEOUT)
                    .await;
            }
        };
        // 13
        let hold = match self.host.begin_hold(job.kind).await {
            Ok(h) => h,
            Err(r) => {
                let (outcome, why) = match r {
                    reason::STARTS_IN_FLIGHT => {
                        (UpdateOutcome::SkippedBusy, reason::STARTS_IN_FLIGHT)
                    }
                    reason::ANOTHER_UPDATE => (UpdateOutcome::Refused, reason::ANOTHER_UPDATE),
                    _ => (UpdateOutcome::Refused, reason::STATION_RESTARTING),
                };
                return self.end_early(&job, outcome, why).await;
            }
        };
        let live = self.host.live_sessions(job.kind).await;
        if !live.is_empty() && !job.force {
            drop(hold);
            return self
                .end_early(&job, UpdateOutcome::SkippedBusy, reason::SESSIONS_RUNNING)
                .await;
        }
        // 14
        let phase =
            tokio::time::timeout(HOLD_PHASE, self.activate_phase(&ctx, &job, &staged, &env)).await;
        match phase {
            Ok(Ok(())) => {
                // 15
                let end = end_of(UpdateOutcome::Updated, None, Some(&job.target));
                let done = self
                    .store
                    .cli_update_finish(
                        job.id.clone(),
                        end,
                        JournalAction::Delete,
                        QuarantineAction::Clear,
                    )
                    .await;
                if let Err(e) = done {
                    tracing::warn!(error = %e, "the update result is not written");
                }
                self.host.release_quarantine(job.kind);
                if let Ok(mut c) = self.checks.lock() {
                    c.remove(&job.kind);
                }
                self.host.refresh().await;
                drop(hold);
                self.clean(&job);
            }
            Ok(Err((Failure::ProcessLeft, _))) => {
                drop(hold);
                self.process_left(&job).await;
            }
            Ok(Err((f, false))) => {
                drop(hold);
                self.end_failure(&job, f).await;
            }
            Ok(Err((f, true))) => {
                self.roll_back(&ctx, &job, &env, f.reason()).await;
                drop(hold);
            }
            Err(_) => {
                if self.fence_job(&ctx, job.kind).await.is_err() {
                    drop(hold);
                    return self.process_left(&job).await;
                }
                let activating = self
                    .store
                    .cli_update_journals()
                    .await
                    .ok()
                    .and_then(|rows| rows.into_iter().find(|j| j.provider == job.kind))
                    .is_some_and(|j| {
                        j.phase == crate::store::cli_update_journal::JournalPhase::Activating
                    });
                if activating {
                    self.roll_back(&ctx, &job, &env, reason::TIMEOUT).await;
                    drop(hold);
                } else {
                    drop(hold);
                    self.end_early(&job, UpdateOutcome::Failed, reason::TIMEOUT)
                        .await;
                }
            }
        }
    }

    /// A1 (journal phase `activating`), A2 and V. The flag tells whether A2
    /// started.
    async fn activate_phase(
        &self,
        ctx: &Ctx<'_>,
        job: &Job,
        staged: &Staged,
        env: &ChildEnv,
    ) -> Result<(), (Failure, bool)> {
        let act = self
            .installer
            .rollback_copy(ctx, &job.install, staged, job.rollback_sha256.as_deref())
            .await
            .map_err(|f| (f, false))?;
        self.store
            .cli_update_journal_activate(job.kind, act.clone())
            .await
            .map_err(|_| (Failure::Failed(reason::INSTALL_FAILED), false))?;
        self.installer
            .activate(ctx, &job.install, staged, &act, &job.target, env)
            .await
            .map_err(|f| (f, true))
    }

    /// Step 16: the fence, the way back and its result.
    async fn roll_back(&self, ctx: &Ctx<'_>, job: &Job, env: &ChildEnv, first: &'static str) {
        if self.fence_job(ctx, job.kind).await.is_err() {
            return self.process_left(job).await;
        }
        let rec = self
            .store
            .cli_update_journals()
            .await
            .ok()
            .and_then(|rows| rows.into_iter().find(|j| j.provider == job.kind));
        let restored = match &rec {
            Some(rec) => {
                self.installer
                    .restore(ctx, rec, &job.install.tool, env)
                    .await
            }
            None => Err(Failure::Failed(reason::ROLLBACK_FAILED)),
        };
        match restored {
            Ok(()) => {
                let end = end_of(UpdateOutcome::RolledBack, Some(first), Some(&job.from));
                self.finish(job, end, JournalAction::Delete, QuarantineAction::Keep)
                    .await;
                self.host.refresh().await;
                self.clean(job);
            }
            Err(Failure::ProcessLeft) => self.process_left(job).await,
            Err(_) => {
                let text = rollback_text(job.kind, job.install.method, &job.from);
                let end = end_of(
                    UpdateOutcome::RollbackFailed,
                    Some(reason::ROLLBACK_FAILED),
                    None,
                );
                let quarantine = QuarantineAction::Set {
                    reason: reason::ROLLBACK_FAILED,
                    text: text.clone(),
                };
                self.finish(job, end, JournalAction::Delete, quarantine)
                    .await;
                self.host.quarantine(job.kind, &text);
                self.host.refresh().await;
            }
        }
    }

    /// `RollbackFailed (process_left)`: the quarantine, and the journal row
    /// stays (§3.6.3).
    async fn process_left(&self, job: &Job) {
        let text = left_text(job.kind);
        let end = end_of(
            UpdateOutcome::RollbackFailed,
            Some(reason::PROCESS_LEFT),
            None,
        );
        let quarantine = QuarantineAction::Set {
            reason: reason::PROCESS_LEFT,
            text: text.clone(),
        };
        self.finish(job, end, JournalAction::Keep, quarantine).await;
        self.host.quarantine(job.kind, &text);
        self.host.refresh().await;
    }

    async fn finish(&self, job: &Job, end: AttemptEnd, j: JournalAction, q: QuarantineAction) {
        if let Err(e) = self
            .store
            .cli_update_finish(job.id.clone(), end, j, q)
            .await
        {
            tracing::warn!(error = %e, "the update result is not written");
        }
    }

    /// A failure before the activation: `Refused` or `Failed`, the journal
    /// row deleted, the stage removed.
    async fn end_failure(&self, job: &Job, f: Failure) {
        let outcome = match f {
            Failure::Refused(_) => UpdateOutcome::Refused,
            _ => UpdateOutcome::Failed,
        };
        self.end_early(job, outcome, f.reason()).await;
    }

    async fn end_early(&self, job: &Job, outcome: UpdateOutcome, why: &str) {
        let end = end_of(outcome, Some(why), Some(&job.from));
        self.finish(job, end, JournalAction::Delete, QuarantineAction::Keep)
            .await;
        self.clean(job);
    }

    /// Removes the stage, the rollback copy and the probe folder of the job.
    fn clean(&self, job: &Job) {
        remove_path(&job.stage_root);
        remove_path(&rollback_dir(&self.station_dir, &job.id));
        remove_path(&self.station_dir.join("cli-update/probe-cwd").join(&job.id));
    }

    async fn fence_job(&self, ctx: &Ctx<'_>, kind: ProviderKind) -> Result<(), Failure> {
        let pgid = self
            .store
            .cli_update_journals()
            .await
            .ok()
            .and_then(|rows| rows.into_iter().find(|j| j.provider == kind))
            .and_then(|j| j.installer_pgid);
        run_fence(ctx, pgid).await
    }
}

#[cfg(test)]
#[path = "recover_tests.rs"]
mod tests;
