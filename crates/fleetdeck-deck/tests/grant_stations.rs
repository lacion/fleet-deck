//! Coordinator grant target stations (design `grant-stations.md`, ADR-006
//! §"Creation", DL-230 item 7): the start request names target stations; the
//! deck stores them, gives each station the derived `coordinate` scope, keeps
//! the link cache in step, and checks a grant in effect for every admission.
//! (The scope rules of the store are in `src/store/grant_scopes_tests.rs`.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use common::board::board;
use common::fleet::{Rig, bound_edit, rig, stop, wire};
use common::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_deck::store::{AuditResult, DeckStore, GrantClass, NewGrant};
use fleetdeck_link::grants::GrantSource;
use fleetdeck_proto::command::{GrantScope, RETRY_WINDOW_MS};
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::mutation::{
    AuthorityCheck, CoordinatorBinding, CoordinatorCall, CoordinatorCallEnvelope, CoordinatorQuery,
};
use fleetdeck_proto::version::feature::COORDINATOR_V2;
use fleetdeck_proto::{CommandId, Receipt, RejectCode, StationId};
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

// PRE-PR round 1 (GS2): the exact rule reads the clock inside the decision's
// own transaction, also after the waits of the admission.

/// A database job that holds the database thread until [`HeldDb::release`].
/// The first poll submits the job, so it is queued before this function
/// returns and every later database call waits behind it.
struct HeldDb {
    release: std::sync::mpsc::Sender<()>,
    job: std::pin::Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl HeldDb {
    async fn release(self) {
        self.release.send(()).unwrap();
        self.job.await;
    }
}

fn hold_db(store: &DeckStore) -> HeldDb {
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let db = store.db();
    let mut job: std::pin::Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
        db.call(move |_| {
            let _ = wait.recv();
            Ok(())
        })
        .await
        .unwrap();
    });
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(job.as_mut().poll(&mut cx).is_pending());
    HeldDb { release, job }
}

/// Runs `work` while the database thread is held, lets the clock go forward
/// by `by` after `work` has started, and returns the result of `work`.
async fn lapse_during<T: Send + 'static>(
    f: &Fixture,
    by: Duration,
    work: impl Future<Output = T> + Send + 'static,
) -> T {
    let held = hold_db(&f.store);
    let task = tokio::spawn(work);
    tokio::time::sleep(Duration::from_millis(100)).await;
    f.det.advance(by);
    held.release().await;
    task.await.unwrap()
}

/// A fleet grant of `coord-1` on the fixture station with the given times.
async fn short_grant(f: &Fixture, ttl_ms: i64, idle_ttl_ms: i64) -> CoordinatorBinding {
    bound_edit(f, &f.station, "coord-1", &[GrantClass::Drive], |g| {
        g.scope = GrantScope::Fleet;
        g.ttl_ms = ttl_ms;
        g.idle_ttl_ms = idle_ttl_ms;
    })
    .await
}

fn stop_call(f: &Fixture, b: &CoordinatorBinding, id: &str) -> CoordinatorCallEnvelope {
    CoordinatorCallEnvelope {
        command_id: CommandId::from(id),
        target_incarnation: f.store.incarnation().clone(),
        retry_until: f.store.now_ms() + RETRY_WINDOW_MS,
        call: CoordinatorCall {
            caller: b.native_id.strip_prefix("n-").unwrap().into(),
            binding: b.clone(),
            idempotency_key: None,
            op: stop(f, "w1"),
        },
    }
}

fn assert_refused(receipt: &Receipt, text: &str) {
    let reject = receipt.reject.as_ref().expect("the call must be refused");
    assert_eq!(reject.code, RejectCode::Forbidden, "{reject:?}");
    assert_eq!(reject.message, text);
}

const LIFETIME_TEXT: &str =
    "Your fleet grant expired (its lifetime ended). The owner can give a new grant on the board.";
const IDLE_TEXT: &str =
    "Your fleet grant expired (idle for 1 second). The owner can renew it on the board.";

#[tokio::test]
async fn a_call_is_refused_when_its_grant_lapses_while_it_waits_for_its_session_read() {
    let r = rig().await;
    let b = short_grant(&r.f, 5_000, 600_000).await;
    let (store, station) = (r.f.store.clone(), r.f.station.clone());
    let envelope = stop_call(&r.f, &b, "k1");
    let receipt = lapse_during(&r.f, Duration::from_millis(5_001), async move {
        store.decide_coordinator(station, envelope).await.unwrap()
    })
    .await;
    assert_refused(&receipt, LIFETIME_TEXT);
}

#[tokio::test]
async fn a_call_is_refused_when_its_grant_goes_idle_while_it_waits_for_its_session_read() {
    let r = rig().await;
    let b = short_grant(&r.f, 600_000, 1_000).await;
    let first = common::fleet::call(&r.f, &b, "k0", stop(&r.f, "w1")).await;
    assert!(first.reject.is_none(), "{first:?}");
    let (store, station) = (r.f.store.clone(), r.f.station.clone());
    let envelope = stop_call(&r.f, &b, "k1");
    let receipt = lapse_during(&r.f, Duration::from_millis(1_001), async move {
        store.decide_coordinator(station, envelope).await.unwrap()
    })
    .await;
    assert_refused(&receipt, IDLE_TEXT);
}

