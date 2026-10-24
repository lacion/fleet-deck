//! B26: the hold of the grants of an offline station, the return check of a
//! `station.info` answer, and the settle state of the links (design
//! `grants-update.md` §3.2, §3.3, §3.3a; build plan T2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{DeckCommandState, SessionCommand};
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::{CoordinatorBinding, CoordinatorQuery, CoordinatorQueryEnvelope};
use fleetdeck_proto::{CommandEnvelope, EventEnvelope, StationId};
use fleetdeck_testkit::Det;

use super::*;
use crate::knobs::DeckKnobs;
use crate::link::CommandKind;
use crate::store::grants::grant_end::{GrantEnd, end_text};
use crate::store::grants::{Grant, GrantState, NewGrant};
use crate::store::stations::NewStation;

struct World {
    det: Det,
    s: DeckStore,
    a: StationId,
    b: StationId,
}

async fn pin(s: &DeckStore, name: &str) -> StationId {
    s.pin_station(NewStation {
        key: Identity::generate().unwrap().public_key(),
        name: name.to_owned(),
        scopes: BTreeSet::from([Scope::Admin]),
        dial_url: None,
    })
    .await
    .unwrap()
    .station_id
}

/// Stations `st-1` and `st-2`, both with the incarnation `inc`.
async fn world() -> World {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let a = pin(&s, "st-1").await;
    let b = pin(&s, "st-2").await;
    for id in [&a, &b] {
        s.apply_snapshot(id.clone(), "inc".to_owned().into(), 0, Vec::new())
            .await
            .unwrap();
    }
    World { det, s, a, b }
}

fn up(w: &World, id: &StationId, token: u64) {
    w.s.set_reachable(id, true);
    w.s.set_link_features(id, token, vec![]);
}

fn down(w: &World, id: &StationId, token: u64) {
    w.s.clear_link_features(id, token);
    w.s.set_reachable(id, false);
}

async fn heard(w: &World, id: &StationId, at: i64) {
    w.s.station_heard(id.clone(), at).await.unwrap();
}

async fn answer(
    w: &World,
    id: &StationId,
    token: u64,
    inc: &str,
    version: Option<&str>,
) -> Resolved {
    w.s.resolve_grant_holds(
        id.clone(),
        token,
        inc.to_owned().into(),
        version.map(str::to_owned),
    )
    .await
    .unwrap()
}

fn advance(w: &World, secs: u64) {
    w.det.advance(Duration::from_secs(secs));
}

async fn sweep(w: &World) {
    w.s.sweep_grants().await.unwrap();
}

fn binding(g: &Grant, generation: u32) -> CoordinatorBinding {
    CoordinatorBinding {
        binding_id: "b-1".to_owned(),
        grant_id: g.grant_id.clone(),
        generation,
        provider_instance: "claude".into(),
        native_id: "native-1".to_owned(),
    }
}

/// A grant of `session` on `station` (it names only that station), with the
/// idle time `idle_ms`; `bind` makes it bound, and its last use is now.
async fn grant(w: &World, station: &StationId, session: &str, idle_ms: i64, bind: bool) -> Grant {
    let mut new = NewGrant::new(station.clone(), session.into());
    new.idle_ttl_ms = idle_ms;
    let g = w.s.create_grant(new).await.unwrap();
    if bind {
        assert!(w.s.bind_grant(binding(&g, 1)).await.unwrap());
        set_used(w, &g, w.s.now_ms()).await;
    }
    g
}

async fn sql(w: &World, text: &'static str, args: Vec<rusqlite::types::Value>) {
    call(&w.s.inner.db, move |conn| {
        conn.execute(text, rusqlite::params_from_iter(args))?;
        Ok(())
    })
    .await
    .unwrap();
}

async fn set_used(w: &World, g: &Grant, at: i64) {
    let args = vec![g.grant_id.clone().into(), at.into()];
    sql(
        w,
        "UPDATE tokens SET last_used_at = ?2 WHERE grant_id = ?1",
        args,
    )
    .await;
}

