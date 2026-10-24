//! DL-402 follow-up: the providers number request ids per session, so two
//! sessions of one station can hold the same request id. Each session's
//! request is stored and answered on its own.

use std::time::Duration;

use fleetdeck_proto::command::{ApprovalDecision, ReceiptState, SessionCommand, StopMode};
use fleetdeck_proto::{Event, EventBody, RequestId, SessionId};

use super::{Station, WAIT};

fn answer(session: &SessionId, request: &RequestId, option: &str) -> SessionCommand {
    SessionCommand::Answer {
        session_id: session.clone(),
        request_id: request.clone(),
        decision: ApprovalDecision {
            answered_by: None,
            option_id: option.to_owned(),
            updated_input: None,
            text: None,
        },
    }
}

/// The request id of the approval that `session` emitted, once it exists.
async fn request_of(st: &Station, session: &SessionId) -> RequestId {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        for e in st.events().await {
            if &e.session_id == session
                && let EventBody::Known(Event::ApprovalRequested { request_id, .. }) = e.event
            {
                return request_id;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no approval for {session}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn two_sessions_with_the_same_request_id_can_each_be_answered() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let a = st.start(&ws, Some("ask ls")).await;
    let b = st.start(&ws, Some("ask ls")).await;
    let (request_a, request_b) = (request_of(&st, &a).await, request_of(&st, &b).await);
    assert_eq!(
        request_a, request_b,
        "both sessions use the same request id"
    );

    // Session B first, then session A: each answer is the first of its own
    // request and is accepted.
    let first_b = st.accepted(answer(&b, &request_b, "allow")).await;
    assert_eq!(first_b.receipt.state, ReceiptState::Accepted);
    assert_eq!(first_b.effects.len(), 1);
    let first_a = st.accepted(answer(&a, &request_a, "allow")).await;
    assert_eq!(first_a.receipt.state, ReceiptState::Accepted);
    assert_eq!(first_a.effects.len(), 1);

    // Both requests resolve, one for each session.
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let resolved: Vec<SessionId> = st
            .events()
            .await
            .into_iter()
            .filter(|e| matches!(e.event, EventBody::Known(Event::ApprovalResolved { .. })))
            .map(|e| e.session_id)
            .collect();
        if resolved.contains(&a) && resolved.contains(&b) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "resolved: {resolved:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A second answer to A returns the first outcome and sends nothing.
    let again = st.accepted(answer(&a, &request_a, "deny")).await;
    assert!(again.effects.is_empty(), "repeated: {:?}", again.effects);
    st.manager.shutdown(StopMode::Kill).await;
}
