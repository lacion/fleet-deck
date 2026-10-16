//! Replays every recorded Codex fixture through the production adapter
//! (M2 P5-2): `CodexAdapter` starts `fake-codex app-server` through
//! `DirectChildHost`, and the fake plays the recorded process
//! (`fixture:<path>#<n>`).
//!
//! For each process of each fixture:
//! - the fake checks the spawn arguments and every frame that the driver
//!   writes against the recording, with the recorder's `<workdir>` replaced
//!   by the session workspace (else it exits 3 or 4, and the test fails);
//! - the adapter does the recorded `initialize` and `open_thread` itself;
//!   the test applies the other host commands through the session after the
//!   driver read the frames recorded before them (the recorded background
//!   polls go through `poll_background`; the automatic poll is off);
//! - the canonical events must equal the events of the sans-IO core for the
//!   same records;
//! - the raw-frame log holds every written frame and every read frame except
//!   token deltas, and every `raw_ref` points at its frame.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test harness: a panic is the failure report"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{ApprovalDecision, SessionSpec, StopMode, UserInput};
use fleetdeck_proto::event::{ResolvedBy, RunOrigin};
use fleetdeck_proto::{Event, InputId, RequestId};
use fleetdeck_providers::claude::RawLogSettings;
use fleetdeck_providers::codex::rpc::parse_value;
use fleetdeck_providers::codex::{
    ClientInfo, CodexAdapter, CodexAdapterConfig, CodexCore, CodexSession, CoreConfig, HostCmd,
    Origin, Output, capabilities, is_token_delta,
};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_providers::rawlog::RawLogLimits;
use fleetdeck_providers::{
    Answer, Emitted, EventSink, HostInput, LiveSession, ModelId, NativeSessionId, PermissionMode,
};
use fleetdeck_testkit::FakeAgent;
use fleetdeck_testkit::fake::SCENARIO_ENV;
use fleetdeck_testkit::script::{process_records, with_workdir};
use serde_json::Value;

const WAIT: Duration = Duration::from_secs(20);

fn dir_of(rec: &Value) -> &str {
    rec.get("dir").and_then(Value::as_str).unwrap_or("")
}

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/s2-codex/fixtures");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let n = p.to_string_lossy();
            n.ends_with(".jsonl") && !n.ends_with(".failed.jsonl")
        })
        .collect();
    files.sort();
    files
}

fn processes(path: &Path) -> Vec<Vec<Value>> {
    (0..).map_while(|i| process_records(path, i).ok()).collect()
}

fn recorder_client() -> ClientInfo {
    ClientInfo {
        name: "fleet-deck-spike".to_owned(),
        version: "0.0.0".to_owned(),
    }
}

/// The recorded `open_thread` of a process, with the real workspace.
fn open_thread(records: &[Value], cwd: &Path) -> HostCmd {
    let rec = records
        .iter()
        .find(|r| dir_of(r) == "host" && r["cmd"]["op"] == "open_thread")
        .expect("every process opens a thread");
    serde_json::from_value(with_workdir(&rec["cmd"], cwd)).unwrap()
}

fn origin_of(open: &HostCmd) -> Origin {
    let HostCmd::OpenThread { resume, fork, .. } = open else {
        unreachable!()
    };
    match (resume, fork) {
        (None, _) => Origin::New,
        (Some(id), false) => Origin::Resume(NativeSessionId(id.clone())),
        (Some(id), true) => Origin::Fork(NativeSessionId(id.clone())),
    }
}

/// The events of the sans-IO core for one process, with the same workspace
/// and the same session configuration as the adapter.
fn core_events(records: &[Value], cwd: &Path) -> Vec<Event> {
    let open = open_thread(records, cwd);
    let mut core = CodexCore::new(CoreConfig {
        provider_instance: "codex".into(),
        cwd: cwd.to_string_lossy().into_owned(),
        capabilities: capabilities(false),
        resumed: matches!(origin_of(&open), Origin::Resume(_)).then_some(true),
        client: recorder_client(),
        request_models: false,
    });
    let mut events = Vec::new();
    let mut push = |out: Vec<Output>| {
        events.extend(out.into_iter().filter_map(|o| match o {
            Output::Emit(e) => Some(e),
            Output::Write(_) | Output::NativeIdentity { .. } => None,
        }));
    };
    for rec in records {
        match dir_of(rec) {
            "host" => {
                let cmd: HostCmd = serde_json::from_value(with_workdir(&rec["cmd"], cwd)).unwrap();
                push(core.apply(&cmd).unwrap());
            }
            "out" => push(core.on_frame(parse_value(rec["frame"].clone()))),
            "exit" => {
                let code = rec
                    .get("code")
                    .and_then(Value::as_i64)
                    .and_then(|c| i32::try_from(c).ok());
                push(core.on_exit(code));
            }
            _ => {}
        }
    }
    events
}