async fn set_state(w: &World, g: &Grant, state: &'static str, reason: Option<&'static str>) {
    let args = vec![
        g.grant_id.clone().into(),
        state.to_owned().into(),
        reason.map(str::to_owned).into(),
    ];
    sql(
        w,
        "UPDATE tokens SET state = ?2, revoked_reason = ?3 WHERE grant_id = ?1",
        args,
    )
    .await;
}

/// Gives the grant a `session_owner_grant` row.
async fn give_row(w: &World, g: &Grant) {
    let args = vec![
        w.a.as_str().to_owned().into(),
        g.session_id.0.clone().into(),
        g.grant_id.clone().into(),
    ];
    sql(
        w,
        "INSERT INTO session_owner_grant (station_id, session_id, grant_id, seq) VALUES (?1, ?2, ?3, 1)",
        args,
    )
    .await;
}

async fn get(w: &World, g: &Grant) -> Grant {
    w.s.grant(g.grant_id.clone()).await.unwrap().unwrap()
}

async fn state(w: &World, g: &Grant) -> (GrantState, Option<String>) {
    let got = get(w, g).await;
    (got.state, got.revoked_reason)
}

/// The `(target, detail)` of the audit rows with `action`, oldest first.
async fn rows(w: &World, action: &str) -> Vec<(String, String)> {
    let mut all: Vec<_> =
        w.s.audit_log(1000)
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.action == action)
            .map(|r| (r.target.unwrap(), r.detail.unwrap()))
            .collect();
    all.reverse();
    all
}

async fn holds(w: &World) -> i64 {
    call(&w.s.inner.db, |conn| {
        Ok(conn.query_row("SELECT COUNT(*) FROM grant_holds", [], |r| r.get(0))?)
    })
    .await
    .unwrap()
}

