//! Real-process tests of `ShimHost` (ADR-005 §2–§4, slice 3, DL-102): the
//! station side of the shim through `SessionHost`/`ProcessControl`, with
//! `/bin/sh` fake providers. A test whose station half (the `shims` table,
//! the adapter checkpoint, recovery) comes in a later slice has the suffix
//! `_at_the_host`.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_providers::host::shim::host::{AckMode, Attached, ShimHost, ShimHostConfig};
use fleetdeck_providers::host::shim::wire::GapReason;
use fleetdeck_providers::host::{
    EndCause, HostError, HostKind, ProcessControl as _, ProcessSpec, SessionHost as _, StopPolicy,
    Termination,
};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, BufReader};

mod shim_reaper;

const WAIT: Duration = Duration::from_secs(10);

/// An echo provider: it answers every stdin line.
const ECHO: &str = r#"while IFS= read -r l; do printf 'got:%s\n' "$l"; done"#;

struct Rig {
    tmp: tempfile::TempDir,
    host: ShimHost,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let station = tmp.path().join("station");
        std::fs::create_dir(&station).unwrap();
        let mut config = ShimHostConfig::new(
            PathBuf::from(env!("CARGO_BIN_EXE_fleetdeck")),
            station,
            "inc-host".to_owned(),
        );
        config.runtime_dir = None;
        config.tmp_dir = tmp.path().to_owned();
        config.buffer_bytes = 1024 * 1024;
        Self {
            tmp,
            host: ShimHost::new(config),
        }
    }

    fn spec(&self, script: &str) -> ProcessSpec {
        let env = ChildEnv::scrubbed([("PATH".into(), "/usr/bin:/bin".into())]);
        ProcessSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: self.tmp.path().to_owned(),
            env,
            launch: None,
        }
    }

    async fn launch(&self, script: &str, mode: AckMode) -> Attached {
        self.host.launch(&self.spec(script), mode).await.unwrap()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        shim_reaper::reap(&self.host.config().shims_dir());
    }
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rfind(')')
                .map(|i| s[i + 1..].trim_start().starts_with('Z'))
        })
        == Some(false)
}

async fn line(r: &mut BufReader<tokio::io::DuplexStream>) -> String {
    let mut text = String::new();
    tokio::time::timeout(WAIT, r.read_line(&mut text))
        .await
        .unwrap()
        .unwrap();
    text
}

