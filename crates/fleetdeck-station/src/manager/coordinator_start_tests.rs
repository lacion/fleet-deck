//! The coordinator start wiring (ADR-006 §5.4, §8.2 test 10; DL-216): a
//! deck start with a grant mints the credential, adds a pending binding
//! under its digest and hands the credential to the real provider child
//! through its environment only. Nothing in the station store holds it.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, StopMode,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{CommandEnvelope, CommandId, SessionId};
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::host::DirectChildHost;
use fleetdeck_testkit::fake::{LAUNCH_LOG_ENV, SCENARIO_ENV};
use fleetdeck_testkit::{Det, FakeAgent};

use super::tests::coordinator_spec;
use super::*;
use crate::coordinator::{BindingState, Bindings, digest};
use crate::store::{Caller, OfflineApprovalPolicy, StoreConfig};

const URL: &str = "http://127.0.0.1:41234/mcp";

fn verified(_: ProviderKind, _: Option<&str>) -> Result<(), &'static str> {
    Ok(())
}

/// Every byte of every file under `dir`.
fn bytes_under(dir: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(bytes_under(&path));
        } else {
            out.extend(std::fs::read(&path).unwrap());
        }
    }
    out
}

#[tokio::test]
async fn a_deck_start_with_a_grant_hands_the_credential_to_the_child_environment_only() {
    let det = Det::new();
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("projects/app");
    std::fs::create_dir_all(&ws).unwrap();
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let launch_log = root.path().join("launch.log");

    let store = Store::open(
        &data.join("station.db"),
        det.clock(),
        det.ids(),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    let bindings = Bindings::default().with_handoff(verified);
    bindings.set_endpoint(URL.to_owned());
    let policy = Policy::new(
        vec![root.path().join("projects")],
        2,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
    )
    .with_coordinator(bindings.clone());
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, "sim").unwrap();
    env.set(LAUNCH_LOG_ENV, &launch_log).unwrap();
    let config =
        ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
    let adapter: Arc<dyn fleetdeck_providers::ProviderAdapter> =
        Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())));
    let manager = SessionManager::start(
        store,
        policy.clone(),
        Adapters::from([("claude".into(), adapter)]),
        det.ids(),
    )
    .await
    .unwrap();

    // An owner start from the deck (origin board).
    let mut spec = SessionSpec {
        provider_instance: "claude".into(),
        cwd: ws.to_string_lossy().into_owned(),
        mode: Some("default".to_owned()),
        model: None,
        title: None,
        coordinator: Some(Box::new(coordinator_spec())),
    };
    spec.title = Some("coordinator".to_owned());
    let envelope = CommandEnvelope {
        command_id: CommandId::from("cmd-coord".to_owned()),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Start {
            spec,
            prompt: None,
            workspace: None,
        },
    };
    let command = envelope.command.clone();
    let decide_policy = policy.clone();
    let decided = manager
        .store()
        .decide(envelope, Caller::deck("k"), move |d| {
            decide::decide(d, &decide_policy, CommandOrigin::Board, command)
        })
        .await
        .unwrap();
    assert_eq!(
        decided.receipt.state,
        ReceiptState::Accepted,
        "{:?}",
        decided.receipt
    );
    let session: SessionId = decided.receipt.result.unwrap().session_id.unwrap();

    // The real child wrote its argv and the credential of its environment.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let log = loop {
        if let Ok(log) = std::fs::read_to_string(&launch_log)
            && log.contains("env:")
        {
            break log;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the child did not start"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let (argv, token) = log.split_once("env:").unwrap();
    let token = token.trim().to_owned();
    assert!(
        token.starts_with("fdc1_"),
        "the child environment holds the credential"
    );
    assert!(!argv.contains(&token), "argv never holds the credential");
    assert!(argv.contains("--mcp-config"));
    assert!(argv.contains("Bearer ${FLEETDECK_COORDINATOR_TOKEN}"));
    assert!(argv.contains(URL));

    // The binding is pending under the digest of the credential.
    assert!(bindings.has_digest(&digest(&token)));
    let binding = bindings.find(&session, "grant-1").unwrap();
    assert_eq!(binding.state, BindingState::Pending);
    assert_eq!(binding.generation, 0);
    assert_eq!(binding.control_skill, "You coordinate the fleet.");

    manager.shutdown(StopMode::Kill).await;
    // No station store file holds the credential (database, WAL).
    let stored = bytes_under(&data);
    assert!(!stored.is_empty());
    assert!(
        !stored.windows(token.len()).any(|w| w == token.as_bytes()),
        "the station store never holds the credential"
    );
}

/// Without a verified handoff the start effect itself refuses too: a
/// grant that passed an older decision never runs without its check.
#[test]
fn the_start_effect_rechecks_the_handoff_and_the_endpoint() {
    let spec = SessionSpec {
        provider_instance: "claude".into(),
        cwd: "/w".to_owned(),
        mode: None,
        model: None,
        title: None,
        coordinator: None,
    };
    let grant = coordinator_spec();
    let session: SessionId = "s-1".into();
    let unverified = Bindings::default();
    unverified.set_endpoint(URL.to_owned());
    let err = coordinator_start::prepare(
        &unverified,
        ProviderKind::Claude,
        &session,
        &spec,
        &grant,
        false,
    )
    .unwrap_err();
    assert_eq!(err, fleetdeck_providers::coordinator::UNVERIFIED);
    let no_endpoint = Bindings::default().with_handoff(verified);
    assert!(
        coordinator_start::prepare(
            &no_endpoint,
            ProviderKind::Claude,
            &session,
            &spec,
            &grant,
            false
        )
        .is_err()
    );
    let ok = Bindings::default().with_handoff(verified);
    ok.set_endpoint(URL.to_owned());
    assert!(
        coordinator_start::prepare(&ok, ProviderKind::Claude, &session, &spec, &grant, true)
            .is_err(),
        "a resume or a shim-hosted start is refused"
    );
    assert!(!ok.has_digest("x"));
    assert!(
        ok.find(&session, "grant-1").is_none(),
        "a refusal adds no binding"
    );
}
