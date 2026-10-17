//! ADR-008 §9 deck tests 1, 5, 6, 7 and 8b: the decision of the fleet-control
//! ops (the target rules of both scopes, the answers and their cap, the
//! modes, the projects) and the provenance of the outbox items. The tests of
//! the elevation, the version gate, the deck setting, the stored scope and
//! answer cap, the rechecks until the send and `coordinator.authority_check`
//! are in `tests/board_fleet_control.rs`; the tests of the fencing are in
//! `fleetdeck-proto` (`orchestration`).

#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::collections::{BTreeMap, BTreeSet};

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{
    GrantScope, RETRY_WINDOW_MS, SessionCommand, SessionSpec, UserInput,
};
use fleetdeck_proto::event::{ApprovalOption, OptionKind, ProviderKind, RequestKind};
use fleetdeck_proto::event::{ResponseChannel, SessionState};
use fleetdeck_proto::integrations::ProjectInput;
use fleetdeck_proto::link::{Scope, StationInfo};
use fleetdeck_proto::mutation::{
    CommandAuthority, CoordinatorBinding, CoordinatorCall, CoordinatorCallEnvelope,
    CoordinatorDelivery, CoordinatorOp, CoordinatorTarget, ProjectTarget,
};
use fleetdeck_proto::session::OpenRequest;
use fleetdeck_proto::share::{ShareKind, ShareRole, Steering};
use fleetdeck_proto::version::feature::COORDINATOR_V2;
use fleetdeck_proto::{CommandId, Receipt, RejectCode, SessionSummary, StationId};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

use crate::link::CommandKind;
use crate::store::grants::{GrantClass, NewGrant};
use crate::store::outbox::OutboxItem;
use crate::store::shares::NewShareRow;
use crate::store::stations::NewStation;
use crate::store::{AuditResult, DeckStore};

struct World {
    s: DeckStore,
    alpha: StationId,
    beta: StationId,
}

