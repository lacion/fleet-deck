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
use fleetdeck_proto::event::{RunOrigin, ToolKind, ToolStatus, TurnStatus};
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
        coordinator: None,
        launch: None,
        extra_env: None,
    }
}

fn input(text: &str) -> HostInput {
    HostInput {
        // A station input id is a ULID (Claude derives its message uuid
        // from it).
        id: InputId::new("01K70000000000000000000001"),
        origin: RunOrigin::Board,
        input: UserInput::new(text.to_owned()),
        context: Vec::new(),
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

/// Reads events until a run completes; returns them all.
async fn one_run(rx: &mut mpsc::Receiver<Emitted>) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let e = tokio::time::timeout(STEP, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("no run.completed within {STEP:?}; saw {seen:#?}"))
            .expect("the event stream ended before the run completed")
            .event;
        let done = matches!(e, Event::RunCompleted { .. });
        seen.push(e);
        if done {
            return seen;
        }
    }
}

/// The index of the first event after `from` that `pred` accepts.
fn find_from(events: &[Event], from: usize, pred: impl Fn(&Event) -> bool) -> Option<usize> {
    events
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, e)| pred(e))
        .map(|(i, _)| i)
}

/// The kind names of `events`, in order, for the run report.
fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::SessionStarted { .. } => "session.started",
            Event::RunStarted { .. } => "run.started",
            Event::TurnStarted { .. } => "turn.started",
            Event::MessageDelta { .. } => "message.delta",
            Event::MessageCompleted { .. } => "message.completed",
            Event::ToolStarted { .. } => "tool.started",
            Event::ToolOutput { .. } => "tool.output",
            Event::ToolCompleted { .. } => "tool.completed",
            Event::TurnCompleted { .. } => "turn.completed",
            Event::RunCompleted { .. } => "run.completed",
            _ => "other",
        })
        .collect()
}

/// B8, DL-196: a `!` command through `thread/shellCommand` on the real
/// `codex app-server`. No model call: the run has the shell node, no
/// message. The first command prints its output and ends 0; the second
/// ends 3.
#[tokio::test]
#[ignore = "live: needs FD_LIVE_SMOKE=1, a signed-in `codex`, and network"]
async fn live_codex_shell_smoke() {
    if !opted_in("live_codex_shell_smoke") {
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
    let canonical = tmp.path().canonicalize().unwrap();
    let dir_name = tmp
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
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

    // The first command: output and exit 0.
    session
        .shell_command("e-live-1".into(), "pwd && echo fd-shell-ok".into(), 600_000)
        .await
        .unwrap();
    let events = one_run(&mut rx).await;
    eprintln!("shell run 1 events: {:?}", kinds(&events));
    let node = "shell-e-live-1";
    let run = find_from(&events, 0, |e| {
        matches!(e, Event::RunStarted { origin: RunOrigin::Shell, input_summary, .. }
            if input_summary == "!pwd && echo fd-shell-ok")
    })
    .unwrap_or_else(|| panic!("run.started of the shell origin: {events:#?}"));
    let started = find_from(&events, run + 1, |e| {
        matches!(e, Event::ToolStarted { node_id, tool_kind: ToolKind::Shell, title, .. }
            if node_id.0 == node && title == "Shell command")
    })
    .unwrap_or_else(|| panic!("tool.started of the shell node: {events:#?}"));
    let output = find_from(&events, started + 1, |e| {
        matches!(e, Event::ToolOutput { node_id, chunk, .. }
            if node_id.0 == node
                && chunk.contains("fd-shell-ok")
                && (chunk.contains(canonical.to_string_lossy().as_ref())
                    || chunk.lines().next().is_some_and(|l| l.trim_end().ends_with(&dir_name))))
    })
    .unwrap_or_else(|| panic!("tool.output with the marker and the directory: {events:#?}"));
    let completed = find_from(&events, output + 1, |e| {
        matches!(e, Event::ToolCompleted { node_id, status: ToolStatus::Ok, exit_code: Some(0), provider_refs, .. }
            if node_id.0 == node
                && provider_refs.get("shell").and_then(|v| v.as_str()) == Some("done"))
    })
    .unwrap_or_else(|| {
        panic!("tool.completed ok, exit 0, shell done, after the output: {events:#?}")
    });
    let turn = find_from(&events, completed + 1, |e| {
        matches!(e, Event::TurnCompleted { .. })
    })
    .unwrap_or_else(|| panic!("turn.completed after the tool: {events:#?}"));
    let end = find_from(&events, turn + 1, |e| {
        matches!(e, Event::RunCompleted { .. })
    })
    .unwrap_or_else(|| panic!("run.completed after the turn: {events:#?}"));
    assert_eq!(
        end,
        events.len() - 1,
        "run.completed ends the run: {events:#?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            Event::MessageDelta { .. } | Event::MessageCompleted { .. }
        )),
        "no model call, no assistant message: {events:#?}"
    );

    // The second command: exit 3.
    session
        .shell_command("e-live-2".into(), "exit 3".into(), 600_000)
        .await
        .unwrap();
    let events = one_run(&mut rx).await;
    eprintln!("shell run 2 events: {:?}", kinds(&events));
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::ToolCompleted { node_id, status: ToolStatus::Failed, exit_code: Some(3), .. }
                if node_id.0 == "shell-e-live-2"
        )),
        "tool.completed failed with exit 3: {events:#?}"
    );
    session.stop(StopMode::Graceful).await.unwrap();
    eprintln!("live_codex_shell_smoke: ok");
}

