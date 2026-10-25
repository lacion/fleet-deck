//! Opt-in live smoke of the stdio bridge of a shim host (DL-194 §9.10): the
//! real `claude` and `codex` pass `FLEETDECK_COORDINATOR_TOKEN` to the
//! real `fleetdeck mcp-bridge`, which relays to a stub station socket.
//!
//! Each test is `#[ignore]`d and does nothing unless `FD_LIVE_SMOKE=1`. A
//! positive round proves that the token reaches the bridge and the station
//! socket; a control round without the forwarding proves that the test
//! detects a missing token. A pass sets `BRIDGE_VERIFIED`. Run:
//!
//! ```sh
//! make smoke-live-bridge
//! ```
//!
//! See `docs/v2/LIVE-SMOKE.md`.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleetdeck_core::{ChildEnv, Secret};
use fleetdeck_proto::command::{SessionSpec, StopMode, UserInput};
use fleetdeck_proto::event::{ProviderKind, RunOrigin};
use fleetdeck_proto::{Event, InputId};
use fleetdeck_providers::coordinator::{CoordinatorLaunch, TOKEN_ENV, bridge_args};
use fleetdeck_providers::host::DirectChildHost;
use fleetdeck_providers::{
    AuthState, Emitted, EventSink, HostInput, ProviderAdapter, claude, codex,
};
use fleetdeck_station::coordinator::bindings::mint;
use fleetdeck_station::coordinator::digest;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

/// The bound of one live step (start, or one turn).
const STEP: Duration = Duration::from_secs(180);
/// The prompt of both rounds.
const PROMPT: &str = "Call the fleet_ping tool of the fleetdeck MCP server exactly once and \
                      reply with its text.";

fn opted_in(name: &str) -> bool {
    let on = std::env::var("FD_LIVE_SMOKE").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("{name}: skipped (set FD_LIVE_SMOKE=1 to run the live smoke tests)");
    }
    on
}

/// What the stub station saw: the digest of each `AUTH` token and each
/// method, in arrival order.
#[derive(Default, Debug)]
struct Seen {
    auth: Vec<String>,
    methods: Vec<String>,
}

/// The stub station: a Unix socket that speaks the bridge wire v1 (DL-194).
struct Stub {
    seen: Arc<Mutex<Seen>>,
    word: String,
    task: tokio::task::JoinHandle<()>,
}

impl Stub {
    fn start(socket: &Path, word: &str) -> Self {
        let listener = UnixListener::bind(socket).unwrap();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (shared, text) = (Arc::clone(&seen), word.to_owned());
        let task = tokio::spawn(async move {
            while let Ok((conn, _)) = listener.accept().await {
                tokio::spawn(serve(conn, Arc::clone(&shared), text.clone()));
            }
        });
        Self {
            seen,
            word: word.to_owned(),
            task,
        }
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One connection: `AUTH <token>`, one JSON-RPC line, one line back. A
/// message without an id gets no line (the connection closes).
async fn serve(conn: UnixStream, seen: Arc<Mutex<Seen>>, word: String) {
    let (read, mut write) = conn.into_split();
    let mut lines = BufReader::new(read).lines();
    let Ok(Some(auth)) = lines.next_line().await else {
        return;
    };
    let Some(token) = auth.strip_prefix("AUTH ") else {
        return;
    };
    let Ok(Some(body)) = lines.next_line().await else {
        return;
    };
    let msg: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let method = msg["method"].as_str().unwrap_or_default().to_owned();
    {
        let mut seen = seen.lock().unwrap();
        seen.auth.push(digest(token));
        seen.methods.push(method.clone());
    }
    let Some(id) = msg.get("id").cloned() else {
        return;
    };
    let result = match method.as_str() {
        "initialize" => json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fleetdeck", "version": "0"},
        }),
        "tools/list" => json!({"tools": [{
            "name": "fleet_ping",
            "description": "Returns the fleet's answer word.",
            "inputSchema": {"type": "object", "properties": {}},
        }]}),
        "tools/call" => json!({"content": [{"type": "text", "text": word}]}),
        "ping" => json!({}),
        _ => {
            let line = json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": "unknown method"}});
            let _ = write.write_all(format!("{line}\n").as_bytes()).await;
            return;
        }
    };
    let line = json!({"jsonrpc": "2.0", "id": id, "result": result});
    let _ = write.write_all(format!("{line}\n").as_bytes()).await;
}

/// The files of one round: a short temporary directory (a Unix socket path
/// has a small limit) with the stub socket, the wrapper and the records.
struct Fixture {
    dir: tempfile::TempDir,
    socket: PathBuf,
    wrapper: PathBuf,
    stub: Stub,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("fdb")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::create_dir(dir.path().join("work")).unwrap();
        std::fs::create_dir(dir.path().join("rec")).unwrap();
        let socket = dir.path().join("s");
        let wrapper = dir.path().join("wrap.sh");
        let word = format!("ANSWER-{}", &mint().unwrap().1[..8]);
        let stub = Stub::start(&socket, &word);
        write_wrapper(&wrapper, &dir.path().join("rec"));
        Self {
            dir,
            socket,
            wrapper,
            stub,
        }
    }

    fn work(&self) -> PathBuf {
        self.dir.path().join("work")
    }

    /// The records `<prefix>.<pid>` of the wrapper: the file contents.
    fn records(&self, prefix: &str) -> Vec<String> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.dir.path().join("rec"))
            .unwrap()
            .flatten()
        {
            if entry.file_name().to_string_lossy().starts_with(prefix) {
                out.push(std::fs::read_to_string(entry.path()).unwrap());
            }
        }
        out
    }
}

