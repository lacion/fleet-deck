//! The execution checks of the manager (DL-182 §4.2): the processes of a
//! shim run.

use super::{SessionManager, restart_gate, shims};
use crate::store::{Result, Store, resumes};
use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::event::SessionState;
use fleetdeck_providers::host::shim::identity::BootIdReader;

impl SessionManager {
    /// The boot id reader of this station: the one of its shim host, or
    /// the real reader.
    pub(super) fn boot_reader(&self) -> fleetdeck_providers::host::shim::identity::BootIdReader {
        self.target
            .boot_reader()
            .unwrap_or_else(fleetdeck_providers::host::shim::identity::real_boot_id_reader)
    }

    /// What runs before the decision of a command, outside the writer
    /// transaction: the settle check of an unverified exit (DL-432) for a
    /// resume, and for an input that resumes a hibernated session; then
    /// [`Self::reconcile_check`].
    ///
    /// `stop_prepare` already holds the per-session lock for `session.resume`;
    /// an input takes it here for the settle only.
    pub(super) async fn pre_decision(
        &self,
        command: &SessionCommand,
    ) -> Option<fleetdeck_proto::execution::ExecutionCheck> {
        match command {
            SessionCommand::Resume { session_id } => {
                settle_unverified_exits(&self.store, &self.boot_reader(), session_id).await;
            }
            command if restart_gate::may_resume(command) => {
                if let Some(session_id) = command.session_id() {
                    let ended = self
                        .store
                        .summary(session_id.clone())
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|s| {
                            matches!(s.state, SessionState::Exited | SessionState::Lost)
                        });
                    if ended {
                        let _serial = self.target.stops().locks.hold(session_id).await;
                        settle_unverified_exits(&self.store, &self.boot_reader(), session_id).await;
                    }
                }
            }
            _ => {}
        }
        self.reconcile_check(command).await
    }

    /// The check of `session.reconcile_execution`: the processes of the
    /// run of `shim_id`. `None` when the row is unknown, belongs to another
    /// session, runs, or is proven ended already: the decision then
    /// refuses from the row.
    pub(super) async fn reconcile_check(
        &self,
        command: &SessionCommand,
    ) -> Option<fleetdeck_proto::execution::ExecutionCheck> {
        let SessionCommand::ReconcileExecution {
            session_id,
            shim_id,
        } = command
        else {
            return None;
        };
        let row = self.store.shim(shim_id.clone()).await.ok().flatten()?;
        if &row.session_id != session_id
            || row.state.is_open()
            || crate::store::resumes::proves_end(&row)
        {
            return None;
        }
        let boot = self.boot_reader();
        tokio::task::spawn_blocking(move || shims::check_row(&boot, &row))
            .await
            .ok()
    }

    /// `session.execution_check` (DL-182 §4.2): the newest unresolved shim
    /// execution of `session` and what still runs of it. It changes nothing
    /// and kills nothing.
    ///
    /// # Errors
    /// A database error.
    pub async fn execution_check(
        &self,
        session: &fleetdeck_proto::SessionId,
    ) -> Result<fleetdeck_proto::execution::ExecutionCheck> {
        let rows = self.store.shims_unresolved(Some(session.clone())).await?;
        let boot = self.boot_reader();
        let newest = rows.last().cloned();
        let check = tokio::task::spawn_blocking(move || shims::check_unresolved(&boot, &rows))
            .await
            .map_err(|e| {
                crate::store::StoreError::Corrupt(format!("the check did not run: {e}"))
            })?;
        // BG-4: while the leader of the run lives, its members are recorded
        // as witnesses. The answer does not change.
        if let Some(row) = &newest {
            self.record_witnesses(row).await;
        }
        Ok(check)
    }

    /// [`Self::execution_check`] for a deck link that negotiated
    /// `execution_stop.v1`: the answer also holds `last_stop`, the newest
    /// settled stop of the run.
    ///
    /// # Errors
    /// A database error.
    pub async fn execution_check_with_stop(
        &self,
        session: &fleetdeck_proto::SessionId,
    ) -> Result<fleetdeck_proto::execution::ExecutionCheck> {
        let check = self.execution_check(session).await?;
        Ok(self.add_last_stop(check).await)
    }
}

/// DL-432: the station's own proof of an unverified exit. For each `exited`
/// row of `session` whose effective reason is `exit_unverified`, it runs the
/// check of "Mark as ended" (budget 3 s). It records the station proof only
/// when the check is complete, on the same boot, and finds no live process
/// of the run. Every other result changes nothing: the decision then refuses
/// from the row as before. A failure is logged; the settle never rejects a
/// command itself.
pub(super) async fn settle_unverified_exits(
    store: &Store,
    boot: &BootIdReader,
    session: &SessionId,
) {
    let rows = match store.shim_rows(session.clone()).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(%session, error = %e, "the settle check could not read the shim rows");
            return;
        }
    };
    for row in rows.into_iter().filter(resumes::settle_eligible) {
        let shim_id = row.shim_id.clone();
        let reader = std::sync::Arc::clone(boot);
        let check = match tokio::task::spawn_blocking(move || shims::check_row(&reader, &row)).await
        {
            Ok(check) => check,
            Err(e) => {
                tracing::warn!(%session, shim = %shim_id, error = %e, "the settle check did not run");
                continue;
            }
        };
        let why = if !check.complete {
            "the check is incomplete"
        } else if !check.same_boot {
            "the run belongs to another boot"
        } else if check.still_runs() {
            "a process of the run lives"
        } else {
            ""
        };
        if !why.is_empty() {
            tracing::info!(%session, shim = %shim_id, why, "the station did not prove the end of an unverified exit");
            continue;
        }
        match store.shim_settle(session.clone(), shim_id.clone()).await {
            Ok(true) => {
                tracing::info!(%session, shim = %shim_id, "the station proved the end of an unverified exit");
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(%session, shim = %shim_id, error = %e, "the station proof was not stored");
            }
        }
    }
}

#[cfg(test)]
#[path = "shim_settle_tests.rs"]
mod shim_settle_tests;
