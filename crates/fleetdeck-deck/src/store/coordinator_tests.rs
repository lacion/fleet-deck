//! The deck decision of `coordinator.call` with grants (M3 6-5, ADR-006
//! §8.3 tests 14–23).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{RETRY_WINDOW_MS, ReceiptState, SessionSpec, UserInput};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::{Scope, StationInfo, StationProvider};
use fleetdeck_proto::mutation::{
    AssignRequirements, CoordinatorBinding, CoordinatorCall, CoordinatorCallEnvelope,
    CoordinatorDelivery, CoordinatorOp, CoordinatorQuery, CoordinatorQueryEnvelope,
    CoordinatorTarget,
};
use fleetdeck_proto::orchestration::TaskState;
use fleetdeck_proto::{CommandId, Receipt, RejectCode, SessionId, SessionSummary, StationId};
use fleetdeck_testkit::Det;

use super::*;
use crate::store::coordinator_ops::mode_within;
use crate::store::grants::{GrantClass, GrantState, NewGrant};
use crate::store::stations::NewStation;

struct World {
    det: Det,
    s: DeckStore,
    alpha: StationId,
    beta: StationId,
}

async fn pin(s: &DeckStore, name: &str) -> StationId {
    s.pin_station(NewStation {
        key: Identity::generate().unwrap().public_key(),
        name: name.to_owned(),
        scopes: BTreeSet::from([Scope::Read, Scope::Coordinate]),
        dial_url: None,
    })
    .await
    .unwrap()
    .station_id
}

fn summary(id: &str, cwd: &str) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.cwd = Some(cwd.to_owned());
    s.provider = Some(ProviderKind::Claude);
    s
}

fn info(name: &str) -> StationInfo {
    let provider = |instance: &str, provider| StationProvider {
        instance: instance.to_owned().into(),
        provider,
        version: Some("1.0.0".to_owned()),
        auth: "signed_in".to_owned(),
        capabilities: None,
    };
    StationInfo {
        name: name.to_owned(),
        incarnation: "inc".to_owned().into(),
        head: 0,
        live_sessions: 2,
        version: None,
        max_sessions: Some(8),
        allow_shell: None,
        allow_setup_cmd: None,
        providers: vec![
            provider("claude", ProviderKind::Claude),
            provider("codex", ProviderKind::Codex),
        ],
    }
}

/// Station `alpha` with the coordinator `coord-1` and `worker-1` in
/// `/w/app`; station `beta` with `other-1`.
async fn world() -> World {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let alpha = pin(&s, "alpha").await;
    let beta = pin(&s, "beta").await;
    for (st, sessions) in [
        (
            &alpha,
            vec![
                summary("coord-1", "/w/coord"),
                summary("worker-1", "/w/app"),
            ],
        ),
        (&beta, vec![summary("other-1", "/w/app")]),
    ] {
        s.apply_snapshot(st.clone(), "inc".to_owned().into(), 0, sessions)
            .await
            .unwrap();
        s.set_reachable(st, true);
        s.set_station_info(st, info(st.as_str()));
    }
    World {
        det,
        s,
        alpha,
        beta,
    }
}

fn binding_of(grant_id: &str, generation: u32) -> CoordinatorBinding {
    CoordinatorBinding {
        binding_id: "b-1".to_owned(),
        grant_id: grant_id.to_owned(),
        generation,
        provider_instance: "claude".into(),
        native_id: "native-1".to_owned(),
    }
}

/// A bound grant of `coord-1` on `alpha`.
async fn grant(
    w: &World,
    classes: &[GrantClass],
    cap: u32,
    modes: &[(&str, &str)],
) -> CoordinatorBinding {
    grant_with(w, classes, cap, modes, |_| {}).await
}

async fn grant_with(
    w: &World,
    classes: &[GrantClass],
    cap: u32,
    modes: &[(&str, &str)],
    edit: impl FnOnce(&mut NewGrant),
) -> CoordinatorBinding {
    let mut g = NewGrant::new(w.alpha.clone(), "coord-1".into());
    g.classes = classes.iter().copied().collect();
    g.spawn_cap = cap;
    g.max_modes = modes
        .iter()
        .map(|(f, m)| ((*f).to_owned(), (*m).to_owned()))
        .collect::<BTreeMap<_, _>>();
    edit(&mut g);
    let stored = w.s.create_grant(g).await.unwrap();
    let b = binding_of(&stored.grant_id, 1);
    assert!(w.s.bind_grant(b.clone()).await.unwrap());
    b
}

