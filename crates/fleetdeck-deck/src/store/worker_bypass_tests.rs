//! B27 T4 tests 1 to 9 and 11: the bypass start of a worker grant, the
//! dispatch rule, the concrete mode of a start without a mode and `meet`.
//! (Test 10, over the store API with the station info, is in
//! `tests/worker_bypass.rs`.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_core::config_worker_grant::WorkerGrantConfig;
use fleetdeck_core::identity::Identity;
use fleetdeck_proto::capability::Capabilities;
use fleetdeck_proto::command::{RETRY_WINDOW_MS, SessionCommand, SessionSpec, UserInput};
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::link::{Scope, StationInfo};
use fleetdeck_proto::mutation::{
    CoordinatorBinding, CoordinatorCall, CoordinatorCallEnvelope, CoordinatorOp,
};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandId, EventEnvelope, Receipt, RejectCode, SessionSummary, StationId};
use fleetdeck_testkit::Det;
use serde_json::json;

use super::{launch_refusal, meet};
use crate::knobs::DeckKnobs;
use crate::link::CommandKind;
use crate::store::DeckStore;
use crate::store::grants::{Grant, GrantClass, NewGrant};
use crate::store::stations::NewStation;
use crate::store::worker_grant::{Provenance, WorkerStart};

const BYPASS: &str = "bypassPermissions";
const CODEX_BYPASS: &str = "never/danger-full-access";
const CLAUDE_ALL: [&str; 6] = ["default", "plan", "acceptEdits", "dontAsk", "auto", BYPASS];
const CODEX_ALL: [&str; 4] = [
    "untrusted/workspace-write",
    "on-request/workspace-write",
    "on-request/danger-full-access",
    CODEX_BYPASS,
];

/// What a test world offers: the modes and the default of each provider, and
/// the instance and the mode of the caller session `worker-1`.
struct Spec {
    claude_modes: Vec<&'static str>,
    claude_default: Option<&'static str>,
    codex_modes: Vec<&'static str>,
    codex_default: Option<&'static str>,
    caller: (&'static str, Option<&'static str>),
}

impl Spec {
    fn new() -> Self {
        Self {
            claude_modes: CLAUDE_ALL.to_vec(),
            claude_default: None,
            codex_modes: CODEX_ALL.to_vec(),
            codex_default: None,
            caller: ("claude", Some(BYPASS)),
        }
    }
}

struct World {
    s: DeckStore,
    a: StationId,
}

fn info(spec: &Spec) -> StationInfo {
    let provider = |instance: &str, modes: &[&str], default: Option<&str>| {
        let mut caps = Capabilities::minimal();
        caps.modes = modes.iter().map(|m| (*m).to_owned()).collect();
        json!({"instance": instance, "provider": instance, "version": "1",
            "auth": "signed_in", "capabilities": caps, "default_mode": default})
    };
    serde_json::from_value(json!({"name": "alpha", "incarnation": "inc", "head": 0,
        "live_sessions": 3, "providers": [
            provider("claude", &spec.claude_modes, spec.claude_default),
            provider("codex", &spec.codex_modes, spec.codex_default)]}))
    .unwrap()
}

fn knobs(worker_bypass: bool) -> DeckKnobs {
    DeckKnobs {
        worker_bypass,
        ..DeckKnobs::default()
    }
}

