//! Replays every recorded Claude fixture through the production adapter
//! (M2 P2-3): `ClaudeAdapter` starts `fake-claude` through `DirectChildHost`,
//! and the fake plays the recorded process (`fixture:<path>#<n>`).
//!
//! For each process of each fixture:
//! - the fake checks the spawn arguments and every frame that the driver
//!   writes against the recording (else it exits 3 or 4, and the test fails);
//! - the test applies each recorded host command through `LiveSession` after
//!   the driver read the frames recorded before it;
//! - the canonical events must equal the events of the sans-IO core for the
//!   same records (so the driver adds, drops and reorders nothing);
//! - the raw-frame log holds every written frame and every read frame except
//!   token deltas, and every `raw_ref` points at its frame.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test harness: a panic is the failure report"
)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{fixtures, launch_of, processes};
use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{ApprovalDecision, SessionSpec, StopMode, UserInput};
use fleetdeck_proto::event::{ResolvedBy, RunOrigin};
use fleetdeck_proto::{Event, InputId, RequestId};
use fleetdeck_providers::PermissionMode;
use fleetdeck_providers::claude::frames::parse_value;
use fleetdeck_providers::claude::{
    BYPASS_MODE, ClaudeAdapter, ClaudeAdapterConfig, ClaudeCore, ClaudeSession, CoreConfig,
    HostCmd, Origin, Output, RawLogSettings, capabilities,
};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_providers::rawlog::RawLogLimits;
use fleetdeck_providers::{Answer, Emitted, EventSink, HostInput, LiveSession, ModelId};
use fleetdeck_testkit::FakeAgent;
use fleetdeck_testkit::fake::SCENARIO_ENV;
use serde_json::Value;

const WAIT: Duration = Duration::from_secs(20);

fn dir_of(rec: &Value) -> &str {
    rec.get("dir").and_then(Value::as_str).unwrap_or("")
}

fn is_token_delta(frame: &Value) -> bool {
    frame["type"] == "stream_event" && frame["event"]["type"] == "content_block_delta"
}

/// The events of the sans-IO core for one process, with the same workspace.
fn core_events(records: &[Value], cwd: &str) -> Vec<Event> {
    let args = records[0]["args"].as_array().unwrap();
    let launch = launch_of(args);
    let mut core = ClaudeCore::new(CoreConfig {
        provider_instance: "claude".into(),
        cwd: cwd.to_owned(),
        capabilities: capabilities(launch.mode.as_deref() == Some(BYPASS_MODE)),
        // A fork is a new native session with a copied history.
        resumed: (launch.resume.is_some() && !launch.fork).then_some(true),
    });
    let mut events = Vec::new();
    let mut push = |out: Vec<Output>| {
        events.extend(out.into_iter().filter_map(|o| match o {
            Output::Emit(e) => Some(e),
            Output::Write(_) | Output::NativeIdentity { .. } | Output::AppCandidate(_) => None,
        }));
    };
    for rec in records {
        match dir_of(rec) {
            "host" => {
                let cmd: HostCmd = serde_json::from_value(rec["cmd"].clone()).unwrap();
                push(core.apply(&cmd).unwrap());
            }
            "out" => push(core.on_frame(parse_value(rec["frame"].clone()).unwrap())),
            "exit" => {
                let code = rec.get("code").and_then(Value::as_i64).map(|c| c as i32);
                push(core.on_exit(code));
            }
            _ => {}
        }
    }
    events
}

async fn apply(session: &mut ClaudeSession, cmd: HostCmd) {
    let result = match cmd {
        HostCmd::Initialize => return, // the adapter sends it at start
        HostCmd::Prompt { text, input_id } => {
            session
                .prompt(HostInput {
                    id: InputId::new(input_id.as_str()),
                    origin: RunOrigin::Board,
                    input: UserInput::new(text),
                    context: Vec::new(),
                })
                .await
        }
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
        HostCmd::SetModel { model } => {
            let model = model.unwrap_or_else(|| "default".to_owned());
            session.set_model(ModelId(model)).await
        }
    };
    result.expect("a recorded host command");
}

