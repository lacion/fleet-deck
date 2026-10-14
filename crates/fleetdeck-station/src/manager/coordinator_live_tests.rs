//! ADR-006 §8.5 test 31, station half (DL-220): the opt-in live smoke of
//! the coordinator handoff through the real station path. A deck start
//! with a grant runs the real `claude` or `codex` CLI with the station's
//! own MCP ingress as its endpoint. The test checks the pending binding,
//! its activation by the native identity of `session.started`, one
//! authenticated tool call that reaches the forward exactly once with the
//! active binding, the answer in the transcript, and that the credential
//! is in no command line, no station store file and no provider settings
//! file. The handoff gate is the real one: the probed version must be in
//! `fleetdeck_providers::coordinator::VERIFIED`.
//!
//! Each test is `#[ignore]`d and does nothing unless `FD_LIVE_SMOKE=1`:
//!
//! ```sh
//! FD_LIVE_SMOKE=1 cargo test -p fleetdeck-station --lib -- --ignored \
//!     --nocapture coordinator_live_tests
//! ```

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, StopMode,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, EventBody, SessionId};
use fleetdeck_providers::ProviderAdapter;
use fleetdeck_providers::coordinator::TOKEN_ENV;
use fleetdeck_providers::host::DirectChildHost;
use fleetdeck_providers::{claude, codex};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

use super::tests::coordinator_spec;
use super::*;
use crate::coordinator::{BindingState, Bindings, Caller as Holder, Forward, Ingress, Limits};
use crate::store::{Caller, OfflineApprovalPolicy, ReadResult, StoreConfig};

/// The bound of one live step.
const STEP: Duration = Duration::from_secs(240);
/// The value that the forward answers; the agent must repeat it.
const ANSWER: &str = "fleet-pong-7342";

fn opted_in(name: &str) -> bool {
    let on = std::env::var("FD_LIVE_SMOKE").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("{name}: skipped (set FD_LIVE_SMOKE=1 to run the live smoke tests)");
    }
    on
}

/// The deck side: records every forwarded call and answers [`ANSWER`].
#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<(Holder, String)>>,
}

#[async_trait]
impl Forward for Recorder {
    async fn call(&self, caller: Holder, tool: &str, _args: Value) -> Result<Value, String> {
        self.calls.lock().unwrap().push((caller, tool.to_owned()));
        Ok(json!({"sessions": [], "answer": ANSWER}))
    }
}

/// The credential in the environment of a running child whose digest
/// names a binding of this test (other tests may run fake children).
fn token_of_child(bindings: &Bindings) -> Option<String> {
    let needle = format!("{TOKEN_ENV}=");
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(env) = std::fs::read(entry.path().join("environ")) else {
            continue;
        };
        for var in env.split(|b| *b == 0) {
            let Ok(var) = std::str::from_utf8(var) else {
                continue;
            };
            if let Some(token) = var.strip_prefix(&needle)
                && bindings.has_digest(&crate::coordinator::digest(token))
            {
                return Some(token.to_owned());
            }
        }
    }
    None
}

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

