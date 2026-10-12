//! The Codex driver against `fake-codex app-server` scenario `sim` (M2
//! P5-2): the probe (trap 9), a live session with approvals, steer and
//! interrupt, and the automatic background poll (DL-024).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test harness: a panic is the failure report"
)]

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{ApprovalDecision, SessionSpec, StopMode, UserInput};
use fleetdeck_proto::event::{ResolvedBy, RunOrigin, TurnStatus};
use fleetdeck_proto::{Event, InputId};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig, CodexSession, Origin};
use fleetdeck_providers::host::DirectChildHost;
use fleetdeck_providers::{
    Answer, AuthState, Emitted, EventSink, HostInput, LiveSession, NativeSessionId, ProviderAdapter,
};
use fleetdeck_testkit::FakeAgent;
use fleetdeck_testkit::fake::SCENARIO_ENV;
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(20);

fn adapter(scenario: &str, poll: Option<Duration>) -> CodexAdapter {
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, scenario).unwrap();
    let mut config =
        CodexAdapterConfig::new("codex".into(), FakeAgent::Codex.locate().unwrap(), env);
    config.background_poll = poll;
    CodexAdapter::new(config, Arc::new(DirectChildHost::new()))
}

fn spec(cwd: &std::path::Path) -> SessionSpec {
    SessionSpec {
        provider_instance: "codex".into(),
        cwd: cwd.to_string_lossy().into_owned(),
        mode: None,
        model: None,
        title: None,
        coordinator: None,
    }
}

fn input(id: &str, text: &str) -> HostInput {
    HostInput {
        id: InputId::new(id),
        origin: RunOrigin::Board,
        input: UserInput::new(text.to_owned()),
    }
}

struct Live {
    session: CodexSession,
    rx: mpsc::Receiver<Emitted>,
    _tmp: tempfile::TempDir,
}

async fn start(poll: Option<Duration>, origin: Origin) -> Live {
    let tmp = tempfile::tempdir().unwrap();
    let (sink, rx) = EventSink::channel(256);
    let session = tokio::time::timeout(
        WAIT,
        adapter("sim", poll).start_session(spec(tmp.path()), sink, origin),
    )
    .await
    .unwrap()
    .unwrap();
    Live {
        session,
        rx,
        _tmp: tmp,
    }
}

/// Reads events until `stop` holds for one; returns them all.
async fn until(rx: &mut mpsc::Receiver<Emitted>, stop: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let e = tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("no matching event; saw {seen:#?}"))
            .expect("the event stream ended")
            .event;
        let hit = stop(&e);
        seen.push(e);
        if hit {
            return seen;
        }
    }
}

fn turn_completed(e: &Event) -> bool {
    matches!(e, Event::TurnCompleted { .. })
}