fn board_input(text: String, input_id: &str) -> HostInput {
    HostInput {
        id: InputId::new(input_id),
        origin: RunOrigin::Board,
        input: UserInput::new(text),
        context: Vec::new(),
    }
}

async fn apply(session: &mut CodexSession, cmd: HostCmd) {
    let result = match cmd {
        // The adapter does the handshake and opens the thread at start.
        HostCmd::Initialize | HostCmd::OpenThread { .. } => return,
        HostCmd::Prompt { text, input_id } => session.prompt(board_input(text, &input_id)).await,
        HostCmd::Steer { text, input_id } => session.steer(board_input(text, &input_id)).await,
        HostCmd::Answer {
            request_id,
            option_id,
            updated_input,
            message,
        } => {
            session
                .answer(Answer {
                    request: RequestId::new(request_id.as_str()),
                    decision: ApprovalDecision {
                        answered_by: None,
                        option_id,
                        updated_input,
                        text: message,
                    },
                    by: ResolvedBy::Board,
                })
                .await
        }
        HostCmd::Interrupt => session.interrupt(None).await,
        HostCmd::SetMode { mode } => session.set_mode(PermissionMode(mode)).await,
        HostCmd::SetModel { model } => session.set_model(ModelId(model)).await,
        HostCmd::SetCollaboration { mode } => session.set_collaboration(&mode).await,
        HostCmd::ListBackground => session.poll_background().await,
    };
    result.expect("a recorded host command");
}

async fn wait_frames(session: &CodexSession, at_least: u64, at: &str) {
    let mut frames = session.frames_seen();
    tokio::time::timeout(WAIT, frames.wait_for(|n| *n >= at_least))
        .await
        .unwrap_or_else(|_| panic!("{at}: the driver did not read {at_least} frames"))
        .unwrap();
}

async fn stop(session: &mut CodexSession, how: StopMode, at: &str) {
    tokio::time::timeout(WAIT, session.stop(how))
        .await
        .unwrap_or_else(|_| panic!("{at}: stop did not return"))
        .unwrap();
}

struct Played {
    events: Vec<Emitted>,
    raw_dir: PathBuf,
}

/// Plays one process of a fixture through the adapter.
async fn play(path: &Path, index: usize, records: &[Value], root: &Path, cwd: &Path) -> Played {
    let at = format!("{}#{index}", path.file_name().unwrap().to_string_lossy());
    let open = open_thread(records, cwd);
    let HostCmd::OpenThread {
        model,
        effort,
        mode,
        ..
    } = open.clone()
    else {
        unreachable!()
    };
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, format!("fixture:{}#{index}", path.display()))
        .unwrap();
    let mut config =
        CodexAdapterConfig::new("codex".into(), FakeAgent::Codex.locate().unwrap(), env);
    config.client = recorder_client();
    config.request_models = false;
    config.effort = effort;
    config.background_poll = None;
    config.stop = StopPolicy {
        grace: Duration::from_secs(10),
        term_grace: Duration::from_secs(2),
    };
    config.raw_log = Some(RawLogSettings {
        root: root.join("raw"),
        limits: RawLogLimits::default(),
    });
    let adapter = CodexAdapter::new(config, Arc::new(DirectChildHost::new()));
    let (sink, mut rx) = EventSink::channel(64);
    let collector = tokio::spawn(async move {
        let mut all = Vec::new();
        while let Some(e) = rx.recv().await {
            all.push(e);
        }
        all
    });
    let spec = SessionSpec {
        provider_instance: "codex".into(),
        cwd: cwd.to_string_lossy().into_owned(),
        mode,
        model,
        title: None,
        coordinator: None,
        launch: None,
    };
    let mut session =
        tokio::time::timeout(WAIT, adapter.start_session(spec, sink, origin_of(&open)))
            .await
            .unwrap_or_else(|_| panic!("{at}: start did not return"))
            .unwrap_or_else(|e| panic!("{at}: start failed: {e}"));
    let raw_dir = session.raw_log_dir().unwrap().to_path_buf();
    let mut outs = 0u64;
    let mut stopped = false;
    for (n, rec) in records.iter().enumerate() {
        let at = format!("{at} record {n}");
        match dir_of(rec) {
            "out" => outs += 1,
            "host" => {
                wait_frames(&session, outs, &at).await;
                let cmd: HostCmd = serde_json::from_value(rec["cmd"].clone()).unwrap();
                apply(&mut session, cmd).await;
            }
            "note" if rec["text"] == "host closed stdin" => {
                wait_frames(&session, outs, &at).await;
                stop(&mut session, StopMode::Graceful, &at).await;
                stopped = true;
            }
            "exit" if !stopped => {
                wait_frames(&session, outs, &at).await;
                let how = if rec.get("code").and_then(Value::as_i64).is_some() {
                    StopMode::Graceful
                } else {
                    StopMode::Kill
                };
                stop(&mut session, how, &at).await;
                stopped = true;
            }
            _ => {}
        }
    }
    assert!(stopped, "{at}: the process never ended");
    drop(session);
    let events = tokio::time::timeout(WAIT, collector)
        .await
        .unwrap()
        .unwrap();
    Played { events, raw_dir }
}