// ---- ADR-006 §8.5 test 31: the coordinator credential handoff ----------

/// A loopback MCP endpoint (streamable HTTP, JSON answers) with one tool.
/// It records the SHA-256 digest of each `Authorization` header and every
/// tool call; a request without the header gets `401`.
mod mcp_probe {
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// The answer of the tool; the agent must repeat it.
    pub const ANSWER: &str = "fleet-pong-7342";

    #[derive(Default, Debug)]
    pub struct Seen {
        /// `(method, digest of the Authorization header)`.
        pub requests: Vec<(String, Option<String>)>,
        pub unauthorized: usize,
    }

    pub fn digest(text: &str) -> String {
        let d = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
        d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }

    pub async fn start() -> (String, Arc<Mutex<Seen>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Seen::default()));
        let shared = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((conn, _)) = listener.accept().await {
                tokio::spawn(serve(conn, Arc::clone(&shared)));
            }
        });
        (url, seen)
    }

    async fn serve(mut conn: TcpStream, seen: Arc<Mutex<Seen>>) {
        let mut buf = Vec::new();
        loop {
            let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                let mut chunk = [0u8; 4096];
                match conn.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
                continue;
            };
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            let mut lines = head.lines();
            let request_line = lines.next().unwrap_or_default().to_owned();
            let mut length = 0usize;
            let mut auth = None;
            for line in lines {
                let Some((k, v)) = line.split_once(':') else {
                    continue;
                };
                match k.trim().to_ascii_lowercase().as_str() {
                    "content-length" => length = v.trim().parse().unwrap_or(0),
                    "authorization" => auth = Some(v.trim().to_owned()),
                    _ => {}
                }
            }
            while buf.len() < end + 4 + length {
                let mut chunk = [0u8; 4096];
                match conn.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let body: Vec<u8> = buf[end + 4..end + 4 + length].to_vec();
            buf.drain(..end + 4 + length);
            let (status, reply) = answer(&request_line, auth.as_deref(), &body, &seen);
            let text = reply.map(|v| v.to_string()).unwrap_or_default();
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{text}",
                text.len()
            );
            if conn.write_all(response.as_bytes()).await.is_err() {
                return;
            }
        }
    }

    fn answer(
        request_line: &str,
        auth: Option<&str>,
        body: &[u8],
        seen: &Mutex<Seen>,
    ) -> (&'static str, Option<Value>) {
        if !request_line.starts_with("POST ") {
            return ("405 Method Not Allowed", None);
        }
        let Some(token) = auth.and_then(|a| a.strip_prefix("Bearer ")) else {
            seen.lock().unwrap().unauthorized += 1;
            return ("401 Unauthorized", None);
        };
        let msg: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let method = msg["method"].as_str().unwrap_or_default().to_owned();
        seen.lock()
            .unwrap()
            .requests
            .push((method.clone(), Some(digest(token))));
        let Some(id) = msg.get("id").cloned() else {
            return ("202 Accepted", None);
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
            "tools/call" => json!({"content": [{"type": "text", "text": ANSWER}]}),
            "ping" => json!({}),
            _ => {
                return (
                    "200 OK",
                    Some(json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": "unknown method"}})),
                );
            }
        };
        (
            "200 OK",
            Some(json!({"jsonrpc": "2.0", "id": id, "result": result})),
        )
    }
}

/// True if any process command line contains `needle`.
fn in_any_cmdline(needle: &str) -> bool {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return false;
    };
    dir.flatten().any(|e| {
        std::fs::read(e.path().join("cmdline"))
            .is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
    })
}

