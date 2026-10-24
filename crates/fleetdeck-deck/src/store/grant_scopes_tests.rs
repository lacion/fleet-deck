//! The derived `coordinate` scope (grant stations design §2.2, §2.3, §2.3a):
//! every write that changes which grants are in effect rebuilds the scope in
//! its own transaction, and every `coordinate` admission checks a grant in
//! effect.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::identity::{Identity, PeerKey};
use fleetdeck_proto::command::GrantKind;
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::{
    CoordinatorBinding, CoordinatorCall, CoordinatorCallEnvelope, CoordinatorDelivery,
    CoordinatorOp, CoordinatorQuery, CoordinatorQueryEnvelope, CoordinatorTarget,
};
use fleetdeck_proto::{
    CommandId, CoordinatorBindingPhase as Phase, CoordinatorBindingState, EventEnvelope,
    RejectCode, StationId,
};
use fleetdeck_testkit::Det;

use super::*;
use crate::store::grants::{GrantClass, GrantState, NewGrant};
use crate::store::stations::NewStation;

struct World {
    det: Det,
    s: DeckStore,
    a: StationId,
    b: StationId,
    c: StationId,
    key_a: PeerKey,
}

async fn pin(s: &DeckStore, name: &str, key: PeerKey, scopes: &[Scope]) -> StationId {
    s.pin_station(NewStation {
        key,
        name: name.to_owned(),
        scopes: scopes.iter().copied().collect(),
        dial_url: None,
    })
    .await
    .unwrap()
    .station_id
}

/// Three stations that hold only `admin`, and a store with a test clock.
async fn world() -> World {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let key_a = Identity::generate().unwrap().public_key();
    let a = pin(&s, "alpha", key_a, &[Scope::Admin]).await;
    let new = || Identity::generate().unwrap().public_key();
    let b = pin(&s, "beta", new(), &[Scope::Admin]).await;
    let c = pin(&s, "gamma", new(), &[Scope::Admin]).await;
    World {
        det,
        s,
        a,
        b,
        c,
        key_a,
    }
}

async fn scopes(w: &World, id: &StationId) -> BTreeSet<Scope> {
    w.s.station(id.clone()).await.unwrap().unwrap().scopes
}

async fn has(w: &World, id: &StationId) -> bool {
    scopes(w, id).await.contains(&Scope::Coordinate)
}

/// A grant of `session` on `own` that also names `targets`.
async fn grant(
    w: &World,
    (own, session): (&StationId, &str),
    targets: &[&StationId],
    edit: impl FnOnce(&mut NewGrant),
) -> Grant {
    let mut g = NewGrant::new(own.clone(), session.into());
    g.classes = BTreeSet::from([GrantClass::Mail]);
    g.stations.extend(targets.iter().map(|s| (*s).clone()));
    edit(&mut g);
    w.s.create_grant(g).await.unwrap()
}

fn binding(grant: &Grant, generation: u32) -> CoordinatorBinding {
    CoordinatorBinding {
        binding_id: "b-1".to_owned(),
        grant_id: grant.grant_id.clone(),
        generation,
        provider_instance: "claude".into(),
        native_id: "native-1".to_owned(),
    }
}

async fn bound(w: &World, g: &Grant) -> CoordinatorBinding {
    let b = binding(g, 1);
    assert!(w.s.bind_grant(b.clone()).await.unwrap());
    b
}

async fn state(w: &World, g: &Grant) -> (GrantState, Option<String>) {
    let got = w.s.grant(g.grant_id.clone()).await.unwrap().unwrap();
    (got.state, got.revoked_reason)
}

fn set(ids: &[&StationId]) -> BTreeSet<StationId> {
    ids.iter().map(|s| (*s).clone()).collect()
}

fn lifecycle(w: &World, seq: u64, event: Event) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: "coord-1".into(),
        session_seq: seq,
        at: w.s.now_ms(),
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(event),
    }
}

fn bound_event(g: &Grant, generation: u32) -> Event {
    Event::CoordinatorBound {
        binding_id: "b-1".to_owned(),
        grant_id: g.grant_id.clone(),
        generation,
        provider_instance: "claude".to_owned(),
        native_id: "native-1".to_owned(),
    }
}

fn ended_event(generation: u32, reason: &str) -> Event {
    Event::CoordinatorEnded {
        binding_id: "b-1".to_owned(),
        generation,
        reason: reason.to_owned(),
    }
}