/// The stored `(info_incarnation, info_version)` of `station`.
async fn info(w: &World, id: &StationId) -> (Option<String>, Option<String>) {
    let id = id.clone();
    call(&w.s.inner.db, move |conn| {
        Ok(conn.query_row(
            "SELECT info_incarnation, info_version FROM stations WHERE station_id = ?1",
            [id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
    .await
    .unwrap()
}

async fn has_coordinate(w: &World, id: &StationId) -> bool {
    w.s.station(id.clone())
        .await
        .unwrap()
        .unwrap()
        .scopes
        .contains(&Scope::Coordinate)
}

fn keep_off(w: &World) {
    w.s.set_knobs(DeckKnobs {
        keep_grants_on_update: false,
        ..DeckKnobs::default()
    });
}

/// The `session.coordinator_set` commands of the deck with no spec, as
/// `(grant_id, state)`.
async fn empty_sets(w: &World) -> Vec<(String, DeckCommandState)> {
    w.s.commands()
        .await
        .unwrap()
        .into_iter()
        .filter(|c| c.method == "session.coordinator_set")
        .filter_map(|c| {
            let env: CommandEnvelope = serde_json::from_value(c.envelope.clone()).unwrap();
            match env.command {
                SessionCommand::CoordinatorSet {
                    grant_id,
                    spec: None,
                    ..
                } => Some((grant_id, c.state)),
                _ => None,
            }
        })
        .collect()
}

/// The grant ids of the queued `session.coordinator_revoke` items.
async fn revoke_items(w: &World) -> Vec<String> {
    w.s.outbox()
        .await
        .unwrap()
        .into_iter()
        .filter_map(
            |i| match serde_json::from_value::<CommandKind>(i.payload).ok()? {
                CommandKind::Session {
                    command: SessionCommand::CoordinatorRevoke { grant_id, .. },
                } => Some(grant_id),
                _ => None,
            },
        )
        .collect()
}

async fn query(w: &World, g: &Grant) -> Result<(), String> {
    let env = CoordinatorQueryEnvelope {
        query_id: "q".to_owned(),
        target_incarnation: w.s.incarnation().clone(),
        caller: g.session_id.clone(),
        binding: binding(g, 1),
        deadline: w.s.now_ms() + 10_000,
        query: CoordinatorQuery::ListSessions,
    };
    w.s.coordinator_query(g.station_id.clone(), env)
        .await
        .unwrap()
        .map(|_| ())
        .map_err(|r| r.message)
}

/// An offline station `st-1` with a bound grant (idle 60 s), heard now, and
/// a sweep after `gap_secs`: the hold row exists when the gap is 30 s or more.
async fn held(w: &World, gap_secs: u64) -> Grant {
    let g = grant(w, &w.a, "coord-1", 60_000, true).await;
    heard(w, &w.a, w.s.now_ms()).await;
    advance(w, gap_secs);
    sweep(w).await;
    g
}

#[tokio::test]
async fn a_bound_grant_of_an_offline_station_stays_in_effect_past_its_idle_time() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", 60_000, true).await;
    heard(&w, &w.a, w.s.now_ms()).await;
    down(&w, &w.a, 1);
    advance(&w, 50);
    sweep(&w).await;
    let got = get(&w, &g).await;
    assert_eq!(got.state, GrantState::Active);
    assert_eq!(got.last_used_at, Some(w.s.now_ms()));
    advance(&w, 50);
    sweep(&w).await;
    assert_eq!(get(&w, &g).await.state, GrantState::Active);
}

#[tokio::test]
async fn the_hold_ends_sixty_minutes_after_the_last_frame() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", 60_000, true).await;
    heard(&w, &w.a, w.s.now_ms()).await;
    for _ in 0..72 {
        advance(&w, 50);
        sweep(&w).await;
        assert_eq!(get(&w, &g).await.state, GrantState::Active);
    }
    w.det.advance(Duration::from_millis(1));
    sweep(&w).await;
    assert_eq!(get(&w, &g).await.state, GrantState::Active);
    advance(&w, 61);
    sweep(&w).await;
    assert_eq!(
        state(&w, &g).await,
        (GrantState::Expired, Some("idle".to_owned()))
    );
    assert_eq!(rows(&w, "coordinator.grant_expired").await.len(), 1);
}

#[tokio::test]
async fn a_grant_idle_at_the_last_frame_is_not_held() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", 60_000, true).await;
    advance(&w, 61);
    heard(&w, &w.a, w.s.now_ms()).await;
    down(&w, &w.a, 1);
    sweep(&w).await;
    assert_eq!(
        state(&w, &g).await,
        (GrantState::Expired, Some("idle".to_owned()))
    );
}

#[tokio::test]
async fn a_deck_restart_holds_the_grants_of_each_station_heard_in_the_last_hour() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.db");
    let det = Det::new();
    let t0 = i64::try_from(det.now_ms()).unwrap();
    let (g1, g2) = {
        let s = DeckStore::open(&path, det.clock(), det.ids())
            .await
            .unwrap();
        let a = pin(&s, "st-1").await;
        let b = pin(&s, "st-2").await;
        let w = World {
            det: Det::new(),
            s: s.clone(),
            a: a.clone(),
            b: b.clone(),
        };
        let (g1, g2) = (
            grant(&w, &a, "coord-1", 30 * 60_000, true).await,
            grant(&w, &b, "coord-2", 30 * 60_000, true).await,
        );
        s.station_heard(a, t0).await.unwrap();
        s.station_heard(b, t0 - 2 * 3_600_000).await.unwrap();
        (g1, g2)
    };
    det.advance(Duration::from_secs(45 * 60));
    let s = DeckStore::open(&path, det.clock(), det.ids())
        .await
        .unwrap();
    let now = s.now_ms();
    let first = s.grant(g1.grant_id).await.unwrap().unwrap();
    assert_eq!(first.state, GrantState::Active);
    assert_eq!(first.last_used_at, Some(now));
    let second = s.grant(g2.grant_id).await.unwrap().unwrap();
    assert_eq!(
        (second.state, second.revoked_reason),
        (GrantState::Expired, Some("idle".to_owned()))
    );
}

#[tokio::test]
async fn an_unbound_grant_or_a_grant_of_an_online_station_is_not_held() {
    let w = world().await;
    let unbound = grant(&w, &w.a, "coord-1", 60_000, false).await;
    let online = grant(&w, &w.b, "coord-2", 60_000, true).await;
    let mut new = NewGrant::new(w.b.clone(), "coord-3".into());
    new.idle_ttl_ms = 60_000;
    new.stations.insert(w.a.clone());
    let names = w.s.create_grant(new).await.unwrap();
    assert!(w.s.bind_grant(binding(&names, 1)).await.unwrap());
    // An unbound coordinator grant starts its idle time at its first use:
    // a use here shows that the hold does not touch it.
    for g in [&unbound, &online, &names] {
        set_used(&w, g, w.s.now_ms()).await;
    }
    let now = w.s.now_ms();
    heard(&w, &w.a, now).await;
    heard(&w, &w.b, now).await;
    up(&w, &w.b, 1);
    advance(&w, 70);
    sweep(&w).await;
    for g in [&unbound, &online, &names] {
        assert_eq!(
            state(&w, g).await,
            (GrantState::Expired, Some("idle".to_owned()))
        );
    }
}

#[tokio::test]
async fn a_station_that_returns_unchanged_keeps_its_held_grants_with_one_kept_row() {
    let w = world().await;
    let g = held(&w, 50).await;
    assert_eq!(holds(&w).await, 1);
    up(&w, &w.a, 1);
    let first = answer(&w, &w.a, 1, "inc-1", Some("v1")).await;
    assert_eq!(
        first,
        Resolved {
            kept: 1,
            dropped: 0,
            settled: false
        }
    );
    assert_eq!(
        rows(&w, "coordinator.grant_kept").await,
        vec![(
            g.grant_id.clone(),
            "reason=station_offline;station=st-1;offline_ms=50000".to_owned()
        )]
    );
    assert_eq!(
        answer(&w, &w.a, 1, "inc-1", Some("v1")).await,
        Resolved::default()
    );
    assert_eq!(holds(&w).await, 0);
}

#[tokio::test]
async fn an_update_keeps_the_grants_when_the_setting_is_on() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", 24 * 3_600_000, true).await;
    up(&w, &w.a, 1);
    answer(&w, &w.a, 1, "inc-1", Some("v1")).await;
    advance(&w, 10);
    let r = answer(&w, &w.a, 1, "inc-1", Some("v2")).await;
    assert_eq!((r.kept, r.dropped), (1, 0));
    assert_eq!(get(&w, &g).await.state, GrantState::Active);
    assert_eq!(
        rows(&w, "coordinator.grant_kept").await,
        vec![(
            g.grant_id.clone(),
            "reason=station_updated;station=st-1;from=v1;to=v2".to_owned()
        )]
    );
}

