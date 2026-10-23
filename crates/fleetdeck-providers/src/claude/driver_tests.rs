//! The write acknowledgement of a host command (PLAN §6.4: `sent` only
//! after the I/O; a write that can be partial is an unknown outcome).

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio::sync::{Mutex, mpsc, watch};

use super::{ClaudeCore, Output, Shared, Write, flush, read_loop};
use crate::adapter::{EventSink, ProviderError};
use crate::claude::tests::config;
use crate::host::HostError;
use crate::stdio::test_support::FakeControl;
use crate::stdio::{MAX_QUEUED_WRITES, is_queue_full};

const LIMIT: Duration = Duration::from_secs(5);

fn is_write_error(r: &Result<(), ProviderError>) -> bool {
    matches!(r, Err(ProviderError::Host(HostError::Write(_))))
}

#[tokio::test]
async fn the_flush_returns_after_the_writer_took_every_frame_before_it() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(Write::Frame(json!({"n": 1}))).unwrap();
    let writer = tokio::spawn(async move {
        let mut frames = 0;
        while let Some(item) = rx.recv().await {
            match item {
                Write::Frame(_) => frames += 1,
                Write::Flush(ack) => {
                    ack.send(()).unwrap();
                    return frames;
                }
                Write::Close => return frames,
            }
        }
        frames
    });
    flush(&tx, LIMIT).await.unwrap();
    assert_eq!(writer.await.unwrap(), 1);
}

#[tokio::test]
async fn a_writer_that_ends_after_it_took_the_frames_is_a_partial_write() {
    // The frames were queued; then the writer ended (for example, a write
    // error after a part of a line) before the acknowledgement could be
    // queued. Some bytes may be in the pipe: the outcome is unknown, never
    // "nothing was sent".
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(Write::Frame(json!({"n": 1}))).unwrap();
    assert!(matches!(rx.recv().await, Some(Write::Frame(_))));
    drop(rx);
    let r = flush(&tx, LIMIT).await;
    assert!(is_write_error(&r), "{r:?}");
}

#[tokio::test]
async fn a_writer_that_drops_the_acknowledgement_is_a_partial_write() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send(Write::Frame(json!({"n": 1}))).unwrap();
    let writer = tokio::spawn(async move {
        // Takes the frame, fails on it, and ends: the pending
        // acknowledgement is dropped with the queue.
        let _ = rx.recv().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(rx);
    });
    let r = flush(&tx, LIMIT).await;
    assert!(is_write_error(&r), "{r:?}");
    writer.await.unwrap();
}

#[tokio::test]
async fn a_process_that_does_not_read_stdin_makes_the_flush_time_out() {
    let (tx, _rx) = mpsc::unbounded_channel::<Write>();
    tx.send(Write::Frame(json!({"n": 1}))).unwrap();
    let r = flush(&tx, Duration::from_millis(50)).await;
    assert!(is_write_error(&r), "{r:?}");
}

fn shared(queued: usize) -> (Arc<Shared>, mpsc::Receiver<crate::adapter::Emitted>) {
    let (sink, events) = EventSink::channel(64);
    let shared = Arc::new(Shared {
        core: Mutex::new(ClaudeCore::new(config(false))),
        candidates: mpsc::channel(8).0,
        sink,
        raw: std::sync::Mutex::new(None),
        queued: AtomicUsize::new(queued),
    });
    (shared, events)
}

/// Runs the read loop on `stdout` with the line limit `max_line`. Returns the
/// control and the number of lines that the loop counted.
async fn run_read_loop(
    shared: &Arc<Shared>,
    stdout: Vec<u8>,
    max_line: usize,
) -> (Arc<FakeControl>, u64) {
    let control = Arc::new(FakeControl::default());
    let (writes, _rx) = mpsc::unbounded_channel::<Write>();
    let (frames_tx, frames_rx) = watch::channel(0);
    tokio::time::timeout(
        Duration::from_secs(5),
        read_loop(
            Arc::clone(shared),
            control.clone(),
            Box::new(Cursor::new(stdout)),
            writes,
            frames_tx,
            max_line,
        ),
    )
    .await
    .unwrap();
    let seen = *frames_rx.borrow();
    (control, seen)
}

#[tokio::test]
async fn a_stdout_line_over_the_limit_stops_the_session() {
    // NV-20: the line is over the limit; the next line is never read.
    let (shared, _events) = shared(0);
    let mut stdout = vec![b'x'; 40];
    stdout.extend_from_slice(b"\n{\"type\":\"unknown\"}\n");
    let (control, seen) = run_read_loop(&shared, stdout, 16).await;
    assert!(control.was_killed());
    assert_eq!(seen, 0);
    assert!(shared.core.lock().await.has_ended());
}