fn env(w: &World, b: &CoordinatorBinding, id: &str, op: CoordinatorOp) -> CoordinatorCallEnvelope {
    CoordinatorCallEnvelope {
        command_id: CommandId::from(id),
        target_incarnation: w.s.incarnation().clone(),
        retry_until: w.s.now_ms() + RETRY_WINDOW_MS,
        call: CoordinatorCall {
            caller: "coord-1".into(),
            binding: b.clone(),
            idempotency_key: None,
            op,
        },
    }
}

fn target(station: &StationId, session: &str) -> CoordinatorTarget {
    CoordinatorTarget {
        station: station.clone(),
        session_id: session.into(),
    }
}

fn mail(to: CoordinatorTarget, text: &str) -> CoordinatorOp {
    CoordinatorOp::Send {
        to,
        text: text.to_owned(),
        delivery: CoordinatorDelivery::FollowUp,
    }
}

fn spawn(station: &StationId, instance: &str, mode: Option<&str>) -> CoordinatorOp {
    CoordinatorOp::Spawn {
        station: station.clone(),
        spec: SessionSpec {
            provider_instance: instance.into(),
            cwd: "/w/app".to_owned(),
            mode: mode.map(str::to_owned),
            model: None,
            title: None,
            coordinator: None,
        },
        prompt: Some(UserInput::new("go")),
    }
}

async fn decide(w: &World, e: CoordinatorCallEnvelope) -> Receipt {
    w.s.decide_coordinator(w.alpha.clone(), e).await.unwrap()
}

fn code(r: &Receipt) -> Option<RejectCode> {
    r.reject.as_ref().map(|j| j.code)
}

async fn spawn_used(w: &World, b: &CoordinatorBinding) -> u32 {
    w.s.grant(b.grant_id.clone())
        .await
        .unwrap()
        .unwrap()
        .spawn_used
}

#[tokio::test]
async fn a_revoked_or_expired_grant_is_forbidden_in_the_decision_transaction_and_not_stored() {
    let w = world().await;
    let mail_class = [GrantClass::Mail];
    let b = grant(&w, &mail_class, 0, &[]).await;
    assert!(
        w.s.revoke_grant(b.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    let r = decide(
        &w,
        env(&w, &b, "k1", mail(target(&w.alpha, "worker-1"), "hi")),
    )
    .await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden));

    // Past `expires_at`.
    let b = grant_with(&w, &mail_class, 0, &[], |g| g.ttl_ms = 1000).await;
    w.det.advance(Duration::from_secs(2));
    let r = decide(
        &w,
        env(&w, &b, "k2", mail(target(&w.alpha, "worker-1"), "hi")),
    )
    .await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden));

    // Idle past its TTL: forbidden, and the grant is `expired` (renewable).
    let b = grant_with(&w, &mail_class, 0, &[], |g| g.idle_ttl_ms = 1000).await;
    w.det.advance(Duration::from_secs(2));
    let r = decide(
        &w,
        env(&w, &b, "k3", mail(target(&w.alpha, "worker-1"), "hi")),
    )
    .await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden));
    let g = w.s.grant(b.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(g.state, GrantState::Expired);
    assert_eq!(w.s.coordinator_receipts().await.unwrap(), 0);
}

#[tokio::test]
async fn a_replay_after_a_revocation_is_forbidden_and_never_returns_the_old_acceptance() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Mail], 0, &[]).await;
    let e = env(&w, &b, "k1", mail(target(&w.alpha, "worker-1"), "hi"));
    let first = decide(&w, e.clone()).await;
    assert_eq!(first.state, ReceiptState::Accepted);
    assert_eq!(decide(&w, e.clone()).await, first);
    w.s.revoke_grant(b.grant_id.clone(), "owner".to_owned())
        .await
        .unwrap();
    let again = decide(&w, e).await;
    assert_eq!(code(&again), Some(RejectCode::Forbidden));
    assert_eq!(w.s.coordinator_receipts().await.unwrap(), 1);
}

