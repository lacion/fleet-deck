//! B27 T4 test 10: a worker session in a bypass mode calls `spawn` without a
//! mode on a station whose default mode is `bypassPermissions` and that
//! offers it; the deck queues the start with that mode, writes the audit row
//! and counts the start. (The rules are in `src/store/worker_bypass_tests.rs`.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_core::config_worker_grant::WorkerGrantConfig;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::knobs::DeckKnobs;
use fleetdeck_deck::link::CommandKind;
use fleetdeck_deck::store::DeckStore;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_deck::store::worker_grant::{Provenance, WorkerStart};
use fleetdeck_proto::capability::Capabilities;
use fleetdeck_proto::command::{RETRY_WINDOW_MS, SessionCommand, SessionSpec, UserInput};
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::{CoordinatorCall, CoordinatorCallEnvelope, CoordinatorOp};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandId, EventEnvelope, SessionSummary};
use fleetdeck_testkit::Det;
use serde_json::json;

const BYPASS: &str = "bypassPermissions";

fn spec(mode: Option<&str>) -> SessionSpec {
    SessionSpec {
        effort: None,
        provider_instance: "claude".into(),
        cwd: "/w/app".to_owned(),
        mode: mode.map(str::to_owned),
        model: None,
        title: None,
        coordinator: None,
        launch: None,
        extra_env: None,
    }
}

#[tokio::test]
async fn a_bypass_worker_spawns_without_a_mode_on_a_bypass_default_station() {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let new = NewStation {
        key: Identity::generate().unwrap().public_key(),
        name: "alpha".to_owned(),
        scopes: BTreeSet::from([Scope::Read, Scope::Coordinate]),
        dial_url: None,
    };
    let a = s.pin_station(new).await.unwrap().station_id;
    let mut card = SessionSummary::new("worker-1".into());
    card.provider_instance = Some("claude".into());
    card.mode = Some(BYPASS.to_owned());
    s.apply_snapshot(a.clone(), "inc".to_owned().into(), 0, vec![card])
        .await
        .unwrap();
    s.set_reachable(&a, true);
    let mut caps = Capabilities::minimal();
    caps.modes = ["default", "acceptEdits", BYPASS]
        .map(str::to_owned)
        .to_vec();
    let info = serde_json::from_value(json!({"name": "alpha", "incarnation": "inc",
        "head": 0, "live_sessions": 1, "providers": [{"instance": "claude",
        "provider": "claude", "version": "1", "auth": "signed_in",
        "capabilities": caps, "default_mode": BYPASS}]}))
    .unwrap();
    s.set_station_info(&a, info);
    let features = [
        feature::WORKER_GRANT,
        feature::COORDINATOR,
        feature::COORDINATOR_V2,
    ];
    s.set_link_features(&a, 1, features.map(str::to_owned).to_vec());
    s.set_fleet_control(true);
    s.set_knobs(DeckKnobs {
        worker_bypass: true,
        ..DeckKnobs::default()
    });
    // The worker grant of an owner start, with the bypass ceiling.
    let setting = WorkerGrantConfig {
        claude_mode: BYPASS.to_owned(),
        allow_bypass: true,
        ..WorkerGrantConfig::default()
    };
    let request = WorkerStart {
        station: a.clone(),
        command: SessionCommand::Start {
            spec: spec(None),
            prompt: Some(UserInput::new("go")),
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
            fleet_git: None,
        },
        command_id: CommandId::from("c1"),
        provenance: Provenance::Owner,
        author: Some("owner".to_owned()),
        setting,
    };
    s.submit_start_with_worker_decision(request).await.unwrap();
    let grant = s.grants(10).await.unwrap().pop().expect("a grant");
    let bound = EventEnvelope {
        station_seq: 1,
        session_id: "worker-1".into(),
        session_seq: 1,
        at: s.now_ms(),
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::CoordinatorBound {
            binding_id: "b-worker-1".to_owned(),
            grant_id: grant.grant_id.clone(),
            generation: 1,
            provider_instance: "claude".to_owned(),
            native_id: "native-worker-1".to_owned(),
        }),
    };
    s.apply_events(a.clone(), "inc".to_owned().into(), vec![bound])
        .await
        .unwrap();
    let binding = s
        .grant(grant.grant_id.clone())
        .await
        .unwrap()
        .unwrap()
        .binding
        .expect("bound");
    let envelope = CoordinatorCallEnvelope {
        command_id: CommandId::from("k1"),
        target_incarnation: s.incarnation().clone(),
        retry_until: s.now_ms() + RETRY_WINDOW_MS,
        call: CoordinatorCall {
            caller: "worker-1".into(),
            binding,
            idempotency_key: None,
            op: CoordinatorOp::Spawn {
                station: a.clone(),
                spec: spec(None),
                prompt: Some(UserInput::new("do")),
            },
        },
    };
    let receipt = s.decide_coordinator(a.clone(), envelope).await.unwrap();
    assert!(receipt.reject.is_none(), "{receipt:?}");
    // The deck queued the start with the station default as a concrete mode.
    let modes: Vec<Option<String>> = s
        .outbox()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|i| match serde_json::from_value(i.payload) {
            Ok(CommandKind::Session {
                command: SessionCommand::Start { spec, .. },
            }) => Some(spec.mode),
            _ => None,
        })
        .collect();
    assert_eq!(modes, vec![Some(BYPASS.to_owned())]);
    let audit = s.audit_log(50).await.unwrap();
    let rows: Vec<_> = audit
        .iter()
        .filter(|r| r.action == "coordinator.worker_bypass_start")
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].result, fleetdeck_deck::store::AuditResult::Ok);
}
