//! Unit tests of the live Claude driver (M2 P2-3) with `fake-claude`:
//! probe, early exit, stop deadlines, a closed event sink, and `fork`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test harness: a panic is the failure report"
)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{SessionSpec, StopMode, UserInput};
use fleetdeck_proto::event::RunOrigin;
use fleetdeck_proto::{Event, InputId};
use fleetdeck_providers::claude::{
    ClaudeAdapter, ClaudeAdapterConfig, ClaudeSession, Origin, RawLogSettings,
};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_providers::rawlog::RawLogLimits;
use fleetdeck_providers::{
    AuthState, Emitted, EventSink, HostInput, LiveSession, ProviderAdapter, ProviderError,
};
use fleetdeck_testkit::FakeAgent;
use fleetdeck_testkit::fake::SCENARIO_ENV;
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(20);

fn adapter(scenario: &str, root: &std::path::Path) -> ClaudeAdapter {
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, scenario).unwrap();
    let mut config =
        ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
    config.extra_args = vec!["--safe-mode".to_owned()];
    config.stop = StopPolicy {
        grace: Duration::from_millis(300),
        term_grace: Duration::from_millis(300),
    };
    config.delivery_grace = Duration::from_millis(300);
    config.raw_log = Some(RawLogSettings {
        root: root.join("raw"),
        limits: RawLogLimits::default(),
    });
    ClaudeAdapter::new(config, Arc::new(DirectChildHost::new()))
}

fn spec(root: &std::path::Path) -> SessionSpec {
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    SessionSpec {
        provider_instance: "claude".into(),
        cwd: ws.to_string_lossy().into_owned(),
        mode: Some("default".to_owned()),
        model: Some("haiku".to_owned()),
        title: None,
    }
}

async fn start(scenario: &str, root: &std::path::Path) -> (ClaudeSession, mpsc::Receiver<Emitted>) {
    start_with(scenario, root, 256).await
}

async fn start_with(
    scenario: &str,
    root: &std::path::Path,
    capacity: usize,
) -> (ClaudeSession, mpsc::Receiver<Emitted>) {
    let (sink, rx) = EventSink::channel(capacity);
    let session = adapter(scenario, root)
        .start_session(spec(root), sink, Origin::New)
        .await
        .unwrap();
    (session, rx)
}

async fn next_event(rx: &mut mpsc::Receiver<Emitted>) -> Event {
    tokio::time::timeout(WAIT, rx.recv())
        .await
        .expect("an event in time")
        .expect("the sink is open")
        .event
}

/// Sends the first prompt of process 0 of a fixture: Claude writes
/// `system/init` only after the first user message.
async fn send_recorded_prompt(session: &mut ClaudeSession, path: &std::path::Path) {
    let records = &common::processes(path)[0];
    let cmd = records
        .iter()
        .find(|r| r["dir"] == "host" && r["cmd"]["op"] == "prompt")
        .unwrap();
    session
        .prompt(HostInput {
            id: InputId::new(cmd["cmd"]["input_id"].as_str().unwrap()),
            origin: RunOrigin::Board,
            input: UserInput {
                text: cmd["cmd"]["text"].as_str().unwrap().to_owned(),
            },
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn probe_reads_the_version_and_the_sign_in_state() {
    let tmp = tempfile::tempdir().unwrap();
    for (scenario, auth) in [
        (
            "auth:signed-in",
            AuthState::SignedIn {
                method: Some("claude.ai".to_owned()),
            },
        ),
        ("auth:signed-out", AuthState::SignedOut),
        // Trap 9: a failed check is unknown, never signed out.
        ("auth:broken", AuthState::Unknown),
    ] {
        let probe = adapter(scenario, tmp.path()).probe().await.unwrap();
        assert_eq!(probe.version.as_deref(), Some("2.1.999"), "{scenario}");
        assert_eq!(probe.auth, auth, "{scenario}");
    }
}

#[tokio::test]
async fn probe_of_a_missing_binary_reports_unknown() {
    let mut config = ClaudeAdapterConfig::new(
        "claude".into(),
        "/nonexistent/claude".into(),
        ChildEnv::from_process(),
    );
    config.probe_timeout = Duration::from_secs(2);
    let probe = ClaudeAdapter::new(config, Arc::new(DirectChildHost::new()))
        .probe()
        .await
        .unwrap();
    assert_eq!(probe.version, None);
    assert_eq!(probe.auth, AuthState::Unknown);
}

#[tokio::test]
async fn a_process_that_dies_at_start_ends_the_session_with_its_code() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut session, mut rx) = start("exit:7", tmp.path()).await;
    let event = next_event(&mut rx).await;
    assert_eq!(
        event,
        Event::SessionEnded {
            reason: "process_exit".to_owned(),
            exit_code: Some(7),
        }
    );
    session.stop(StopMode::Graceful).await.unwrap();
}

#[tokio::test]
async fn graceful_stop_kills_a_child_that_ignores_stdin_within_the_grace() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut session, mut rx) = start("deaf", tmp.path()).await;
    let started = Instant::now();
    tokio::time::timeout(WAIT, session.stop(StopMode::Graceful))
        .await
        .expect("stop keeps its deadline")
        .unwrap();
    // grace 300 ms + term grace 300 ms + the settle time of the host.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    let event = next_event(&mut rx).await;
    assert!(
        matches!(
            event,
            Event::SessionEnded {
                exit_code: None,
                ..
            }
        ),
        "{event:?}"
    );
}