#[tokio::test]
async fn a_stale_generation_a_foreign_native_id_or_another_binding_is_binding_stale_and_not_stored()
{
    let w = world().await;
    let b = grant(&w, &[GrantClass::Mail], 0, &[]).await;
    let op = || mail(target(&w.alpha, "worker-1"), "hi");
    let mut newer = b.clone();
    newer.generation = 2;
    let mut foreign = b.clone();
    foreign.native_id = "native-2".to_owned();
    let mut other = b.clone();
    other.binding_id = "b-2".to_owned();
    for (i, stale) in [newer, foreign, other].into_iter().enumerate() {
        let r = decide(&w, env(&w, &stale, &format!("k{i}"), op())).await;
        assert_eq!(code(&r), Some(RejectCode::BindingStale));
    }
    // A generation that is not higher than the stored one never binds.
    let mut lower = b.clone();
    lower.native_id = "native-3".to_owned();
    assert!(!w.s.bind_grant(lower).await.unwrap());
    assert_eq!(w.s.coordinator_receipts().await.unwrap(), 0);
}

#[tokio::test]
async fn a_caller_session_of_another_station_is_forbidden() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Mail], 0, &[]).await;
    // The grant is for `alpha/coord-1`; `beta` sends the same caller id.
    let e = env(&w, &b, "k1", mail(target(&w.alpha, "worker-1"), "hi"));
    let r = w.s.decide_coordinator(w.beta.clone(), e).await.unwrap();
    assert_eq!(code(&r), Some(RejectCode::Forbidden));
    assert_eq!(w.s.coordinator_receipts().await.unwrap(), 0);
}

#[tokio::test]
async fn a_target_outside_the_grant_stations_or_lineage_is_a_stored_forbidden() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Mail, GrantClass::Drive], 0, &[]).await;
    let r = decide(
        &w,
        env(&w, &b, "k1", mail(target(&w.beta, "other-1"), "hi")),
    )
    .await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden));
    let stop = || CoordinatorOp::Stop {
        station: w.alpha.clone(),
        session_id: "worker-1".into(),
    };
    let r = decide(&w, env(&w, &b, "k2", stop())).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden));
    assert_eq!(w.s.coordinator_receipts().await.unwrap(), 2);
    // In the lineage: accepted.
    w.s.add_lineage(
        b.grant_id.clone(),
        w.alpha.clone(),
        "worker-1".into(),
        "start-1".to_owned(),
    )
    .await
    .unwrap();
    let r = decide(&w, env(&w, &b, "k3", stop())).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    // A class that the grant does not have: a stored forbidden.
    let r = decide(&w, env(&w, &b, "k4", spawn(&w.alpha, "claude", None))).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden));
}

#[tokio::test]
async fn a_spawn_that_carries_a_grant_or_a_mode_above_max_mode_is_refused() {
    let w = world().await;
    let modes = [
        ("claude", "acceptEdits"),
        ("codex", "on-request/workspace-write"),
    ];
    let b = grant(&w, &[GrantClass::Spawn], 5, &modes).await;
    let above = [
        spawn(&w.alpha, "claude", Some("bypassPermissions")),
        spawn(&w.alpha, "codex", Some("never/workspace-write")),
        spawn(&w.alpha, "codex", Some("untrusted/danger-full-access")),
        spawn(&w.alpha, "claude", Some("unknown-mode")),
    ];
    for (i, op) in above.into_iter().enumerate() {
        let r = decide(&w, env(&w, &b, &format!("k{i}"), op)).await;
        assert_eq!(code(&r), Some(RejectCode::Forbidden), "{i}");
    }
    assert_eq!(spawn_used(&w, &b).await, 0);
    // A spec cannot carry a grant: the field does not exist and is dropped
    // at decode, so no grant reaches a station from a coordinator.
    let raw = serde_json::json!({"op": "spawn", "station": w.alpha, "prompt": null,
        "spec": {"provider_instance": "claude", "cwd": "/w/app", "mode": null,
                 "model": null, "title": null, "grant": {"classes": ["spawn"]}}});
    let op: CoordinatorOp = serde_json::from_value(raw).unwrap();
    assert!(!serde_json::to_string(&op).unwrap().contains("grant"));
    let r = decide(&w, env(&w, &b, "k9", op)).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    let r = decide(
        &w,
        env(
            &w,
            &b,
            "k10",
            spawn(&w.alpha, "codex", Some("on-request/read-only")),
        ),
    )
    .await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    assert_eq!(spawn_used(&w, &b).await, 2);
}