#[tokio::test]
async fn a_stdout_line_at_the_limit_is_fed_to_the_core() {
    let (shared, _events) = shared(0);
    let line = "{\"type\":\"unknown\"}";
    let stdout = format!("{line}\n{line}\n").into_bytes();
    let (control, seen) = run_read_loop(&shared, stdout, line.len()).await;
    assert!(!control.was_killed());
    assert_eq!(seen, 2);
}

#[tokio::test]
async fn a_dispatch_with_a_full_stdin_queue_writes_nothing() {
    // NV-19.
    let (shared, _events) = shared(MAX_QUEUED_WRITES);
    let (tx, mut rx) = mpsc::unbounded_channel::<Write>();
    let r = shared
        .dispatch(vec![Output::Write(json!({"n": 1}))], None, &tx)
        .await;
    assert!(is_write_error(&r), "{r:?}");
    assert!(is_queue_full(&r.unwrap_err()));
    assert!(rx.try_recv().is_err());
    assert_eq!(shared.queued.load(Ordering::Acquire), MAX_QUEUED_WRITES);
}

#[tokio::test]
async fn a_dispatch_counts_the_queued_frames_and_the_writer_frees_them() {
    let (shared, _events) = shared(0);
    let (tx, mut rx) = mpsc::unbounded_channel::<Write>();
    shared
        .dispatch(
            vec![
                Output::Write(json!({"n": 1})),
                Output::Write(json!({"n": 2})),
            ],
            None,
            &tx,
        )
        .await
        .unwrap();
    assert_eq!(shared.queued.load(Ordering::Acquire), 2);
    drop(rx.try_recv().unwrap());
    // A closed writer: the frame is not counted.
    drop(rx);
    let r = shared
        .dispatch(vec![Output::Write(json!({"n": 3}))], None, &tx)
        .await;
    assert!(matches!(r, Err(ProviderError::SessionEnded)), "{r:?}");
    assert_eq!(shared.queued.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn a_process_that_does_not_read_stdin_is_killed_when_the_queue_is_full() {
    // NV-19: a refusal reply has no room in the queue.
    let (shared, _events) = shared(MAX_QUEUED_WRITES);
    let frame = json!({"type":"control_request","request_id":"r1",
                       "request":{"subtype":"hook_callback","callback_id":"c","input":{}}});
    let stdout = format!("{frame}\n{frame}\n").into_bytes();
    let (control, seen) = run_read_loop(&shared, stdout, 1 << 20).await;
    assert!(control.was_killed());
    assert_eq!(seen, 1, "the loop stops after the first failed line");
}

// MCP P3 §4.2, test 14: the plugin dir and the fleet skill rule.

mod skill {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use fleetdeck_core::{ChildEnv, Secret};
    use fleetdeck_proto::command::SessionSpec;
    use fleetdeck_proto::event::ProviderKind;

    use crate::adapter::EventSink;
    use crate::claude::driver::{ClaudeAdapter, ClaudeAdapterConfig, Origin};
    use crate::coordinator::{CoordinatorLaunch, bridge_args, mcp_args};
    use crate::host::{HostError, HostKind, HostedProcess, ProcessSpec, SessionHost};

    const URL: &str = "http://127.0.0.1:41234/mcp";
    const DIR: &str = "/station/share/fleet-skill/abc123";
    const ALLOWED: &str = "mcp__fleetdeck Skill(fleetdeck:fleet)";

    fn launch(args: Vec<String>, skill_dir: Option<&str>) -> CoordinatorLaunch {
        CoordinatorLaunch {
            args,
            token: Secret::new("t".to_owned()),
            skill_dir: skill_dir.map(PathBuf::from),
        }
    }

    fn value_of<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
        args.windows(2)
            .filter(|w| w[0] == flag)
            .map(|w| w[1].as_str())
            .collect()
    }

    #[test]
    fn claude_starts_with_the_plugin_dir_and_only_the_fleet_skill_allowed() {
        let direct = mcp_args(ProviderKind::Claude, URL).unwrap();
        let bridge = bridge_args(
            ProviderKind::Claude,
            std::path::Path::new("/bin/fleetdeck"),
            std::path::Path::new("/run/s.sock"),
        )
        .unwrap();
        for args in [direct, bridge] {
            // No skill tree: the arguments are the handoff arguments.
            let without = launch(args.clone(), None).claude_args();
            assert_eq!(without, args);
            assert_eq!(value_of(&without, "--allowedTools"), ["mcp__fleetdeck"]);
            assert!(value_of(&without, "--plugin-dir").is_empty());
            // A skill tree: the plugin dir and the one extra rule.
            let with = launch(args.clone(), Some(DIR)).claude_args();
            assert_eq!(value_of(&with, "--plugin-dir"), [DIR]);
            assert_eq!(value_of(&with, "--allowedTools"), [ALLOWED]);
            // Only the allowed-tools value changes; the plugin dir follows.
            assert_eq!(with[..args.len() - 1], args[..args.len() - 1]);
            assert_eq!(with.len(), args.len() + 2);
            for arg in &with {
                for bad in ["ListMcpResourcesTool", "ReadMcpResourceTool"] {
                    assert!(!arg.contains(bad), "{arg}");
                }
            }
            let allowed = value_of(&with, "--allowedTools")[0];
            let rules: Vec<&str> = allowed.split_whitespace().collect();
            assert_eq!(rules, ["mcp__fleetdeck", "Skill(fleetdeck:fleet)"]);
            assert!(!rules.contains(&"Skill"));
            assert!(!with.iter().any(|a| a.contains("fdc1_")));
        }
    }

    #[test]
    fn a_skill_tree_with_no_allowed_tools_argument_adds_the_skill_rule_alone() {
        let with = launch(Vec::new(), Some(DIR)).claude_args();
        assert_eq!(
            value_of(&with, "--allowedTools"),
            ["Skill(fleetdeck:fleet)"]
        );
        assert_eq!(value_of(&with, "--plugin-dir"), [DIR]);
    }

    #[test]
    fn a_skill_tree_path_that_is_not_utf8_gives_no_injection() {
        use std::os::unix::ffi::OsStrExt as _;
        let args = mcp_args(ProviderKind::Claude, URL).unwrap();
        let mut l = launch(args.clone(), None);
        l.skill_dir = Some(PathBuf::from(std::ffi::OsStr::from_bytes(b"/odd\xffdir")));
        assert_eq!(l.claude_args(), args);
    }

    /// A host that records the spawn spec and refuses to start.
    #[derive(Debug)]
    struct Recorder(Arc<Mutex<Vec<ProcessSpec>>>);

    #[async_trait]
    impl SessionHost for Recorder {
        fn kind(&self) -> HostKind {
            HostKind::DirectChild
        }

        fn survives_station_restart(&self) -> bool {
            false
        }

        async fn spawn(&self, spec: ProcessSpec) -> Result<HostedProcess, HostError> {
            self.0.lock().unwrap().push(spec);
            Err(HostError::NoPipe)
        }
    }

    async fn spawned_args(skill_dir: Option<&str>, origin: Origin) -> Vec<String> {
        let spawned = Arc::new(Mutex::new(Vec::new()));
        let config =
            ClaudeAdapterConfig::new("claude".into(), "claude".into(), ChildEnv::from_process());
        let adapter = ClaudeAdapter::new(config, Arc::new(Recorder(Arc::clone(&spawned))));
        let dir = tempfile::tempdir().unwrap();
        let spec = SessionSpec {
            provider_instance: "claude".into(),
            cwd: dir.path().to_string_lossy().into_owned(),
            mode: None,
            model: None,
            title: None,
            coordinator: None,
            launch: None,
            extra_env: None,
        };
        let (sink, _events) = EventSink::channel(8);
        let l = launch(mcp_args(ProviderKind::Claude, URL).unwrap(), skill_dir);
        let started = adapter
            .start_session_with(spec, sink, origin, Some(&l))
            .await;
        assert!(started.is_err());
        let spec = spawned.lock().unwrap().remove(0);
        spec.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[tokio::test]
    async fn the_driver_passes_the_skill_arguments_to_a_start_and_a_resume() {
        use crate::adapter::NativeSessionId;
        let id = NativeSessionId("s-1".to_owned());
        for origin in [Origin::New, Origin::Resume(id.clone()), Origin::Fork(id)] {
            let args = spawned_args(Some(DIR), origin.clone()).await;
            assert_eq!(value_of(&args, "--plugin-dir"), [DIR]);
            assert_eq!(value_of(&args, "--allowedTools"), [ALLOWED]);
            let args = spawned_args(None, origin).await;
            assert!(value_of(&args, "--plugin-dir").is_empty());
            assert_eq!(value_of(&args, "--allowedTools"), ["mcp__fleetdeck"]);
        }
    }
}
