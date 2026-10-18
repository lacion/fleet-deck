//! The launch path of the shim credential (DL-194 T4a): a session on the
//! shim host gets its tools through the MCP bridge. Real shims, the fake
//! Claude and Codex agents, the real `fleetdeck mcp-bridge` process and a
//! station bridge socket with a stub forward.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetdeck_core::identity::Identity;
use fleetdeck_link::tls::Role;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, ControlSkill, CoordinatorSpec, GrantScope, RETRY_WINDOW_MS,
    ReceiptState, SessionCommand, SessionSpec, StopMode, UserInput,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{CommandId, Event, SessionId};
use fleetdeck_station::coordinator::socket::mcp_socket_path;
use fleetdeck_station::coordinator::{
    BindingState, Bindings, BridgeEndpoint, BridgeSocket, Caller as ToolCaller, Forward, digest,
};
use fleetdeck_station::store::Caller;
use fleetdeck_station::store::peers::PairedKey;
use fleetdeck_station::store::shim_bindings::{BindingRow, RowState};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

mod shim_resume_rig;
use shim_resume_rig::*;

/// The forward of the station socket. The tests call only the catalog.
struct NoForward;

// `Forward` is an `async_trait`; this crate has no `async-trait`, so the
// method has the desugared form.
impl Forward for NoForward {
    fn call<'life0, 'life1, 'async_trait>(
        &'life0 self,
        _caller: ToolCaller,
        _tool: &'life1 str,
        _args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async { Err("no forward in this test".to_owned()) })
    }
}

/// The bridge of one station start: the bindings name it and its socket
/// listens until this value drops.
struct Bridge {
    bindings: Bindings,
    socket: PathBuf,
    _listener: BridgeSocket,
}

/// Names the bridge of the rig's station, records the verified provider
/// versions and starts the station socket.
async fn bridge_of(rig: &Rig, st: &Station) -> Bridge {
    let bindings = st.manager.coordinator_bindings().clone();
    let socket = mcp_socket_path(
        None,
        &rig.tmp.path().join("station"),
        rig.tmp.path(),
        st.store.incarnation().as_str(),
    )
    .expect("a socket path");
    bindings.set_bridge(BridgeEndpoint {
        program: PathBuf::from(env!("CARGO_BIN_EXE_fleetdeck")),
        socket: socket.clone(),
    });
    bindings.record_version(ProviderKind::Claude, "2.1.296".to_owned());
    bindings.record_version(ProviderKind::Codex, "0.160.0".to_owned());
    let listener = BridgeSocket::start_for(&bindings, Arc::new(NoForward), None)
        .await
        .expect("the bridge socket starts");
    Bridge {
        bindings,
        socket,
        _listener: listener,
    }
}

/// Runs `fleetdeck mcp-bridge` with `token`, sends one request line and
/// returns the answer line.
async fn bridge_call(socket: &Path, token: &str, request: &Value) -> Value {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
        .args(["mcp-bridge", "--socket"])
        .arg(socket)
        .env("FLEETDECK_COORDINATOR_TOKEN", token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(WAIT, lines.next_line())
        .await
        .expect("the bridge answers")
        .unwrap()
        .expect("an answer line");
    drop(stdin);
    let _ = child.wait().await;
    serde_json::from_str(&line).unwrap()
}

fn tools_list() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
}

