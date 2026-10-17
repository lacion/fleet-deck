//! Coordinator grant target stations (design `grant-stations.md`, ADR-006
//! §"Creation", DL-230 item 7): the start request names target stations; the
//! deck stores them, gives each station the derived `coordinate` scope, keeps
//! the link cache in step, and checks a grant in effect for every admission.
//! (The scope rules of the store are in `src/store/grant_scopes_tests.rs`.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::board::board;
use common::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_deck::store::{AuditResult, DeckStore, NewGrant};
use fleetdeck_link::grants::GrantSource;
use fleetdeck_proto::StationId;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::mutation::AuthorityCheck;
use fleetdeck_proto::version::feature::COORDINATOR_V2;
use serde::Deserialize;
use serde_json::{Value, json};
use settings_support::*;

const START: &str = "/api/commands/session.start";

fn start_body(id: &str, targets: &[&StationId], elevation: Option<&str>) -> Value {
    let spec = json!({"grant_id": "", "classes": ["read", "mail"], "idle_ttl_ms": 0,
        "expires_at": 0, "control_skill": {"version": "", "text": ""}, "stations": targets});
    let mut body = json!({"station": "fake", "command_id": id, "command": {"op": "start",
        "prompt": null, "spec": {"provider_instance": "claude", "cwd": "/w", "mode": null,
        "model": null, "title": null, "coordinator": spec}}});
    if let Some(e) = elevation {
        body["elevation"] = json!(e);
    }
    body
}

/// A paired station of the deck; `v2` says whether its link negotiated
/// `coordinator.v2`.
async fn extra(store: &DeckStore, name: &str, v2: bool) -> StationId {
    let rec = store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: name.to_owned(),
            scopes: BTreeSet::from([Scope::Admin]),
            dial_url: None,
        })
        .await
        .unwrap();
    if v2 {
        store.set_link_features(&rec.station_id, 1, vec![COORDINATOR_V2.to_owned()]);
    }
    rec.station_id
}

/// The coordinator spec of the station command as the station decoded it
/// before the `stations` field existed (both `coordinator.v1` and the
/// earlier `coordinator.v2` decoders refuse unknown fields).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct LegacySpec {
    grant_id: String,
    classes: Vec<String>,
    idle_ttl_ms: u64,
    expires_at: i64,
    control_skill: LegacySkill,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    answer_cap: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct LegacySkill {
    version: String,
    text: String,
}