async fn world(spec: &Spec, worker_bypass: bool) -> World {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let key = Identity::generate().unwrap().public_key();
    let new = NewStation {
        key,
        name: "alpha".to_owned(),
        scopes: BTreeSet::from([Scope::Read, Scope::Coordinate]),
        dial_url: None,
    };
    let a = s.pin_station(new).await.unwrap().station_id;
    let mut card = SessionSummary::new("worker-1".into());
    card.provider_instance = Some(spec.caller.0.into());
    card.mode = spec.caller.1.map(str::to_owned);
    let target = |id: &str, mode: &str| {
        let mut card = SessionSummary::new(id.into());
        card.provider_instance = Some("claude".into());
        card.mode = Some(mode.to_owned());
        card
    };
    let cards = vec![card, target("t-1", "default"), target("t-bypass", BYPASS)];
    s.apply_snapshot(a.clone(), "inc".to_owned().into(), 0, cards)
        .await
        .unwrap();
    s.set_reachable(&a, true);
    s.set_station_info(&a, info(spec));
    let features = [
        feature::WORKER_GRANT,
        feature::COORDINATOR,
        feature::COORDINATOR_V2,
    ];
    s.set_link_features(&a, 1, features.map(str::to_owned).to_vec());
    s.set_fleet_control(true);
    s.set_knobs(knobs(worker_bypass));
    World { s, a }
}

fn setting(claude: &str, codex: &str) -> WorkerGrantConfig {
    WorkerGrantConfig {
        claude_mode: claude.to_owned(),
        codex_mode: codex.to_owned(),
        spawn_cap: Some(5),
        allow_bypass: true,
        ..WorkerGrantConfig::default()
    }
}

fn start_command() -> SessionCommand {
    start_with(None)
}

fn start_with(mode: Option<&str>) -> SessionCommand {
    SessionCommand::Start {
        spec: spec_of("claude", mode),
        prompt: Some(UserInput::new("go")),
        workspace: None,
        setup_cmd: None,
        batch: None,
        issue: None,
        origin_text: false,
        fleet_git: None,
    }
}

fn spec_of(instance: &str, mode: Option<&str>) -> SessionSpec {
    SessionSpec {
        effort: None,
        provider_instance: instance.into(),
        cwd: "/w/app".to_owned(),
        mode: mode.map(str::to_owned),
        model: None,
        title: None,
        coordinator: None,
        launch: None,
        extra_env: None,
    }
}

/// A worker grant of the board start `id`, bound to the session `worker-1`.
async fn bound_worker(w: &World, cfg: &WorkerGrantConfig) -> (Grant, CoordinatorBinding) {
    let req = WorkerStart {
        station: w.a.clone(),
        command: start_command(),
        command_id: CommandId::from("c1"),
        provenance: Provenance::Owner,
        author: Some("owner".to_owned()),
        setting: cfg.clone(),
    };
    w.s.submit_start_with_worker_decision(req).await.unwrap();
    let grant = w.s.grants(10).await.unwrap().pop().expect("a grant");
    bind(w, grant, "claude").await
}

async fn bind(w: &World, grant: Grant, instance: &str) -> (Grant, CoordinatorBinding) {
    let event = Event::CoordinatorBound {
        binding_id: "b-worker-1".to_owned(),
        grant_id: grant.grant_id.clone(),
        generation: 1,
        provider_instance: instance.to_owned(),
        native_id: "native-worker-1".to_owned(),
    };
    let env = EventEnvelope {
        station_seq: 1,
        session_id: "worker-1".into(),
        session_seq: 1,
        at: w.s.now_ms(),
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(event),
    };
    w.s.apply_events(w.a.clone(), "inc".to_owned().into(), vec![env])
        .await
        .unwrap();
    let grant = w.s.grant(grant.grant_id).await.unwrap().unwrap();
    let binding = grant.binding.clone().expect("bound");
    (grant, binding)
}

async fn spawn(w: &World, b: &CoordinatorBinding, id: &str, spec: SessionSpec) -> Receipt {
    let op = CoordinatorOp::Spawn {
        station: w.a.clone(),
        spec,
        prompt: Some(UserInput::new("do")),
    };
    let envelope = CoordinatorCallEnvelope {
        command_id: CommandId::from(id),
        target_incarnation: w.s.incarnation().clone(),
        retry_until: w.s.now_ms() + RETRY_WINDOW_MS,
        call: CoordinatorCall {
            caller: "worker-1".into(),
            binding: b.clone(),
            idempotency_key: None,
            op,
        },
    };
    w.s.decide_coordinator(w.a.clone(), envelope).await.unwrap()
}

