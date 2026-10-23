//! The tests of the read loop and the dispatch of the Codex driver.

use std::io::Cursor;
use std::sync::atomic::Ordering;

use serde_json::json;

use super::*;
use crate::codex::tests::config;
use crate::stdio::test_support::FakeControl;
use crate::stdio::{MAX_QUEUED_WRITES, is_queue_full};

fn shared(queued: usize) -> Arc<Shared> {
    let (sink, _events) = EventSink::channel(64);
    Arc::new(Shared {
        core: Mutex::new(CodexCore::new(config("/w"))),
        candidates: mpsc::channel(1).0,
        sink,
        raw: std::sync::Mutex::new(None),
        poll: Notify::new(),
        queued: AtomicUsize::new(queued),
    })
}

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
    let shared = shared(0);
    let mut stdout = vec![b'x'; 40];
    stdout.extend_from_slice(b"\n{\"method\":\"brand/new\"}\n");
    let (control, seen) = run_read_loop(&shared, stdout, 16).await;
    assert!(control.was_killed());
    assert_eq!(seen, 0);
    assert!(shared.core.lock().await.has_ended());
}

#[tokio::test]
async fn a_stdout_line_at_the_limit_is_fed_to_the_core() {
    let shared = shared(0);
    let line = "{\"method\":\"brand/new\"}";
    let stdout = format!("{line}\n{line}\n").into_bytes();
    let (control, seen) = run_read_loop(&shared, stdout, line.len()).await;
    assert!(!control.was_killed());
    assert_eq!(seen, 2);
}

#[tokio::test]
async fn a_dispatch_with_a_full_stdin_queue_writes_nothing() {
    // NV-19.
    let shared = shared(MAX_QUEUED_WRITES);
    let (tx, mut rx) = mpsc::unbounded_channel::<Write>();
    let r = shared
        .dispatch(vec![Output::Write(json!({"n": 1}))], None, &tx)
        .await;
    assert!(is_queue_full(&r.unwrap_err()));
    assert!(rx.try_recv().is_err());
    assert_eq!(shared.queued.load(Ordering::Acquire), MAX_QUEUED_WRITES);
}

#[tokio::test]
async fn a_dispatch_counts_the_queued_frames_and_a_closed_writer_leaves_the_count() {
    let shared = shared(0);
    let (tx, rx) = mpsc::unbounded_channel::<Write>();
    shared
        .dispatch(vec![Output::Write(json!({"n": 1}))], None, &tx)
        .await
        .unwrap();
    assert_eq!(shared.queued.load(Ordering::Acquire), 1);
    drop(rx);
    let r = shared
        .dispatch(vec![Output::Write(json!({"n": 2}))], None, &tx)
        .await;
    assert!(matches!(r, Err(ProviderError::SessionEnded)), "{r:?}");
    assert_eq!(shared.queued.load(Ordering::Acquire), 1);
}

// MCP P3 §4.2, test 15: the fleet skill requests before every thread open.

use std::sync::Mutex as StdMutex;

use fleetdeck_core::Secret;
use tokio::io::{AsyncWriteExt as _, DuplexStream};

use crate::adapter::Emitted;
use crate::host::{
    EndCause, ExitReport, HostError, HostKind, HostedProcess, StopPolicy as HostStopPolicy,
    Termination,
};

/// How the scripted app-server answers a skill request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answered {
    Ok,
    Error,
    Silence,
}

/// The script of the in-memory app-server: how it answers the two skill
/// requests, and the frames that it read.
#[derive(Debug, Clone)]
struct Script {
    set: Answered,
    list: Answered,
    seen: Arc<StdMutex<Vec<Value>>>,
}