#[tokio::test]
async fn concurrent_spawn_and_assign_auto_never_exceed_the_cap() {
    let w = world().await;
    let classes = [GrantClass::Mail, GrantClass::Spawn];
    let b = grant(&w, &classes, 3, &[("claude", "default")]).await;
    let mut set = tokio::task::JoinSet::new();
    for i in 0..32 {
        let op = if i % 2 == 0 {
            spawn(&w.alpha, "claude", None)
        } else {
            CoordinatorOp::AssignAuto {
                task: "build it".to_owned(),
                requirements: AssignRequirements {
                    repo: Some("/w/new".to_owned()),
                    provider: None,
                },
            }
        };
        let (s, alpha, e) = (
            w.s.clone(),
            w.alpha.clone(),
            env(&w, &b, &format!("k{i}"), op),
        );
        set.spawn(async move { s.decide_coordinator(alpha, e).await.unwrap() });
    }
    let mut accepted = 0;
    while let Some(r) = set.join_next().await {
        let r = r.unwrap();
        if r.state == ReceiptState::Accepted {
            accepted += 1;
        } else {
            assert_eq!(code(&r), Some(RejectCode::SpawnCapReached), "{r:?}");
        }
    }
    assert_eq!(accepted, 3);
    assert_eq!(spawn_used(&w, &b).await, 3);
    assert_eq!(w.s.coordinator_starts().await.unwrap(), 3);
    // An assignment to an existing session leaves the counter.
    let op = CoordinatorOp::AssignAuto {
        task: "review it".to_owned(),
        requirements: AssignRequirements {
            repo: Some("/w/app".to_owned()),
            provider: None,
        },
    };
    let r = decide(&w, env(&w, &b, "k-existing", op)).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    assert_eq!(
        r.result.unwrap().session_id,
        Some(SessionId::from("worker-1".to_owned()))
    );
    assert_eq!(spawn_used(&w, &b).await, 3);
}

#[tokio::test]
async fn receipt_pruning_during_a_seven_day_grant_keeps_spawn_used() {
    let w = world().await;
    let b = grant_with(&w, &[GrantClass::Spawn], 1, &[("claude", "default")], |g| {
        g.ttl_ms = 7 * 24 * 60 * 60 * 1000;
        g.idle_ttl_ms = 24 * 60 * 60 * 1000;
    })
    .await;
    let r = decide(&w, env(&w, &b, "k1", spawn(&w.alpha, "claude", None))).await;
    assert_eq!(r.state, ReceiptState::Accepted);
    w.det.advance(Duration::from_secs(25 * 60 * 60 - 3600 * 2));
    let pruned = w.s.prune_coordinator_receipts(w.s.now_ms()).await.unwrap();
    assert_eq!(pruned, 1);
    let r = decide(&w, env(&w, &b, "k2", spawn(&w.alpha, "claude", None))).await;
    assert_eq!(code(&r), Some(RejectCode::SpawnCapReached));
    assert_eq!(spawn_used(&w, &b).await, 1);
}

#[tokio::test]
async fn a_spawn_for_a_provider_family_without_a_mode_ceiling_is_refused() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Spawn], 3, &[("claude", "default")]).await;
    let r = decide(&w, env(&w, &b, "k1", spawn(&w.alpha, "codex", None))).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden));
    // An instance that the station does not have.
    let r = decide(&w, env(&w, &b, "k2", spawn(&w.alpha, "gemini", None))).await;
    assert_eq!(code(&r), Some(RejectCode::UnknownProvider));
    assert_eq!(spawn_used(&w, &b).await, 0);
}

#[tokio::test]
async fn a_duplicate_spawn_returns_the_stored_receipt_and_counts_once() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Spawn], 3, &[("claude", "default")]).await;
    let e = env(&w, &b, "k1", spawn(&w.alpha, "claude", None));
    let first = decide(&w, e.clone()).await;
    assert_eq!(first.state, ReceiptState::Accepted);
    assert_eq!(decide(&w, e).await, first);
    assert_eq!(spawn_used(&w, &b).await, 1);
    assert_eq!(w.s.coordinator_starts().await.unwrap(), 1);
}

