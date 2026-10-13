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
use crate::store::{Caller, OfflineApprovalPolicy, ReadResult, StoreConfig};
use fleetdeck_proto::{Event, EventBody};

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
            prompt: Some(super::tests::text("hello")),
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
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

    // The binding is under the digest of the credential: pending at
    // generation 0 until `session.started` names the native identity.
    assert!(bindings.has_digest(&digest(&token)));
    let binding = bindings.find(&session, "grant-1").unwrap();
    assert!(matches!(
        (binding.state, binding.generation),
        (BindingState::Pending, 0) | (BindingState::Active, 1)
    ));
    assert_eq!(binding.control_skill, "You coordinate the fleet.");

    // DL-217: the native identity of `session.started` binds it
    // (generation 1, `coordinator.bound`); the process end revokes it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let bound = loop {
        let b = bindings.find(&session, "grant-1").unwrap();
        if b.state == BindingState::Active {
            break b;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the binding never became active"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(bound.generation, 1);
    let native = bound.native_id.clone().unwrap();
    let lifecycle = |events: Vec<fleetdeck_proto::EventEnvelope>| -> Vec<Event> {
        events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(
                    e @ (Event::CoordinatorBound { .. } | Event::CoordinatorEnded { .. }),
                ) => Some(e),
                _ => None,
            })
            .collect()
    };
    let read = || async {
        match manager.store().read_after(0, 100_000).await.unwrap() {
            ReadResult::Events { events, .. } => events,
            ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
        }
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let facts = loop {
        let facts = lifecycle(read().await);
        if !facts.is_empty() {
            break facts;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no coordinator.bound"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(matches!(
        &facts[0],
        Event::CoordinatorBound { grant_id, generation: 1, native_id, .. }
            if grant_id == "grant-1" && native_id == &native
    ));

    let stop = CommandEnvelope {
        command_id: CommandId::from("cmd-stop".to_owned()),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Stop {
            session_id: session.clone(),
            how: StopMode::Kill,
        },
    };
    let command = stop.command.clone();
    let decide_policy = policy.clone();
    manager
        .store()
        .decide(stop, Caller::deck("k"), move |d| {
            decide::decide(d, &decide_policy, CommandOrigin::Board, command)
        })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let facts = lifecycle(read().await);
        if facts.iter().any(|e| {
            matches!(e, Event::CoordinatorEnded { reason, generation: 1, .. }
                if reason == coordinator_start::REASON_PROCESS_EXIT)
        }) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no coordinator.ended"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        bindings.find(&session, "grant-1").unwrap().state,
        BindingState::Revoked
    );

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

/// DL-165: a session without a grant gets a fleet-only binding when the
/// provider can take the handoff; it never reaches the deck as a
/// coordinator binding; a resume, a missing endpoint or an unverified
/// provider start without it.
#[test]
fn a_session_without_a_grant_gets_a_fleet_only_binding_when_the_handoff_works() {
    let spec = SessionSpec {
        provider_instance: "claude".into(),
        cwd: "/w".to_owned(),
        mode: None,
        model: None,
        title: None,
        coordinator: None,
    };
    let session: SessionId = "s-1".into();
    let ok = Bindings::default().with_handoff(verified);
    ok.set_endpoint(URL.to_owned());
    let launch =
        coordinator_start::prepare_fleet(&ok, ProviderKind::Claude, &session, &spec, false)
            .expect("a fleet handoff");
    assert!(launch.args.iter().all(|a| !a.contains("fdc1_")));
    let binding = ok.get("s-1:fleet").expect("the binding");
    assert!(binding.is_fleet_only());
    assert_eq!(
        binding.classes.iter().copied().collect::<Vec<_>>(),
        [crate::coordinator::ToolClass::Own]
    );
    // The deck never sees it: not in the projection, no lifecycle event.
    assert!(ok.projection().is_empty());
    let started = Event::SessionStarted {
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        provider_version: None,
        model: None,
        cwd: "/w".to_owned(),
        mode: None,
        capabilities: Box::new(fleetdeck_proto::Capabilities::minimal()),
        resumed: None,
        title: None,
        branch: None,
        repo: None,
        provider_refs: serde_json::json!({"session_id": "native-1"})
            .as_object()
            .unwrap()
            .clone(),
    };
    assert!(coordinator_start::bound_events(&ok, &session, &[started]).is_empty());
    assert_eq!(ok.get("s-1:fleet").unwrap().state, BindingState::Active);
    assert!(coordinator_start::ended_events(&ok, &session).is_empty());
    assert_eq!(ok.get("s-1:fleet").unwrap().state, BindingState::Revoked);
    // Without the handoff, the start goes on without the tools.
    let other: SessionId = "s-2".into();
    let unverified = Bindings::default();
    unverified.set_endpoint(URL.to_owned());
    let no_endpoint = Bindings::default().with_handoff(verified);
    for (bindings, resumed) in [(&unverified, false), (&no_endpoint, false), (&ok, true)] {
        assert!(
            coordinator_start::prepare_fleet(
                bindings,
                ProviderKind::Claude,
                &other,
                &spec,
                resumed
            )
            .is_none()
        );
        assert!(bindings.get("s-2:fleet").is_none());
    }
}

/// DL-165: a Codex session gets the same fleet-only tools, through
/// `mcp_servers.fleetdeck` with `bearer_token_env_var` (never the token on
/// the command line), with the default (real) handoff check and a probed
/// version that the live smoke verified.
#[test]
fn a_codex_session_without_a_grant_gets_the_fleet_tools_too() {
    let spec = SessionSpec {
        provider_instance: "codex".into(),
        cwd: "/w".to_owned(),
        mode: None,
        model: None,
        title: None,
        coordinator: None,
    };
    let session: SessionId = "s-3".into();
    let bindings = Bindings::default();
    bindings.set_endpoint(URL.to_owned());
    bindings.record_version(ProviderKind::Codex, "0.160.0".to_owned());
    let launch =
        coordinator_start::prepare_fleet(&bindings, ProviderKind::Codex, &session, &spec, false)
            .expect("a Codex fleet handoff");
    assert!(
        launch
            .args
            .iter()
            .any(|a| a.contains("mcp_servers.fleetdeck.bearer_token_env_var"))
    );
    assert!(launch.args.iter().all(|a| !a.contains("fdc1_")));
    assert!(
        bindings
            .get("s-3:fleet")
            .expect("the binding")
            .is_fleet_only()
    );
}
