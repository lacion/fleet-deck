//! The two automatic uses of the model help (R10.1, R10.2, DL-167). A
//! background task follows the event log of the station:
//!
//! - after the first `turn.completed` of a session without a title, it asks
//!   for a title, sends an internal rename (origin `local`) and stores
//!   `session.title_generated`;
//! - on `session.ended` it asks for a digest of the session.
//!
//! The task only reads events that arrive after it started, so a station
//! restart never calls the model for old sessions. A failed call leaves the
//! session as it was; there is no retry.

use std::collections::HashSet;
use std::sync::Arc;

use fleetdeck_proto::assist::AssistKind;
use fleetdeck_proto::command::{CommandEnvelope, CommandOrigin, RETRY_WINDOW_MS, ReceiptState};
use fleetdeck_proto::event::{EventBody, TurnStatus};
use fleetdeck_proto::{CommandId, Event, EventEnvelope, SessionCommand, SessionId};
use tokio::sync::watch;

use super::{Assist, input};
use crate::manager::SessionManager;
use crate::store::{Caller, ReadResult};

/// The events that one read takes.
const BATCH: usize = 200;

/// Follows the event log until `stop` changes to true.
pub async fn run(
    assist: Arc<Assist>,
    manager: Arc<SessionManager>,
    mut stop: watch::Receiver<bool>,
) {
    let store = manager.store().clone();
    let mut head = store.subscribe();
    let mut cursor = *head.borrow();
    let mut titled: HashSet<SessionId> = HashSet::new();
    loop {
        tokio::select! {
            changed = head.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    return;
                }
                continue;
            }
        }
        loop {
            match store.read_after(cursor, BATCH).await {
                Ok(ReadResult::Events { events, .. }) => {
                    if events.is_empty() {
                        break;
                    }
                    for e in events {
                        cursor = e.station_seq;
                        observe(&assist, &manager, &mut titled, &e);
                    }
                }
                Ok(ReadResult::SnapshotRequired { head }) => {
                    cursor = head;
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "model help: event log");
                    break;
                }
            }
        }
    }
}

fn observe(
    assist: &Arc<Assist>,
    manager: &Arc<SessionManager>,
    titled: &mut HashSet<SessionId>,
    e: &EventEnvelope,
) {
    if !assist.enabled() {
        return;
    }
    let EventBody::Known(event) = &e.event else {
        return;
    };
    match event {
        Event::TurnCompleted {
            status: TurnStatus::Done,
            ..
        } if titled.insert(e.session_id.clone()) => {
            let (assist, manager, session) = (
                Arc::clone(assist),
                Arc::clone(manager),
                e.session_id.clone(),
            );
            tokio::spawn(async move { title_session(&assist, &manager, session).await });
        }
        Event::SessionEnded { .. } => {
            let (assist, manager, session) = (
                Arc::clone(assist),
                Arc::clone(manager),
                e.session_id.clone(),
            );
            tokio::spawn(async move { digest_session(&assist, &manager, session).await });
        }
        _ => {}
    }
}

/// R10.1: names a session that has no title.
pub async fn title_session(assist: &Assist, manager: &SessionManager, session: SessionId) {
    let store = manager.store();
    let untitled = |summary: &Option<fleetdeck_proto::SessionSummary>| {
        summary.as_ref().is_some_and(|s| s.title.is_none())
    };
    match store.summary(session.clone()).await {
        Ok(summary) if untitled(&summary) => {}
        Ok(_) => return,
        Err(e) => return tracing::warn!(error = %e, "model help: summary"),
    }
    let prompt = match store.assist_first_prompt(session.clone()).await {
        Ok(Some(prompt)) if !prompt.trim().is_empty() => prompt,
        Ok(_) => return,
        Err(e) => return tracing::warn!(error = %e, "model help: first prompt"),
    };
    let providers = manager.providers().await;
    let text = input::one_line(&prompt, input::PROMPT_CHARS);
    let title = match assist.generate(AssistKind::Title, &text, &providers).await {
        Ok(answer) => answer.text,
        Err(e) => return tracing::debug!(error = %e, "model help: no title"),
    };
    // The owner may have named the session while the call ran.
    match store.summary(session.clone()).await {
        Ok(summary) if untitled(&summary) => {}
        Ok(_) => return,
        Err(e) => return tracing::warn!(error = %e, "model help: summary"),
    }
    let envelope = CommandEnvelope {
        authority: None,
        command_id: manager.new_id::<CommandId>(),
        target_incarnation: None,
        retry_until: store.now_ms().saturating_add(RETRY_WINDOW_MS),
        origin: CommandOrigin::Local,
        command: SessionCommand::Rename {
            session_id: session.clone(),
            title: Some(title.clone()),
        },
    };
    match manager.submit(envelope, Caller::local()).await {
        Ok(decided) if decided.receipt.state == ReceiptState::Accepted => {}
        Ok(decided) => return tracing::debug!(receipt = ?decided.receipt, "model help: rename"),
        Err(e) => return tracing::warn!(error = %e, "model help: rename"),
    }
    let marker = Event::SessionTitleGenerated { title };
    if let Err(e) = store.append(session, vec![marker.into()]).await {
        tracing::warn!(error = %e, "model help: title marker");
    }
}

/// R10.2: summarizes an ended session.
pub async fn digest_session(assist: &Assist, manager: &SessionManager, session: SessionId) {
    let providers = manager.providers().await;
    if let Err(e) = assist
        .generate(AssistKind::Digest, session.as_str(), &providers)
        .await
    {
        tracing::debug!(error = %e, "model help: no digest");
    }
}
