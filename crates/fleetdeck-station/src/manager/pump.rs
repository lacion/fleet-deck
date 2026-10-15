//! The pump of a direct session (PLAN §6.4): it appends the events of one
//! adapter session to the log. Moved out of `target.rs` (DL-191); the
//! behavior is the same, with the subagent binding step added.

use std::sync::Arc;

use fleetdeck_proto::{Event, SessionId};
use tokio::sync::{mpsc, oneshot};

use super::super::turns::SessionTurns;
use super::{REASON_PROCESS_EXIT, Sessions};
use crate::history::subagent::LaunchFacts;
use crate::store::{NewEvent, ProcessIncarnation, Store};

/// Events that the pump appends in one transaction at most.
const PUMP_BATCH: usize = 64;

/// Appends one session's events in order. After the process registered, a
/// `session.ended` (or a closed channel) ends the process in the store. If
/// the log fails, the pump stops reading: the adapter's sink closes and the
/// adapter kills the process (fail closed, DL-033).
// The pump task owns every value it needs; a struct would only rename them.
#[allow(clippy::too_many_arguments)]
pub(super) async fn pump(
    store: Store,
    sessions: Sessions,
    session_id: SessionId,
    turns: Arc<SessionTurns>,
    mut rx: mpsc::Receiver<fleetdeck_providers::adapter::Emitted>,
    ready: oneshot::Receiver<ProcessIncarnation>,
    coordinator: crate::coordinator::Bindings,
    launch: Option<LaunchFacts>,
) {
    // Hold the events until the process is recorded, so that its end cannot
    // come first. A failed start drops `ready`: the pump still stores what
    // the adapter said.
    let process = ready.await.ok();
    let mut ended = false;
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while batch.len() < PUMP_BATCH {
            match rx.try_recv() {
                Ok(next) => batch.push(next),
                Err(_) => break,
            }
        }
        ended |= batch.iter().any(|e| e.event.is_terminal_for_session());
        let observed: Vec<Event> = batch.iter().map(|e| e.event.clone()).collect();
        let events = batch.into_iter().map(NewEvent::from).collect();
        if let Err(e) = store.append(session_id.clone(), events).await {
            tracing::error!(error = %e, session = %session_id, "events not stored; closing the session");
            break;
        }
        // The events are in the log first: `turn.completed` never waits for
        // a checkpoint (L13).
        let bound =
            super::super::coordinator_start::bound_events(&coordinator, &session_id, &observed);
        if !bound.is_empty()
            && let Err(e) = store.append(session_id.clone(), bound).await
        {
            tracing::error!(error = %e, "coordinator binding facts not stored; closing the session");
            break;
        }
        crate::artifacts::suggest::observe(&store, &session_id, &observed).await;
        crate::history::subagent::observe(&store, &session_id, launch.as_ref(), &observed).await;
        if turns.observe(&observed).await {
            tokio::spawn(Arc::clone(&turns).capture_due());
        }
        if ended {
            break;
        }
    }
    drop(rx);
    // The process is gone: its coordinator credential ends with it (§5.5).
    let gone = super::super::coordinator_start::ended_events(&coordinator, &session_id);
    if !gone.is_empty()
        && let Err(e) = store.append(session_id.clone(), gone).await
    {
        tracing::error!(error = %e, "coordinator binding end not stored");
    }
    if let Some(process) = process {
        if let Ok(mut map) = sessions.lock() {
            map.remove(&session_id);
        }
        if let Err(e) = store
            .process_ended(process, REASON_PROCESS_EXIT.to_owned())
            .await
        {
            tracing::error!(error = %e, "process end not stored");
        }
    }
}