/// DL-148: a coordinator spawn never carries a setup command; the field is
/// absent from the queued start.
#[tokio::test]
async fn a_coordinator_spawn_carries_no_setup_command() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Spawn], 3, &[("claude", "default")]).await;
    let first = decide(&w, env(&w, &b, "k1", spawn(&w.alpha, "claude", None))).await;
    assert_eq!(first.state, ReceiptState::Accepted);
    let starts: Vec<_> =
        w.s.outbox()
            .await
            .unwrap()
            .into_iter()
            .filter(|i| i.origin == "coordinator")
            .collect();
    assert_eq!(starts.len(), 1);
    let payload = starts[0].payload.to_string();
    assert!(payload.contains(r#""op":"start""#), "{payload}");
    assert!(!payload.contains("setup_cmd"), "{payload}");
}

#[tokio::test]
async fn a_reused_command_id_with_another_payload_is_rejected() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Mail], 0, &[]).await;
    let first = decide(
        &w,
        env(&w, &b, "k1", mail(target(&w.alpha, "worker-1"), "a")),
    )
    .await;
    assert_eq!(first.state, ReceiptState::Accepted);
    let r = decide(
        &w,
        env(&w, &b, "k1", mail(target(&w.alpha, "worker-1"), "b")),
    )
    .await;
    assert_eq!(code(&r), Some(RejectCode::CommandIdReused));
    assert_eq!(w.s.coordinator_receipts().await.unwrap(), 1);
    assert_eq!(w.s.mails(10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_call_after_retry_until_is_expired_and_not_stored() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Mail], 0, &[]).await;
    let mut e = env(&w, &b, "k1", mail(target(&w.alpha, "worker-1"), "a"));
    e.retry_until = w.s.now_ms() - 10 * 60 * 1000;
    let r = decide(&w, e).await;
    assert_eq!(code(&r), Some(RejectCode::Expired));
    assert_eq!(w.s.coordinator_receipts().await.unwrap(), 0);
    assert!(w.s.mails(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_task_completes_only_for_its_assignee() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Mail], 0, &[]).await;
    let assign = |to: &str| CoordinatorOp::Assign {
        task: "check it".to_owned(),
        to: target(&w.alpha, to),
    };
    let r = decide(&w, env(&w, &b, "t-worker", assign("worker-1"))).await;
    assert_eq!(r.state, ReceiptState::Accepted);
    let complete = |id: &str| CoordinatorOp::CompleteTask {
        task_id: id.to_owned(),
        result: Some("ok".to_owned()),
        state: None,
        reason: None,
        question: None,
    };
    let r = decide(&w, env(&w, &b, "c1", complete("t-worker"))).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden));
    let r = decide(&w, env(&w, &b, "t-self", assign("coord-1"))).await;
    assert_eq!(r.state, ReceiptState::Accepted);
    let r = decide(&w, env(&w, &b, "c2", complete("t-self"))).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    let task = w.s.task("t-self".to_owned()).await.unwrap().unwrap();
    assert_eq!(task.state, TaskState::Done);
    assert_eq!(task.result.as_deref(), Some("ok"));
}

fn query(
    w: &World,
    b: &CoordinatorBinding,
    q: CoordinatorQuery,
    wait_ms: i64,
) -> CoordinatorQueryEnvelope {
    CoordinatorQueryEnvelope {
        query_id: "q-1".to_owned(),
        target_incarnation: w.s.incarnation().clone(),
        caller: "coord-1".into(),
        binding: b.clone(),
        deadline: w.s.now_ms() + wait_ms,
        query: q,
    }
}

async fn ask(w: &World, e: CoordinatorQueryEnvelope) -> Result<serde_json::Value, RejectCode> {
    w.s.coordinator_query(w.alpha.clone(), e)
        .await
        .unwrap()
        .map_err(|r| r.code)
}

