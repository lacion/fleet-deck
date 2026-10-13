//! A prompt, a steer or a follow-up to a hibernated session resumes it with
//! that input (R6.6, DL-169). Any other ended session still refuses input.

use fleetdeck_proto::command::UserInput;

use super::*;
use crate::coordinator::Bindings;
use crate::manager::hibernate;
use crate::store::EffectPayload;

const AFTER: u32 = 30;
const MIN_MS: i64 = 60_000;

/// Starts a session, waits until it is idle, and hibernates it.
async fn hibernated(st: &mut Station) -> SessionId {
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    st.wait_for("run completed", |e| {
        matches!(e, Event::RunCompleted { .. }).then_some(())
    })
    .await;
    st.det.advance(Duration::from_millis(
        u64::try_from(i64::from(AFTER) * MIN_MS).unwrap() + 1,
    ));
    let store = st.manager.store().clone();
    let policy = st.manager.policy().clone();
    let swept = hibernate::sweep(&store, &policy, &Bindings::default(), AFTER)
        .await
        .unwrap();
    assert_eq!(swept, 1);
    st.wait_for("session ended", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    let summary = store.summary(s.clone()).await.unwrap().unwrap();
    assert!(summary.hibernated);
    assert_eq!(summary.state, SessionState::Exited);
    st.settled(&s).await;
    s
}

fn prompt(s: &SessionId, input: UserInput) -> SessionCommand {
    SessionCommand::Prompt {
        session_id: s.clone(),
        input,
    }
}

fn steer(s: &SessionId, input: UserInput) -> SessionCommand {
    SessionCommand::Steer {
        session_id: s.clone(),
        input,
    }
}

fn follow_up(s: &SessionId, input: UserInput) -> SessionCommand {
    SessionCommand::FollowUp {
        session_id: s.clone(),
        input,
    }
}

/// Sends `command` to a hibernated session and checks that it enqueued a
/// resume whose prompt is that input, and that the receipt names the input.
async fn resumes_with(command: impl Fn(&SessionId, UserInput) -> SessionCommand) {
    let mut st = Station::new(4).await;
    let s = hibernated(&mut st).await;
    let d = st.accepted(command(&s, text("wake up"))).await;
    let result = d.receipt.result.unwrap();
    assert_eq!(result.session_id.as_ref(), Some(&s));
    let input_id = result.input_id.expect("the receipt has the input id");
    let effects = st.manager.store().session_effects(s.clone()).await.unwrap();
    let resume = effects
        .iter()
        .find_map(|e| match &e.payload {
            EffectPayload::Resume {
                prompt: Some(p), ..
            } => Some(p.clone()),
            _ => None,
        })
        .expect("a resume effect with a prompt");
    assert_eq!(resume.input_id, input_id);
    assert_eq!(resume.input, text("wake up"));
    assert_eq!(st.state(&s).await, SessionState::Starting);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_follow_up_to_a_hibernated_session_resumes_it_with_that_input() {
    resumes_with(follow_up).await;
}

#[tokio::test]
async fn a_steer_to_a_hibernated_session_resumes_it_with_that_input() {
    resumes_with(steer).await;
}

#[tokio::test]
async fn a_prompt_to_a_hibernated_session_resumes_it_with_that_input() {
    resumes_with(prompt).await;
}

#[tokio::test]
async fn a_prompt_to_an_ended_session_that_is_not_hibernated_is_refused() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    st.wait_for("run completed", |e| {
        matches!(e, Event::RunCompleted { .. }).then_some(())
    })
    .await;
    st.accepted(SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Graceful,
    })
    .await;
    st.wait_for("session ended", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    st.settled(&s).await;
    for command in [prompt, steer, follow_up] {
        let d = st.send(command(&s, text("hi"))).await;
        assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
        let reject = d.receipt.reject.unwrap();
        assert_eq!(reject.code, RejectCode::InvalidState);
        assert_eq!(reject.message, "the session has ended");
    }
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_follow_up_to_a_hibernated_session_is_refused_at_the_session_limit() {
    let mut st = Station::new(1).await;
    let s = hibernated(&mut st).await;
    // The one slot is taken by another session.
    let ws = st.workspace();
    st.start(&ws, Some("hello")).await;
    assert_eq!(
        st.rejected(follow_up(&s, text("wake up"))).await,
        RejectCode::Forbidden
    );
    st.manager.shutdown(StopMode::Kill).await;
}