impl Script {
    fn answer(&self, frame: &Value) -> Option<Value> {
        let id = frame.get("id")?.clone();
        let method = frame.get("method")?.as_str()?;
        let skill = match method {
            "skills/extraRoots/set" => Some(self.set),
            "skills/list" => Some(self.list),
            _ => None,
        };
        match (method, skill) {
            (_, Some(Answered::Silence)) => None,
            (_, Some(Answered::Error)) => Some(
                json!({"id": id, "error": {"code": -32601, "message": "method not supported"}}),
            ),
            (_, Some(Answered::Ok)) => Some(json!({"id": id, "result": {}})),
            ("initialize", None) => {
                Some(json!({"id": id, "result": {"userAgent": "fleetdeck/0.163.0 (test) x"}}))
            }
            (_, None) => {
                Some(json!({"id": id, "result": {"thread": {"id": "thr-1"}, "model": "m"}}))
            }
        }
    }
}

/// A host whose process is the scripted app-server.
#[derive(Debug)]
struct AppHost(Script);

#[derive(Debug)]
struct AppControl {
    script: Script,
    out: Mutex<DuplexStream>,
}

fn killed() -> ExitReport {
    ExitReport {
        termination: Termination::Signal(9),
        cause: EndCause::Killed,
        stderr_tail: String::new(),
    }
}

#[async_trait]
impl ProcessControl for AppControl {
    fn pid(&self) -> u32 {
        0
    }

    async fn write(&self, bytes: &[u8]) -> std::result::Result<(), HostError> {
        for line in String::from_utf8_lossy(bytes).lines() {
            let frame: Value = serde_json::from_str(line).unwrap();
            self.script.seen.lock().unwrap().push(frame.clone());
            if let Some(reply) = self.script.answer(&frame) {
                let mut out = self.out.lock().await;
                out.write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
            }
        }
        Ok(())
    }

    async fn close_stdin(&self) {}

    async fn stop(&self, _policy: HostStopPolicy) -> ExitReport {
        killed()
    }

    async fn kill(&self) -> ExitReport {
        killed()
    }

    async fn wait(&self) -> ExitReport {
        killed()
    }

    fn exit(&self) -> Option<ExitReport> {
        None
    }
}

#[async_trait]
impl SessionHost for AppHost {
    fn kind(&self) -> HostKind {
        HostKind::DirectChild
    }

    fn survives_station_restart(&self) -> bool {
        false
    }

    async fn spawn(&self, _spec: ProcessSpec) -> std::result::Result<HostedProcess, HostError> {
        let (stdout, out) = tokio::io::duplex(64 * 1024);
        Ok(HostedProcess {
            stdout: Box::new(stdout),
            control: Arc::new(AppControl {
                script: self.0.clone(),
                out: Mutex::new(out),
            }),
        })
    }
}

struct Opened {
    result: Result<CodexSession>,
    /// The frames that the app-server read, in order.
    frames: Vec<Value>,
    elapsed: Duration,
    _events: mpsc::Receiver<Emitted>,
}

impl Opened {
    fn methods(&self) -> Vec<&str> {
        self.frames
            .iter()
            .filter_map(|f| f["method"].as_str())
            .collect()
    }
}

async fn open_with(
    origin: Origin,
    skill_dir: Option<&str>,
    set: Answered,
    list: Answered,
    open_timeout: Duration,
) -> Opened {
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let host = Arc::new(AppHost(Script {
        set,
        list,
        seen: Arc::clone(&seen),
    }));
    let mut config =
        CodexAdapterConfig::new("codex".into(), "codex".into(), ChildEnv::from_process());
    config.request_models = false;
    config.background_poll = None;
    config.open_timeout = open_timeout;
    let adapter = CodexAdapter::new(config, host);
    let dir = tempfile::tempdir().unwrap();
    let spec = SessionSpec {
        provider_instance: "codex".into(),
        cwd: dir.path().to_string_lossy().into_owned(),
        mode: None,
        model: None,
        title: None,
        coordinator: None,
        launch: None,
        extra_env: None,
    };
    let launch = CoordinatorLaunch {
        args: Vec::new(),
        token: Secret::new("t".to_owned()),
        skill_dir: skill_dir.map(PathBuf::from),
    };
    let (sink, events) = EventSink::channel(64);
    let started = tokio::time::Instant::now();
    let result = adapter
        .start_session_with(spec, sink, origin, Some(&launch))
        .await;
    let elapsed = started.elapsed();
    let frames = seen.lock().unwrap().clone();
    Opened {
        result,
        frames,
        elapsed,
        _events: events,
    }
}