fn projected(g: &Grant, generation: u32, state: Phase) -> CoordinatorBindingState {
    CoordinatorBindingState {
        binding_id: "b-1".to_owned(),
        grant_id: g.grant_id.clone(),
        session_id: "coord-1".into(),
        generation,
        provider_instance: "claude".to_owned(),
        native_id: Some("native-1".to_owned()),
        state,
    }
}

fn call_env(
    w: &World,
    caller: &str,
    b: CoordinatorBinding,
    op: CoordinatorOp,
) -> CoordinatorCallEnvelope {
    CoordinatorCallEnvelope {
        command_id: CommandId::from(format!("k-{caller}-{}", w.s.now_ms())),
        target_incarnation: w.s.incarnation().clone(),
        retry_until: w.s.now_ms() + 60_000,
        call: CoordinatorCall {
            caller: caller.into(),
            binding: b,
            idempotency_key: None,
            op,
        },
    }
}

fn send_op(station: &StationId) -> CoordinatorOp {
    CoordinatorOp::Send {
        to: CoordinatorTarget {
            station: station.clone(),
            session_id: "worker-1".into(),
        },
        text: "hi".to_owned(),
        delivery: CoordinatorDelivery::FollowUp,
    }
}

fn worker_binding() -> CoordinatorBinding {
    CoordinatorBinding {
        binding_id: "w-1".to_owned(),
        grant_id: String::new(),
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "native-w".to_owned(),
    }
}

#[test]
fn a_grant_is_in_effect_only_while_active_inside_its_lifetime_and_not_idle() {
    let base = Grant {
        grant_id: "g".to_owned(),
        station_id: "a".into(),
        session_id: "s".into(),
        classes: BTreeSet::new(),
        stations: BTreeSet::new(),
        spawn_cap: 0,
        spawn_used: 0,
        max_modes: Default::default(),
        idle_ttl_ms: 1_000,
        created_at: 0,
        expires_at: 10_000,
        state: GrantState::Active,
        revoked_reason: None,
        binding: None,
        last_used_at: None,
        scope: fleetdeck_proto::command::GrantScope::Lineage,
        answer_cap: None,
        answer_used: 0,
        desktop_cap: None,
        desktop_used: 0,
        capture_cap: None,
        capture_used: 0,
        image_byte_cap: None,
        image_byte_used: 0,
        desktop_attempts: 0,
        recording_cap: 0,
        recording_used: 0,
        recording_second_cap: 0,
        recording_second_used: 0,
        allow_input: false,
        input_scope: None,
        input_cap: 0,
        input_used: 0,
        input_character_cap: 0,
        input_character_used: 0,
        allow_click: false,
        kind: GrantKind::Coordinator,
    };
    assert!(in_effect(&base, 10_000));
    assert!(!in_effect(&base, 10_001));
    let used = Grant {
        last_used_at: Some(5_000),
        ..base.clone()
    };
    assert!(in_effect(&used, 6_000));
    assert!(!in_effect(&used, 6_001));
    for state in [GrantState::Revoked, GrantState::Expired] {
        let ended = Grant {
            state,
            ..base.clone()
        };
        assert!(!in_effect(&ended, 0));
    }
}

#[tokio::test]
async fn a_grant_gives_coordinate_to_every_station_it_names_and_keeps_the_other_scopes() {
    let w = world().await;
    grant(&w, (&w.a, "coord-1"), &[&w.b], |_| {}).await;
    let want = BTreeSet::from([Scope::Admin, Scope::Coordinate]);
    assert_eq!(scopes(&w, &w.a).await, want);
    assert_eq!(scopes(&w, &w.b).await, want);
    assert_eq!(scopes(&w, &w.c).await, BTreeSet::from([Scope::Admin]));
    // The stations to refresh are noted once.
    assert_eq!(w.s.take_scope_changes(), set(&[&w.a, &w.b]));
    assert!(w.s.take_scope_changes().is_empty());
}

