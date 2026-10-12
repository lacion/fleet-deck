//! `session.resume` end to end (DL-118) against `fake-claude sim` and
//! `fake-codex sim`: an ended session gets a new provider process that
//! continues the same native session, and every admission rule stores a
//! rejection that spawns nothing.

use fleetdeck_proto::event::ProviderRefs;

use super::*;
use crate::store::NewShim;

/// The `session.started` events of `session`: `(resumed, provider_refs)`.
async fn starts(st: &Station, session: &SessionId) -> Vec<(Option<bool>, ProviderRefs)> {
    st.events()
        .await
        .into_iter()
        .filter(|e| e.session_id == *session)
        .filter_map(|e| match e.event {
            EventBody::Known(Event::SessionStarted {
                resumed,
                provider_refs,
                ..
            }) => Some((resumed, provider_refs)),
            _ => None,
        })
        .collect()
}

async fn wait_state(st: &Station, session: &SessionId, want: SessionState) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let state = st.state(session).await;
        if state == want {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{state:?}, not {want:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_starts(
    st: &Station,
    session: &SessionId,
    n: usize,
) -> Vec<(Option<bool>, ProviderRefs)> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let found = starts(st, session).await;
        if found.len() >= n {
            return found;
        }
        assert!(tokio::time::Instant::now() < deadline, "{found:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn done_turns(st: &Station, session: &SessionId) -> usize {
    st.events()
        .await
        .into_iter()
        .filter(|e| e.session_id == *session)
        .filter(|e| {
            matches!(
                &e.event,
                EventBody::Known(Event::TurnCompleted {
                    status: TurnStatus::Done,
                    ..
                })
            )
        })
        .count()
}

async fn wait_done(st: &Station, session: &SessionId, n: usize) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while done_turns(st, session).await < n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:#?}",
            kinds(&st.events().await)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Starts a session of `instance`, runs one turn, and stops it.
async fn ended_session(st: &mut Station, instance: &str) -> SessionId {
    let ws = st.workspace();
    let mut spec = spec(&ws, instance);
    if instance == "codex" {
        spec.mode = None;
    }
    let s = st.start_with(spec, Some("hello")).await;
    wait_done(st, &s, 1).await;
    st.accepted(SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Graceful,
    })
    .await;
    wait_state(st, &s, SessionState::Exited).await;
    st.settled(&s).await;
    s
}

async fn resumes_the_same_native_session(instance: &str, key: &str) {
    let mut st = Station::new(4).await;
    let s = ended_session(&mut st, instance).await;
    let before = starts(&st, &s).await;
    assert_eq!(before.len(), 1);
    let native = before[0].1.get(key).cloned().expect("a native id");

    st.accepted(SessionCommand::Resume {
        session_id: s.clone(),
    })
    .await;
    assert_eq!(st.state(&s).await, SessionState::Starting);
    // The resumed process takes input once it is live (a Claude CLI
    // reports `session.started` only after its first input).
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let d = st
            .send(SessionCommand::Prompt {
                session_id: s.clone(),
                input: text("again"),
            })
            .await;
        if d.receipt.state == ReceiptState::Accepted {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{:?}", d.receipt);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    wait_done(&st, &s, 2).await;
    let after = wait_starts(&st, &s, 2).await;
    assert_eq!(after[1].0, Some(true), "the board shows `resumed`");
    assert_eq!(
        after[1].1.get(key),
        Some(&native),
        "the same native session"
    );
    wait_state(&st, &s, SessionState::Running).await;
    let effects = st.settled(&s).await;
    assert!(
        effects.contains(&("resume", EffectState::Confirmed)),
        "{effects:?}"
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_stopped_codex_session_resumes_its_thread_in_a_new_process() {
    resumes_the_same_native_session("codex", "thread_id").await;
}

#[tokio::test]
async fn a_stopped_claude_session_resumes_its_native_session_in_a_new_process() {
    resumes_the_same_native_session("claude", "session_id").await;
}

#[tokio::test]
async fn a_resume_is_refused_unless_the_session_ended_and_its_execution_is_proven_ended() {
    let mut st = Station::new(4).await;
    let unknown = st
        .rejected(SessionCommand::Resume {
            session_id: "nope".into(),
        })
        .await;
    assert_eq!(unknown, RejectCode::UnknownSession);

    // A running session is not resumed.
    let ws = st.workspace();
    let running = st.start(&ws, Some("hello")).await;
    wait_done(&st, &running, 1).await;
    let code = st
        .rejected(SessionCommand::Resume {
            session_id: running.clone(),
        })
        .await;
    assert_eq!(code, RejectCode::InvalidState);
    assert_eq!(starts(&st, &running).await.len(), 1);

    // An ended session whose shim was lost without termination evidence
    // stays `not_resumable` (ADR-005 §6).
    let s = ended_session(&mut st, "claude").await;
    let store = st.manager.store().clone();
    store
        .shim_starting(NewShim {
            shim_id: "shim-1".to_owned(),
            session_id: s.clone(),
            socket_path: "/nonexistent/sock".into(),
        })
        .await
        .unwrap();
    store
        .shim_end(
            "shim-1".to_owned(),
            crate::store::ShimState::Lost,
            "execution_unknown".to_owned(),
        )
        .await
        .unwrap();
    let effects_before = st.effect_states(&s).await;
    let d = st
        .send(SessionCommand::Resume {
            session_id: s.clone(),
        })
        .await;
    assert_eq!(d.receipt.state, ReceiptState::Rejected);
    let reject = d.receipt.reject.clone().unwrap();
    assert_eq!(reject.code, RejectCode::InvalidState);
    assert!(reject.message.contains("not_resumable"), "{reject:?}");
    // Nothing was queued or spawned, and a repeat returns the same receipt.
    assert_eq!(st.effect_states(&s).await, effects_before);
    assert_eq!(starts(&st, &s).await.len(), 1);
    let again = st
        .manager
        .submit(
            CommandEnvelope {
                command_id: CommandId::from(format!("cmd-{}", st.next)),
                target_incarnation: None,
                retry_until: i64::try_from(st.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
                origin: CommandOrigin::Local,
                command: SessionCommand::Resume {
                    session_id: s.clone(),
                },
            },
            Caller::local(),
        )
        .await
        .unwrap();
    assert_eq!(again.receipt, d.receipt);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_resume_without_a_free_session_slot_is_refused() {
    let mut st = Station::new(1).await;
    let s = ended_session(&mut st, "claude").await;
    let ws = st.workspace();
    let other = st.start(&ws, Some("hello")).await;
    wait_done(&st, &other, 1).await;
    let code = st
        .rejected(SessionCommand::Resume {
            session_id: s.clone(),
        })
        .await;
    assert_eq!(code, RejectCode::Forbidden);
    assert_eq!(starts(&st, &s).await.len(), 1);
    st.manager.shutdown(StopMode::Kill).await;
}