#[tokio::test]
async fn an_update_drops_the_grants_when_the_setting_is_off() {
    let w = world().await;
    keep_off(&w);
    let with_row = grant(&w, &w.a, "coord-1", 24 * 3_600_000, true).await;
    let plain = grant(&w, &w.a, "coord-2", 24 * 3_600_000, true).await;
    give_row(&w, &with_row).await;
    up(&w, &w.a, 1);
    assert!(has_coordinate(&w, &w.a).await);
    answer(&w, &w.a, 1, "inc-1", Some("v1")).await;
    let r = answer(&w, &w.a, 1, "inc-1", Some("v2")).await;
    assert_eq!((r.kept, r.dropped), (0, 2));
    for g in [&with_row, &plain] {
        assert_eq!(
            state(&w, g).await,
            (GrantState::Revoked, Some("station_updated".to_owned()))
        );
    }
    let dropped = rows(&w, "coordinator.grant_dropped").await;
    assert_eq!(dropped.len(), 2);
    assert!(
        dropped
            .iter()
            .all(|(_, d)| { d == "reason=station_updated;station=st-1;from=v1;to=v2" })
    );
    assert_eq!(
        empty_sets(&w).await,
        vec![(with_row.grant_id.clone(), DeckCommandState::Pending)]
    );
    assert_eq!(revoke_items(&w).await, vec![plain.grant_id.clone()]);
    assert!(!has_coordinate(&w, &w.a).await);
    let text = query(&w, &plain).await.unwrap_err();
    assert_eq!(text, end_text(GrantEnd::StationUpdated));
}

