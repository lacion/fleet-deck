//! The execution checks of the manager (DL-182 §4.2): the processes of a
//! shim run.

use super::{SessionManager, shims};
use crate::store::Result;
use fleetdeck_proto::command::SessionCommand;

impl SessionManager {
    /// The boot id reader of this station: the one of its shim host, or
    /// the real reader.
    pub(super) fn boot_reader(&self) -> fleetdeck_providers::host::shim::identity::BootIdReader {
        self.target
            .boot_reader()
            .unwrap_or_else(fleetdeck_providers::host::shim::identity::real_boot_id_reader)
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