#[tokio::test]
async fn the_start_request_names_target_stations_that_the_deck_validates_stores_echoes_and_audits()
{
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    let beta = extra(&f.store, "beta", true).await;
    let no_v2 = extra(&f.store, "no-v2", false).await;
    let unknown = StationId::from("nope");

    // An unknown station (400), more than 16 stations (400).
    let r = post(&c, &h, START, start_body("t1", &[&unknown], Some(&token))).await;
    expect(&r, 400, "invalid_grant");
    assert!(
        r.body.to_string().contains("Unknown station nope."),
        "{}",
        r.body
    );
    let mut many = vec![beta.clone()];
    for i in 0..15 {
        many.push(extra(&f.store, &format!("many-{i}"), true).await);
    }
    // Sixteen targets and the coordinator's own station are 17.
    let refs: Vec<&StationId> = many.iter().collect();
    let r = post(&c, &h, START, start_body("t2", &refs, Some(&token))).await;
    expect(&r, 400, "invalid_grant");
    assert!(
        r.body
            .to_string()
            .contains("A grant has at most 16 stations.")
    );
    // Targets need the owner's elevation, also for a lineage grant.
    let r = post(&c, &h, START, start_body("t3", &[&beta], None)).await;
    expect(&r, 400, "elevation_required");
    // Every station of the grant needs a link that negotiated v2.
    let r = post(&c, &h, START, start_body("t4", &[&no_v2], Some(&token))).await;
    expect(&r, 409, "unsupported");
    no_request(&mut station, Duration::from_millis(200)).await;
    assert!(c.store.grants(10).await.unwrap().is_empty());

    // A valid request: the station command never carries the list.
    let call = post(&c, &h, START, start_body("t5", &[&beta], Some(&token)));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let sent = req.params["command"]["spec"]["coordinator"].clone();
    assert!(sent.get("stations").is_none(), "{sent}");
    serde_json::from_value::<LegacySpec>(sent.clone()).expect("the unchanged decoders accept it");
    let mut with_list = sent;
    with_list["stations"] = json!([beta]);
    assert!(serde_json::from_value::<LegacySpec>(with_list).is_err());

    // The stored list, the echo of the grant routes, the scopes.
    let g = c.store.grants(10).await.unwrap().remove(0);
    let want: BTreeSet<StationId> = BTreeSet::from([f.station.clone(), beta.clone()]);
    assert_eq!(g.stations, want);
    let listed = get(&c, &h, "/api/coordinator/grants").await;
    let echoed: BTreeSet<StationId> = listed.body[0]["stations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| StationId::from(s.as_str().unwrap()))
        .collect();
    assert_eq!(echoed, want);
    for st in [&f.station, &beta] {
        let rec = c.store.station(st.clone()).await.unwrap().unwrap();
        assert!(rec.scopes.contains(&Scope::Coordinate), "{st}");
    }
    let rows = c
        .audited("coordinator.grant_created", AuditResult::Ok)
        .await;
    let mut ids = [f.station.to_string(), beta.to_string()];
    ids.sort();
    assert_eq!(rows, [Some(format!("stations={}", ids.join(",")))]);
}

fn check(f: &Fixture) -> AuthorityCheck {
    AuthorityCheck {
        grant_id: "g".to_owned(),
        generation: 1,
        caller_station: f.station.clone(),
        caller_session: "coord-1".into(),
        target_session: "w1".into(),
        command_id: "c1".into(),
        launch: None,
    }
}

/// The wire answer of `coordinator.authority_check` from the fixture station.
async fn ask(f: &Fixture, station: &fleetdeck_link::LiveLink) -> Outcome {
    station
        .control
        .request("coordinator.authority_check", None, json!(check(f)))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_new_target_station_is_admitted_right_after_creation_and_refused_the_moment_its_grant_lapses()
 {
    let mut f = fixture().await;
    let station = f.connect().await;
    let other = extra(&f.store, "other", true).await;
    let Outcome::Err(refused) = ask(&f, &station).await else {
        panic!("a station without a grant is refused");
    };
    assert_eq!(refused.code, "forbidden");
    // The coordinator is on `other`; the fixture station is a target.
    let mut g = NewGrant::new(other, "coord-1".into());
    g.stations.insert(f.station.clone());
    g.ttl_ms = 5_000;
    f.store.create_grant(g).await.unwrap();
    f.ctx.peers.refresh_changed().await.unwrap();
    // Admitted without a restart: the answer is the authority check's own.
    match ask(&f, &station).await {
        Outcome::Ok(v) => assert_eq!(v["valid"], false, "{v}"),
        Outcome::Err(e) => panic!("{e}"),
    }
    // One millisecond after the grant lapses and before any sweep, the
    // exact rule refuses although the cache and the row still hold the scope.
    f.det.advance(Duration::from_millis(5_001));
    let Outcome::Err(lapsed) = ask(&f, &station).await else {
        panic!("the lapsed grant must not cover the station");
    };
    assert_eq!(lapsed.code, "forbidden");
    assert_eq!(
        lapsed.message,
        "No coordinator grant covers this station now."
    );
}

#[tokio::test]
async fn concurrent_creates_and_revocations_leave_the_link_cache_as_the_store_has_it() {
    let f = fixture().await;
    let other = extra(&f.store, "other", true).await;
    let rec = f.store.station(f.station.clone()).await.unwrap().unwrap();
    let fingerprint = rec.key.fingerprint();
    for _ in 0..10 {
        let mut g = NewGrant::new(other.clone(), "coord-1".into());
        g.stations.insert(f.station.clone());
        let created = f.store.create_grant(g).await.unwrap();
        let peers = Arc::clone(&f.ctx.peers);
        let revoke = async {
            f.store
                .revoke_grant(created.grant_id.clone(), "owner".to_owned())
                .await
                .unwrap();
            peers.refresh_changed().await.unwrap();
        };
        let refresh = async {
            peers.refresh(f.station.clone()).await.unwrap();
            peers.refresh_changed().await.unwrap();
        };
        let (_, _, third) = tokio::join!(revoke, refresh, peers.refresh(f.station.clone()));
        third.unwrap();
        peers.refresh_changed().await.unwrap();
        let stored = f.store.station(f.station.clone()).await.unwrap().unwrap();
        assert_eq!(peers.scopes(&fingerprint), stored.scopes);
        assert!(!stored.scopes.contains(&Scope::Coordinate));
    }
}

#[tokio::test]
async fn deck_start_ends_lapsed_grants_and_removes_stale_scopes_of_stations_that_are_offline() {
    let det = fleetdeck_testkit::Det::new();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.db");
    let store = DeckStore::open(&path, det.clock(), det.ids())
        .await
        .unwrap();
    let pin = |name: &'static str| {
        let store = store.clone();
        async move {
            store
                .pin_station(NewStation {
                    key: Identity::generate().unwrap().public_key(),
                    name: name.to_owned(),
                    scopes: BTreeSet::from([Scope::Admin]),
                    dial_url: None,
                })
                .await
                .unwrap()
                .station_id
        }
    };
    let (a, b, stale) = (pin("alpha").await, pin("beta").await, pin("stale").await);
    let mut g = NewGrant::new(a.clone(), "coord-1".into());
    g.stations.insert(b.clone());
    g.ttl_ms = 1_000;
    let grant = store.create_grant(g).await.unwrap();
    // Older code left `coordinate` on a station that no grant names.
    let id = stale.clone();
    store
        .db()
        .call(move |conn| {
            conn.execute(
                "UPDATE stations SET scopes = ?2 WHERE station_id = ?1",
                rusqlite::params![id.as_str(), r#"["admin","coordinate"]"#],
            )
            .map(drop)
            .map_err(Into::into)
        })
        .await
        .unwrap();
    for st in [&a, &b] {
        let rec = store.station(st.clone()).await.unwrap().unwrap();
        assert!(rec.scopes.contains(&Scope::Coordinate));
    }
    drop(store);
    // The deck is down while the grant lapses; no station is linked at start.
    det.advance(Duration::from_secs(2));
    let store = DeckStore::open(&path, det.clock(), det.ids())
        .await
        .unwrap();
    for st in [&a, &b, &stale] {
        let rec = store.station(st.clone()).await.unwrap().unwrap();
        assert!(!rec.scopes.contains(&Scope::Coordinate), "{st}");
        assert!(rec.scopes.contains(&Scope::Admin), "{st}");
    }
    let got = store.grant(grant.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(got.revoked_reason.as_deref(), Some("expired"));
    assert!(store.take_scope_changes().is_empty());
    let rows = store.audit_log(10).await.unwrap();
    assert!(rows.iter().any(|r| r.action == "coordinator.grant_expired"
        && r.actor == "deck"
        && r.target.as_deref() == Some(grant.grant_id.as_str())));
}

#[tokio::test]
async fn the_sweep_task_ends_lapsed_grants_and_the_link_cache_follows() {
    let f = fixture().await;
    let other = extra(&f.store, "other", true).await;
    let rec = f.store.station(f.station.clone()).await.unwrap().unwrap();
    let fingerprint = rec.key.fingerprint();
    let mut g = NewGrant::new(other, "coord-1".into());
    g.stations.insert(f.station.clone());
    g.ttl_ms = 1_000;
    f.store.create_grant(g).await.unwrap();
    f.ctx.peers.refresh_changed().await.unwrap();
    assert!(
        f.ctx
            .peers
            .scopes(&fingerprint)
            .contains(&Scope::Coordinate)
    );
    f.det.advance(Duration::from_secs(2));
    let (stop, rx) = tokio::sync::watch::channel(false);
    let task = fleetdeck_deck::serve::spawn_grant_sweep(
        f.store.clone(),
        Arc::clone(&f.ctx.peers),
        Duration::from_millis(10),
        rx,
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while f
        .ctx
        .peers
        .scopes(&fingerprint)
        .contains(&Scope::Coordinate)
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the sweep did not run"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let stored = f.store.station(f.station.clone()).await.unwrap().unwrap();
    assert!(!stored.scopes.contains(&Scope::Coordinate));
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}