#[tokio::test]
async fn a_shim_hosted_process_writes_reads_and_ends_through_the_session_host() {
    let rig = Rig::new();
    assert_eq!(rig.host.kind(), HostKind::Shim);
    assert!(rig.host.survives_station_restart());
    let hosted = rig.host.spawn(rig.spec(ECHO)).await.unwrap();
    let pid = hosted.control.pid();
    assert!(alive(pid));
    let mut out = BufReader::new(hosted.stdout);
    hosted.control.write(b"one\ntwo\n").await.unwrap();
    let mut text = String::new();
    while text.lines().count() < 2 {
        tokio::time::timeout(WAIT, out.read_line(&mut text))
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(text, "got:one\ngot:two\n");
    // A clean end: stdin closes, the provider exits, stdout ends.
    hosted.control.close_stdin().await;
    let report = tokio::time::timeout(WAIT, hosted.control.wait())
        .await
        .unwrap();
    assert_eq!(report.termination, Termination::Code(0));
    assert_eq!(report.cause, EndCause::Exited);
    let mut rest = Vec::new();
    tokio::time::timeout(WAIT, out.read_to_end(&mut rest))
        .await
        .unwrap()
        .unwrap();
    assert!(rest.is_empty());
    assert!(matches!(
        hosted.control.write(b"late\n").await,
        Err(HostError::StdinClosed)
    ));
    assert!(!alive(pid));
}

#[tokio::test]
async fn dropping_every_station_handle_never_stops_the_provider_and_a_reattach_takes_over_at_the_host()
 {
    let rig = Rig::new();
    let first = rig.launch(ECHO, AckMode::OnDelivery).await;
    let record = first.record.clone();
    let pid = first.process.pid();
    let mut out = BufReader::new(first.stdout);
    first.process.write(b"before\n").await.unwrap();
    assert_eq!(line(&mut out).await, "got:before\n");
    let mut record = record;
    record.cursor = first.process.acked();
    record.next_write_seq = first.process.next_write_seq();
    assert_eq!(record.cursor, 11);
    drop((first.process, out));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(alive(pid));
    let second = rig
        .host
        .reattach(&record, AckMode::OnDelivery)
        .await
        .unwrap();
    assert_eq!(second.record.generation, 2);
    let mut out = BufReader::new(second.stdout);
    second.process.write(b"after\n").await.unwrap();
    // The replay starts at the cursor: the old line is not delivered again.
    assert_eq!(line(&mut out).await, "got:after\n");
    let report = second.process.kill().await;
    assert_eq!(report.cause, EndCause::Killed);
    assert_eq!(report.termination, Termination::Signal(9));
    assert!(!alive(pid));
}

#[tokio::test]
async fn output_written_while_the_station_was_down_is_replayed_from_the_committed_cursor_exactly_once_at_the_host()
 {
    let rig = Rig::new();
    let script = "i=0; while [ $i -lt 30 ]; do echo line$i; i=$((i+1)); sleep 0.02; done; sleep 30";
    let first = rig.launch(script, AckMode::Manual).await;
    let mut out = BufReader::new(first.stdout);
    let mut seen = String::new();
    for _ in 0..5 {
        seen.push_str(&line(&mut out).await);
    }
    // The station commits the events of five lines, then acknowledges.
    let cursor = seen.len() as u64;
    assert!(first.process.ack(cursor).await);
    let mut record = first.record.clone();
    record.cursor = cursor;
    // The station dies after two more lines that it never committed.
    line(&mut out).await;
    line(&mut out).await;
    drop((first.process, out));
    tokio::time::sleep(Duration::from_millis(700)).await;
    let second = rig.host.reattach(&record, AckMode::Manual).await.unwrap();
    assert_eq!(second.floor, cursor);
    let mut out = BufReader::new(second.stdout);
    for i in 5..30 {
        seen.push_str(&line(&mut out).await);
        assert!(seen.ends_with(&format!("line{i}\n")), "{seen}");
    }
    let expected: String = (0..30).map(|i| format!("line{i}\n")).collect();
    assert_eq!(seen, expected);
    second.process.kill().await;
}

#[tokio::test]
async fn an_exit_while_the_station_was_down_is_reported_with_its_exit_state_after_reattach_at_the_host()
 {
    let rig = Rig::new();
    let first = rig
        .launch("read l; echo bye; echo oops >&2; exit 7", AckMode::Manual)
        .await;
    let pid = first.process.pid();
    first.process.write(b"go\n").await.unwrap();
    let record = first.record.clone();
    drop(first);
    let deadline = tokio::time::Instant::now() + WAIT;
    while alive(pid) {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let second = rig.host.reattach(&record, AckMode::Manual).await.unwrap();
    let mut out = BufReader::new(second.stdout);
    assert_eq!(line(&mut out).await, "bye\n");
    let report = tokio::time::timeout(WAIT, second.process.wait())
        .await
        .unwrap();
    assert_eq!(report.termination, Termination::Code(7));
    assert_eq!(report.cause, EndCause::Exited);
    assert!(report.stderr_tail.contains("oops"));
    // The exit stays until the station acknowledges it.
    assert!(record.socket_path.exists());
    assert!(second.process.ack_exit().await);
    let deadline = tokio::time::Instant::now() + WAIT;
    while record.socket_path.exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_stale_attachment_after_a_takeover_cannot_write_or_stop() {
    let rig = Rig::new();
    let first = rig.launch(ECHO, AckMode::OnDelivery).await;
    let pid = first.process.pid();
    let second = rig
        .host
        .reattach(&first.record, AckMode::OnDelivery)
        .await
        .unwrap();
    // The old attachment is closed: a write fails, a kill does nothing.
    assert!(first.process.write(b"old\n").await.is_err());
    let report = tokio::time::timeout(WAIT, first.process.kill())
        .await
        .unwrap();
    assert_eq!(report.termination, Termination::Unknown);
    assert!(alive(pid));
    let mut out = BufReader::new(second.stdout);
    second.process.write(b"new\n").await.unwrap();
    assert_eq!(line(&mut out).await, "got:new\n");
    // A reattach that claims an old generation is refused.
    let mut stale = first.record.clone();
    stale.generation = 0;
    let refused = rig.host.reattach(&stale, AckMode::OnDelivery).await;
    assert!(matches!(refused, Err(HostError::Shim(_))), "{refused:?}");
    second.process.kill().await;
}

#[tokio::test]
async fn a_reattach_with_another_provider_identity_is_refused() {
    let rig = Rig::new();
    let first = rig.launch(ECHO, AckMode::OnDelivery).await;
    let mut other = first.record.clone();
    if let Some(identity) = other.provider_identity.as_mut() {
        identity.start_time += 1;
    }
    let refused = rig.host.reattach(&other, AckMode::OnDelivery).await;
    assert!(
        matches!(&refused, Err(HostError::Shim(m)) if m == "identity_mismatch"),
        "{refused:?}"
    );
    first.process.kill().await;
}

#[tokio::test]
async fn a_blocked_stdin_write_returns_stdin_closed_after_a_kill() {
    let rig = Rig::new();
    let attached = rig.launch("sleep 60", AckMode::OnDelivery).await;
    let process = attached.process;
    let writer = {
        let process = std::sync::Arc::clone(&process);
        tokio::spawn(async move { process.write(&vec![b'x'; 900 * 1024]).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!writer.is_finished());
    // The kill returns after the shim's exit check of /proc, which can use
    // its whole budget (5 s) on a loaded machine.
    let report = tokio::time::timeout(WAIT, process.kill()).await.unwrap();
    assert_eq!(report.cause, EndCause::Killed);
    let result = tokio::time::timeout(WAIT, writer).await.unwrap().unwrap();
    assert!(matches!(result, Err(HostError::StdinClosed)), "{result:?}");
}

/// A kill sent at once after the launch can reach the shim before the
/// exec helper has called `setsid()`. It still ends the provider (the
/// shim signals the child itself when its group does not exist yet).
#[tokio::test]
async fn a_kill_right_after_the_launch_ends_the_provider() {
    let rig = Rig::new();
    for _ in 0..10 {
        let attached = rig.launch("sleep 60", AckMode::OnDelivery).await;
        let report = tokio::time::timeout(Duration::from_secs(5), attached.process.kill())
            .await
            .expect("the kill ends the provider without a wait for its natural exit");
        assert_eq!(report.cause, EndCause::Killed);
    }
}

#[tokio::test]
async fn a_stop_escalates_for_a_provider_that_ignores_stdin_and_term() {
    let rig = Rig::new();
    let attached = rig
        .launch(
            "trap '' TERM; while :; do sleep 0.1; done",
            AckMode::OnDelivery,
        )
        .await;
    let policy = StopPolicy {
        grace: Duration::from_millis(200),
        term_grace: Duration::from_millis(200),
    };
    let report = tokio::time::timeout(WAIT, attached.process.stop(policy))
        .await
        .unwrap();
    assert_eq!(report.cause, EndCause::StopForced);
    assert_eq!(report.termination, Termination::Signal(9));
    assert!(!alive(attached.process.pid()));
}

#[tokio::test]
async fn an_overlong_line_reaches_the_host_as_an_explicit_gap() {
    let rig = Rig::new();
    let attached = rig
        .launch(
            "echo first; head -c 3000000 /dev/zero | tr '\\0' x; sleep 30",
            AckMode::OnDelivery,
        )
        .await;
    let mut out = BufReader::new(attached.stdout);
    assert_eq!(line(&mut out).await, "first\n");
    let report = tokio::time::timeout(WAIT, attached.process.wait())
        .await
        .unwrap();
    assert_eq!(report.cause, EndCause::StopForced);
    let gaps = attached.process.gaps();
    assert!(
        gaps.iter()
            .any(|(_, _, reason)| *reason == GapReason::OutputFrameTooLarge),
        "{gaps:?}"
    );
}

#[tokio::test]
async fn a_claude_session_runs_a_turn_through_the_shim_host() {
    use fleetdeck_proto::command::{SessionSpec, StopMode, UserInput};
    use fleetdeck_proto::event::RunOrigin;
    use fleetdeck_proto::{Event, InputId};
    use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig, Origin};
    use fleetdeck_providers::{EventSink, HostInput, LiveSession as _};
    use fleetdeck_testkit::FakeAgent;
    use fleetdeck_testkit::fake::SCENARIO_ENV;

    let rig = Rig::new();
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, "sim").unwrap();
    let mut config =
        ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
    config.stop = StopPolicy {
        grace: Duration::from_millis(300),
        term_grace: Duration::from_millis(300),
    };
    let adapter = ClaudeAdapter::new(config, std::sync::Arc::new(rig.host.clone()));
    let ws = rig.tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let spec = SessionSpec {
        provider_instance: "claude".into(),
        cwd: ws.to_string_lossy().into_owned(),
        mode: Some("default".to_owned()),
        model: Some("haiku".to_owned()),
        title: None,
        coordinator: None,
        launch: None,
    };
    let (sink, mut rx) = EventSink::channel(256);
    let mut session = adapter
        .start_session(spec, sink, Origin::New)
        .await
        .unwrap();
    session
        .prompt(HostInput {
            id: InputId::new("01J00000000000000000000001"),
            origin: RunOrigin::Board,
            input: UserInput::new("hello".to_owned()),
            context: Vec::new(),
        })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let emitted = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("a completed turn in time")
            .expect("the sink is open");
        if matches!(emitted.event, Event::TurnCompleted { .. }) {
            break;
        }
    }
    session.stop(StopMode::Graceful).await.unwrap();
    // The shim ends after the exit acknowledgment: no socket stays.
    let shims = rig.host.config().shims_dir();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let sockets = std::fs::read_dir(rig.tmp.path().join("station/run/shims"))
            .map(|d| d.count())
            .unwrap_or(0);
        if sockets == 0 {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "a socket stays");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The journal stays as launch evidence.
    assert_eq!(std::fs::read_dir(shims).unwrap().count(), 1);
}
