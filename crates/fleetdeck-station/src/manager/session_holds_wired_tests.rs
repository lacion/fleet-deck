//! A hold on a session refuses the inputs that could change the session or
//! its branch (FG-8, design §11.4), through the whole command path.

use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, StopMode, UserInput,
};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, RejectCode, SessionId};

use super::tests::{Station, text};
use crate::store::Caller;
use crate::store::session_holds::{HELD, KIND_LAND};

async fn send(
    st: &Station,
    caller: &Caller,
    id: &str,
    origin: CommandOrigin,
    command: SessionCommand,
) -> Option<String> {
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from(id.to_owned()),
        target_incarnation: None,
        retry_until: st.manager.store().now_ms() + RETRY_WINDOW_MS,
        origin,
        command,
    };
    let decided = st.manager.submit(envelope, caller.clone()).await.unwrap();
    match decided.receipt.state {
        ReceiptState::Rejected => {
            let reject = decided.receipt.reject.unwrap();
            assert_eq!(reject.code, RejectCode::InvalidState, "{reject:?}");
            Some(reject.message)
        }
        _ => None,
    }
}

/// A paired deck: the shell commands come only from the board (B8).
async fn paired_deck(st: &Station) -> Caller {
    let key = Identity::generate().unwrap().public_key();
    st.manager
        .store()
        .pin_peer(crate::store::peers::PairedKey {
            key,
            role: fleetdeck_link::tls::Role::Deck,
            scopes: [Scope::Read, Scope::Drive, Scope::Admin]
                .into_iter()
                .collect(),
            dial_url: None,
        })
        .await
        .unwrap();
    Caller::peer(key.fingerprint(), Scope::Admin)
}

async fn hold(st: &Station, session: &SessionId) {
    st.manager
        .store()
        .session_hold_insert(session.clone(), KIND_LAND, "land-1".to_owned())
        .await
        .unwrap();
}

async fn release(st: &Station, session: &SessionId) {
    let gone = st
        .manager
        .store()
        .session_hold_delete(session.clone(), "land-1".to_owned())
        .await
        .unwrap();
    assert!(gone);
}

async fn started(st: &Station) {
    st.wait_for("the start", |e| {
        matches!(e, Event::SessionStarted { .. }).then_some(())
    })
    .await;
}

#[tokio::test]
async fn a_held_live_session_refuses_input_a_revert_and_a_shell_command() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let deck = paired_deck(&st).await;
    let s = st.start(&ws, Some("hello")).await;
    started(&st).await;
    hold(&st, &s).await;
    let commands = [
        SessionCommand::Prompt {
            session_id: s.clone(),
            input: UserInput::new("hi".to_owned()),
        },
        SessionCommand::Steer {
            session_id: s.clone(),
            input: UserInput::new("hi".to_owned()),
        },
        SessionCommand::FollowUp {
            session_id: s.clone(),
            input: UserInput::new("hi".to_owned()),
        },
        SessionCommand::Revert {
            session_id: s.clone(),
            turn_id: "t1".into(),
        },
        SessionCommand::ShellCommand {
            session_id: s.clone(),
            command: "ls".to_owned(),
        },
    ];
    for (n, command) in commands.into_iter().enumerate() {
        let method = command.method();
        let got = send(
            &st,
            &deck,
            &format!("held-{n}"),
            CommandOrigin::Board,
            command,
        )
        .await;
        assert_eq!(got.as_deref(), Some(HELD), "{method}");
    }
    // A stop is not an input: the hold does not refuse it.
    let stop = SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Kill,
    };
    assert_eq!(
        send(&st, &deck, "stop", CommandOrigin::Board, stop).await,
        None
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_prompt_goes_through_when_the_hold_ends() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let deck = paired_deck(&st).await;
    let s = st.start(&ws, Some("hello")).await;
    started(&st).await;
    hold(&st, &s).await;
    let prompt = |s: &SessionId| SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("again"),
    };
    assert_eq!(
        send(&st, &deck, "p1", CommandOrigin::Board, prompt(&s))
            .await
            .as_deref(),
        Some(HELD)
    );
    release(&st, &s).await;
    assert_eq!(
        send(&st, &deck, "p2", CommandOrigin::Board, prompt(&s)).await,
        None
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_held_ended_session_refuses_a_resume_and_the_trash() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let deck = paired_deck(&st).await;
    let s = st.start(&ws, Some("hello")).await;
    started(&st).await;
    let stop = SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Kill,
    };
    assert_eq!(
        send(&st, &deck, "stop", CommandOrigin::Board, stop).await,
        None
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while st.state(&s).await != SessionState::Exited {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the session did not end"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    hold(&st, &s).await;
    let resume = SessionCommand::Resume {
        session_id: s.clone(),
    };
    assert_eq!(
        send(&st, &deck, "resume", CommandOrigin::Board, resume)
            .await
            .as_deref(),
        Some(HELD)
    );
    let trash = SessionCommand::WorktreeTrash {
        session_id: s.clone(),
    };
    assert_eq!(
        send(&st, &deck, "trash", CommandOrigin::Board, trash)
            .await
            .as_deref(),
        Some(HELD)
    );
    st.manager.shutdown(StopMode::Kill).await;
}