#[tokio::test]
async fn a_query_is_refused_when_its_grant_lapses_while_it_waits_for_the_database() {
    let r = rig().await;
    let b = short_grant(&r.f, 2_000, 600_000).await;
    let (store, station) = (r.f.store.clone(), r.f.station.clone());
    let envelope = common::fleet::query(&r.f, &b, CoordinatorQuery::ListSessions);
    let answer = lapse_during(&r.f, Duration::from_millis(2_001), async move {
        store.coordinator_query(station, envelope).await.unwrap()
    })
    .await;
    let Err(reject) = answer else {
        panic!("the lapsed grant must not answer a query");
    };
    assert_eq!(reject.code, RejectCode::Forbidden);
    assert_eq!(reject.message, LIFETIME_TEXT);
}

/// `coordinator.authority_check` whose `coordinator.policy_now` answer comes
/// only after `during` ran; the wire outcome, also an error.
async fn authority_held(
    r: &mut Rig,
    check: &AuthorityCheck,
    during: impl Future<Output = ()>,
) -> Outcome {
    let control = r.station.control.clone();
    let ask = control.request("coordinator.authority_check", None, json!(check));
    let serve = async {
        let req = next_request(&mut r.station).await;
        assert_eq!(req.method, "coordinator.policy_now");
        during.await;
        let policy = json!({"fleet_control": true, "bound": true});
        r.station
            .control
            .respond(req.id, Outcome::Ok(policy))
            .await
            .unwrap();
    };
    let (out, ()) = tokio::join!(ask, serve);
    out.unwrap()
}

/// A rig with a sent command whose authority the fake station checks.
async fn sent_command(ttl_ms: i64, idle_ttl_ms: i64) -> (Rig, AuthorityCheck) {
    let mut r = rig().await;
    let b = short_grant(&r.f, ttl_ms, idle_ttl_ms).await;
    let receipt = common::fleet::call(&r.f, &b, "k1", stop(&r.f, "w1")).await;
    assert!(receipt.reject.is_none(), "{receipt:?}");
    let (id, _) = wire(&mut r).await;
    let check = AuthorityCheck {
        grant_id: b.grant_id,
        generation: 1,
        caller_station: r.f.station.clone(),
        caller_session: "coord-1".into(),
        target_session: "w1".into(),
        command_id: id,
        launch: None,
    };
    (r, check)
}

#[tokio::test]
async fn an_authority_check_is_refused_when_its_grant_goes_idle_while_the_deck_waits_for_policy_now()
 {
    let (mut r, check) = sent_command(600_000, 1_000).await;
    // No change: valid.
    let got = authority_held(&mut r, &check, async {}).await;
    assert!(
        matches!(&got, Outcome::Ok(v) if v["valid"] == true),
        "{got:?}"
    );
    // The grant is idle-expired when `policy_now` answers; the row is still
    // `active` and the sweep has not run.
    let det = r.f.det.clone();
    let idle = async move { det.advance(Duration::from_millis(1_001)) };
    let Outcome::Err(refused) = authority_held(&mut r, &check, idle).await else {
        panic!("the idle grant must not cover the station");
    };
    assert_eq!(refused.code, "forbidden");
    assert_eq!(
        refused.message,
        "No coordinator grant covers this station now."
    );
}

#[tokio::test]
async fn an_authority_check_is_invalid_when_its_grant_lapses_while_the_deck_waits_for_policy_now() {
    let (mut r, check) = sent_command(5_000, 600_000).await;
    let det = r.f.det.clone();
    let lapse = async move { det.advance(Duration::from_millis(5_001)) };
    let got = authority_held(&mut r, &check, lapse).await;
    assert!(
        matches!(&got, Outcome::Ok(v) if v["valid"] == false && v["reason"] == "grant_expired"),
        "{got:?}"
    );
}

#[tokio::test]
async fn refresh_changed_returns_only_after_the_batch_that_the_follower_is_publishing() {
    let mut f = fixture().await;
    let station = f.connect().await;
    let other = extra(&f.store, "other", true).await;
    let rec = f.store.station(f.station.clone()).await.unwrap().unwrap();
    let fingerprint = rec.key.fingerprint();
    let mut g = NewGrant::new(other, "coord-1".into());
    g.stations.insert(f.station.clone());
    f.store.create_grant(g).await.unwrap();
    // The follower is woken; it has not run yet (one test thread). Hold the
    // database before it reads, so it stops in the middle of its batch with
    // the noted set already drained.
    let held = hold_db(&f.store);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let peers = Arc::clone(&f.ctx.peers);
    let explicit = tokio::spawn(async move { peers.refresh_changed().await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !explicit.is_finished(),
        "an explicit refresh must wait for the batch in progress"
    );
    assert!(
        !f.ctx
            .peers
            .scopes(&fingerprint)
            .contains(&Scope::Coordinate)
    );
    held.release().await;
    explicit.await.unwrap().unwrap();
    // The authority check of the target runs at once after the refresh.
    assert!(
        f.ctx
            .peers
            .scopes(&fingerprint)
            .contains(&Scope::Coordinate)
    );
    match ask(&f, &station).await {
        Outcome::Ok(v) => assert_eq!(v["valid"], false, "{v}"),
        Outcome::Err(e) => panic!("{e}"),
    }
}