const LONG: Duration = Duration::from_secs(30);

fn origins() -> [(Origin, &'static str); 3] {
    let thread = NativeSessionId("thr-0".to_owned());
    [
        (Origin::New, "thread/start"),
        (Origin::Resume(thread.clone()), "thread/resume"),
        (Origin::Fork(thread), "thread/fork"),
    ]
}

#[tokio::test]
async fn codex_sets_the_skill_root_and_reloads_before_every_thread_open() {
    for (origin, open) in origins() {
        let o = open_with(
            origin,
            Some("/s/fleet-skill/abc"),
            Answered::Ok,
            Answered::Ok,
            LONG,
        )
        .await;
        assert!(o.result.is_ok(), "{open}");
        assert_eq!(
            o.methods(),
            [
                "initialize",
                "initialized",
                "skills/extraRoots/set",
                "skills/list",
                open
            ]
        );
        assert_eq!(
            o.frames[2]["params"],
            json!({"extraRoots": ["/s/fleet-skill/abc/skills"]})
        );
        assert_eq!(o.frames[3]["params"], json!({"forceReload": true}));
    }
}

#[tokio::test]
async fn codex_sends_no_skill_request_without_a_skill_dir() {
    for (origin, open) in origins() {
        let o = open_with(origin, None, Answered::Ok, Answered::Ok, LONG).await;
        assert!(o.result.is_ok(), "{open}");
        assert_eq!(o.methods(), ["initialize", "initialized", open]);
    }
}

#[tokio::test]
async fn an_error_answer_of_a_skill_request_does_not_stop_the_open() {
    for (set, list) in [
        (Answered::Error, Answered::Ok),
        (Answered::Ok, Answered::Error),
        (Answered::Error, Answered::Error),
    ] {
        for (origin, open) in origins() {
            let o = open_with(origin, Some("/s/t"), set, list, LONG).await;
            assert!(o.result.is_ok(), "{open}");
            assert_eq!(
                o.methods(),
                [
                    "initialize",
                    "initialized",
                    "skills/extraRoots/set",
                    "skills/list",
                    open
                ]
            );
        }
    }
}

#[tokio::test]
async fn a_skill_request_without_an_answer_is_cut_at_its_bound_and_the_open_goes_on() {
    let bound = CodexCore::SKILL_REQUEST_TIMEOUT;
    for (set, list) in [
        (Answered::Silence, Answered::Ok),
        (Answered::Ok, Answered::Silence),
    ] {
        let o = open_with(Origin::New, Some("/s/t"), set, list, LONG).await;
        assert!(o.result.is_ok());
        assert_eq!(
            o.methods(),
            [
                "initialize",
                "initialized",
                "skills/extraRoots/set",
                "skills/list",
                "thread/start"
            ]
        );
        assert!(o.elapsed >= bound, "{:?}", o.elapsed);
        assert!(o.elapsed < bound * 2, "{:?}", o.elapsed);
    }
    // The resume and the fork open behave the same.
    for (origin, open) in origins().into_iter().skip(1) {
        let o = open_with(origin, Some("/s/t"), Answered::Silence, Answered::Ok, LONG).await;
        assert!(o.result.is_ok(), "{open}");
        assert_eq!(o.methods().last().copied(), Some(open));
    }
}

#[tokio::test]
async fn less_than_two_bounds_of_the_handshake_deadline_skips_both_skill_requests() {
    let limit = CodexCore::SKILL_REQUEST_TIMEOUT * 2 - Duration::from_millis(1);
    let o = open_with(Origin::New, Some("/s/t"), Answered::Ok, Answered::Ok, limit).await;
    assert!(o.result.is_ok());
    assert_eq!(o.methods(), ["initialize", "initialized", "thread/start"]);
}