#[tokio::test]
async fn a_new_incarnation_drops_the_grants_with_either_setting() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", 24 * 3_600_000, true).await;
    up(&w, &w.a, 1);
    answer(&w, &w.a, 1, "inc-1", Some("v1")).await;
    let r = answer(&w, &w.a, 1, "inc-2", Some("v1")).await;
    assert_eq!((r.kept, r.dropped), (0, 1));
    assert_eq!(
        state(&w, &g).await,
        (GrantState::Revoked, Some("station_reset".to_owned()))
    );
    assert_eq!(
        rows(&w, "coordinator.grant_dropped").await,
        vec![(g.grant_id, "reason=station_reset;station=st-1".to_owned())]
    );
}

#[tokio::test]
async fn a_station_without_a_version_is_never_updated() {
    let w = world().await;
    keep_off(&w);
    let g = grant(&w, &w.a, "coord-1", 24 * 3_600_000, true).await;
    up(&w, &w.a, 1);
    for version in [None, None, Some("v1"), None] {
        let r = answer(&w, &w.a, 1, "inc-1", version).await;
        assert_eq!(r.dropped, 0);
    }
    assert_eq!(get(&w, &g).await.state, GrantState::Active);
    assert_eq!(info(&w, &w.a).await.1.as_deref(), Some("v1"));
}

#[tokio::test]
async fn the_first_answer_after_the_upgrade_only_records() {
    let w = world().await;
    keep_off(&w);
    let g = grant(&w, &w.a, "coord-1", 24 * 3_600_000, true).await;
    up(&w, &w.a, 1);
    assert_eq!(info(&w, &w.a).await, (None, None));
    let r = answer(&w, &w.a, 1, "inc-1", Some("v1")).await;
    assert_eq!((r.kept, r.dropped), (0, 0));
    assert_eq!(
        info(&w, &w.a).await,
        (Some("inc-1".to_owned()), Some("v1".to_owned()))
    );
    assert_eq!(get(&w, &g).await.state, GrantState::Active);
    assert!(rows(&w, "coordinator.grant_dropped").await.is_empty());
}

#[tokio::test]
async fn an_answer_of_an_ended_link_settles_nothing() {
    let w = world().await;
    held(&w, 50).await;
    up(&w, &w.a, 1);
    let r = answer(&w, &w.a, 2, "inc-1", Some("v1")).await;
    assert_eq!(r, Resolved::default());
    assert_eq!(info(&w, &w.a).await, (None, None));
    assert_eq!(holds(&w).await, 1);
}

#[tokio::test]
async fn the_hold_never_revives_an_ended_grant() {
    let w = world().await;
    let idle = grant(&w, &w.a, "coord-1", 24 * 3_600_000, true).await;
    let expired = grant(&w, &w.a, "coord-2", 24 * 3_600_000, true).await;
    let revoked = grant(&w, &w.a, "coord-3", 24 * 3_600_000, true).await;
    set_state(&w, &idle, "expired", Some("idle")).await;
    set_state(&w, &expired, "expired", Some("expired")).await;
    set_state(&w, &revoked, "revoked", Some("owner")).await;
    heard(&w, &w.a, w.s.now_ms()).await;
    advance(&w, 50);
    sweep(&w).await;
    up(&w, &w.a, 1);
    answer(&w, &w.a, 1, "inc-1", Some("v1")).await;
    assert_eq!(
        state(&w, &idle).await,
        (GrantState::Expired, Some("idle".to_owned()))
    );
    assert_eq!(
        state(&w, &expired).await,
        (GrantState::Expired, Some("expired".to_owned()))
    );
    assert_eq!(
        state(&w, &revoked).await,
        (GrantState::Revoked, Some("owner".to_owned()))
    );
    assert!(rows(&w, "coordinator.grant_kept").await.is_empty());
}