fn texts(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::MessageCompleted { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn probe_reports_the_version_and_a_definite_sign_in_only() {
    let signed_in = adapter("auth:signed-in", None).probe().await.unwrap();
    assert_eq!(signed_in.version.as_deref(), Some("0.160.999"));
    assert_eq!(
        signed_in.auth,
        AuthState::SignedIn {
            method: Some("chatgpt".into())
        }
    );
    let signed_out = adapter("auth:signed-out", None).probe().await.unwrap();
    assert_eq!(signed_out.auth, AuthState::SignedOut);
    // An app-server that fails at once: unknown, never signed out (trap 9).
    let broken = adapter("exit:1", None).probe().await.unwrap();
    assert_eq!(broken.auth, AuthState::Unknown);
}

#[tokio::test]
async fn a_session_is_open_when_start_returns_and_runs_turns() {
    let mut live = start(None, Origin::New).await;
    let thread = live.session.thread_id().await.expect("an open thread");
    let started = until(&mut live.rx, |e| matches!(e, Event::SessionStarted { .. })).await;
    let Some(Event::SessionStarted {
        provider_version,
        provider_refs,
        ..
    }) = started.last()
    else {
        unreachable!()
    };
    assert_eq!(provider_version.as_deref(), Some("0.160.999"));
    assert_eq!(
        provider_refs.get("thread_id").and_then(|v| v.as_str()),
        Some(thread.as_str())
    );
    live.session.prompt(input("in-1", "hello")).await.unwrap();
    let events = until(&mut live.rx, turn_completed).await;
    assert_eq!(texts(&events), ["echo: hello"]);
    live.session.stop(StopMode::Graceful).await.unwrap();
    let rest = until(&mut live.rx, |e| matches!(e, Event::SessionEnded { .. })).await;
    assert!(matches!(rest.last(), Some(Event::SessionEnded { .. })));
}

#[tokio::test]
async fn approvals_steer_and_interrupt_reach_the_app_server() {
    let mut live = start(None, Origin::New).await;
    live.session
        .prompt(input("in-1", "ask touch x"))
        .await
        .unwrap();
    let events = until(&mut live.rx, |e| {
        matches!(e, Event::ApprovalRequested { .. })
    })
    .await;
    let Some(Event::ApprovalRequested {
        request_id,
        options,
        ..
    }) = events.last()
    else {
        unreachable!()
    };
    assert!(
        options.iter().any(|o| o.option_id == "accept"),
        "{options:?}"
    );
    live.session
        .answer(Answer {
            request: request_id.clone(),
            decision: ApprovalDecision {
                option_id: "accept".into(),
                updated_input: None,
                text: None,
            },
            by: ResolvedBy::Board,
        })
        .await
        .unwrap();
    let events = until(&mut live.rx, turn_completed).await;
    assert!(
        events.iter().any(
            |e| matches!(e, Event::ApprovalResolved { option_id, .. } if option_id == "accept")
        )
    );
    assert_eq!(texts(&events), ["ran: touch x"]);

    live.session.prompt(input("in-2", "wait a")).await.unwrap();
    until(&mut live.rx, |e| matches!(e, Event::MessageDelta { .. })).await;
    live.session.steer(input("in-3", "stop now")).await.unwrap();
    let events = until(&mut live.rx, turn_completed).await;
    assert_eq!(texts(&events), ["steered: stop now"]);

    live.session.prompt(input("in-4", "wait b")).await.unwrap();
    until(&mut live.rx, |e| matches!(e, Event::MessageDelta { .. })).await;
    live.session.interrupt(None).await.unwrap();
    let events = until(&mut live.rx, turn_completed).await;
    assert!(matches!(
        events.last(),
        Some(Event::TurnCompleted {
            status: TurnStatus::Interrupted,
            ..
        })
    ));
    live.session.stop(StopMode::Kill).await.unwrap();
}

#[tokio::test]
async fn the_driver_polls_background_terminals_until_they_finish() {
    let mut live = start(Some(Duration::from_millis(50)), Origin::New).await;
    live.session
        .prompt(input("in-1", "bg sleep 1"))
        .await
        .unwrap();
    let events = until(
        &mut live.rx,
        |e| matches!(e, Event::BackgroundUpdated { tasks } if !tasks.is_empty()),
    )
    .await;
    assert!(events.iter().any(|e| matches!(
        e,
        Event::RunCompleted {
            background_pending: true,
            ..
        }
    )));
    // The second poll completes the command: the list empties without any
    // host command.
    let events = until(
        &mut live.rx,
        |e| matches!(e, Event::BackgroundUpdated { tasks } if tasks.is_empty()),
    )
    .await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ToolCompleted { .. })),
        "{events:#?}"
    );
    live.session.stop(StopMode::Graceful).await.unwrap();
}

#[tokio::test]
async fn resume_and_fork_open_the_named_thread() {
    let resumed = start(None, Origin::Resume(NativeSessionId("t-old".into()))).await;
    assert_eq!(resumed.session.thread_id().await.as_deref(), Some("t-old"));
    let mut forked = start(None, Origin::Fork(NativeSessionId("t-old".into()))).await;
    let forked_id = forked.session.thread_id().await.unwrap();
    assert_ne!(forked_id, "t-old");
    assert_eq!(
        forked.session.fork().await.unwrap(),
        NativeSessionId(forked_id)
    );
}

#[tokio::test]
async fn a_failed_handshake_fails_the_start() {
    let tmp = tempfile::tempdir().unwrap();
    let (sink, _rx) = EventSink::channel(16);
    let started = tokio::time::timeout(
        WAIT,
        adapter("exit:3", None).start_session(spec(tmp.path()), sink, Origin::New),
    )
    .await
    .unwrap();
    let Err(err) = started else {
        panic!("no session without a handshake");
    };
    assert!(
        matches!(err, fleetdeck_providers::ProviderError::SessionEnded),
        "{err}"
    );
}
