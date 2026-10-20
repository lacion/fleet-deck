//! The station side of the default worker grant (MCP P4a §4, §6.1, §6.2;
//! tests 14, 15 and 17 of §9): admission, the skip at launch with its
//! durable outcome event, the reasons, and the kind that a shim reattach
//! keeps.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::time::Duration;

use fleetdeck_proto::command::{
    CommandOrigin, CoordinatorSpec, GrantKind, RETRY_WINDOW_MS, ReceiptState, SessionCommand,
    SessionSpec,
};
use fleetdeck_proto::event::{ProviderKind, RunOrigin, WorkerOutcome};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, EventBody, RejectCode, SessionId};
use fleetdeck_testkit::{Det, FakeAgent};

use super::coordinator_command::check_spec;
use super::coordinator_spec_tests::coordinator_spec;
use super::coordinator_start::Tools;
use super::worker_grant::{
    BRIDGE_UNVERIFIED, HANDOFF_UNVERIFIED, PREPARE_FAILED, RESUMED, SHIM_ROW_FAILED,
    WorkerAdmission, WorkerSkip, check_worker_spec, tools_for_start,
};
use super::*;
use crate::coordinator::{BindingState, Bindings, BridgeEndpoint};
use crate::store::{
    Caller, DecideError, NewSession, OfflineApprovalPolicy, ReadResult, StoreConfig,
};

const URL: &str = "http://127.0.0.1:41234/mcp";
const START: &str = "cmd-worker";

fn verified(_: ProviderKind, _: Option<&str>) -> Result<(), &'static str> {
    Ok(())
}

fn worker() -> CoordinatorSpec {
    CoordinatorSpec {
        kind: GrantKind::Worker,
        ..coordinator_spec()
    }
}

fn session_spec(coordinator: Option<CoordinatorSpec>) -> SessionSpec {
    SessionSpec {
        provider_instance: "claude".into(),
        cwd: "/w".to_owned(),
        mode: None,
        model: None,
        title: None,
        coordinator: coordinator.map(Box::new),
        launch: None,
        extra_env: None,
    }
}

/// Bindings with a verified handoff, the loopback endpoint, the bridge and
/// Claude 2.1.296 (the bridge-verified version).
fn with_bridge() -> Bindings {
    let bindings = Bindings::default().with_handoff(verified);
    bindings.record_version(ProviderKind::Claude, "2.1.296".to_owned());
    bindings.set_endpoint(URL.to_owned());
    bindings.set_bridge(BridgeEndpoint {
        program: "/station/bin/current".into(),
        socket: "/run/fleetdeck/mcp.s".into(),
    });
    bindings
}

fn command_id() -> Option<CommandId> {
    Some(CommandId::from(START.to_owned()))
}