type Opts<'a> = &'a [(&'a str, Option<OptionKind>)];

fn req(id: &str, kind: RequestKind, opts: Opts) -> OpenRequest {
    let options = opts
        .iter()
        .map(|(o, kind)| ApprovalOption {
            option_id: (*o).to_owned(),
            label: (*o).to_owned(),
            kind: *kind,
        })
        .collect();
    OpenRequest {
        request_id: id.into(),
        node_id: None,
        request_kind: kind,
        title: "t".to_owned(),
        details: "d".to_owned(),
        input: None,
        options,
        response: ResponseChannel::RpcReply,
        requested_at: 1,
    }
}

fn summary(id: &str, instance: &str, mode: Option<&str>) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.cwd = Some("/w/app".to_owned());
    s.provider = Some(if instance == "codex" {
        ProviderKind::Codex
    } else {
        ProviderKind::Claude
    });
    s.provider_instance = Some(instance.into());
    s.mode = mode.map(str::to_owned);
    s.state = SessionState::Running;
    s
}

fn info(name: &str) -> StationInfo {
    let p = |i: &str| json!({"instance": i, "provider": i, "version": "1", "auth": "signed_in"});
    serde_json::from_value(json!({"name": name, "incarnation": "inc", "head": 0,
        "live_sessions": 2, "providers": [p("claude"), p("codex")]}))
    .unwrap()
}

async fn pin(s: &DeckStore, name: &str) -> StationId {
    let scopes = BTreeSet::from([Scope::Read, Scope::Coordinate]);
    let key = Identity::generate().unwrap().public_key();
    let new = NewStation {
        key,
        name: name.to_owned(),
        scopes,
        dial_url: None,
    };
    s.pin_station(new).await.unwrap().station_id
}

/// Station `alpha`: the coordinators `coord-1` and `coord-2`, the shared
/// session `sh`, the owner sessions `w1` (with open requests), `w-auto`,
/// `w-bypass`, `w-cdx*`. Station `beta`: `other-1`. Both stations negotiated
/// `coordinator.v2`, and fleet control is on.
async fn world() -> World {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let (alpha, beta) = (pin(&s, "alpha").await, pin(&s, "beta").await);
    let any = Some(OptionKind::AllowOnce);
    let mut w1 = summary("w1", "claude", Some("default"));
    w1.open_requests = vec![
        req(
            "r-ap",
            RequestKind::Command,
            &[
                ("once", any),
                ("session", Some(OptionKind::AllowSession)),
                ("always", Some(OptionKind::AllowAlways)),
                ("amend", Some(OptionKind::Amend)),
                ("other", Some(OptionKind::Other)),
                ("plain", None),
                ("no", Some(OptionKind::Deny)),
            ],
        ),
        req("r-q", RequestKind::Question, &[]),
        req("r-q2", RequestKind::Question, &[("a", None), ("b", None)]),
        req("r-ap2", RequestKind::Tool, &[("once", any)]),
    ];
    let mut alpha_sessions = vec![w1];
    for (id, instance, mode) in [
        ("coord-1", "claude", None),
        ("coord-2", "claude", None),
        ("sh", "claude", None),
        ("w-cdx", "codex", None),
        ("w-auto", "claude", Some("auto")),
        ("w-bypass", "claude", Some("bypassPermissions")),
        ("w-cdx-full", "codex", Some("never/danger-full-access")),
    ] {
        alpha_sessions.push(summary(id, instance, mode));
    }
    let beta_sessions = vec![summary("other-1", "claude", None)];
    for (st, sessions, token) in [(&alpha, alpha_sessions, 1), (&beta, beta_sessions, 2)] {
        s.apply_snapshot(st.clone(), "inc".to_owned().into(), 0, sessions)
            .await
            .unwrap();
        s.set_reachable(st, true);
        s.set_station_info(st, info(st.as_str()));
        s.set_link_features(st, token, vec![COORDINATOR_V2.to_owned()]);
    }
    s.set_fleet_control(true);
    let share = NewShareRow {
        kind: ShareKind::Session,
        target: format!("{alpha}/sh"),
        role: ShareRole::Collaborator,
        steering: Steering::Open,
        expires_in_days: Some(1),
        uses: Some(1),
        history: false,
    };
    s.create_share(share).await.unwrap();
    World { s, alpha, beta }
}

const ALL: [GrantClass; 7] = [
    GrantClass::Read,
    GrantClass::Mail,
    GrantClass::Drive,
    GrantClass::Spawn,
    GrantClass::Inspect,
    GrantClass::Manage,
    GrantClass::Answer,
];

/// A bound grant of `session` on `alpha`, with a Claude ceiling `acceptEdits`
/// and a Codex ceiling `on-request/workspace-write`.
async fn grant_of(
    w: &World,
    session: &str,
    classes: &[GrantClass],
    edit: impl FnOnce(&mut NewGrant),
) -> CoordinatorBinding {
    let mut g = NewGrant::new(w.alpha.clone(), session.into());
    g.classes = classes.iter().copied().collect();
    g.spawn_cap = 5;
    g.max_modes = BTreeMap::from([
        ("claude".to_owned(), "acceptEdits".to_owned()),
        ("codex".to_owned(), "on-request/workspace-write".to_owned()),
    ]);
    edit(&mut g);
    let stored = w.s.create_grant(g).await.unwrap();
    let b = CoordinatorBinding {
        binding_id: format!("b-{session}"),
        grant_id: stored.grant_id,
        generation: 1,
        provider_instance: "claude".into(),
        native_id: format!("native-{session}"),
    };
    assert!(w.s.bind_grant(b.clone()).await.unwrap());
    b
}

/// The fleet-scope grant of `coord-1`; `coord-2` holds a grant too.
async fn fleet(w: &World) -> CoordinatorBinding {
    grant_of(w, "coord-2", &[GrantClass::Mail], |_| {}).await;
    grant_of(w, "coord-1", &ALL, |g| g.scope = GrantScope::Fleet).await
}

async fn call(w: &World, b: &CoordinatorBinding, id: &str, op: CoordinatorOp) -> Receipt {
    let envelope = CoordinatorCallEnvelope {
        command_id: CommandId::from(id),
        target_incarnation: w.s.incarnation().clone(),
        retry_until: w.s.now_ms() + RETRY_WINDOW_MS,
        call: CoordinatorCall {
            caller: b.native_id.strip_prefix("native-").unwrap().into(),
            binding: b.clone(),
            idempotency_key: None,
            op,
        },
    };
    w.s.decide_coordinator(w.alpha.clone(), envelope)
        .await
        .unwrap()
}

fn code(r: &Receipt) -> Option<RejectCode> {
    r.reject.as_ref().map(|j| j.code)
}

fn msg(r: &Receipt) -> String {
    r.reject
        .as_ref()
        .map(|j| j.message.clone())
        .unwrap_or_default()
}

fn mail(w: &World, session: &str) -> CoordinatorOp {
    CoordinatorOp::Send {
        to: CoordinatorTarget {
            station: w.alpha.clone(),
            session_id: session.into(),
        },
        text: "hi".to_owned(),
        delivery: CoordinatorDelivery::FollowUp,
    }
}

fn answer(w: &World, request: &str, option: Option<&str>) -> CoordinatorOp {
    CoordinatorOp::Answer {
        station: w.alpha.clone(),
        session_id: "w1".into(),
        request_id: request.into(),
        option_id: option.map(str::to_owned),
        question_answers: None,
        text: None,
    }
}

/// Every strict op (drive, manage, answer) on `session` of `alpha`.
fn strict_ops(w: &World, session: &str) -> Vec<(&'static str, CoordinatorOp)> {
    let extras = [
        ("interrupt", json!({})),
        ("stop", json!({})),
        ("resume", json!({})),
        ("rename", json!({"title": "a"})),
        ("set_mode", json!({"mode": "plan"})),
        ("project_assign", json!({"target": {"kind": "none"}})),
        (
            "answer",
            json!({"request_id": "r-ap2", "option_id": "once"}),
        ),
    ];
    let make = |(name, extra): (&'static str, Value)| {
        let mut v = json!({"op": name, "station": w.alpha, "session_id": session});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        (name, serde_json::from_value(v).unwrap())
    };
    extras.into_iter().map(make).collect()
}

fn sent(items: &[OutboxItem]) -> Vec<SessionCommand> {
    let kinds = items
        .iter()
        .map(|i| serde_json::from_value(i.payload.clone()));
    kinds
        .filter_map(|k| match k {
            Ok(CommandKind::Session { command }) => Some(command),
            _ => None,
        })
        .collect()
}

/// ADR-008 §9 test 1: class present or absent, target on or off a grant
/// station, the caller, a coordinator, a shared session, no such session.
#[tokio::test]
async fn the_strict_ops_pass_or_break_the_four_target_rules_in_fleet_scope() {
    let w = world().await;
    let b = fleet(&w).await;
    for (name, op) in strict_ops(&w, "w1") {
        let r = call(&w, &b, &format!("ok-{name}"), op).await;
        assert!(code(&r).is_none(), "{name}: {r:?}");
    }
    for (session, expect) in [
        ("coord-1", RejectCode::Forbidden),
        ("coord-2", RejectCode::Forbidden),
        ("sh", RejectCode::Forbidden),
        ("ghost", RejectCode::UnknownSession),
    ] {
        for (name, op) in strict_ops(&w, session) {
            let r = call(&w, &b, &format!("{session}-{name}"), op).await;
            assert_eq!(code(&r), Some(expect), "{session} {name}: {r:?}");
        }
    }
    let stop = CoordinatorOp::Stop {
        station: w.beta.clone(),
        session_id: "other-1".into(),
    };
    assert_eq!(
        code(&call(&w, &b, "beta", stop).await),
        Some(RejectCode::Forbidden)
    );
    // Mail is not strict: it may reach the caller, a coordinator, a shared session.
    for to in ["coord-1", "coord-2", "sh"] {
        assert!(code(&call(&w, &b, &format!("m-{to}"), mail(&w, to)).await).is_none());
    }
    // A missing class is a stored forbidden naming the class.
    let few = grant_of(&w, "coord-1", &[GrantClass::Read, GrantClass::Mail], |g| {
        g.scope = GrantScope::Fleet;
    })
    .await;
    for (name, op) in strict_ops(&w, "w1") {
        let r = call(&w, &few, &format!("c-{name}"), op).await;
        assert_eq!(code(&r), Some(RejectCode::Forbidden), "{name}");
        assert!(msg(&r).contains("class"), "{}", msg(&r));
    }
}

#[tokio::test]
async fn a_lineage_scope_grant_needs_the_session_in_its_lineage_for_every_strict_op() {
    let w = world().await;
    let b = grant_of(&w, "coord-1", &ALL, |_| {}).await;
    for (name, op) in strict_ops(&w, "w1") {
        let r = call(&w, &b, &format!("l-{name}"), op).await;
        assert_eq!(code(&r), Some(RejectCode::Forbidden), "{name}");
    }
    let id = b.grant_id.clone();
    w.s.add_lineage(id, w.alpha.clone(), "w1".into(), "start-1".to_owned())
        .await
        .unwrap();
    for (name, op) in strict_ops(&w, "w1") {
        let r = call(&w, &b, &format!("m-{name}"), op).await;
        assert!(code(&r).is_none(), "{name}: {r:?}");
    }
}

#[tokio::test]
async fn the_deck_setting_and_the_v2_gate_refuse_a_fleet_control_call_and_only_that() {
    let w = world().await;
    let b = fleet(&w).await;
    w.s.set_fleet_control(false);
    let mut every = strict_ops(&w, "w1");
    every.push(("mail", mail(&w, "w1")));
    for (name, op) in every.clone() {
        let r = call(&w, &b, &format!("off-{name}"), op).await;
        assert_eq!(code(&r), Some(RejectCode::Forbidden), "{name}");
        assert!(msg(&r).starts_with("fleet_control_off:"), "{}", msg(&r));
    }
    // A live link without `coordinator.v2` makes the call `unsupported`.
    w.s.set_fleet_control(true);
    w.s.clear_link_features(&w.alpha, 1);
    w.s.set_link_features(&w.alpha, 3, vec![]);
    for (name, op) in every {
        let r = call(&w, &b, &format!("v1-{name}"), op).await;
        assert_eq!(code(&r), Some(RejectCode::Unsupported), "{name}: {r:?}");
    }
    // A station with no live link passes the gate: the send waits for it.
    w.s.clear_link_features(&w.alpha, 3);
    assert!(code(&call(&w, &b, "down", mail(&w, "w1")).await).is_none());
    // A grant without a fleet-control field is unaffected by the setting.
    w.s.set_fleet_control(false);
    let plain = grant_of(&w, "coord-1", &[GrantClass::Mail], |_| {}).await;
    assert!(code(&call(&w, &plain, "plain", mail(&w, "w1")).await).is_none());
}

/// ADR-008 §9 test 5.
#[tokio::test]
async fn an_answer_is_one_allow_or_deny_that_counts_once_and_is_attributed() {
    let w = world().await;
    let b = fleet(&w).await;
    let used = || async {
        w.s.grant(b.grant_id.clone())
            .await
            .unwrap()
            .unwrap()
            .answer_used
    };
    for (id, option, expect) in [
        ("session", Some("session"), RejectCode::Forbidden),
        ("always", Some("always"), RejectCode::Forbidden),
        ("amend", Some("amend"), RejectCode::Forbidden),
        ("other", Some("other"), RejectCode::Forbidden),
        ("plain", Some("plain"), RejectCode::Forbidden),
        ("zzz", Some("zzz"), RejectCode::InvalidCommand),
        ("none", None, RejectCode::InvalidCommand),
    ] {
        let r = call(&w, &b, &format!("x-{id}"), answer(&w, "r-ap", option)).await;
        assert_eq!(code(&r), Some(expect), "{id}: {r:?}");
    }
    let mut with_answers = answer(&w, "r-ap", Some("once"));
    if let CoordinatorOp::Answer {
        question_answers, ..
    } = &mut with_answers
    {
        *question_answers = Some(json!({"a": 1}));
    }
    let r = call(&w, &b, "x-qa", with_answers).await;
    assert_eq!(code(&r), Some(RejectCode::InvalidCommand));
    let r = call(&w, &b, "x-nope", answer(&w, "r-nope", Some("once"))).await;
    assert_eq!(code(&r), Some(RejectCode::UnknownRequest));
    assert_eq!(used().await, 0);
    let first = answer(&w, "r-ap", Some("once"));
    assert!(code(&call(&w, &b, "ok-once", first.clone()).await).is_none());
    let deny = answer(&w, "r-ap", Some("no"));
    assert!(code(&call(&w, &b, "ok-no", deny).await).is_none());
    assert_eq!(used().await, 2);
    let commands = sent(&w.s.outbox().await.unwrap());
    let SessionCommand::Answer {
        decision,
        request_id,
        ..
    } = &commands[0]
    else {
        panic!("{commands:?}");
    };
    let got = (decision.option_id.as_str(), request_id.as_str());
    assert_eq!(got, ("once", "r-ap"));
    assert_eq!(decision.answered_by.as_deref(), Some("coordinator:coord-1"));
    assert!(decision.updated_input.is_none());
    let audit = w.s.audit_log(100).await.unwrap();
    let row = audit
        .iter()
        .find(|a| a.command_id.as_ref().map(CommandId::as_str) == Some("ok-once"))
        .unwrap();
    assert_eq!(
        (row.action.as_str(), row.result),
        ("coordinator.answer", AuditResult::Ok)
    );
    assert_eq!(row.actor, format!("coordinator:{}/coord-1", w.alpha));
    let target = format!("{}/w1", w.alpha);
    assert_eq!(row.target.as_deref(), Some(target.as_str()));
    let detail = row.detail.clone().unwrap();
    assert!(
        detail.contains(&b.grant_id) && detail.contains("request=r-ap"),
        "{detail}"
    );
    assert!(detail.contains("kind=allow_once"), "{detail}");
    let refused = audit
        .iter()
        .any(|a| a.action == "coordinator.answer" && a.result == AuditResult::Refused);
    assert!(refused);
    // A replay adds neither a count nor an audit row.
    call(&w, &b, "ok-once", first).await;
    assert_eq!(used().await, 2);
    assert_eq!(w.s.audit_log(100).await.unwrap().len(), audit.len());
}

#[tokio::test]
async fn a_question_takes_answers_or_text_within_the_size_limits() {
    let w = world().await;
    let b = fleet(&w).await;
    let q = |id: &str, option: Option<&str>, qa: Option<Value>, text: Option<String>| {
        CoordinatorOp::Answer {
            station: w.alpha.clone(),
            session_id: "w1".into(),
            request_id: id.into(),
            option_id: option.map(str::to_owned),
            question_answers: qa,
            text,
        }
    };
    let r = call(&w, &b, "q1", q("r-q", None, Some(json!({"k": "v"})), None)).await;
    assert!(code(&r).is_none(), "{r:?}");
    let commands = sent(&w.s.outbox().await.unwrap());
    let SessionCommand::Answer { decision, .. } = &commands[0] else {
        panic!("{commands:?}");
    };
    assert_eq!(decision.option_id, "answer");
    assert_eq!(decision.updated_input, Some(json!({"k": "v"})));
    let both = q("r-q", None, Some(json!({})), Some("why".to_owned()));
    assert!(code(&call(&w, &b, "q2", both).await).is_none());
    let big = json!({"k": "x".repeat(17_000)});
    let cases = [
        ("empty", q("r-q", None, None, None)),
        ("text", q("r-q", None, None, Some("x".repeat(4097)))),
        ("big", q("r-q", None, Some(big), None)),
        (
            "reason",
            q("r-ap", Some("no"), None, Some("x".repeat(4097))),
        ),
    ];
    for (id, op) in cases {
        let r = call(&w, &b, id, op).await;
        assert_eq!(code(&r), Some(RejectCode::InvalidCommand), "{id}: {r:?}");
    }
    let named = q("r-q2", Some("a"), None, None);
    let r = call(&w, &b, "q3", named).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
}

#[tokio::test]
async fn a_closed_request_is_request_closed_and_an_unknown_one_is_unknown() {
    let w = world().await;
    let b = fleet(&w).await;
    // `r-old` was requested and resolved: only the timeline knows it.
    let envelope = json!({"station_seq": 1, "session_id": "w1", "session_seq": 1, "at": 1,
        "proto_version": 1, "raw_ref": null, "event": {"kind": "approval.requested",
        "request_id": "r-old", "node_id": null, "request_kind": "command", "title": "t",
        "details": "d", "input": null, "options": [], "response": "rpc_reply",
        "provider_refs": {}}});
    let (station, text) = (w.alpha.0.clone(), envelope.to_string());
    crate::store::call(&w.s.db(), move |conn| {
        conn.execute(
            "INSERT INTO timeline (station_id, station_seq, session_id, session_seq, kind, envelope)
             VALUES (?1, 1, 'w1', 1, 'approval.requested', ?2)",
            rusqlite::params![station, text],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let r = call(&w, &b, "closed", answer(&w, "r-old", Some("once"))).await;
    assert_eq!(code(&r), Some(RejectCode::RequestClosed));
}

/// ADR-008 §9 tests 5 (cap) and 8b.
#[tokio::test]
async fn the_answer_cap_is_a_stored_rejection_and_two_calls_never_take_the_last_slot() {
    let w = world().await;
    grant_of(&w, "coord-2", &[GrantClass::Mail], |_| {}).await;
    let cap = |g: &mut NewGrant| {
        g.scope = GrantScope::Fleet;
        g.answer_cap = Some(1);
    };
    let b = grant_of(&w, "coord-1", &ALL, cap).await;
    let one = call(&w, &b, "c1", answer(&w, "r-ap", Some("once")));
    let two = call(&w, &b, "c2", answer(&w, "r-ap2", Some("once")));
    let (r1, r2) = tokio::join!(one, two);
    let codes = [code(&r1), code(&r2)];
    assert_eq!(codes.iter().filter(|c| c.is_none()).count(), 1, "{codes:?}");
    assert!(
        codes.contains(&Some(RejectCode::AnswerCapReached)),
        "{codes:?}"
    );
    // A duplicate call id does not count twice; pruning keeps the counter.
    call(&w, &b, "c1", answer(&w, "r-ap", Some("once"))).await;
    call(&w, &b, "c2", answer(&w, "r-ap2", Some("once"))).await;
    w.s.prune_coordinator_receipts(i64::MAX).await.unwrap();
    assert_eq!(w.s.grant(b.grant_id).await.unwrap().unwrap().answer_used, 1);
}

/// ADR-008 §9 test 6.
#[tokio::test]
async fn modes_above_the_ceiling_or_in_bypass_are_refused_by_every_op() {
    let w = world().await;
    let b = fleet(&w).await;
    let set = |s: &str, mode: &str| CoordinatorOp::SetMode {
        station: w.alpha.clone(),
        session_id: s.into(),
        mode: mode.to_owned(),
    };
    let resume = |s: &str| CoordinatorOp::Resume {
        station: w.alpha.clone(),
        session_id: s.into(),
    };
    let spawn = |mode: &str| CoordinatorOp::Spawn {
        station: w.alpha.clone(),
        spec: SessionSpec {
            provider_instance: "claude".into(),
            cwd: "/w/app".to_owned(),
            mode: Some(mode.to_owned()),
            model: None,
            title: None,
            coordinator: None,
            launch: None,
            extra_env: None,
        },
        prompt: Some(UserInput::new("go")),
    };
    let cases = [
        (set("w1", "plan"), true),
        (set("w1", "auto"), false),
        (set("w1", "bypassPermissions"), false),
        (set("w1", "junk"), false),
        (set("w-cdx", "never/workspace-write"), false),
        (set("w-cdx", "on-request/workspace-write"), true),
        (set("w-cdx", "never/danger-full-access"), false),
        (resume("w1"), true),
        (resume("w-cdx"), true),
        (resume("w-auto"), false),
        (resume("w-bypass"), false),
        (resume("w-cdx-full"), false),
        (spawn("auto"), false),
        (spawn("bypassPermissions"), false),
    ];
    for (i, (op, ok)) in cases.into_iter().enumerate() {
        let r = call(&w, &b, &format!("m{i}"), op).await;
        assert_eq!(code(&r).is_none(), ok, "case {i}: {r:?}");
        assert!(
            ok || code(&r) == Some(RejectCode::Forbidden),
            "case {i}: {r:?}"
        );
    }
    // A bypass or full-access mode is refused although the ceiling allows
    // it; `never/workspace-write` stays allowed within its ceiling.
    let wide = grant_of(&w, "coord-1", &ALL, |g| {
        g.scope = GrantScope::Fleet;
        g.max_modes = BTreeMap::from([
            ("claude".to_owned(), "bypassPermissions".to_owned()),
            ("codex".to_owned(), "never/danger-full-access".to_owned()),
        ]);
    })
    .await;
    let cases = [
        (set("w1", "bypassPermissions"), false),
        (set("w-cdx", "never/danger-full-access"), false),
        (resume("w-bypass"), false),
        (resume("w-cdx-full"), false),
        (spawn("bypassPermissions"), false),
        (set("w-cdx", "never/workspace-write"), true),
        (set("w1", "auto"), true),
    ];
    for (i, (op, ok)) in cases.into_iter().enumerate() {
        let r = call(&w, &wide, &format!("wide{i}"), op).await;
        assert_eq!(code(&r).is_none(), ok, "wide case {i}: {r:?}");
    }
    // A provider default above the ceiling refuses a resume with no mode.
    let low = grant_of(&w, "coord-1", &ALL, |g| {
        g.scope = GrantScope::Fleet;
        g.max_modes = BTreeMap::from([("codex".to_owned(), "untrusted/read-only".to_owned())]);
    })
    .await;
    let r = call(&w, &low, "low", resume("w-cdx")).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
}

/// ADR-008 §9 test 7, and `rename`.
#[tokio::test]
async fn projects_start_under_the_spawn_cap_and_a_move_keeps_the_station() {
    let w = world().await;
    let b = fleet(&w).await;
    let project = |station: &StationId| {
        let input = ProjectInput {
            name: "p".to_owned(),
            station: station.0.clone(),
            path: "/w/app".to_owned(),
            forge: None,
            trackers: vec![],
            prompt_template: None,
            review_prompt: None,
        };
        w.s.create_project(station.0.clone(), input)
    };
    let pa = project(&w.alpha).await.unwrap().project_id;
    let pb = project(&w.beta).await.unwrap().project_id;
    let assign = |s: &str, target| CoordinatorOp::ProjectAssign {
        station: w.alpha.clone(),
        session_id: s.into(),
        target,
    };
    let key = (w.alpha.0.clone(), "w1".to_owned());
    let moves = [
        (
            "a1",
            ProjectTarget::Project { id: pa.clone() },
            Some(Some(pa.clone())),
        ),
        ("a2", ProjectTarget::None, Some(None)),
        ("a3", ProjectTarget::Auto, None),
    ];
    for (id, target, expect) in moves {
        assert!(code(&call(&w, &b, id, assign("w1", target)).await).is_none());
        let rows = w.s.project_assignments().await.unwrap();
        assert_eq!(rows.get(&key).cloned(), expect, "{id}");
    }
    let other = ProjectTarget::Project { id: pb.clone() };
    let r = call(&w, &b, "a4", assign("w1", other)).await;
    assert_eq!(code(&r), Some(RejectCode::InvalidCommand));
    assert!(msg(&r).starts_with("wrong_station:"), "{}", msg(&r));
    let nope = ProjectTarget::Project {
        id: "nope".to_owned(),
    };
    let r = call(&w, &b, "a5", assign("w1", nope)).await;
    let got = (code(&r), msg(&r));
    assert_eq!(
        got,
        (
            Some(RejectCode::InvalidCommand),
            "unknown project".to_owned()
        )
    );
    let r = call(&w, &b, "a6", assign("ghost", ProjectTarget::None)).await;
    assert_eq!(code(&r), Some(RejectCode::UnknownSession));
    // `start_in_project` counts against the spawn cap and joins the lineage.
    let start = |project: &str, worktree| CoordinatorOp::ProjectStart {
        project_id: project.to_owned(),
        prompt: "do it".to_owned(),
        provider_instance: None,
        worktree,
    };
    let capped = grant_of(&w, "coord-1", &ALL, |g| {
        g.scope = GrantScope::Fleet;
        g.spawn_cap = 1;
    })
    .await;
    assert!(code(&call(&w, &capped, "s1", start(&pa, true)).await).is_none());
    assert_eq!(w.s.coordinator_starts().await.unwrap(), 1);
    let r = call(&w, &capped, "s2", start(&pa, false)).await;
    assert_eq!(code(&r), Some(RejectCode::SpawnCapReached));
    let commands = sent(&w.s.outbox().await.unwrap());
    let SessionCommand::Start {
        spec,
        workspace,
        prompt,
        ..
    } = commands.last().unwrap()
    else {
        panic!("{commands:?}");
    };
    let got = (spec.provider_instance.as_str(), spec.cwd.as_str());
    assert_eq!(got, ("claude", "/w/app"));
    assert!(workspace.is_some() && prompt.is_some());
    for (id, project, expect) in [
        ("s3", pb.as_str(), RejectCode::Forbidden),
        ("s4", "nope", RejectCode::InvalidCommand),
    ] {
        let r = call(&w, &capped, id, start(project, false)).await;
        assert_eq!(code(&r), Some(expect), "{id}");
    }
    // `rename`: 1 to 80 characters after trimming, no clearing.
    let rename = |t: &str| CoordinatorOp::Rename {
        station: w.alpha.clone(),
        session_id: "w1".into(),
        title: t.to_owned(),
    };
    let long = "x".repeat(81);
    for (id, title, ok) in [
        ("t1", "  hi  ", true),
        ("t2", "   ", false),
        ("t3", &long, false),
    ] {
        assert_eq!(
            code(&call(&w, &capped, id, rename(title)).await).is_none(),
            ok,
            "{id}"
        );
    }
    let renamed = sent(&w.s.outbox().await.unwrap());
    let hi = |c: &SessionCommand| matches!(c, SessionCommand::Rename { title: Some(t), .. } if t == "hi");
    assert!(renamed.iter().any(hi));
}

/// ADR-008 §6.1: the authority, the call and the expiry of the outbox items.
#[tokio::test]
async fn an_intent_of_a_fleet_control_grant_carries_its_authority_and_expires_in_ten_minutes() {
    let w = world().await;
    let b = fleet(&w).await;
    let stop = strict_ops(&w, "w1").remove(1).1;
    assert!(code(&call(&w, &b, "p1", stop).await).is_none());
    let item = w.s.outbox().await.unwrap().remove(0);
    let authority = CommandAuthority {
        grant_id: b.grant_id.clone(),
        caller_station: w.alpha.clone(),
        caller_session: "coord-1".into(),
        generation: 1,
    };
    assert_eq!(item.authority, Some(authority));
    assert_eq!(item.call_id.as_deref(), Some("p1"));
    assert_eq!(item.expires_at - item.created_at, 10 * 60 * 1000);
    // Mail of a fleet-control grant is a 10-minute intent too, with authority.
    assert!(code(&call(&w, &b, "p2", mail(&w, "w1")).await).is_none());
    let all = w.s.outbox().await.unwrap();
    assert!(
        all.iter()
            .all(|i| i.authority.is_some() && i.expires_at - i.created_at == 600_000)
    );
    // A lineage grant without fleet control: 24 h, no authority, but the call.
    let w2 = world().await;
    let plain = grant_of(&w2, "coord-1", &[GrantClass::Mail], |_| {}).await;
    assert!(code(&call(&w2, &plain, "p3", mail(&w2, "w1")).await).is_none());
    let item = w2.s.outbox().await.unwrap().remove(0);
    assert_eq!(
        (item.authority, item.call_id.as_deref()),
        (None, Some("p3"))
    );
    assert_eq!(item.expires_at - item.created_at, 24 * 60 * 60 * 1000);
}