#[tokio::test]
async fn revocation_removes_coordinate_unless_another_grant_in_effect_names_the_station() {
    let w = world().await;
    let g1 = grant(&w, (&w.a, "coord-1"), &[&w.b], |_| {}).await;
    let g2 = grant(&w, (&w.c, "coord-2"), &[&w.b], |_| {}).await;
    drop(w.s.take_scope_changes());
    w.s.revoke_grant(g1.grant_id, "owner".to_owned())
        .await
        .unwrap();
    assert!(!has(&w, &w.a).await);
    assert!(has(&w, &w.b).await, "g2 still names beta");
    assert!(has(&w, &w.c).await);
    assert_eq!(w.s.take_scope_changes(), set(&[&w.a]));
    w.s.revoke_grant(g2.grant_id, "owner".to_owned())
        .await
        .unwrap();
    assert!(!has(&w, &w.b).await);
    assert!(!has(&w, &w.c).await);
    assert_eq!(w.s.take_scope_changes(), set(&[&w.b, &w.c]));
}

#[tokio::test]
async fn the_sweep_ends_lapsed_grants_rebuilds_the_scopes_and_audits_each_one() {
    let w = world().await;
    let lifetime = grant(&w, (&w.a, "s1"), &[&w.b], |g| g.ttl_ms = 1_000).await;
    let idle = grant(&w, (&w.c, "s2"), &[], |g| g.idle_ttl_ms = 1_000).await;
    bound(&w, &idle).await;
    // A grant of alpha that stays in effect.
    let keep = grant(&w, (&w.a, "s3"), &[], |_| {}).await;
    drop(w.s.take_scope_changes());
    w.det.advance(Duration::from_secs(2));
    // Before the sweep the stored scope lags, but the exact rule refuses.
    assert!(has(&w, &w.b).await);
    assert!(!w.s.station_covered_now(&w.b).await.unwrap());
    assert!(w.s.station_covered_now(&w.a).await.unwrap());
    assert_eq!(w.s.sweep_grants().await.unwrap(), set(&[&w.b, &w.c]));
    assert_eq!(w.s.take_scope_changes(), set(&[&w.b, &w.c]));
    assert!(has(&w, &w.a).await, "the third grant still names alpha");
    assert!(!has(&w, &w.b).await);
    assert!(!has(&w, &w.c).await);
    assert_eq!(
        state(&w, &lifetime).await,
        (GrantState::Expired, Some("expired".to_owned()))
    );
    assert_eq!(
        state(&w, &idle).await,
        (GrantState::Expired, Some("idle".to_owned()))
    );
    assert_eq!(state(&w, &keep).await.0, GrantState::Active);
    let rows: Vec<_> =
        w.s.audit_log(10)
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.action == "coordinator.grant_expired")
            .collect();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.actor == "deck"));
    let mut seen: Vec<_> = rows
        .iter()
        .map(|r| (r.target.clone().unwrap(), r.detail.clone().unwrap()))
        .collect();
    seen.sort();
    let mut want = vec![
        (lifetime.grant_id.clone(), "expired".to_owned()),
        (idle.grant_id.clone(), "idle".to_owned()),
    ];
    want.sort();
    assert_eq!(seen, want);
    // A second sweep ends nothing and writes nothing.
    assert!(w.s.sweep_grants().await.unwrap().is_empty());
    assert_eq!(w.s.audit_log(10).await.unwrap().len(), rows.len());
}