fn refused(r: &Receipt) -> String {
    let reject = r.reject.as_ref().expect("a refusal");
    assert_eq!(reject.code, RejectCode::Forbidden, "{r:?}");
    reject.message.clone()
}

/// The `spec.mode` of every start in the outbox.
async fn start_modes(w: &World) -> Vec<Option<String>> {
    w.s.outbox()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|i| match serde_json::from_value(i.payload) {
            Ok(CommandKind::Session {
                command: SessionCommand::Start { spec, .. },
            }) => Some(spec.mode),
            _ => None,
        })
        .collect()
}

async fn bypass_rows(w: &World) -> Vec<(Option<String>, String)> {
    w.s.audit_log(100)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.action == "coordinator.worker_bypass_start")
        .map(|r| (r.target, r.detail.unwrap_or_default()))
        .collect()
}

async fn spawn_used(w: &World, grant: &Grant) -> u32 {
    w.s.grant(grant.grant_id.clone())
        .await
        .unwrap()
        .unwrap()
        .spawn_used
}

#[tokio::test]
async fn a_worker_starts_a_bypass_session_when_every_switch_allows_it() {
    let w = world(&Spec::new(), true).await;
    let (grant, b) = bound_worker(&w, &setting(BYPASS, "on-request/workspace-write")).await;
    let r = spawn(&w, &b, "k1", spec_of("claude", Some(BYPASS))).await;
    assert!(r.reject.is_none(), "{r:?}");
    assert_eq!(start_modes(&w).await, vec![Some(BYPASS.to_owned())]);
    let rows = bypass_rows(&w).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.as_deref(), Some(w.a.as_str()));
    let detail = format!(
        "grant={};caller={}/worker-1;mode=claude:{BYPASS}",
        grant.grant_id, w.a
    );
    assert_eq!(rows[0].1, detail);
    assert_eq!(spawn_used(&w, &grant).await, 1);
}

#[tokio::test]
async fn each_missing_switch_refuses_the_bypass_start() {
    // An owner grant keeps the absolute rule.
    let w = world(&Spec::new(), true).await;
    let mut owner = NewGrant::new(w.a.clone(), "worker-1".into());
    owner.classes = BTreeSet::from([GrantClass::Read, GrantClass::Spawn]);
    owner.spawn_cap = 3;
    owner.max_modes = [("claude".to_owned(), "acceptEdits".to_owned())].into();
    let owner = w.s.create_grant(owner).await.unwrap();
    let (grant, b) = bind(&w, owner, "claude").await;
    let r = spawn(&w, &b, "k1", spec_of("claude", Some(BYPASS))).await;
    assert_eq!(
        refused(&r),
        "a coordinator never starts a session in a bypass mode"
    );
    assert_eq!(spawn_used(&w, &grant).await, 0);
    assert!(bypass_rows(&w).await.is_empty());

    let cfg = setting(BYPASS, "on-request/workspace-write");
    // The setting is off now (it was on at the creation).
    let w = world(&Spec::new(), true).await;
    let (grant, b) = bound_worker(&w, &cfg).await;
    w.s.set_knobs(knobs(false));
    let r = spawn(&w, &b, "k2", spec_of("claude", Some(BYPASS))).await;
    assert_eq!(
        refused(&r),
        "a worker can start a bypass session only while the deck allows bypass mode for workers"
    );
    assert_eq!(spawn_used(&w, &grant).await, 0);
    assert!(bypass_rows(&w).await.is_empty());

    // The ceiling is `auto`.
    let w = world(&Spec::new(), true).await;
    let (grant, b) = bound_worker(&w, &setting("auto", &cfg.codex_mode)).await;
    let r = spawn(&w, &b, "k3", spec_of("claude", Some(BYPASS))).await;
    assert_eq!(refused(&r), "the mode is above the grant's ceiling `auto`");
    assert_eq!(spawn_used(&w, &grant).await, 0);
    assert!(bypass_rows(&w).await.is_empty());

    // The station does not offer the mode.
    let mut spec = Spec::new();
    spec.claude_modes = CLAUDE_ALL[..5].to_vec();
    let w = world(&spec, true).await;
    let (grant, b) = bound_worker(&w, &cfg).await;
    let r = spawn(&w, &b, "k4", spec_of("claude", Some(BYPASS))).await;
    assert_eq!(refused(&r), "the station does not allow bypass permissions");
    assert_eq!(spawn_used(&w, &grant).await, 0);
    assert!(bypass_rows(&w).await.is_empty());

    // The caller runs in `acceptEdits`.
    let mut spec = Spec::new();
    spec.caller = ("claude", Some("acceptEdits"));
    let w = world(&spec, true).await;
    let (grant, b) = bound_worker(&w, &cfg).await;
    let r = spawn(&w, &b, "k5", spec_of("claude", Some(BYPASS))).await;
    assert_eq!(
        refused(&r),
        "a worker can start a bypass session only while it runs in a bypass mode itself"
    );
    assert_eq!(spawn_used(&w, &grant).await, 0);
    assert!(bypass_rows(&w).await.is_empty());
}

