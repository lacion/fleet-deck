//! The tools of one start or resume (DL-165, DL-194, MCP P4b §5): the
//! target side of [`start_tools`], which fails the start when the tools
//! cannot be prepared.

use fleetdeck_proto::command::SessionSpec;
use fleetdeck_proto::event::ProviderKind;

use super::SessionTarget;
use crate::manager::coordinator_command::set::record::{StartTools, start_tools};
use crate::store::{Dispatch, Effect};

impl SessionTarget {
    /// The tools of the start effect `effect`. A grant with a record is
    /// prepared from the record, at a higher generation.
    ///
    /// # Errors
    /// The refusal of the dispatch; the start already failed.
    pub(super) async fn start_tools(
        &self,
        effect: &Effect,
        provider: ProviderKind,
        spec: &SessionSpec,
        shim_host: bool,
        resume: Option<&str>,
    ) -> Result<StartTools, Dispatch> {
        let session_id = &effect.session_id;
        let prepared = start_tools(
            &self.store,
            &self.coordinator,
            provider,
            session_id,
            spec,
            shim_host,
            resume,
            effect.command_id.clone(),
        )
        .await;
        match prepared {
            Ok(tools) => Ok(tools),
            Err(why) => {
                self.start_failed(session_id, &why).await;
                Err(Dispatch::NotDispatched(format!("coordinator: {why}")))
            }
        }
    }
}

/// Hands the session lock of a start on (MCP P4b PRE-PR R1). A shim-host
/// start moves it into its bridge start, which the launch holds until the
/// row and the binding are stored. Any other start (the direct host, whose
/// binding is in memory now, or a start without tools) drops it at once.
pub(super) fn pass_lock(tools: &mut StartTools) {
    let lock = tools.lock.take();
    match tools.bridge.as_mut() {
        Some(bridge) => bridge.lock = lock,
        None => drop(lock),
    }
}
