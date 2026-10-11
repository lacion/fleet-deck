//! Opt-in live smoke tests (M2 P5-5, DL-067): one per provider, with the
//! real CLI, one tiny prompt, in a new temporary directory.
//!
//! They are not part of the default run (`make check`): each test is
//! `#[ignore]`d and also does nothing unless `FD_LIVE_SMOKE=1`. They use the
//! owner's provider sign-in and spend a few tokens. Run them with:
//!
//! ```sh
//! make smoke-live                      # both providers
//! FD_LIVE_SMOKE=1 cargo test -p fleetdeck-providers --test live_smoke \
//!     -- --ignored --nocapture live_codex   # one provider
//! ```
//!
//! `FD_CLAUDE_BIN` and `FD_CODEX_BIN` choose other binaries (default:
//! `claude` and `codex` on `PATH`). See `docs/v2/LIVE-SMOKE.md`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test harness: a panic is the failure report"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{SessionSpec, StopMode, UserInput};
use fleetdeck_proto::event::{RunOrigin, TurnStatus};
use fleetdeck_proto::{Event, InputId};
use fleetdeck_providers::host::DirectChildHost;
use fleetdeck_providers::{
    AuthState, Emitted, EventSink, HostInput, LiveSession, ProviderAdapter, claude, codex,
};
use tokio::sync::mpsc;

/// The bound of one live step (start, or one turn).
const STEP: Duration = Duration::from_secs(180);
/// The prompt: one short answer and no tool.
const PROMPT: &str = "Reply with the single word pong. Do not use any tool.";

/// True when the owner opted in; otherwise the test says why it skips.
fn opted_in(name: &str) -> bool {
    let on = std::env::var("FD_LIVE_SMOKE").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("{name}: skipped (set FD_LIVE_SMOKE=1 to run the live smoke tests)");
    }
    on
}

fn program(var: &str, default: &str) -> PathBuf {
    PathBuf::from(std::env::var(var).unwrap_or_else(|_| default.to_owned()))
}

fn spec(instance: &str, cwd: &std::path::Path, mode: &str) -> SessionSpec {
    SessionSpec {
        provider_instance: instance.into(),
        cwd: cwd.to_string_lossy().into_owned(),
        mode: Some(mode.to_owned()),
        model: None,
        title: None,
    }
}

fn input(text: &str) -> HostInput {
    HostInput {
        // A station input id is a ULID (Claude derives its message uuid
        // from it).
        id: InputId::new("01K70000000000000000000001"),
        origin: RunOrigin::Board,
        input: UserInput {
            text: text.to_owned(),
        },
    }
}

/// Reads events until a turn completes; returns them all.
async fn one_turn(rx: &mut mpsc::Receiver<Emitted>) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let e = tokio::time::timeout(STEP, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("no turn.completed within {STEP:?}; saw {seen:#?}"))
            .expect("the event stream ended before the turn completed")
            .event;
        let done = matches!(e, Event::TurnCompleted { .. });
        seen.push(e);
        if done {
            return seen;
        }
    }
}

/// The checks that both providers share: a started session with the
/// provider's version, one completed turn with a non-empty answer, and no
/// approval request (the prompt needs no tool).
fn check_turn(events: &[Event]) {
    assert!(
        matches!(events.first(), Some(Event::SessionStarted { .. })),
        "the first event is session.started: {events:#?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::TurnCompleted {
                status: TurnStatus::Done,
                ..
            }
        )),
        "the turn ends done: {events:#?}"
    );
    let answer: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::MessageCompleted { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        answer.iter().any(|t| t.to_lowercase().contains("pong")),
        "the answer says pong: {answer:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::ApprovalRequested { .. })),
        "no approval request: {events:#?}"
    );
}

fn signed_in(auth: &AuthState) {
    assert!(
        matches!(auth, AuthState::SignedIn { .. } | AuthState::Unknown),
        "the provider is not signed out: {auth:?}"
    );
}

#[tokio::test]
#[ignore = "live: needs FD_LIVE_SMOKE=1, a signed-in `claude`, and network"]
async fn live_claude_smoke() {
    if !opted_in("live_claude_smoke") {
        return;
    }
    let mut config = claude::ClaudeAdapterConfig::new(
        "claude".into(),
        program("FD_CLAUDE_BIN", "claude"),
        ChildEnv::from_process(),
    );
    // No hooks, plugins, CLAUDE.md or MCP servers of the owner's setup
    // (DL-005); the sign-in still works.
    config.extra_args = vec!["--safe-mode".to_owned()];
    let adapter = claude::ClaudeAdapter::new(config, Arc::new(DirectChildHost::new()));
    let probe = adapter.probe().await.unwrap();
    eprintln!("claude probe: {probe:?}");
    assert!(probe.version.is_some(), "claude --version answers");
    signed_in(&probe.auth);

    let tmp = tempfile::tempdir().unwrap();
    let (sink, mut rx) = EventSink::channel(1024);
    let mut session = tokio::time::timeout(
        STEP,
        adapter.start_session(
            spec("claude", tmp.path(), claude::SAFEST_MODE),
            sink,
            claude::Origin::New,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    session.prompt(input(PROMPT)).await.unwrap();
    let events = one_turn(&mut rx).await;
    check_turn(&events);
    session.stop(StopMode::Graceful).await.unwrap();
    eprintln!("live_claude_smoke: ok ({} events)", events.len());
}

#[tokio::test]
#[ignore = "live: needs FD_LIVE_SMOKE=1, a signed-in `codex`, and network"]
async fn live_codex_smoke() {
    if !opted_in("live_codex_smoke") {
        return;
    }
    let mut config = codex::CodexAdapterConfig::new(
        "codex".into(),
        program("FD_CODEX_BIN", "codex"),
        ChildEnv::from_process(),
    );
    config.effort = Some("low".to_owned());
    let adapter = codex::CodexAdapter::new(config, Arc::new(DirectChildHost::new()));
    let probe = adapter.probe().await.unwrap();
    eprintln!("codex probe: {probe:?}");
    assert!(probe.version.is_some(), "codex --version answers");
    signed_in(&probe.auth);

    let tmp = tempfile::tempdir().unwrap();
    let (sink, mut rx) = EventSink::channel(1024);
    let mut session = tokio::time::timeout(
        STEP,
        adapter.start_session(
            spec("codex", tmp.path(), codex::SAFEST_MODE),
            sink,
            codex::Origin::New,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    session.prompt(input(PROMPT)).await.unwrap();
    let events = one_turn(&mut rx).await;
    check_turn(&events);
    session.stop(StopMode::Graceful).await.unwrap();
    eprintln!("live_codex_smoke: ok ({} events)", events.len());
}