fn code(r: Result<WorkerAdmission, DecideError>) -> RejectCode {
    match r {
        Err(DecideError::Reject(reject)) => reject.code,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

/// The `tools_for_start` of a worker start on a host.
fn worker_tools(
    bindings: &Bindings,
    provider: ProviderKind,
    coordinator: CoordinatorSpec,
    shim_host: bool,
    resumed: bool,
) -> (Tools, Option<WorkerSkip>) {
    tools_for_start(
        bindings,
        provider,
        &"s-1".into(),
        &session_spec(Some(coordinator)),
        shim_host,
        resumed,
        command_id(),
    )
    .unwrap()
}

fn reason(skip: &Option<WorkerSkip>) -> &'static str {
    skip.as_ref().expect("a skip").reason
}

#[test]
fn the_station_offers_the_worker_grant_feature() {
    assert!(feature::ALL.contains(&feature::WORKER_GRANT));
    assert_eq!(feature::WORKER_GRANT, "worker_grant.v1");
}

/// Test 14, the checks: a worker spec keeps every origin, import and
/// structure refusal of an owner spec; only the handoff refusal differs.
#[test]
fn a_worker_spec_from_the_deck_is_accepted_and_coordinator_origin_local_and_import_still_refused() {
    let spec = worker();
    let ok = Bindings::default().with_handoff(verified);
    let admit = |origin, import, spec: &CoordinatorSpec, b: &Bindings| {
        check_worker_spec(origin, ProviderKind::Claude, spec, import, b)
    };
    assert_eq!(
        admit(RunOrigin::Board, false, &spec, &ok).unwrap(),
        WorkerAdmission::Keep
    );
    for (origin, import, want) in [
        (RunOrigin::Coordinator, false, RejectCode::Forbidden),
        (RunOrigin::Mail, false, RejectCode::Forbidden),
        (RunOrigin::Local, false, RejectCode::InvalidCommand),
        (RunOrigin::Board, true, RejectCode::InvalidCommand),
    ] {
        assert_eq!(
            code(admit(origin, import, &spec, &ok)),
            want,
            "{origin:?} import={import}"
        );
    }
    let mut empty = worker();
    empty.classes.clear();
    assert_eq!(
        code(admit(RunOrigin::Board, false, &empty, &ok)),
        RejectCode::InvalidCommand
    );
    let mut unknown = worker();
    unknown.classes = vec!["bogus".to_owned()];
    assert_eq!(
        code(admit(RunOrigin::Board, false, &unknown, &ok)),
        RejectCode::InvalidCommand
    );
    // The handoff refusal is a skip for a worker, a refusal for an owner.
    let none = Bindings::default();
    assert_eq!(
        admit(RunOrigin::Board, false, &spec, &none).unwrap(),
        WorkerAdmission::Skip {
            reason: HANDOFF_UNVERIFIED
        }
    );
    let owner = coordinator_spec();
    assert!(matches!(
        check_spec(RunOrigin::Board, ProviderKind::Claude, &owner, false, &none),
        Err(DecideError::Reject(r)) if r.code == RejectCode::Unsupported
    ));
}

/// The outcome event names the grant, the start command and the reason.
#[test]
fn a_skip_without_a_start_command_appends_no_event() {
    let mut skip = WorkerSkip {
        grant_id: "grant-1".to_owned(),
        start_command_id: command_id(),
        reason: RESUMED,
    };
    assert!(matches!(
        skip.event().unwrap().event,
        EventBody::Known(Event::CoordinatorWorkerOutcome {
            ref grant_id,
            ref start_command_id,
            outcome: WorkerOutcome::Skipped,
            ref reason,
        }) if grant_id == "grant-1" && start_command_id.as_str() == START && reason == "resumed"
    ));
    skip.start_command_id = None;
    assert!(skip.event().is_none());
}

/// Test 15, the direct host: a worker grant is prepared when the handoff is
/// verified; otherwise the start goes on, with the fleet-only tools only
/// when the fleet handoff is verified.
#[test]
fn an_unverified_provider_or_bridge_skips_the_worker_grant_at_launch_on_the_direct_host() {
    // Verified: the grant binding, kind worker, no skip.
    let bindings = with_bridge();
    let (tools, skip) = worker_tools(&bindings, ProviderKind::Claude, worker(), false, false);
    assert!(matches!(tools, Tools::Direct(_)));
    assert!(skip.is_none());
    let binding = bindings.find(&"s-1".into(), "grant-1").unwrap();
    assert_eq!(binding.kind, GrantKind::Worker);
    assert_eq!(binding.state, BindingState::Pending);

    // An unverified provider version: no tools at all.
    let bindings = Bindings::default();
    bindings.set_endpoint(URL.to_owned());
    let (tools, skip) = worker_tools(&bindings, ProviderKind::Claude, worker(), false, false);
    assert!(matches!(tools, Tools::None));
    assert_eq!(reason(&skip), HANDOFF_UNVERIFIED);
    assert_eq!(skip.unwrap().start_command_id, command_id());
    assert!(bindings.get("s-1:grant-1").is_none());
    assert!(bindings.get("s-1:fleet").is_none());

    // A verified handoff without a loopback endpoint: no tools either.
    let bindings = Bindings::default().with_handoff(verified);
    let (tools, skip) = worker_tools(&bindings, ProviderKind::Claude, worker(), false, false);
    assert!(matches!(tools, Tools::None));
    assert_eq!(reason(&skip), PREPARE_FAILED);

    // A grant that cannot be prepared with a verified fleet handoff: the
    // fleet-only binding, no grant binding.
    let bindings = with_bridge();
    let mut bad = worker();
    bad.classes = vec!["bogus".to_owned()];
    let (tools, skip) = worker_tools(&bindings, ProviderKind::Claude, bad, false, false);
    assert!(matches!(tools, Tools::Direct(_)));
    assert_eq!(reason(&skip), PREPARE_FAILED);
    assert!(bindings.get("s-1:fleet").is_some());
    assert!(bindings.get("s-1:grant-1").is_none());
}

/// Test 15, a resume: the grant is dropped, the fleet-only tools stay.
#[test]
fn a_resumed_worker_start_drops_the_grant_and_keeps_the_fleet_tools() {
    for shim_host in [false, true] {
        if shim_host && !cfg!(target_os = "linux") {
            continue;
        }
        let bindings = with_bridge();
        let (tools, skip) =
            worker_tools(&bindings, ProviderKind::Claude, worker(), shim_host, true);
        assert_eq!(reason(&skip), RESUMED, "shim_host={shim_host}");
        match tools {
            Tools::Direct(_) => assert!(!shim_host),
            Tools::Bridge(start) => {
                assert!(shim_host);
                assert_eq!(start.grant_id, None);
                assert_eq!(start.binding.grant_id, "");
            }
            Tools::None => panic!("the fleet tools are expected"),
        }
        assert!(bindings.get("s-1:grant-1").is_none());
    }
    // An owner grant on a resume still fails the start.
    let err = tools_for_start(
        &with_bridge(),
        ProviderKind::Claude,
        &"s-1".into(),
        &session_spec(Some(coordinator_spec())),
        false,
        true,
        command_id(),
    )
    .err()
    .expect("a refusal");
    assert_eq!(
        err,
        "a coordinator session starts only as a new direct child"
    );
}

/// Test 15, the shim host.
#[cfg(target_os = "linux")]
#[test]
fn an_unverified_provider_or_bridge_skips_the_worker_grant_at_launch_on_the_shim_host() {
    // Verified: a bridge start that names the grant and the start command.
    let bindings = with_bridge();
    let (tools, skip) = worker_tools(&bindings, ProviderKind::Claude, worker(), true, false);
    let Tools::Bridge(start) = tools else {
        panic!("expected a bridge start");
    };
    assert!(skip.is_none());
    assert_eq!(start.grant_id.as_deref(), Some("grant-1"));
    assert_eq!(start.start_command_id, command_id());
    assert_eq!(start.binding.kind, GrantKind::Worker);
    assert!(
        bindings.get("s-1:grant-1").is_none(),
        "memory holds nothing"
    );

    // Codex has no verified bridge: no tools.
    let bindings = with_bridge();
    bindings.record_version(ProviderKind::Codex, "0.160.0".to_owned());
    let (tools, skip) = worker_tools(&bindings, ProviderKind::Codex, worker(), true, false);
    assert!(matches!(tools, Tools::None));
    assert_eq!(reason(&skip), BRIDGE_UNVERIFIED);

    // No bridge endpoint: no tools.
    let bindings = Bindings::default().with_handoff(verified);
    bindings.record_version(ProviderKind::Claude, "2.1.296".to_owned());
    bindings.set_endpoint(URL.to_owned());
    let (tools, skip) = worker_tools(&bindings, ProviderKind::Claude, worker(), true, false);
    assert!(matches!(tools, Tools::None));
    assert_eq!(reason(&skip), BRIDGE_UNVERIFIED);

    // An unverified handoff: no tools.
    let bindings = Bindings::default();
    let (tools, skip) = worker_tools(&bindings, ProviderKind::Claude, worker(), true, false);
    assert!(matches!(tools, Tools::None));
    assert_eq!(reason(&skip), HANDOFF_UNVERIFIED);

    // An owner grant on the shim host without a verified bridge still fails.
    let err = tools_for_start(
        &Bindings::default().with_handoff(verified),
        ProviderKind::Claude,
        &"s-1".into(),
        &session_spec(Some(coordinator_spec())),
        true,
        false,
        command_id(),
    )
    .err()
    .expect("a refusal");
    assert!(err.ends_with("needs a provider version whose bridge is verified"));
}

/// Test 15, the shim row: a binding row that cannot be stored starts the
/// session without tools and appends `shim_row_failed`.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_failed_shim_row_save_starts_without_tools_and_appends_the_outcome() {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
        .create_session(NewSession {
            session_id: "s-1".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    store
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
    let bindings = with_bridge();
    let (tools, _) = worker_tools(&bindings, ProviderKind::Claude, worker(), true, false);
    let Tools::Bridge(start) = tools else {
        panic!("expected a bridge start");
    };
    let digest = start.digest.clone();
    let (secrets, stored) =
        super::coordinator_start::store_tools(&store, &bindings, "shim-1", Some(start)).await;
    assert!(secrets.is_none() && stored.is_none());
    assert!(!bindings.has_digest(&digest));
    let outcomes = outcomes(&store).await;
    assert_eq!(
        outcomes,
        vec![(
            "s-1".into(),
            "grant-1".to_owned(),
            START.to_owned(),
            SHIM_ROW_FAILED.to_owned()
        )]
    );
}

/// The outcome events of the store: session, grant, start command, reason.
async fn outcomes(store: &Store) -> Vec<(SessionId, String, String, String)> {
    let ReadResult::Events { events, .. } = store.read_after(0, 100_000).await.unwrap() else {
        panic!("snapshot required");
    };
    events
        .into_iter()
        .filter_map(|e| match e.event {
            EventBody::Known(Event::CoordinatorWorkerOutcome {
                grant_id,
                start_command_id,
                outcome: WorkerOutcome::Skipped,
                reason,
            }) => Some((
                e.session_id,
                grant_id,
                start_command_id.as_str().to_owned(),
                reason,
            )),
            _ => None,
        })
        .collect()
}

/// A manager with `bindings` and one fake Claude, and the decision of a
/// start from the deck.
struct Rig {
    det: Det,
    root: tempfile::TempDir,
    manager: SessionManager,
    policy: Policy,
    bindings: Bindings,
}

impl Rig {
    async fn new(bindings: Bindings) -> Self {
        let det = Det::new();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("projects/app")).unwrap();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        let policy = Policy::new(
            vec![root.path().join("projects")],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
        );
        let mut env = fleetdeck_core::ChildEnv::from_process();
        env.set(fleetdeck_testkit::fake::SCENARIO_ENV, "sim")
            .unwrap();
        let config = fleetdeck_providers::claude::ClaudeAdapterConfig::new(
            "claude".into(),
            FakeAgent::Claude.locate().unwrap(),
            env,
        );
        let adapter: std::sync::Arc<dyn fleetdeck_providers::ProviderAdapter> =
            std::sync::Arc::new(fleetdeck_providers::claude::ClaudeAdapter::new(
                config,
                std::sync::Arc::new(fleetdeck_providers::host::DirectChildHost::new()),
            ));
        let holders = Holders {
            coordinator: bindings.clone(),
            ..Holders::default()
        };
        let manager = SessionManager::start_with_holders(
            store,
            policy.clone(),
            holders,
            Adapters::from([("claude".into(), adapter)]),
            det.ids(),
            std::sync::Arc::new(crate::workspace::PrivatePaths::default()),
        )
        .await
        .unwrap();
        Self {
            det,
            root,
            manager,
            policy,
            bindings,
        }
    }

    /// The start from the deck (origin board) with `coordinator`.
    async fn start(&self, coordinator: Option<CoordinatorSpec>) -> SessionId {
        let mut spec = session_spec(coordinator);
        spec.cwd = self
            .root
            .path()
            .join("projects/app")
            .to_string_lossy()
            .into_owned();
        spec.mode = Some("default".to_owned());
        let command = SessionCommand::Start {
            spec,
            prompt: Some(super::tests::text("hello")),
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
            fleet_git: None,
        };
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(START.to_owned()),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command: command.clone(),
        };
        let policy = self.policy.clone();
        let bindings = self.bindings.clone();
        let decided = self
            .manager
            .store()
            .decide(envelope, Caller::deck("k"), move |d| {
                decide::decide(d, &policy, &bindings, CommandOrigin::Board, false, command)
            })
            .await
            .unwrap();
        assert_eq!(
            decided.receipt.state,
            ReceiptState::Accepted,
            "{:?}",
            decided.receipt
        );
        decided.receipt.result.unwrap().session_id.unwrap()
    }

    async fn events(&self) -> Vec<Event> {
        let ReadResult::Events { events, .. } =
            self.manager.store().read_after(0, 100_000).await.unwrap()
        else {
            panic!("snapshot required");
        };
        events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(event) => Some(event),
                EventBody::Unknown { .. } => None,
            })
            .collect()
    }

    /// Waits until `pick` finds an event.
    async fn wait<T>(&self, what: &str, pick: impl Fn(&Event) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(found) = self.events().await.iter().find_map(&pick) {
                return found;
            }
            assert!(tokio::time::Instant::now() < deadline, "no {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// Test 15, the admission skip through the whole manager: an unverified
/// handoff drops the worker spec, the start succeeds, the stored effect has
/// no grant and the outcome event follows the start receipt.
#[tokio::test]
async fn an_unverified_handoff_skips_the_worker_grant_at_admission() {
    let rig = Rig::new(Bindings::default()).await;
    let session = rig.start(Some(worker())).await;
    let effects = rig
        .manager
        .store()
        .session_effects(session.clone())
        .await
        .unwrap();
    let crate::store::EffectPayload::Start { spec, .. } = &effects[0].payload else {
        panic!("a start effect");
    };
    assert!(spec.coordinator.is_none(), "the worker spec is dropped");
    rig.wait("session.started", |e| {
        matches!(e, Event::SessionStarted { .. }).then_some(())
    })
    .await;
    let found = outcomes(rig.manager.store()).await;
    assert_eq!(
        found,
        vec![(
            session.clone(),
            "grant-1".to_owned(),
            START.to_owned(),
            HANDOFF_UNVERIFIED.to_owned()
        )]
    );
    assert!(rig.bindings.find(&session, "grant-1").is_none());
    rig.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

/// Test 15, the launch skip through the whole manager: the handoff is
/// verified at admission but the endpoint does not listen at launch; the
/// session still starts and the outcome event says `prepare_failed`.
#[tokio::test]
async fn a_launch_that_cannot_prepare_the_worker_grant_starts_the_session_and_appends_the_outcome()
{
    let rig = Rig::new(Bindings::default().with_handoff(verified)).await;
    let session = rig.start(Some(worker())).await;
    rig.wait("session.started", |e| {
        matches!(e, Event::SessionStarted { .. }).then_some(())
    })
    .await;
    let reason = rig
        .wait("coordinator.worker_outcome", |e| match e {
            Event::CoordinatorWorkerOutcome {
                grant_id, reason, ..
            } if grant_id == "grant-1" => Some(reason.clone()),
            _ => None,
        })
        .await;
    assert_eq!(reason, PREPARE_FAILED);
    assert_eq!(outcomes(rig.manager.store()).await.len(), 1);
    assert!(rig.bindings.find(&session, "grant-1").is_none());
    rig.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

/// A used worker grant has no outcome event and its binding is a worker one.
#[tokio::test]
async fn a_prepared_worker_grant_appends_no_outcome_event() {
    let bindings = Bindings::default().with_handoff(verified);
    bindings.set_endpoint(URL.to_owned());
    let rig = Rig::new(bindings).await;
    let session = rig.start(Some(worker())).await;
    rig.wait("coordinator.bound", |e| {
        matches!(e, Event::CoordinatorBound { .. }).then_some(())
    })
    .await;
    let binding = rig.bindings.find(&session, "grant-1").unwrap();
    assert_eq!(binding.kind, GrantKind::Worker);
    assert!(outcomes(rig.manager.store()).await.is_empty());
    rig.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

/// Rule 3 (§4): a worker binding is not a coordinator that mutates, but it
/// is still bound.
#[test]
fn rule_3_ignores_a_worker_binding_and_keeps_an_owner_binding() {
    let bindings = Bindings::default().with_handoff(verified);
    bindings.set_endpoint(URL.to_owned());
    let session: SessionId = "s-1".into();
    let mut mail = worker();
    mail.classes = vec!["mail".to_owned()];
    let (tools, _) = worker_tools(&bindings, ProviderKind::Claude, mail, false, false);
    assert!(matches!(tools, Tools::Direct(_)));
    bindings.update(&format!("{session}:grant-1"), |b| {
        b.state = BindingState::Active;
    });
    assert!(!bindings.holds_mutating(&session));
    assert!(bindings.holds_grant(&session));
    let other: SessionId = "s-2".into();
    let mut owner = coordinator_spec();
    owner.classes = vec!["mail".to_owned()];
    super::coordinator_start::prepare(
        &bindings,
        ProviderKind::Claude,
        &other,
        &session_spec(Some(owner.clone())),
        &owner,
        false,
    )
    .unwrap();
    bindings.update(&format!("{other}:grant-1"), |b| {
        b.state = BindingState::Active;
    });
    assert!(bindings.holds_mutating(&other));
}

/// Test 17: the kind is in the row, so a reattach keeps it.
#[tokio::test]
async fn a_shim_reattach_keeps_the_worker_kind() {
    use crate::store::shim_bindings::BindingRow;

    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
        .create_session(NewSession {
            session_id: "s-1".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    // The binding that a worker bridge start builds, and an owner one.
    let bindings = with_bridge();
    let (tools, _) = worker_tools(&bindings, ProviderKind::Claude, worker(), true, false);
    let Tools::Bridge(start) = tools else {
        panic!("expected a bridge start");
    };
    // Another grant id: a binding replaces the one with the same binding id.
    let owner_spec = CoordinatorSpec {
        grant_id: "grant-2".to_owned(),
        ..coordinator_spec()
    };
    let owner = super::coordinator_start::prepare_bridge(
        &bindings,
        ProviderKind::Claude,
        &"s-1".into(),
        &session_spec(Some(owner_spec.clone())),
        &owner_spec,
    )
    .unwrap();
    for (shim, start) in [("shim-w", &start), ("shim-o", &owner)] {
        let row = BindingRow::from_binding(
            start.digest.clone(),
            shim.to_owned(),
            &start.binding,
            start.expires_at,
            1,
        );
        store.shim_binding_insert(row).await.unwrap();
    }
    // The rows read back with their kind and give the bindings back.
    let rows = store.shim_bindings_load().await.unwrap();
    let kind_of = |digest: &str| {
        rows.iter()
            .find(|r| r.digest == digest)
            .unwrap()
            .to_binding()
            .unwrap()
            .kind
    };
    assert_eq!(kind_of(&start.digest), GrantKind::Worker);
    assert_eq!(kind_of(&owner.digest), GrantKind::Coordinator);
    // The start order of the station loads them into memory the same way.
    let restarted = Bindings::default();
    let run_dir = tempfile::tempdir().unwrap();
    super::binding_start::prepare(&store, &restarted, run_dir.path().join("run"))
        .await
        .unwrap();
    assert_eq!(
        restarted.get_digest(&start.digest).unwrap().kind,
        GrantKind::Worker
    );
    assert_eq!(
        restarted.get_digest(&owner.digest).unwrap().kind,
        GrantKind::Coordinator
    );
}