#[tokio::test]
async fn the_codex_bypass_follows_the_same_rules() {
    let mut spec = Spec::new();
    spec.caller = ("codex", Some(CODEX_BYPASS));
    let w = world(&spec, true).await;
    let (grant, b) = bound_worker(&w, &setting("acceptEdits", CODEX_BYPASS)).await;
    let r = spawn(&w, &b, "k1", spec_of("codex", Some(CODEX_BYPASS))).await;
    assert!(r.reject.is_none(), "{r:?}");
    assert_eq!(start_modes(&w).await, vec![Some(CODEX_BYPASS.to_owned())]);
    let rows = bypass_rows(&w).await;
    assert!(rows[0].1.ends_with(&format!("mode=codex:{CODEX_BYPASS}")));
    assert_eq!(spawn_used(&w, &grant).await, 1);
    // A Codex ceiling below the bypass mode refuses it.
    let w = world(&spec, true).await;
    let (_, b) = bound_worker(&w, &setting("acceptEdits", "on-request/workspace-write")).await;
    let r = spawn(&w, &b, "k2", spec_of("codex", Some(CODEX_BYPASS))).await;
    assert_eq!(
        refused(&r),
        "the mode is above the grant's ceiling `on-request/workspace-write`"
    );
    assert!(bypass_rows(&w).await.is_empty());
}

#[tokio::test]
async fn the_dispatch_check_applies_the_switch() {
    let w = world(&Spec::new(), true).await;
    let (grant, b) = bound_worker(&w, &setting(BYPASS, "on-request/workspace-write")).await;
    let r = spawn(&w, &b, "k1", spec_of("claude", Some(BYPASS))).await;
    assert!(r.reject.is_none(), "{r:?}");
    let payload = w.s.outbox().await.unwrap().pop().unwrap().payload;
    let CommandKind::Session { command } = serde_json::from_value(payload).unwrap() else {
        panic!("a session command");
    };
    let claude = Some("claude");
    // The switch is on: the exact bypass launch is valid.
    assert!(!launch_refusal(
        &grant,
        &command,
        (claude, Some(BYPASS)),
        true
    ));
    // The switch goes off: the start that is still in the outbox is refused.
    assert!(launch_refusal(
        &grant,
        &command,
        (claude, Some(BYPASS)),
        false
    ));
    // A resume in bypass is always refused.
    let resume = SessionCommand::Resume {
        session_id: "worker-1".into(),
    };
    assert!(launch_refusal(
        &grant,
        &resume,
        (claude, Some(BYPASS)),
        true
    ));
    // The same start with the launch mode `acceptEdits` (an L8 fallback).
    assert!(!launch_refusal(
        &grant,
        &command,
        (claude, Some("acceptEdits")),
        true
    ));
}