#[tokio::test]
async fn the_sweep_prunes_the_holds_of_ended_grants() {
    let w = world().await;
    let g = held(&w, 50).await;
    assert_eq!(holds(&w).await, 1);
    assert!(
        w.s.revoke_grant(g.grant_id, "owner".to_owned())
            .await
            .unwrap()
    );
    sweep(&w).await;
    assert_eq!(holds(&w).await, 0);
}

#[tokio::test]
async fn a_clock_behind_the_last_frame_holds_nothing() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", 24 * 3_600_000, true).await;
    let before = get(&w, &g).await.last_used_at;
    heard(&w, &w.a, w.s.now_ms() + 10_000).await;
    sweep(&w).await;
    assert_eq!(get(&w, &g).await.last_used_at, before);
    assert_eq!(holds(&w).await, 0);
}

/// A bound grant of `session` at generation 1 that idled out, with a pending
/// renewal at generation 2. `row` gives it a `session_owner_grant` row.
async fn pending_renewal(w: &World, row: bool) -> Grant {
    let g = grant(w, &w.a, "coord-1", 60_000, true).await;
    if row {
        give_row(w, &g).await;
    }
    advance(w, 61);
    sweep(w).await;
    assert_eq!(
        state(w, &g).await,
        (GrantState::Expired, Some("idle".to_owned()))
    );
    assert_eq!(w.s.renew_grant(g.grant_id.clone()).await.unwrap(), Some(2));
    g
}

fn bound_gen_2(w: &World, g: &Grant) -> EventEnvelope {
    EventEnvelope {
        station_seq: 1,
        session_id: g.session_id.clone(),
        session_seq: 1,
        at: w.s.now_ms(),
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::CoordinatorBound {
            binding_id: "b-1".to_owned(),
            grant_id: g.grant_id.clone(),
            generation: 2,
            provider_instance: "claude".to_owned(),
            native_id: "native-1".to_owned(),
        }),
    }
}

#[tokio::test]
async fn a_drop_revokes_an_expired_grant_with_a_pending_renewal() {
    for (row, off, second, reason) in [
        (false, true, ("inc-1", "v2"), "station_updated"),
        (false, false, ("inc-2", "v1"), "station_reset"),
        (true, true, ("inc-1", "v2"), "station_updated"),
        (true, false, ("inc-2", "v1"), "station_reset"),
    ] {
        let w = world().await;
        if off {
            keep_off(&w);
        }
        let g = pending_renewal(&w, row).await;
        up(&w, &w.a, 1);
        answer(&w, &w.a, 1, "inc-1", Some("v1")).await;
        let r = answer(&w, &w.a, 1, second.0, Some(second.1)).await;
        assert_eq!(r.dropped, 1);
        assert_eq!(
            state(&w, &g).await,
            (GrantState::Revoked, Some(reason.to_owned()))
        );
        assert_eq!(rows(&w, "coordinator.grant_dropped").await.len(), 1);
        if row {
            assert_eq!(
                empty_sets(&w).await,
                vec![(g.grant_id.clone(), DeckCommandState::Pending)]
            );
            assert!(revoke_items(&w).await.is_empty());
        }
        let env = bound_gen_2(&w, &g);
        w.s.apply_events(w.a.clone(), "inc".to_owned().into(), vec![env])
            .await
            .unwrap();
        assert_eq!(
            state(&w, &g).await,
            (GrantState::Revoked, Some(reason.to_owned()))
        );
        assert_eq!(rows(&w, "coordinator.grant_dropped").await.len(), 1);
    }
}