/// The raw-frame log lines of a session, in order.
fn raw_lines(dir: &Path) -> Vec<(String, u64, Value)> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    let mut out = Vec::new();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&file).unwrap();
        let mut offset = 0u64;
        for line in text.split_inclusive('\n') {
            out.push((name.clone(), offset, serde_json::from_str(line).unwrap()));
            offset += line.len() as u64;
        }
    }
    out
}

fn check_raw_log(at: &str, records: &[Value], played: &Played, cwd: &Path) {
    let lines = raw_lines(&played.raw_dir);
    let logged = |dir: &str| -> Vec<Value> {
        lines
            .iter()
            .filter(|(_, _, l)| l["dir"] == dir)
            .map(|(_, _, l)| l["frame"].clone())
            .collect()
    };
    let recorded = |dir: &str| -> Vec<Value> {
        records
            .iter()
            .filter(|r| dir_of(r) == dir)
            .map(|r| r["frame"].clone())
            .filter(|f| !is_token_delta(f))
            .collect()
    };
    let recorded_in: Vec<Value> = recorded("in")
        .iter()
        .map(|f| with_workdir(f, cwd))
        .collect();
    assert_eq!(logged("in"), recorded_in, "{at}: raw log of written frames");
    assert_eq!(
        logged("out"),
        recorded("out"),
        "{at}: raw log of read frames"
    );
    for e in &played.events {
        if let Some(r) = &e.raw_ref {
            let line = lines
                .iter()
                .find(|(f, o, _)| *f == r.file && *o == r.offset)
                .unwrap_or_else(|| panic!("{at}: dangling raw_ref {r:?}"));
            assert_eq!(line.2["dir"], "out", "{at}: raw_ref {r:?}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_recorded_codex_fixture_replays_through_the_production_adapter() {
    let files = fixtures();
    assert_eq!(files.len(), 18, "the recorded Codex fixtures");
    let mut played_processes = 0;
    let mut with_refs = 0;
    for path in &files {
        for (index, records) in processes(path).iter().enumerate() {
            let at = format!("{}#{index}", path.file_name().unwrap().to_string_lossy());
            let tmp = tempfile::tempdir().unwrap();
            let ws = tmp.path().join("ws");
            std::fs::create_dir_all(&ws).unwrap();
            let cwd = std::fs::canonicalize(&ws).unwrap();
            let played = play(path, index, records, tmp.path(), &cwd).await;
            let expected = core_events(records, &cwd);
            let actual: Vec<Event> = played.events.iter().map(|e| e.event.clone()).collect();
            assert_eq!(actual, expected, "{at}: canonical events");
            check_raw_log(&at, records, &played, &cwd);
            with_refs += played.events.iter().filter(|e| e.raw_ref.is_some()).count();
            played_processes += 1;
        }
    }
    assert_eq!(played_processes, 21, "every recorded process");
    assert!(with_refs > 0, "events carry raw_refs");
}