#[tokio::test]
async fn resume_and_set_mode_never_go_into_bypass() {
    let w = world(&Spec::new(), true).await;
    let mut owner = NewGrant::new(w.a.clone(), "worker-1".into());
    owner.classes = BTreeSet::from([GrantClass::Read, GrantClass::Manage]);
    owner.scope = fleetdeck_proto::command::GrantScope::Fleet;
    owner.max_modes = [("claude".to_owned(), "acceptEdits".to_owned())].into();
    let owner = w.s.create_grant(owner).await.unwrap();
    let (_, b) = bind(&w, owner, "claude").await;
    let call = |id: &str, op: CoordinatorOp| {
        let envelope = CoordinatorCallEnvelope {
            command_id: CommandId::from(id),
            target_incarnation: w.s.incarnation().clone(),
            retry_until: w.s.now_ms() + RETRY_WINDOW_MS,
            call: CoordinatorCall {
                caller: "worker-1".into(),
                binding: b.clone(),
                idempotency_key: None,
                op,
            },
        };
        w.s.decide_coordinator(w.a.clone(), envelope)
    };
    let text = "a coordinator never moves a session into a bypass mode";
    let resume = CoordinatorOp::Resume {
        station: w.a.clone(),
        session_id: "t-bypass".into(),
    };
    let r = call("k1", resume).await.unwrap();
    assert_eq!(refused(&r), text);
    let set = CoordinatorOp::SetMode {
        station: w.a.clone(),
        session_id: "t-1".into(),
        mode: BYPASS.to_owned(),
    };
    let r = call("k2", set).await.unwrap();
    assert_eq!(refused(&r), text);
}

/// The receipt and the start modes of one start without a mode on
/// `instance` by a worker with `setting`, on the station of `spec`.
async fn defaulted(
    spec: &Spec,
    setting: &WorkerGrantConfig,
    instance: &str,
) -> (Receipt, Vec<Option<String>>) {
    let w = world(spec, true).await;
    let (_, b) = bound_worker(&w, setting).await;
    let r = spawn(&w, &b, "k1", spec_of(instance, None)).await;
    let modes = start_modes(&w).await;
    (r, modes)
}

fn some(mode: &str) -> Vec<Option<String>> {
    vec![Some(mode.to_owned())]
}

#[tokio::test]
async fn a_start_without_a_mode_gets_the_station_default_within_the_ceiling() {
    let codex = "on-request/workspace-write";
    let mut spec = Spec::new();
    spec.caller = ("claude", Some("default"));
    // Default `acceptEdits`, ceiling `auto`: the default.
    spec.claude_default = Some("acceptEdits");
    let (r, modes) = defaulted(&spec, &setting("auto", codex), "claude").await;
    assert!(r.reject.is_none(), "{r:?}");
    assert_eq!(modes, some("acceptEdits"));
    // Default `auto`, ceiling `acceptEdits`: the ceiling.
    spec.claude_default = Some("auto");
    let (r, modes) = defaulted(&spec, &setting("acceptEdits", codex), "claude").await;
    assert!(r.reject.is_none(), "{r:?}");
    assert_eq!(modes, some("acceptEdits"));
    // Codex: the lower policy and the lower sandbox.
    spec.codex_default = Some("never/workspace-write");
    let cfg = setting("acceptEdits", "on-request/danger-full-access");
    let (r, modes) = defaulted(&spec, &cfg, "codex").await;
    assert!(r.reject.is_none(), "{r:?}");
    assert_eq!(modes, some("on-request/workspace-write"));
}