async fn wait_for<T>(what: &str, mut f: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + STEP;
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(tokio::time::Instant::now() < deadline, "timeout: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// One live round for `kind`: the adapter, its mode and the provider
/// settings files that must not hold the credential.
#[allow(clippy::too_many_lines, reason = "one linear live scenario")]
async fn live_round(kind: ProviderKind) {
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let (adapter, mode, settings): (Arc<dyn ProviderAdapter>, &str, Vec<PathBuf>) = match kind {
        ProviderKind::Claude => {
            let mut config = claude::ClaudeAdapterConfig::new(
                "live".into(),
                PathBuf::from(std::env::var("FD_CLAUDE_BIN").unwrap_or("claude".into())),
                ChildEnv::from_process(),
            );
            // As the provider half (DL-219): the user settings for the
            // sign-in and no MCP server but the station's; the handoff
            // arguments allow its tools (DL-220).
            config.extra_args = vec![
                "--disable-slash-commands".to_owned(),
                "--strict-mcp-config".to_owned(),
            ];
            let adapter = claude::ClaudeAdapter::new(config, Arc::new(DirectChildHost::new()));
            let settings = vec![
                home.join(".claude.json"),
                home.join(".claude").join("settings.json"),
                home.join(".claude").join("settings.local.json"),
            ];
            (Arc::new(adapter), claude::SAFEST_MODE, settings)
        }
        _ => {
            let mut config = codex::CodexAdapterConfig::new(
                "live".into(),
                PathBuf::from(std::env::var("FD_CODEX_BIN").unwrap_or("codex".into())),
                ChildEnv::from_process(),
            );
            config.effort = Some("low".to_owned());
            let adapter = codex::CodexAdapter::new(config, Arc::new(DirectChildHost::new()));
            // `never` asks no approval; the sandbox stays read-only in a
            // temporary directory.
            let settings = vec![home.join(".codex").join("config.toml")];
            (Arc::new(adapter), "never/read-only", settings)
        }
    };
    let probe = adapter.probe().await.unwrap();
    eprintln!("{kind:?} probe: {probe:?}");

    let det = Det::new();
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("projects/app");
    std::fs::create_dir_all(&ws).unwrap();
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let store = Store::open(
        &data.join("station.db"),
        det.clock(),
        det.ids(),
        StoreConfig::default(),
    )
    .await
    .unwrap();

    // The real gate: the probed version must be verified (DL-219).
    let bindings = Bindings::default();
    bindings.record_version(kind, probe.version.clone().unwrap());
    let forward = Arc::new(Recorder::default());
    let ingress = Ingress::start(bindings.clone(), forward.clone(), Limits::default())
        .await
        .unwrap();
    bindings.set_endpoint(ingress.url());
    let policy = Policy::new(
        vec![root.path().join("projects")],
        2,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([("live".into(), kind)]),
    );
    let holders = Holders {
        coordinator: bindings.clone(),
        ..Holders::default()
    };
    let manager = SessionManager::start_with_holders(
        store,
        policy.clone(),
        holders,
        Adapters::from([("live".into(), adapter)]),
        det.ids(),
        Arc::new(crate::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();

    let decide = async |id: &str, command: SessionCommand| {
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(id.to_owned()),
            target_incarnation: None,
            retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command: command.clone(),
        };
        let p = policy.clone();
        let b = bindings.clone();
        manager
            .store()
            .decide(envelope, Caller::deck("k"), move |d| {
                decide::decide(d, &p, &b, CommandOrigin::Board, command)
            })
            .await
            .unwrap()
    };
    let spec = SessionSpec {
        provider_instance: "live".into(),
        cwd: ws.to_string_lossy().into_owned(),
        mode: Some(mode.to_owned()),
        model: None,
        title: Some("coordinator".to_owned()),
        coordinator: Some(Box::new(coordinator_spec())),
    };
    let prompt = "Call the list_sessions tool of the fleetdeck MCP server exactly once. \
                  Then reply with exactly the value of the `answer` field of its result.";
    let decided = decide(
        "cmd-live",
        SessionCommand::Start {
            spec,
            prompt: Some(super::tests::text(prompt)),
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
        },
    )
    .await;
    assert_eq!(
        decided.receipt.state,
        ReceiptState::Accepted,
        "{:?}",
        decided.receipt
    );
    let session: SessionId = decided.receipt.result.unwrap().session_id.unwrap();

    // Pending at generation 0 until the native identity, then active at 1.
    // The start effect runs after the decision.
    let first = wait_for("the start adds the binding", async || {
        bindings.find(&session, "grant-1")
    })
    .await;
    assert!(matches!(
        (first.state, first.generation),
        (BindingState::Pending, 0) | (BindingState::Active, 1)
    ));
    let token = wait_for("the child holds the credential", async || {
        token_of_child(&bindings)
    })
    .await;
    assert!(
        !in_any_cmdline(&token),
        "the credential is in a command line"
    );
    let bound = wait_for("the binding becomes active", async || {
        bindings
            .find(&session, "grant-1")
            .filter(|b| b.state == BindingState::Active)
    })
    .await;
    assert_eq!(bound.generation, 1);
    let native = bound.native_id.clone().unwrap();

    let read = async || match manager.store().read_after(0, 100_000).await.unwrap() {
        ReadResult::Events { events, .. } => events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(e) => Some(e),
                EventBody::Unknown { .. } => None,
            })
            .collect::<Vec<Event>>(),
        ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
    };
    let events = wait_for("the turn completes", async || {
        let events = read().await;
        events
            .iter()
            .any(|e| matches!(e, Event::TurnCompleted { .. }))
            .then_some(events)
    })
    .await;
    assert!(
        events.iter().any(|e| matches!(e,
            Event::CoordinatorBound { grant_id, generation: 1, native_id, .. }
                if grant_id == "grant-1" && native_id == &native)),
        "coordinator.bound: {events:#?}"
    );
    let answer: String = events
        .iter()
        .filter_map(|e| match e {
            Event::MessageCompleted { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(answer.contains(ANSWER), "the agent saw the tool: {answer}");
    {
        let calls = forward.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "the tool ran exactly once: {calls:?}");
        let (holder, tool) = &calls[0];
        assert_eq!(tool, "list_sessions");
        assert_eq!(holder.binding_id, bound.binding_id);
        assert_eq!(holder.generation, 1);
        assert_eq!(holder.native_id, native);
    }

    decide(
        "cmd-live-stop",
        SessionCommand::Stop {
            session_id: session.clone(),
            how: StopMode::Graceful,
        },
    )
    .await;
    wait_for("the binding is revoked", async || {
        bindings
            .find(&session, "grant-1")
            .filter(|b| b.state == BindingState::Revoked)
    })
    .await;
    manager.shutdown(StopMode::Kill).await;
    ingress.shutdown();

    for place in std::iter::once(root.path().to_path_buf()).chain(settings) {
        assert!(
            !in_files(&place, &token),
            "the credential is in {}",
            place.display()
        );
    }
    eprintln!("{kind:?} {:?}: station handoff verified", probe.version);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: needs FD_LIVE_SMOKE=1, a signed-in `claude`, and network"]
async fn a_live_claude_coordinator_binds_and_calls_the_station_ingress_once() {
    if opted_in("a_live_claude_coordinator_binds_and_calls_the_station_ingress_once") {
        live_round(ProviderKind::Claude).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: needs FD_LIVE_SMOKE=1, a signed-in `codex`, and network"]
async fn a_live_codex_coordinator_binds_and_calls_the_station_ingress_once() {
    if opted_in("a_live_codex_coordinator_binds_and_calls_the_station_ingress_once") {
        live_round(ProviderKind::Codex).await;
    }
}