fn tool_names(answer: &Value) -> BTreeSet<String> {
    answer["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("no tool list: {answer}"))
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

/// Repeats the call until the station answers with a result: the hold of
/// a reattached shim answers "starting" until the replay is committed.
async fn bridge_result(socket: &Path, token: &str) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let answer = bridge_call(socket, token, &tools_list()).await;
        if answer.get("result").is_some() {
            return answer;
        }
        assert!(Instant::now() < deadline, "no result: {answer}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The launch log of the fake provider once a launch wrote it: the
/// argument lines and the token of `FLEETDECK_COORDINATOR_TOKEN`.
async fn launch_of(log: &Path, other_than: Option<&str>) -> (String, String) {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(text) = std::fs::read_to_string(log)
            && text.ends_with('\n')
            && let Some((argv, token)) = text.rsplit_once("env:")
        {
            let token = token.trim();
            if other_than != Some(token) {
                return (argv.to_owned(), token.to_owned());
            }
        }
        assert!(Instant::now() < deadline, "no launch log at {log:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn rows_of(st: &Station, session: &SessionId) -> Vec<BindingRow> {
    st.store
        .shim_bindings_load()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| &r.session_id == session)
        .collect()
}

async fn wait_row(st: &Station, session: &SessionId, digest: &str, want: RowState) {
    let deadline = Instant::now() + WAIT;
    loop {
        let rows = rows_of(st, session).await;
        if rows.iter().any(|r| r.digest == digest && r.state == want) {
            return;
        }
        assert!(Instant::now() < deadline, "no {want:?} row: {rows:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A start command without a prompt: the Claude CLI reports its identity
/// only after its first input.
fn start_command(spec: SessionSpec, prompt: Option<&str>) -> SessionCommand {
    SessionCommand::Start {
        spec,
        prompt: prompt.map(|p| UserInput::new(p.to_owned())),
        workspace: None,
        setup_cmd: None,
        batch: None,
        issue: None,
        origin_text: false,
        fleet_git: None,
    }
}

/// (a) A Claude start on the shim host: the launch has the bridge
/// arguments and the token in its environment, never in its arguments; the
/// row is `pending` until `session.started`, then `active`, and memory
/// authenticates the token.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claude_start_on_the_shim_host_gets_the_bridge_and_a_token_in_its_environment_only() {
    let mut rig = Rig::new();
    let st = rig.station().await;
    let bridge = bridge_of(&rig, &st).await;
    let spec = rig.spec("claude");
    let session = {
        let decided = rig.send(&st, start_command(spec, None)).await;
        decided.receipt.result.and_then(|r| r.session_id).unwrap()
    };
    let (argv, token) = launch_of(&rig.launch_log(), None).await;
    assert!(token.starts_with("fdc1_"), "the environment has the token");
    assert!(!argv.contains(&token), "argv never holds the token");
    assert!(argv.lines().any(|l| l == "--mcp-config"), "{argv}");
    let config = argv
        .lines()
        .find(|l| l.contains("mcpServers"))
        .expect("the MCP config");
    assert!(config.contains("\"type\":\"stdio\""), "{config}");
    assert!(config.contains("mcp-bridge"), "{config}");
    assert!(config.contains(bridge.socket.to_str().unwrap()), "{config}");

    // Before `session.started`: the row and the memory entry are pending.
    let key = digest(&token);
    let rows = rows_of(&st, &session).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].digest, key);
    assert_eq!(rows[0].state, RowState::Pending);
    assert_eq!(rows[0].binding_id, format!("{session}:fleet"));
    assert_eq!(rows[0].expires_at, None);
    assert_eq!(
        bridge.bindings.get_digest(&key).unwrap().state,
        BindingState::Pending
    );
    // The token is in no database column.
    assert!(!format!("{rows:?}").contains(&token));

    prompt_when_live(&mut rig, &st, &session, "hello").await;
    wait_turns(&st.store, &session, 1).await;
    wait_row(&st, &session, &key, RowState::Active).await;
    assert_eq!(
        bridge.bindings.get_digest(&key).unwrap().state,
        BindingState::Active
    );
    st.manager.shutdown(StopMode::Graceful).await;
}

/// (b) The real bridge reaches the station socket; after a station restart
/// (the shim and the provider keep running) and the end of the hold, a new
/// bridge process with the same token gets its answer again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_bridge_reaches_the_station_and_survives_a_station_restart() {
    let mut rig = Rig::new();
    let st = rig.station().await;
    let bridge = bridge_of(&rig, &st).await;
    let session = rig.start(&st, "claude", "hello").await;
    wait_turns(&st.store, &session, 1).await;
    let (_, token) = launch_of(&rig.launch_log(), None).await;
    let key = digest(&token);
    wait_row(&st, &session, &key, RowState::Active).await;

    let answer = bridge_call(&bridge.socket, &token, &tools_list()).await;
    assert_eq!(answer["id"], 1);
    assert!(tool_names(&answer).contains("publish_artifact"), "{answer}");

    // The station stops; its socket goes with it. The bridge answers with
    // an error and keeps running.
    st.manager.shutdown(StopMode::Graceful).await;
    let socket = bridge.socket.clone();
    drop(bridge);
    rig.close(st).await;
    let down = bridge_call(&socket, &token, &tools_list()).await;
    assert_eq!(down["id"], 1);
    assert!(
        down["error"]["message"]
            .as_str()
            .unwrap()
            .contains("restarting"),
        "{down}"
    );

    // The next station loads the row and holds it until the replay ends.
    let st = rig.station().await;
    let bridge = bridge_of(&rig, &st).await;
    let answer = bridge_result(&bridge.socket, &token).await;
    assert_eq!(answer["id"], 1);
    assert!(tool_names(&answer).contains("publish_artifact"), "{answer}");
    assert_eq!(
        bridge.bindings.get_digest(&key).unwrap().state,
        BindingState::Active
    );
    st.manager.shutdown(StopMode::Graceful).await;
}

/// (c) A resume of an exited Claude session gets a new token; the old
/// token is refused and its row is `ended`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resume_gives_a_new_token_and_the_old_one_is_refused() {
    let mut rig = Rig::new();
    let st = rig.station().await;
    let bridge = bridge_of(&rig, &st).await;
    let session = rig.ended(&st, "claude").await;
    let (_, old) = launch_of(&rig.launch_log(), None).await;
    let old_key = digest(&old);
    wait_row(&st, &session, &old_key, RowState::Ended).await;

    rig.send(
        &st,
        SessionCommand::Resume {
            session_id: session.clone(),
        },
    )
    .await;
    let (argv, new) = launch_of(&rig.launch_log(), Some(&old)).await;
    assert_ne!(new, old);
    assert!(!argv.contains(&new), "argv never holds the token");
    assert!(argv.lines().any(|l| l == "--resume"), "{argv}");
    assert!(argv.lines().any(|l| l == "--mcp-config"), "{argv}");
    prompt_when_live(&mut rig, &st, &session, "again").await;
    wait_turns(&st.store, &session, 2).await;
    let new_key = digest(&new);
    wait_row(&st, &session, &new_key, RowState::Active).await;

    let rows = rows_of(&st, &session).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    let state_of = |key: &str| rows.iter().find(|r| r.digest == key).unwrap().state;
    assert_eq!(state_of(&old_key), RowState::Ended);
    assert_eq!(state_of(&new_key), RowState::Active);
    assert!(
        !bridge.bindings.has_digest(&old_key),
        "the old entry is gone"
    );

    let refused = bridge_call(&bridge.socket, &old, &tools_list()).await;
    assert_eq!(refused["error"]["code"], -32001, "{refused}");
    let allowed = bridge_call(&bridge.socket, &new, &tools_list()).await;
    assert!(
        tool_names(&allowed).contains("publish_artifact"),
        "{allowed}"
    );
    st.manager.shutdown(StopMode::Graceful).await;
}

/// (d) A coordinator start on the shim host succeeds; its row holds the
/// grant and the expiry, and `coordinator.bound` follows `session.started`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_coordinator_start_on_the_shim_host_binds_after_session_started() {
    let mut rig = Rig::new();
    let st = rig.station().await;
    let bridge = bridge_of(&rig, &st).await;
    let key = Identity::generate().unwrap().public_key();
    st.store
        .pin_peer(PairedKey {
            key,
            role: Role::Deck,
            scopes: BTreeSet::from([Scope::Read, Scope::Drive]),
            dial_url: None,
        })
        .await
        .unwrap();
    let mut spec = rig.spec("claude");
    spec.coordinator = Some(Box::new(CoordinatorSpec {
        grant_id: "grant-1".to_owned(),
        classes: vec!["read".to_owned()],
        idle_ttl_ms: 60_000,
        expires_at: 4_102_444_800_000,
        control_skill: ControlSkill {
            version: "1".to_owned(),
            text: "You coordinate the fleet.".to_owned(),
        },
        scope: GrantScope::Lineage,
        answer_cap: None,
        desktop_cap: None,
        capture_cap: None,
        image_byte_cap: None,
        allow_recording: false,
        recording_cap: None,
        recording_second_cap: None,
        allow_input: false,
        input_scope: None,
        input_cap: None,
        input_character_cap: None,
        stations: Vec::new(),
    }));
    rig.next += 1;
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from(format!("cmd-{}", rig.next)),
        target_incarnation: None,
        retry_until: i64::try_from(rig.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: start_command(spec, Some("hello")),
    };
    let decided = st
        .manager
        .submit(envelope, Caller::peer(key.fingerprint(), Scope::Drive))
        .await
        .unwrap();
    assert_eq!(
        decided.receipt.state,
        ReceiptState::Accepted,
        "{:?}",
        decided.receipt
    );
    let session = decided.receipt.result.and_then(|r| r.session_id).unwrap();
    let (argv, token) = launch_of(&rig.launch_log(), None).await;
    assert!(!argv.contains(&token));
    assert!(argv.contains("mcp-bridge"), "{argv}");
    let digest_key = digest(&token);
    wait_row(&st, &session, &digest_key, RowState::Active).await;
    let rows = rows_of(&st, &session).await;
    assert_eq!(rows[0].grant_id, "grant-1");
    assert_eq!(rows[0].binding_id, format!("{session}:grant-1"));
    assert_eq!(rows[0].expires_at, Some(4_102_444_800_000));
    wait_for(&st.store, &session, "coordinator.bound", |e| {
        e.iter()
            .any(|e| matches!(e, Event::CoordinatorBound { .. }))
    })
    .await;
    let all = events(&st.store, &session).await;
    let started = all
        .iter()
        .position(|e| matches!(e, Event::SessionStarted { .. }))
        .expect("session.started");
    let bound = all
        .iter()
        .position(|e| matches!(e, Event::CoordinatorBound { .. }))
        .expect("coordinator.bound");
    assert!(started < bound, "{all:?}");
    assert_eq!(
        bridge.bindings.get_digest(&digest_key).unwrap().state,
        BindingState::Active
    );
    st.manager.shutdown(StopMode::Graceful).await;
}