#[tokio::test]
async fn an_older_station_gets_the_provider_default_written() {
    let mut spec = Spec::new();
    spec.caller = ("claude", Some("default"));
    let cfg = setting("acceptEdits", "on-request/workspace-write");
    let (r, modes) = defaulted(&spec, &cfg, "claude").await;
    assert!(r.reject.is_none(), "{r:?}");
    assert_eq!(modes, some("default"));
    let (r, modes) = defaulted(&spec, &cfg, "codex").await;
    assert!(r.reject.is_none(), "{r:?}");
    assert_eq!(modes, some("untrusted/workspace-write"));
    // A Codex ceiling below the provider default refuses, as before.
    let cfg = setting("acceptEdits", "untrusted/read-only");
    let (r, modes) = defaulted(&spec, &cfg, "codex").await;
    assert_eq!(
        refused(&r),
        "the mode is above the grant's ceiling `untrusted/read-only`"
    );
    assert!(modes.is_empty());
}

#[tokio::test]
async fn a_bypass_station_default_is_never_used_silently() {
    let cfg = setting(BYPASS, "on-request/workspace-write");
    let mut spec = Spec::new();
    spec.claude_default = Some(BYPASS);
    spec.caller = ("claude", Some("acceptEdits"));
    let (r, modes) = defaulted(&spec, &cfg, "claude").await;
    assert_eq!(
        refused(&r),
        "this station's default mode is a bypass mode that your grant cannot use here: give a mode"
    );
    assert!(modes.is_empty());
    // Every switch on: a bypass start with its audit row.
    spec.caller = ("claude", Some(BYPASS));
    let w = world(&spec, true).await;
    let (_, b) = bound_worker(&w, &cfg).await;
    let r = spawn(&w, &b, "k1", spec_of("claude", None)).await;
    assert!(r.reject.is_none(), "{r:?}");
    assert_eq!(start_modes(&w).await, some(BYPASS));
    assert_eq!(bypass_rows(&w).await.len(), 1);
}

#[test]
fn meet_is_the_lower_mode() {
    let m = |f, a, b| meet(f, a, b);
    assert_eq!(
        m("claude", "acceptEdits", "auto").as_deref(),
        Some("acceptEdits")
    );
    assert_eq!(m("claude", "auto", "plan").as_deref(), Some("plan"));
    assert_eq!(m("claude", BYPASS, BYPASS).as_deref(), Some(BYPASS));
    assert_eq!(
        m(
            "codex",
            "never/workspace-write",
            "on-request/danger-full-access"
        )
        .as_deref(),
        Some("on-request/workspace-write")
    );
    assert_eq!(
        m("codex", "untrusted/danger-full-access", "never/read-only").as_deref(),
        Some("untrusted/read-only")
    );
    assert_eq!(m("claude", "nope", "auto"), None);
    assert_eq!(m("codex", "never", "never/read-only"), None);
    assert_eq!(m("other", "a", "a"), None);
}

#[tokio::test]
async fn an_upgrade_between_decision_and_dispatch_never_launches_bypass() {
    let mut spec = Spec::new();
    spec.caller = ("claude", Some("acceptEdits"));
    let w = world(&spec, true).await;
    let (grant, b) = bound_worker(&w, &setting(BYPASS, "on-request/workspace-write")).await;
    let r = spawn(&w, &b, "k1", spec_of("claude", None)).await;
    assert!(r.reject.is_none(), "{r:?}");
    assert_eq!(start_modes(&w).await, some("default"));
    let payload = w.s.outbox().await.unwrap().pop().unwrap().payload;
    let CommandKind::Session { command } = serde_json::from_value(payload).unwrap() else {
        panic!("a session command");
    };
    let claude = Some("claude");
    // A station that filled a bypass default: refused.
    assert!(launch_refusal(
        &grant,
        &command,
        (claude, Some(BYPASS)),
        true
    ));
    // A row of a deck before B27 (`mode: None`) and a row with another mode.
    let row = start_with;
    assert!(launch_refusal(
        &grant,
        &row(None),
        (claude, Some(BYPASS)),
        true
    ));
    assert!(launch_refusal(
        &grant,
        &row(Some("auto")),
        (claude, Some(BYPASS)),
        true
    ));
    assert!(!launch_refusal(
        &grant,
        &row(Some(BYPASS)),
        (claude, Some(BYPASS)),
        true
    ));
}