/// True if a file under `root` (depth-first, regular files below 8 MiB)
/// contains `needle`.
fn in_files(root: &std::path::Path, needle: &str) -> bool {
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

/// One handoff round of test 31 for one adapter: a tiny session with the
/// coordinator handoff of the real adapter path (`start_coordinated`)
/// against a loopback MCP endpoint. The endpoint sees only the
/// credential's digest, the tool runs exactly once, and the value is in no
/// command line and in none of `files`.
async fn handoff_round(
    adapter: &dyn ProviderAdapter,
    kind: fleetdeck_proto::event::ProviderKind,
    instance: &str,
    mode: &str,
    files: &[PathBuf],
) {
    use fleetdeck_core::Secret;
    use fleetdeck_providers::coordinator::{CoordinatorLaunch, mcp_args};
    let (url, seen) = mcp_probe::start().await;
    let mut raw = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut raw).unwrap();
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let probe = adapter.probe().await.unwrap();
    eprintln!("{kind:?} probe: {probe:?}");
    signed_in(&probe.auth);
    let launch = CoordinatorLaunch {
        args: mcp_args(kind, &url).unwrap(),
        token: Secret::new(token.clone()),
        skill_dir: None,
    };
    assert!(!launch.args.iter().any(|a| a.contains(&token)));

    let tmp = tempfile::tempdir().unwrap();
    let (sink, mut rx) = EventSink::channel(1024);
    let mut session = tokio::time::timeout(
        STEP,
        adapter.start_coordinated(spec(instance, tmp.path(), mode), sink, launch),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !in_any_cmdline(&token),
        "the credential is in a command line"
    );
    session
        .prompt(input(
            "Call the fleet_ping tool of the fleetdeck MCP server exactly once. Then reply \
             with exactly the text that it returned.",
        ))
        .await
        .unwrap();
    let events = one_turn(&mut rx).await;
    assert!(
        !in_any_cmdline(&token),
        "the credential is in a command line"
    );
    session.stop(StopMode::Graceful).await.unwrap();

    let seen = seen.lock().unwrap();
    eprintln!("mcp probe: {seen:?}");
    let want = mcp_probe::digest(&token);
    let methods: Vec<&str> = seen.requests.iter().map(|(m, _)| m.as_str()).collect();
    assert!(
        methods.contains(&"initialize"),
        "{methods:?}; events: {events:#?}"
    );
    assert_eq!(
        methods.iter().filter(|m| **m == "tools/call").count(),
        1,
        "the tool ran exactly once: {methods:?}; events: {events:#?}"
    );
    assert!(
        seen.requests
            .iter()
            .all(|(_, d)| d.as_deref() == Some(want.as_str())),
        "every request carries the handed-off credential"
    );
    let answer: String = events
        .iter()
        .filter_map(|e| match e {
            Event::MessageCompleted { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        answer.contains(mcp_probe::ANSWER),
        "the agent saw the tool: {answer}"
    );
    for place in std::iter::once(tmp.path().to_path_buf()).chain(files.iter().cloned()) {
        assert!(
            !in_files(&place, &token),
            "the credential is in {}",
            place.display()
        );
    }
    eprintln!("{kind:?} handoff: verified for {:?}", probe.version);
}

/// ADR-006 §8.5 test 31 (DL-067 gate, DL-219, DL-220), provider half: one
/// [`handoff_round`] per provider. The station half (pending binding, the
/// native binding, one call through the station ingress) is
/// `manager::coordinator_live_tests` of `fleetdeck-station`.
#[tokio::test]
#[ignore = "live: needs FD_LIVE_SMOKE=1, a signed-in `claude` and `codex`, and network"]
async fn claude_and_codex_authenticate_to_a_loopback_mcp_endpoint_with_the_env_handoff() {
    use fleetdeck_proto::event::ProviderKind;
    if !opted_in("claude_and_codex_authenticate_to_a_loopback_mcp_endpoint_with_the_env_handoff") {
        return;
    }
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let mut config = claude::ClaudeAdapterConfig::new(
        "claude".into(),
        program("FD_CLAUDE_BIN", "claude"),
        ChildEnv::from_process(),
    );
    // `--safe-mode` also drops `--mcp-config` servers, and the sign-in
    // needs the user settings: this test loads the user settings, no MCP
    // server but the probe, and no skill. The handoff arguments allow the
    // probe's tools (DL-220).
    config.extra_args = vec![
        "--disable-slash-commands".to_owned(),
        "--strict-mcp-config".to_owned(),
    ];
    let adapter = claude::ClaudeAdapter::new(config, Arc::new(DirectChildHost::new()));
    let files = [
        home.join(".claude.json"),
        home.join(".claude").join("settings.json"),
        home.join(".claude").join("settings.local.json"),
    ];
    handoff_round(
        &adapter,
        ProviderKind::Claude,
        "claude",
        claude::SAFEST_MODE,
        &files,
    )
    .await;

    let mut config = codex::CodexAdapterConfig::new(
        "codex".into(),
        program("FD_CODEX_BIN", "codex"),
        ChildEnv::from_process(),
    );
    config.effort = Some("low".to_owned());
    let adapter = codex::CodexAdapter::new(config, Arc::new(DirectChildHost::new()));
    // `never` asks no approval; the sandbox stays read-only in the
    // temporary directory.
    let files = [home.join(".codex").join("config.toml")];
    handoff_round(
        &adapter,
        ProviderKind::Codex,
        "codex",
        "never/read-only",
        &files,
    )
    .await;
}