#[tokio::test]
async fn a_reconnect_before_the_first_sweep_keeps_a_grant_near_its_idle_deadline() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", 60_000, true).await;
    advance(&w, 55);
    heard(&w, &w.a, w.s.now_ms()).await;
    advance(&w, 10);
    assert!(!w.s.hold_returning(w.a.clone()).await.unwrap());
    let got = get(&w, &g).await;
    assert_eq!(got.state, GrantState::Active);
    assert_eq!(got.last_used_at, Some(w.s.now_ms()));
    assert_eq!(holds(&w).await, 0);
    up(&w, &w.a, 1);
    sweep(&w).await;
    assert_eq!(get(&w, &g).await.state, GrantState::Active);
}

#[tokio::test]
async fn a_gap_of_thirty_seconds_or_more_at_the_link_start_writes_one_kept_row() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", 60_000, true).await;
    heard(&w, &w.a, w.s.now_ms()).await;
    advance(&w, 40);
    w.s.hold_returning(w.a.clone()).await.unwrap();
    assert_eq!(holds(&w).await, 1);
    up(&w, &w.a, 1);
    let r = answer(&w, &w.a, 1, "inc-1", Some("v1")).await;
    assert_eq!((r.kept, r.dropped), (1, 0));
    assert_eq!(
        rows(&w, "coordinator.grant_kept").await,
        vec![(
            g.grant_id,
            "reason=station_offline;station=st-1;offline_ms=40000".to_owned()
        )]
    );
}

#[tokio::test]
async fn hold_returning_says_whether_the_station_returns() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", 24 * 3_600_000, true).await;
    assert!(!w.s.hold_returning(w.a.clone()).await.unwrap());
    up(&w, &w.a, 1);
    answer(&w, &w.a, 1, "inc-1", Some("v1")).await;
    assert!(w.s.hold_returning(w.a.clone()).await.unwrap());
    assert!(!w.s.hold_returning("nope".to_owned().into()).await.unwrap());
    // Another live link: no touch, still `true`.
    let used = get(&w, &g).await.last_used_at;
    heard(&w, &w.a, w.s.now_ms()).await;
    advance(&w, 10);
    assert!(w.s.hold_returning(w.a.clone()).await.unwrap());
    assert_eq!(get(&w, &g).await.last_used_at, used);
}

#[tokio::test]
async fn the_settle_state_opens_only_after_a_committed_check() {
    let w = world().await;
    w.s.mark_unsettled(&w.a, 1);
    up(&w, &w.a, 1);
    assert!(!w.s.link_settled(&w.a, 1));
    assert!(!w.s.station_settled(&w.a));
    let r = answer(&w, &w.a, 1, "inc-1", Some("v1")).await;
    assert!(r.settled);
    assert!(w.s.link_settled(&w.a, 1) && w.s.station_settled(&w.a));
    assert!(!answer(&w, &w.a, 1, "inc-1", Some("v1")).await.settled);

    // A link that was marked and ended is never settled and blocks nothing.
    w.s.mark_unsettled(&w.a, 2);
    w.s.set_link_features(&w.a, 2, vec![]);
    down(&w, &w.a, 2);
    assert!(!w.s.link_settled(&w.a, 2));
    assert!(w.s.station_settled(&w.a));

    // The wait ends at the deadline, or when the check commits.
    w.s.mark_unsettled(&w.a, 3);
    w.s.set_link_features(&w.a, 3, vec![]);
    assert!(
        !w.s.wait_link_settled(&w.a, 3, Duration::from_millis(50))
            .await
    );
    let (s, a) = (w.s.clone(), w.a.clone());
    let task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        s.resolve_grant_holds(a, 3, "inc-1".to_owned().into(), Some("v1".to_owned()))
            .await
            .unwrap()
    });
    assert!(w.s.wait_link_settled(&w.a, 3, Duration::from_secs(5)).await);
    assert!(task.await.unwrap().settled);

    // An answer of a link that is not live leaves the mark of the live one.
    w.s.mark_unsettled(&w.b, 4);
    up(&w, &w.b, 4);
    let r = answer(&w, &w.b, 5, "inc-1", Some("v1")).await;
    assert!(!r.settled);
    assert!(!w.s.link_settled(&w.b, 4));
}

#[path = "grant_hold_review_tests.rs"]
mod review;