async fn wait_frames(session: &ClaudeSession, at_least: u64, at: &str) {
    let mut frames = session.frames_seen();
    tokio::time::timeout(WAIT, frames.wait_for(|n| *n >= at_least))
        .await
        .unwrap_or_else(|_| panic!("{at}: the driver did not read {at_least} frames"))
        .unwrap();
}

struct Played {
    events: Vec<Emitted>,
    raw_dir: std::path::PathBuf,
}

/// Plays one process of a fixture through the adapter.
async fn play(path: &Path, index: usize, records: &[Value], root: &Path) -> Played {
    let at = format!("{}#{index}", path.file_name().unwrap().to_string_lossy());
    let launch = launch_of(records[0]["args"].as_array().unwrap());
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, format!("fixture:{}#{index}", path.display()))
        .unwrap();
    let mut config =
        ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
    config.allow_bypass = launch.mode.as_deref() == Some(BYPASS_MODE);
    config.extra_args = launch.extra_args.clone();
    config.stop = StopPolicy {
        grace: Duration::from_secs(10),
        term_grace: Duration::from_secs(2),
    };
    config.raw_log = Some(RawLogSettings {
        root: root.join("raw"),
        limits: RawLogLimits::default(),
    });
    let workspace = root.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let adapter = ClaudeAdapter::new(config, Arc::new(DirectChildHost::new()));
    let (sink, mut rx) = EventSink::channel(64);
    let collector = tokio::spawn(async move {
        let mut all = Vec::new();
        while let Some(e) = rx.recv().await {
            all.push(e);
        }
        all
    });
    let origin = match (&launch.resume, launch.fork) {
        (None, _) => Origin::New,
        (Some(id), false) => Origin::Resume(id.clone()),
        (Some(id), true) => Origin::Fork(id.clone()),
    };
    let spec = SessionSpec {
        provider_instance: "claude".into(),
        cwd: workspace.to_string_lossy().into_owned(),
        mode: launch.mode.clone(),
        model: launch.model.clone(),
        title: None,
        coordinator: None,
        launch: None,
        extra_env: None,
    };
    let mut session = adapter.start_session(spec, sink, origin).await.unwrap();
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

async fn stop(session: &mut ClaudeSession, how: StopMode, at: &str) {
    tokio::time::timeout(WAIT, session.stop(how))
        .await
        .unwrap_or_else(|_| panic!("{at}: stop did not return"))
        .unwrap();
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

fn check_raw_log(at: &str, records: &[Value], played: &Played) {
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
    assert_eq!(
        logged("in"),
        recorded("in"),
        "{at}: raw log of written frames"
    );
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
async fn every_recorded_fixture_replays_through_the_production_adapter() {
    let files = fixtures();
    assert_eq!(files.len(), 20, "the recorded Rust fixtures");
    let mut played_processes = 0;
    let mut with_refs = 0;
    for path in &files {
        for (index, records) in processes(path).iter().enumerate() {
            let at = format!("{}#{index}", path.file_name().unwrap().to_string_lossy());
            let tmp = tempfile::tempdir().unwrap();
            let played = play(path, index, records, tmp.path()).await;
            let cwd = std::fs::canonicalize(tmp.path().join("ws")).unwrap();
            let expected = core_events(records, &cwd.to_string_lossy());
            let actual: Vec<Event> = played.events.iter().map(|e| e.event.clone()).collect();
            assert_eq!(actual, expected, "{at}: canonical events");
            check_raw_log(&at, records, &played);
            with_refs += played.events.iter().filter(|e| e.raw_ref.is_some()).count();
            played_processes += 1;
        }
    }
    assert_eq!(played_processes, 23, "every recorded process");
    assert!(with_refs > 0, "events carry raw_refs");
}