#[tokio::test]
async fn an_expired_query_is_rejected_and_a_wait_ends_forbidden_when_the_grant_is_revoked() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Read, GrantClass::Mail], 0, &[]).await;
    let mut late = query(&w, &b, CoordinatorQuery::ListSessions, 0);
    late.deadline -= 1;
    assert_eq!(ask(&w, late).await, Err(RejectCode::Expired));
    let far = query(&w, &b, CoordinatorQuery::ListSessions, 31_000);
    assert_eq!(ask(&w, far).await, Err(RejectCode::InvalidCommand));
    // The list holds only the grant's stations.
    let list = ask(&w, query(&w, &b, CoordinatorQuery::ListSessions, 1000))
        .await
        .unwrap();
    let stations: Vec<&str> = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["station"].as_str().unwrap())
        .collect();
    assert_eq!(stations.len(), 2);
    assert!(stations.iter().all(|s| *s == w.alpha.as_str()));
    // A wait on an open task ends `forbidden` when the grant is revoked.
    let assign = CoordinatorOp::Assign {
        task: "check it".to_owned(),
        to: target(&w.alpha, "worker-1"),
    };
    assert_eq!(
        decide(&w, env(&w, &b, "t-1", assign)).await.state,
        ReceiptState::Accepted
    );
    let wait = query(
        &w,
        &b,
        CoordinatorQuery::WaitTask {
            task_id: "t-1".to_owned(),
        },
        60_000,
    );
    let (s, grant_id) = (w.s.clone(), b.grant_id.clone());
    let revoke = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        s.revoke_grant(grant_id, "owner".to_owned()).await.unwrap()
    });
    let started = std::time::Instant::now();
    assert_eq!(ask(&w, wait).await, Err(RejectCode::Forbidden));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(revoke.await.unwrap());
}

#[tokio::test]
async fn a_query_against_a_replaced_deck_database_is_rejected_without_a_receipt() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Read], 0, &[]).await;
    let mut q = query(&w, &b, CoordinatorQuery::ListSessions, 1000);
    q.target_incarnation = "another-deck".to_owned().into();
    assert_eq!(ask(&w, q).await, Err(RejectCode::IncarnationMismatch));
    let mut stale = query(&w, &b, CoordinatorQuery::ListSessions, 1000);
    stale.binding.generation = 7;
    assert_eq!(ask(&w, stale).await, Err(RejectCode::BindingStale));
    assert_eq!(w.s.coordinator_receipts().await.unwrap(), 0);
}

#[tokio::test]
async fn a_spawn_only_grant_reads_its_command_status_after_a_tool_timeout_and_no_fleet_state() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Spawn], 1, &[("claude", "default")]).await;
    let first = decide(&w, env(&w, &b, "k1", spawn(&w.alpha, "claude", None))).await;
    assert_eq!(first.state, ReceiptState::Accepted);
    let status = CoordinatorQuery::CommandStatus {
        command_id: CommandId::from("k1"),
    };
    let v = ask(&w, query(&w, &b, status, 1000)).await.unwrap();
    assert_eq!(v["receipt"]["state"], "accepted");
    let unknown = CoordinatorQuery::CommandStatus {
        command_id: CommandId::from("k9"),
    };
    assert!(ask(&w, query(&w, &b, unknown, 1000)).await.unwrap()["receipt"].is_null());
    let list = query(&w, &b, CoordinatorQuery::ListSessions, 1000);
    assert_eq!(ask(&w, list).await, Err(RejectCode::Forbidden));
}

#[test]
fn mode_ceilings_are_per_family_and_unknown_modes_fail_closed() {
    assert!(mode_within("claude", None, "default"));
    assert!(mode_within("claude", Some("acceptEdits"), "acceptEdits"));
    assert!(!mode_within("claude", Some("bypassPermissions"), "auto"));
    assert!(!mode_within(
        "claude",
        Some("untrusted/read-only"),
        "bypassPermissions"
    ));
    assert!(mode_within("codex", None, "untrusted/workspace-write"));
    assert!(!mode_within("codex", None, "untrusted/read-only"));
    assert!(mode_within(
        "codex",
        Some("on-request/read-only"),
        "never/workspace-write"
    ));
    assert!(!mode_within(
        "codex",
        Some("never/read-only"),
        "on-request/danger-full-access"
    ));
    assert!(!mode_within(
        "codex",
        Some("default"),
        "never/danger-full-access"
    ));
    assert!(!mode_within("gemini", None, "default"));
}

#[path = "coordinator_lifecycle_tests.rs"]
mod lifecycle;

#[path = "coordinator_fleet_tests.rs"]
mod fleet_ops;