/// The MCP `command`: it records the SHA-256 of the token (`seen.<pid>`)
/// or that there was none (`none.<pid>`), never the raw value, then runs
/// the real bridge (DL-194).
fn write_wrapper(path: &Path, rec: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let hasher = std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("sha256sum"))
                .find(|f| f.is_file())
        })
        .expect("sha256sum on PATH");
    let bridge = env!("CARGO_BIN_EXE_fleetdeck");
    for text in [rec.to_str().unwrap(), hasher.to_str().unwrap(), bridge] {
        assert!(!text.contains('\''), "a quote in {text}");
    }
    let script = format!(
        "#!/bin/sh\n\
         if [ -n \"${{{TOKEN_ENV}:-}}\" ]; then\n\
         printf %s \"${TOKEN_ENV}\" | '{hasher}' | {{ read -r h _; printf %s \"$h\" > '{rec}'/seen.$$; }}\n\
         else\n\
         : > '{rec}'/none.$$\n\
         fi\n\
         exec '{bridge}' \"$@\"\n",
        hasher = hasher.display(),
        rec = rec.display(),
    );
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn adapter_for(kind: ProviderKind, extra: Vec<String>) -> Box<dyn ProviderAdapter> {
    let host = Arc::new(DirectChildHost::new());
    match kind {
        ProviderKind::Claude => {
            let mut config = claude::ClaudeAdapterConfig::new(
                "claude".into(),
                PathBuf::from(std::env::var("FD_CLAUDE_BIN").unwrap_or_else(|_| "claude".into())),
                ChildEnv::from_process(),
            );
            // The user settings keep the sign-in; no other MCP server and
            // no skill (as the handoff smoke).
            config.extra_args = vec![
                "--disable-slash-commands".to_owned(),
                "--strict-mcp-config".to_owned(),
            ];
            config.extra_args.extend(extra);
            Box::new(claude::ClaudeAdapter::new(config, host))
        }
        ProviderKind::Codex => {
            let mut config = codex::CodexAdapterConfig::new(
                "codex".into(),
                PathBuf::from(std::env::var("FD_CODEX_BIN").unwrap_or_else(|_| "codex".into())),
                ChildEnv::from_process(),
            );
            config.effort = Some("low".to_owned());
            Box::new(codex::CodexAdapter::new(config, host))
        }
        other => panic!("no live adapter for {other:?}"),
    }
}