#[tokio::test]
async fn a_worker_call_is_refused_one_millisecond_after_the_last_covering_grant_expires_and_nothing_is_written()
 {
    let w = world().await;
    grant(&w, (&w.a, "coord-1"), &[&w.b], |g| g.ttl_ms = 1_000).await;
    w.det.advance(Duration::from_millis(1_000));
    // Exactly at `expires_at` the grant is still in effect.
    assert!(w.s.station_covered_now(&w.b).await.unwrap());
    w.det.advance(Duration::from_millis(1));
    assert!(!w.s.station_covered_now(&w.b).await.unwrap());
    // No sweep ran: the stored scope still has `coordinate`.
    assert!(has(&w, &w.b).await);
    let ask = CoordinatorOp::AskCoordinator {
        question: "which key?".to_owned(),
    };
    let r =
        w.s.decide_coordinator(w.b.clone(), call_env(&w, "worker-1", worker_binding(), ask))
            .await
            .unwrap();
    let reject = r.reject.expect("refused");
    assert_eq!(reject.code, RejectCode::Forbidden);
    assert_eq!(reject.message, NOT_COVERED);
    assert_eq!(w.s.coordinator_receipts().await.unwrap(), 0);
    assert!(w.s.mails(10).await.unwrap().is_empty());
    assert!(w.s.outbox().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_lazy_idle_expiry_of_a_call_and_of_a_query_rebuilds_the_scopes_in_its_transaction() {
    let w = world().await;
    let g1 = grant(&w, (&w.a, "coord-1"), &[&w.b], |g| g.idle_ttl_ms = 1_000).await;
    let b1 = bound(&w, &g1).await;
    // Alpha stays covered by another grant; the lazy expiry then runs.
    grant(&w, (&w.a, "coord-2"), &[], |_| {}).await;
    drop(w.s.take_scope_changes());
    w.det.advance(Duration::from_secs(2));
    let r =
        w.s.decide_coordinator(
            w.a.clone(),
            call_env(&w, "coord-1", b1.clone(), send_op(&w.a)),
        )
        .await
        .unwrap();
    assert_eq!(r.reject.unwrap().code, RejectCode::Forbidden);
    assert_eq!(
        state(&w, &g1).await,
        (GrantState::Expired, Some("idle".to_owned()))
    );
    assert!(!has(&w, &w.b).await);
    assert!(has(&w, &w.a).await);
    assert_eq!(w.s.take_scope_changes(), set(&[&w.b]));

    // The same on the query path (gamma is named by the second grant).
    let g3 = grant(&w, (&w.a, "coord-3"), &[&w.c], |g| g.idle_ttl_ms = 1_000).await;
    let b3 = bound(&w, &g3).await;
    drop(w.s.take_scope_changes());
    w.det.advance(Duration::from_secs(2));
    let query = CoordinatorQueryEnvelope {
        query_id: "q".to_owned(),
        target_incarnation: w.s.incarnation().clone(),
        caller: "coord-3".into(),
        binding: b3,
        deadline: w.s.now_ms() + 10_000,
        query: CoordinatorQuery::ListSessions,
    };
    let refused = w.s.coordinator_query(w.a.clone(), query).await.unwrap();
    assert_eq!(
        refused.unwrap_err().message,
        "Your fleet grant expired (idle for 1 second). The owner can renew it on the board."
    );
    assert_eq!(state(&w, &g3).await.0, GrantState::Expired);
    assert!(!has(&w, &w.c).await);
    assert_eq!(w.s.take_scope_changes(), set(&[&w.c]));
}

#[tokio::test]
async fn a_renewal_that_reactivates_a_grant_gives_coordinate_back_with_and_without_another_grant() {
    let w = world().await;
    let inc: fleetdeck_proto::IncarnationId = "inc".to_owned().into();
    w.s.apply_snapshot(w.a.clone(), inc.clone(), 0, Vec::new())
        .await
        .unwrap();
    let g1 = grant(&w, (&w.a, "coord-1"), &[&w.b], |_| {}).await;
    // Gamma's grant also names beta.
    grant(&w, (&w.c, "coord-2"), &[&w.b], |_| {}).await;
    bound(&w, &g1).await;
    let apply = |seq, event| {
        let e = lifecycle(&w, seq, event);
        w.s.apply_events(w.a.clone(), inc.clone(), vec![e])
    };
    drop(w.s.take_scope_changes());
    apply(1, ended_event(1, "idle")).await.unwrap();
    assert!(!has(&w, &w.a).await, "alpha lost its only grant");
    assert!(has(&w, &w.b).await, "the other grant still names beta");
    assert_eq!(w.s.take_scope_changes(), set(&[&w.a]));
    assert_eq!(w.s.renew_grant(g1.grant_id.clone()).await.unwrap(), Some(2));
    apply(2, bound_event(&g1, 2)).await.unwrap();
    assert_eq!(state(&w, &g1).await.0, GrantState::Active);
    assert!(has(&w, &w.a).await, "the renewal gives it back");
    assert_eq!(w.s.take_scope_changes(), set(&[&w.a]));

    // Without another grant: beta is named only by the renewed grant.
    let w2 = world().await;
    w2.s.apply_snapshot(w2.a.clone(), inc.clone(), 0, Vec::new())
        .await
        .unwrap();
    let only = grant(&w2, (&w2.a, "coord-1"), &[&w2.b], |_| {}).await;
    bound(&w2, &only).await;
    let apply2 = |seq, event| {
        let e = lifecycle(&w2, seq, event);
        w2.s.apply_events(w2.a.clone(), inc.clone(), vec![e])
    };
    apply2(1, ended_event(1, "idle")).await.unwrap();
    assert!(!has(&w2, &w2.a).await);
    assert!(!has(&w2, &w2.b).await);
    assert_eq!(
        w2.s.renew_grant(only.grant_id.clone()).await.unwrap(),
        Some(2)
    );
    apply2(2, bound_event(&only, 2)).await.unwrap();
    assert!(has(&w2, &w2.a).await);
    assert!(has(&w2, &w2.b).await);
}

#[tokio::test]
async fn a_snapshot_projection_that_ends_or_reactivates_a_grant_rebuilds_the_scopes() {
    let w = world().await;
    let inc: fleetdeck_proto::IncarnationId = "inc".to_owned().into();
    w.s.apply_snapshot(w.a.clone(), inc.clone(), 0, Vec::new())
        .await
        .unwrap();
    let g = grant(&w, (&w.a, "coord-1"), &[&w.b], |_| {}).await;
    bound(&w, &g).await;
    drop(w.s.take_scope_changes());
    let project = |mark, p| {
        w.s.apply_snapshot_with(
            w.a.clone(),
            inc.clone(),
            mark,
            Vec::new(),
            Some(vec![p]),
            None,
        )
    };
    project(1, projected(&g, 1, Phase::Expired)).await.unwrap();
    assert!(!has(&w, &w.a).await);
    assert!(!has(&w, &w.b).await);
    assert_eq!(w.s.take_scope_changes(), set(&[&w.a, &w.b]));
    assert_eq!(w.s.renew_grant(g.grant_id.clone()).await.unwrap(), Some(2));
    // The projection carries the station time of its own binding.
    project(2, projected(&g, 2, Phase::Active)).await.unwrap();
    assert!(has(&w, &w.a).await);
    assert!(has(&w, &w.b).await);
    assert_eq!(w.s.take_scope_changes(), set(&[&w.a, &w.b]));
}

#[tokio::test]
async fn a_pairing_write_keeps_coordinate_for_a_grant_in_effect_and_never_adds_it_otherwise() {
    let w = world().await;
    grant(&w, (&w.a, "coord-1"), &[&w.b], |_| {}).await;
    // Re-pairing alpha replaces its scopes with the pairing scopes.
    pin(&w.s, "alpha", w.key_a, &[Scope::Admin, Scope::Read]).await;
    assert_eq!(
        scopes(&w, &w.a).await,
        BTreeSet::from([Scope::Admin, Scope::Read, Scope::Coordinate])
    );
    assert!(has(&w, &w.b).await);
    // A pairing that asks for `coordinate` with no grant in effect gets none.
    let fresh = Identity::generate().unwrap().public_key();
    let d = pin(&w.s, "delta", fresh, &[Scope::Admin, Scope::Coordinate]).await;
    assert!(!has(&w, &d).await);
    assert!(has(&w, &w.a).await);
}

#[tokio::test]
async fn a_refused_query_of_an_ended_grant_says_why() {
    let w = world().await;
    // Another grant keeps alpha covered while the first ones end.
    grant(&w, (&w.a, "coord-9"), &[], |_| {}).await;
    let query = |w: &World, caller: &str, b: CoordinatorBinding| CoordinatorQueryEnvelope {
        query_id: "q".to_owned(),
        target_incarnation: w.s.incarnation().clone(),
        caller: caller.into(),
        binding: b,
        deadline: w.s.now_ms() + 10_000,
        query: CoordinatorQuery::ListSessions,
    };

    let g1 = grant(&w, (&w.a, "coord-1"), &[], |g| g.ttl_ms = 1_000).await;
    let b1 = bound(&w, &g1).await;
    w.det.advance(Duration::from_secs(2));
    let refused =
        w.s.coordinator_query(w.a.clone(), query(&w, "coord-1", b1))
            .await
            .unwrap();
    assert_eq!(
        refused.unwrap_err().message,
        "Your fleet grant expired (its lifetime ended). The owner can give a new grant on the board."
    );

    let g2 = grant(&w, (&w.a, "coord-2"), &[], |_| {}).await;
    let b2 = bound(&w, &g2).await;
    assert!(
        w.s.revoke_grant(g2.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    let refused =
        w.s.coordinator_query(w.a.clone(), query(&w, "coord-2", b2))
            .await
            .unwrap();
    assert_eq!(
        refused.unwrap_err().message,
        "Your fleet grant was revoked. The owner can give a new grant on the board."
    );
}
