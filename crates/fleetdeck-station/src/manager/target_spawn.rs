//! The provider spawn of a start or a resume (DL-165): the adapter call
//! with the handoff of the session's own tools when the station prepared
//! one, and the fallback without it when the adapter refuses.

use std::sync::Arc;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::SessionSpec;
use fleetdeck_providers::coordinator::CoordinatorLaunch;
use fleetdeck_providers::{
    EventSink, LiveSession, NativeSessionId, ProviderAdapter, ProviderError,
};

use crate::coordinator::Bindings;
use crate::manager::coordinator_start::{NoTools, log_attached, log_no_tools};

/// What one spawn needs besides the adapter.
pub(super) struct Spawn<'a> {
    pub bindings: &'a Bindings,
    pub session_id: &'a SessionId,
    pub spec: &'a SessionSpec,
    /// The native id of a resume.
    pub resume: Option<&'a str>,
    /// The handoff that the station prepared, if any.
    pub launch: Option<CoordinatorLaunch>,
    /// The session has no coordinator grant: it may start without tools.
    pub fleet_only: bool,
}

impl Spawn<'_> {
    /// Starts or resumes the provider session. A fleet-only session whose
    /// adapter refuses the handoff goes on without the session tools; a
    /// grant session never does (the error ends the start).
    pub(super) async fn run(
        self,
        adapter: &Arc<dyn ProviderAdapter>,
        sink: EventSink,
    ) -> Result<Box<dyn LiveSession>, ProviderError> {
        let Self {
            bindings,
            session_id,
            spec,
            resume,
            launch,
            fleet_only,
        } = self;
        let kind = adapter.kind();
        let native = resume.map(|n| NativeSessionId(n.to_owned()));
        let spec = spec.clone();
        let coordinated = match (&native, launch) {
            (Some(native), Some(launch)) if fleet_only => Some(
                adapter
                    .resume_coordinated(native, spec.clone(), sink.clone(), launch)
                    .await,
            ),
            (None, Some(launch)) => Some(
                adapter
                    .start_coordinated(spec.clone(), sink.clone(), launch)
                    .await,
            ),
            _ => None,
        };
        match coordinated {
            Some(Ok(session)) => {
                log_attached(session_id, kind);
                return Ok(session);
            }
            // An adapter that cannot take the handoff still starts a
            // session that asked for no grant (DL-165).
            Some(Err(ProviderError::CoordinatorHandoff(_))) if fleet_only => {
                log_no_tools(session_id, kind, &NoTools::Refused);
                bindings.revoke_session(session_id);
            }
            Some(Err(e)) => return Err(e),
            None => {}
        }
        match native {
            Some(native) => adapter.resume(&native, spec, sink).await,
            None => adapter.start(spec, sink).await,
        }
    }
}