#[tokio::test]
async fn kill_ends_the_session_at_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut session, mut rx) = start("deaf", tmp.path()).await;
    tokio::time::timeout(WAIT, session.stop(StopMode::Kill))
        .await
        .expect("kill returns")
        .unwrap();
    let event = next_event(&mut rx).await;
    assert!(matches!(
        event,
        Event::SessionEnded {
            exit_code: None,
            ..
        }
    ));
}

#[tokio::test]
async fn fork_before_the_session_id_is_known_is_not_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut session, _rx) = start("deaf", tmp.path()).await;
    assert!(matches!(
        session.fork().await,
        Err(ProviderError::NotReady(_))
    ));
    session.stop(StopMode::Kill).await.unwrap();
}

#[tokio::test]
async fn fork_returns_the_native_session_id_after_init() {
    let tmp = tempfile::tempdir().unwrap();
    let path = common::fixture_dir().join("simple.jsonl");
    let (mut session, mut rx) = start(&format!("fixture:{}#0", path.display()), tmp.path()).await;
    send_recorded_prompt(&mut session, &path).await;
    let refs = loop {
        if let Event::SessionStarted { provider_refs, .. } = next_event(&mut rx).await {
            break provider_refs;
        }
    };
    let id = session.fork().await.unwrap();
    assert_eq!(Some(id.0.as_str()), refs["session_id"].as_str());
    session.stop(StopMode::Kill).await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_closed_event_sink_kills_the_process() {
    let tmp = tempfile::tempdir().unwrap();
    let path = common::fixture_dir().join("simple.jsonl");
    let (mut session, rx) = start(&format!("fixture:{}#0", path.display()), tmp.path()).await;
    let proc_dir = std::path::PathBuf::from(format!("/proc/{}", session.pid()));
    drop(rx);
    send_recorded_prompt(&mut session, &path).await;
    // Nobody stops the session: the reader kills the process at the first
    // event that it cannot deliver, and the host reaps it.
    let started = Instant::now();
    while proc_dir.exists() {
        assert!(started.elapsed() < WAIT, "the process still runs");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    session.stop(StopMode::Graceful).await.unwrap();
    assert!(session.raw_log_dir().is_some_and(std::path::Path::is_dir));
}

/// Starts the simple fixture with a capacity-one sink that nobody drains,
/// and waits until the reader is blocked on the second event.
async fn blocked_reader(root: &std::path::Path) -> (ClaudeSession, mpsc::Receiver<Emitted>) {
    let path = common::fixture_dir().join("simple.jsonl");
    let (mut session, rx) = start_with(&format!("fixture:{}#0", path.display()), root, 1).await;
    tokio::time::timeout(WAIT, send_recorded_prompt(&mut session, &path))
        .await
        .expect("the prompt emits no event before session.started");
    let started = Instant::now();
    while rx.is_empty() {
        assert!(started.elapsed() < WAIT, "no event arrived");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The fixture sends several frames at once: give the reader time to
    // reach the full sink.
    tokio::time::sleep(Duration::from_millis(200)).await;
    (session, rx)
}

async fn assert_bounded_stop(how: StopMode) {
    let tmp = tempfile::tempdir().unwrap();
    let (mut session, rx) = blocked_reader(tmp.path()).await;
    let proc_dir = std::path::PathBuf::from(format!("/proc/{}", session.pid()));
    let started = Instant::now();
    let result = tokio::time::timeout(WAIT, session.stop(how))
        .await
        .expect("stop keeps its deadline with a full event sink");
    // grace 300 ms + term 300 ms + delivery 300 ms, plus slack.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{how:?}: {:?}",
        started.elapsed()
    );
    assert!(
        matches!(result, Err(ProviderError::EventsUndelivered)),
        "{how:?}: {result:?}"
    );
    if cfg!(target_os = "linux") {
        assert!(!proc_dir.exists(), "{how:?}: the process was reaped");
    }
    assert_eq!(rx.len(), 1, "the undrained sink still holds one event");
}

#[tokio::test]
async fn graceful_stop_is_bounded_when_an_undrained_sink_is_full() {
    assert_bounded_stop(StopMode::Graceful).await;
}

#[tokio::test]
async fn kill_is_bounded_when_an_undrained_sink_is_full() {
    assert_bounded_stop(StopMode::Kill).await;
}