fn spec(kind: ProviderKind, cwd: &Path) -> SessionSpec {
    let (instance, mode) = match kind {
        ProviderKind::Claude => ("claude", claude::SAFEST_MODE),
        _ => ("codex", "never/read-only"),
    };
    SessionSpec {
        effort: None,
        provider_instance: instance.into(),
        cwd: cwd.to_string_lossy().into_owned(),
        mode: Some(mode.to_owned()),
        model: None,
        title: None,
        coordinator: None,
        launch: None,
        extra_env: None,
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

/// The text of the completed messages of a turn.
fn answer_of(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::MessageCompleted { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// How a round starts the provider.
enum Start {
    /// The token goes into the child environment (`start_coordinated`).
    WithToken(CoordinatorLaunch),
    /// The provider gets its arguments from the adapter and no token.
    NoToken,
}

/// Starts the provider, sends [`PROMPT`], waits for the turn, stops. The
/// token (when there is one) is in no command line during the round.
async fn run_round(
    adapter: &dyn ProviderAdapter,
    kind: ProviderKind,
    fixture: &Fixture,
    start: Start,
    token: &str,
) -> String {
    let (sink, mut rx) = EventSink::channel(1024);
    let spec = spec(kind, &fixture.work());
    let mut session = tokio::time::timeout(STEP, async {
        match start {
            Start::WithToken(launch) => adapter.start_coordinated(spec, sink, launch).await,
            Start::NoToken => adapter.start(spec, sink).await,
        }
    })
    .await
    .unwrap()
    .unwrap();
    assert!(!in_any_cmdline(token), "the token is in a command line");
    session
        .prompt(HostInput {
            // A station input id is a ULID (Claude derives its message
            // uuid from it).
            id: InputId::new("01K70000000000000000000001"),
            origin: RunOrigin::Board,
            input: UserInput::new(PROMPT.to_owned()),
            context: Vec::new(),
        })
        .await
        .unwrap();
    let events = one_turn(&mut rx).await;
    assert!(!in_any_cmdline(token), "the token is in a command line");
    session.stop(StopMode::Graceful).await.unwrap();
    answer_of(&events)
}

/// True if any process command line contains `needle`.
fn in_any_cmdline(needle: &str) -> bool {
    std::fs::read_dir("/proc").is_ok_and(|dir| {
        dir.flatten().any(|e| {
            std::fs::read(e.path().join("cmdline"))
                .is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
        })
    })
}

/// True if a regular file under `root` (below 8 MiB) contains `needle`.
fn in_files(root: &Path, needle: &str) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(root) else {
        return false;
    };
    if meta.is_file() {
        return meta.len() < 8 << 20
            && std::fs::read(root)
                .is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()));
    }
    meta.is_dir()
        && std::fs::read_dir(root).is_ok_and(|d| d.flatten().any(|e| in_files(&e.path(), needle)))
}

/// The positive round, then the control round, for one provider.
async fn bridge_round(kind: ProviderKind) {
    // Positive: the token goes to the child; the bridge must reach the stub.
    let fixture = Fixture::new();
    let (secret, want) = mint().unwrap();
    let token = secret.expose().clone();
    let args = bridge_args(kind, &fixture.wrapper, &fixture.socket).unwrap();
    assert!(!args.iter().any(|a| a.contains(&token)));
    let adapter = adapter_for(kind, Vec::new());
    let probe = adapter.probe().await.unwrap();
    eprintln!("{kind:?} probe: {probe:?}");
    assert!(
        matches!(probe.auth, AuthState::SignedIn { .. } | AuthState::Unknown),
        "the provider is not signed out: {:?}",
        probe.auth
    );
    let launch = CoordinatorLaunch {
        args,
        token: Secret::new(token.clone()),
        skill_dir: None,
    };
    let answer = run_round(&*adapter, kind, &fixture, Start::WithToken(launch), &token).await;
    {
        let seen = fixture.stub.seen.lock().unwrap();
        eprintln!("{kind:?} stub saw: {seen:?}");
        assert!(!seen.auth.is_empty(), "no authenticated connection");
        assert!(
            seen.auth.iter().all(|d| *d == want),
            "every AUTH carries the minted token: {seen:?}"
        );
        assert!(
            seen.methods.iter().any(|m| m == "initialize"),
            "{:?}",
            seen.methods
        );
        assert_eq!(
            seen.methods.iter().filter(|m| *m == "tools/call").count(),
            1,
            "the tool ran exactly once: {:?}",
            seen.methods
        );
    }
    let hashes = fixture.records("seen.");
    assert!(
        !hashes.is_empty() && hashes.iter().all(|h| *h == want),
        "the wrapper saw the token: {hashes:?}"
    );
    assert!(
        answer.contains(&fixture.stub.word),
        "the agent saw the tool: {answer}"
    );
    assert!(!in_any_cmdline(&token), "the token is in a command line");
    assert!(
        !in_files(fixture.dir.path(), &token),
        "the token is in a file of the temporary directory"
    );
    eprintln!("{kind:?} bridge: verified for {:?}", probe.version);
    drop(fixture);

    // Control: the same arguments without the forwarding; the provider
    // still starts the server, which then has no token.
    let fixture = Fixture::new();
    let (secret, _) = mint().unwrap();
    let token = secret.expose().clone();
    let args = bridge_args(kind, &fixture.wrapper, &fixture.socket).unwrap();
    let (adapter, start) = match kind {
        ProviderKind::Claude => (adapter_for(kind, args), Start::NoToken),
        _ => {
            // Drop the `-c mcp_servers.fleetdeck.env_vars=[...]` pair.
            let args = args
                .chunks(2)
                .filter(|pair| !pair.iter().any(|a| a.contains("env_vars")))
                .flatten()
                .cloned()
                .collect();
            let launch = CoordinatorLaunch {
                args,
                token: Secret::new(token.clone()),
                skill_dir: None,
            };
            (adapter_for(kind, Vec::new()), Start::WithToken(launch))
        }
    };
    run_round(&*adapter, kind, &fixture, start, &token).await;
    // The server starts with the provider; give a late start a moment.
    for _ in 0..100 {
        if !fixture.records("none.").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !fixture.records("none.").is_empty(),
        "the provider did not start the server"
    );
    assert!(
        fixture.records("seen.").is_empty(),
        "the server saw a token without the forwarding"
    );
    let seen = fixture.stub.seen.lock().unwrap();
    assert!(seen.auth.is_empty(), "a connection authenticated: {seen:?}");
    eprintln!("{kind:?} control: no token without the forwarding");
}

#[tokio::test]
#[ignore = "live: needs FD_LIVE_SMOKE=1, a signed-in `claude`, and network"]
async fn claude_passes_the_token_to_the_stdio_bridge() {
    if !opted_in("claude_passes_the_token_to_the_stdio_bridge") {
        return;
    }
    bridge_round(ProviderKind::Claude).await;
}

#[tokio::test]
#[ignore = "live: needs FD_LIVE_SMOKE=1, a signed-in `codex`, and network"]
async fn codex_passes_the_token_to_the_stdio_bridge() {
    if !opted_in("codex_passes_the_token_to_the_stdio_bridge") {
        return;
    }
    bridge_round(ProviderKind::Codex).await;
}