/// (f) A failing row insert (a trigger of the database): the session still
/// starts, without a token and without bridge arguments, and nothing is in
/// memory (DL-194 §2.3 "a failed launch insert: the start goes on without
/// tools"). The in-crate twin is `shim_credential_tests.rs`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failing_row_insert_starts_the_session_without_tools() {
    let mut rig = Rig::new();
    let st = rig.station().await;
    let bridge = bridge_of(&rig, &st).await;
    st.store
        .db()
        .call(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER fail_binding_insert BEFORE INSERT ON shim_bindings
                 BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let session = rig.start(&st, "claude", "hello").await;
    wait_turns(&st.store, &session, 1).await;
    let (argv, token) = launch_of(&rig.launch_log(), None).await;
    assert_eq!(token, "", "no token in the environment");
    assert!(!argv.contains("mcp-bridge") && !argv.contains("--mcp-config"));
    assert!(rows_of(&st, &session).await.is_empty());
    assert!(bridge.bindings.get(&format!("{session}:fleet")).is_none());
    st.manager.shutdown(StopMode::Graceful).await;
}

/// (e) A Codex start on the shim host with a Codex below the verified
/// bridge version (`bridge_of` records 0.160.0; `BRIDGE_VERIFIED` names
/// 0.162.1) has no token, no bridge arguments and no binding row (DL-194).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_codex_start_on_the_shim_host_has_no_token() {
    let mut rig = Rig::new();
    let st = rig.station().await;
    let bridge = bridge_of(&rig, &st).await;
    let session = rig.start(&st, "codex", "hello").await;
    wait_turns(&st.store, &session, 1).await;
    let log = std::fs::read_to_string(rig.codex_launch_log()).unwrap();
    let (argv, token) = log.rsplit_once("env:").unwrap();
    assert_eq!(token.trim(), "", "no token in the environment");
    assert!(!argv.contains("mcp-bridge") && !argv.contains("mcp_servers"));
    assert!(rows_of(&st, &session).await.is_empty());
    assert!(st.store.shim_bindings_load().await.unwrap().is_empty());
    assert!(bridge.bindings.get(&format!("{session}:fleet")).is_none());
    st.manager.shutdown(StopMode::Graceful).await;
}
