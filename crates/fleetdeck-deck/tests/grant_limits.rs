//! B27 T3 (security-options §3.2, §3.3): the deck reads the grant limits from
//! its settings. The worker attach with "no limit" and the count of worker
//! grants, the resolved defaults over the board routes, and the V52 migration
//! of the `tokens` checks. (The rules without a database are in
//! `src/store/grant_limits_tests.rs`.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, board};
use common::*;
use fleetdeck_core::config_grant_limits::GrantLimits;
use fleetdeck_core::config_worker_grant::WorkerGrantConfig;
use fleetdeck_deck::store::grant_scopes::in_effect;
use fleetdeck_deck::store::worker_grant::{Provenance, WorkerStart};
use fleetdeck_deck::store::{DeckCommand, DeckStore, Grant, GrantClass};
use fleetdeck_proto::CommandId;
use fleetdeck_proto::command::{GrantKind, SessionCommand, SessionSpec};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::grant_limit::NO_LIMIT;
use fleetdeck_proto::version::feature;
use rusqlite::Connection;
use serde_json::{Value, json};
use settings_support::*;

const ELEVATION: &str = "x-fleetdeck-elevation";
const HOUR: i64 = 60 * 60 * 1000;
const OVER_ANSWER: &str =
    "answer_cap is above the grant limit of the deck (Settings, Grant limits)";
const OVER_SPAWN: &str = "spawn_cap is above the grant limit of the deck (Settings, Grant limits)";
const START: &str = "/api/commands/session.start";

/// Replaces the grant limits of the deck store and of the board.
fn set_limits(store: &DeckStore, c: Option<&BoardClient>, change: impl Fn(&mut GrantLimits)) {
    let mut knobs = store.knobs();
    change(&mut knobs.grant);
    store.set_knobs(knobs);
    if let Some(c) = c {
        let mut live = (*c.state.live.get()).clone();
        change(&mut live.knobs.grant);
        c.state.live.publish(live);
    }
}

// ---------------------------------------------------------------------------
// The worker attach
// ---------------------------------------------------------------------------

fn start(
    id: &str,
    setting: WorkerGrantConfig,
    station: &fleetdeck_proto::StationId,
) -> WorkerStart {
    WorkerStart {
        station: station.clone(),
        command: SessionCommand::Start {
            spec: SessionSpec {
                effort: None,
                provider_instance: "claude".into(),
                cwd: "/w".to_owned(),
                mode: None,
                model: None,
                title: None,
                coordinator: None,
                launch: None,
                extra_env: None,
            },
            prompt: None,
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
            fleet_git: None,
        },
        command_id: CommandId::from(id),
        provenance: Provenance::Owner,
        author: Some("owner".to_owned()),
        setting,
    }
}

fn carries_grant(c: &DeckCommand) -> bool {
    c.envelope["command"]["spec"]
        .get("coordinator")
        .is_some_and(|v| !v.is_null())
}

async fn worker_grants(store: &DeckStore) -> Vec<Grant> {
    let all = store.grants(1000).await.unwrap();
    all.into_iter()
        .filter(|g| g.kind == GrantKind::Worker)
        .collect()
}

/// Test 2.
#[tokio::test]
async fn a_worker_grant_with_no_spawn_limit_stores_no_limit() {
    let mut f = fixture().await;
    let _station = f.connect().await;
    // The owner limit stays at its default 50: it never applies to a worker grant.
    set_limits(&f.store, None, |l| l.worker_spawn_cap = None);
    let setting = WorkerGrantConfig {
        spawn_cap: None,
        ..WorkerGrantConfig::default()
    };
    let out = f
        .store
        .submit_start_with_worker_decision(start("n1", setting, &f.station))
        .await
        .unwrap();
    assert!(carries_grant(&out.command));
    let g = worker_grants(&f.store).await.remove(0);
    assert_eq!(g.spawn_cap, NO_LIMIT);
    assert_eq!(f.store.knobs().grant.spawn_cap, Some(50));
}

/// Test 3.
#[tokio::test]
async fn no_idle_limit_stores_the_lifetime() {
    let mut f = fixture().await;
    let _station = f.connect().await;
    set_limits(&f.store, None, |l| l.idle_hours = None);
    let setting = WorkerGrantConfig {
        idle_minutes: None,
        lifetime_hours: 48,
        ..WorkerGrantConfig::default()
    };
    f.store
        .submit_start_with_worker_decision(start("i1", setting, &f.station))
        .await
        .unwrap();
    let g = worker_grants(&f.store).await.remove(0);
    assert_eq!(g.idle_ttl_ms, 48 * HOUR);
    // After 25 h without use the grant is still in effect; after 48 h it is not.
    f.det
        .advance(Duration::from_millis(u64::try_from(25 * HOUR).unwrap()));
    assert!(in_effect(&g, f.store.now_ms()));
    f.det
        .advance(Duration::from_millis(u64::try_from(23 * HOUR).unwrap() + 1));
    assert!(!in_effect(&g, f.store.now_ms()));
}

/// Test 4.
#[tokio::test]
async fn the_worker_grant_count_follows_the_setting() {
    let mut f = fixture().await;
    let _station = f.connect().await;
    set_limits(&f.store, None, |l| l.worker_grants = Some(2));
    for n in 0..2 {
        let out = f
            .store
            .submit_start_with_worker_decision(start(
                &format!("c{n}"),
                WorkerGrantConfig::default(),
                &f.station,
            ))
            .await
            .unwrap();
        assert!(carries_grant(&out.command), "start {n}");
    }
    let third = f
        .store
        .submit_start_with_worker_decision(start("c2", WorkerGrantConfig::default(), &f.station))
        .await
        .unwrap();
    assert!(!carries_grant(&third.command));
    let skipped: Vec<_> = f
        .store
        .audit_log(1000)
        .await
        .unwrap()
        .into_iter()
        .filter(|a| a.action == "coordinator.worker_grant_skipped")
        .collect();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].detail.as_deref(), Some("worker_cap"));
    // No limit: 201 worker grants are in effect.
    set_limits(&f.store, None, |l| l.worker_grants = None);
    for n in 3..202 {
        let out = f
            .store
            .submit_start_with_worker_decision(start(
                &format!("c{n}"),
                WorkerGrantConfig::default(),
                &f.station,
            ))
            .await
            .unwrap();
        assert!(carries_grant(&out.command), "start {n}");
    }
    assert_eq!(worker_grants(&f.store).await.len(), 201);
}

/// Review finding (B27): a deck.toml with `[grant_limits] worker_spawn_cap = 1`
/// and no `[worker_grant]` table gives a worker grant with the cap 1. Before
/// the fix the absent table kept the default cap 2 and the start failed.
#[tokio::test]
async fn an_absent_worker_table_starts_within_a_low_worker_spawn_limit() {
    let mut f = fixture().await;
    let _station = f.connect().await;
    let config =
        fleetdeck_core::config::DeckConfig::parse("[grant_limits]\nworker_spawn_cap = 1\n")
            .unwrap();
    assert_eq!(config.worker_grant.spawn_cap, Some(1));
    set_limits(&f.store, None, |l| l.worker_spawn_cap = Some(1));
    let out = f
        .store
        .submit_start_with_worker_decision(start("low1", config.worker_grant, &f.station))
        .await
        .unwrap();
    assert!(carries_grant(&out.command));
    let g = worker_grants(&f.store).await.remove(0);
    assert_eq!(g.spawn_cap, 1);
}

// ---------------------------------------------------------------------------
// The board routes: an omitted cap is resolved to the limit
// ---------------------------------------------------------------------------

const LIVE: [&str; 3] = [
    feature::COORDINATOR,
    feature::COORDINATOR_V2,
    feature::COORDINATOR_LIVE,
];

struct Rig {
    f: Fixture,
    c: BoardClient,
    h: Headers,
}

/// A deck whose station negotiated the live features and knows the session
/// `run-1`, with fleet control on.
async fn rig() -> Rig {
    let f = fixture().await;
    let mut card = fleetdeck_proto::SessionSummary::new("run-1".into());
    card.state = SessionState::Running;
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card])
        .await
        .unwrap();
    f.store
        .set_link_features(&f.station, 98, LIVE.map(str::to_owned).to_vec());
    f.store.set_fleet_control(true);
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    Rig { f, c, h }
}

fn spec(classes: Value, extra: Value) -> Value {
    let mut spec = json!({"grant_id": "", "classes": classes, "idle_ttl_ms": 0, "expires_at": 0,
        "control_skill": {"version": "", "text": ""}});
    spec.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    spec
}

impl Rig {
    /// A live give to `session` (the grant route of `grant_check.rs`).
    async fn give(&self, session: &str, body: Value, elevation: &str) -> Http {
        let path = format!(
            "/api/coordinator/sessions/{}/{session}/grant",
            self.f.station
        );
        let mut headers = refs(&self.h);
        headers.push((ELEVATION, elevation));
        self.c.post(&path, &headers, body).await
    }

    async fn newest(&self) -> Grant {
        self.f.store.grants(1).await.unwrap().remove(0)
    }
}

/// Test 11, the live give and the owner start.
#[tokio::test]
async fn an_omitted_answer_cap_is_lowered_to_the_limit() {
    let r = rig().await;
    set_limits(&r.f.store, Some(&r.c), |l| l.answer_cap = Some(1));
    let token = elevate(&r.c, &r.h).await;
    // The board grant route.
    let got = r
        .give("run-1", spec(json!(["answer"]), json!({})), &token)
        .await;
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(r.newest().await.answer_cap, Some(1));
    let got = r
        .give(
            "run-1",
            spec(json!(["answer"]), json!({"answer_cap": 2})),
            &token,
        )
        .await;
    expect(&got, 400, "invalid_grant");
    assert_eq!(got.body["message"], OVER_ANSWER, "{}", got.body);
}

#[tokio::test]
async fn an_omitted_answer_cap_is_lowered_on_the_owner_start_route() {
    let mut r = rig().await;
    let mut station = r.f.connect().await;
    set_limits(&r.f.store, Some(&r.c), |l| l.answer_cap = Some(1));
    let token = elevate(&r.c, &r.h).await;
    let body = |id: &str, extra: Value| {
        json!({"station": "fake", "command_id": id, "elevation": token,
            "command": {"op": "start", "prompt": null, "spec": {"provider_instance": "claude",
            "cwd": "/w", "mode": null, "model": null, "title": null,
            "coordinator": spec(json!(["answer"]), extra)}}})
    };
    let call = post(&r.c, &r.h, START, body("o1", json!({})));
    let (got, _) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(r.newest().await.answer_cap, Some(1));
    let got = post(&r.c, &r.h, START, body("o2", json!({"answer_cap": 2}))).await;
    expect(&got, 400, "invalid_grant");
    assert_eq!(got.body["message"], OVER_ANSWER, "{}", got.body);
}

/// Test 12, the live give and the owner start.
#[tokio::test]
async fn an_omitted_spawn_cap_is_lowered_to_the_limit() {
    let mut r = rig().await;
    let mut station = r.f.connect().await;
    set_limits(&r.f.store, Some(&r.c), |l| l.spawn_cap = Some(2));
    let token = elevate(&r.c, &r.h).await;
    let got = r
        .give("run-1", spec(json!(["read", "spawn"]), json!({})), &token)
        .await;
    assert_eq!(got.status, 200, "{}", got.body);
    let g = r.newest().await;
    assert!(g.classes.contains(&GrantClass::Spawn));
    assert_eq!(g.spawn_cap, 2);
    let got = r
        .give(
            "run-1",
            spec(json!(["read", "spawn"]), json!({"spawn_cap": 3})),
            &token,
        )
        .await;
    expect(&got, 400, "invalid_grant");
    assert_eq!(got.body["message"], OVER_SPAWN, "{}", got.body);
    // The owner start route.
    let body = |id: &str, classes: Value| {
        json!({"station": "fake", "command_id": id, "elevation": token,
            "command": {"op": "start", "prompt": null, "spec": {"provider_instance": "claude",
            "cwd": "/w", "mode": null, "model": null, "title": null,
            "coordinator": spec(classes, json!({}))}}})
    };
    let call = post(&r.c, &r.h, START, body("s1", json!(["read", "spawn"])));
    let (got, _) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(r.newest().await.spawn_cap, 2);
    // A grant without the spawn class is accepted: its unused cap is not checked.
    let call = post(&r.c, &r.h, START, body("s2", json!(["read"])));
    let (got, _) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(got.status, 200, "{}", got.body);
    assert!(!r.newest().await.classes.contains(&GrantClass::Spawn));
}

// ---------------------------------------------------------------------------
// Migration V52
// ---------------------------------------------------------------------------

/// Every column of every `tokens` row as text, by grant id.
fn dump(conn: &Connection) -> Vec<Vec<Option<String>>> {
    let mut stmt = conn.prepare("PRAGMA table_info(tokens)").unwrap();
    let names: Vec<String> = stmt
        .query_map([], |r| r.get(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let select = names
        .iter()
        .map(|n| format!("CAST({n} AS TEXT)"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn
        .prepare(&format!("SELECT {select} FROM tokens ORDER BY grant_id"))
        .unwrap();
    stmt.query_map([], |r| {
        (0..names.len())
            .map(|i| r.get::<_, Option<String>>(i))
            .collect()
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn insert(
    conn: &Connection,
    grant: &str,
    kind: &str,
    (spawn, idle, answer): (u64, u64, u64),
) -> rusqlite::Result<usize> {
    conn.execute(
        &format!(
            "INSERT INTO tokens (grant_id, station_id, session_id, classes, stations, spawn_cap,
                 spawn_used, max_modes, idle_ttl_ms, created_at, expires_at, state,
                 revoked_reason, bound_generation, bound_native_id, bound_provider_instance,
                 binding_id, last_used_at, renew_generation, start_command_id, scope, answer_cap,
                 answer_used, desktop_cap, desktop_used, capture_cap, capture_used,
                 image_byte_cap, image_byte_used, desktop_attempts, recording_cap, recording_used,
                 recording_second_cap, recording_second_used, allow_input, input_scope, input_cap,
                 input_used, input_character_cap, input_character_used, allow_click, kind)
             VALUES ('{grant}', 'st-1', 'se-{grant}', '[\"read\",\"spawn\"]', '[\"st-1\"]', {spawn},
                 2, '{{\"claude\":\"plan\"}}', {idle}, 1, 99, 'active', NULL, 1, 'native', 'claude',
                 'binding', 5, 3, 'start-{grant}', 'lineage', {answer}, 4, 10, 1, 11, 2, 1048576, 0, 1,
                 5, 1, 60, 3, 1, 'own_windows', 8, 2, 100, 20, 1, '{kind}')
             "
        ),
        [],
    )
}

/// Test 9.
#[test]
fn v52_keeps_every_grant_and_widens_the_checks() {
    let migrations = fleetdeck_deck::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    // The last version before V52 is V51 (B26, the grant holds).
    migrations.to_version(&mut conn, 51).unwrap();
    insert(&conn, "g-owner", "coordinator", (7, 60_000, 40)).unwrap();
    insert(&conn, "g-worker", "worker", (2, 60_000, 40)).unwrap();
    conn.execute(
        "INSERT INTO coordinator_lineage (command_id, grant_id, station_id, session_id,
             start_outbox_id, created_at)
         VALUES ('cmd-1', 'g-worker', 'st-1', 'se-g-worker', 'ob-1', 3)",
        [],
    )
    .unwrap();
    let before = dump(&conn);
    assert_eq!(before.len(), 2);

    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(dump(&conn), before, "every value is equal");
    // Both indexes are back.
    let mut stmt = conn.prepare("PRAGMA index_list(tokens)").unwrap();
    let indexes: Vec<String> = stmt
        .query_map([], |r| r.get(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for name in ["tokens_by_caller", "tokens_by_start_command"] {
        assert!(indexes.iter().any(|i| i == name), "{name} in {indexes:?}");
    }
    // The foreign keys hold, and the lineage row is still there.
    let broken: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(broken, 0);
    let lineage: i64 = conn
        .query_row("SELECT count(*) FROM coordinator_lineage", [], |r| r.get(0))
        .unwrap();
    assert_eq!(lineage, 1);
    // The wider checks: an insert with the widest values works, one more fails.
    let widest = (4_294_967_295, 604_800_000, 4_294_967_295);
    insert(&conn, "g-wide", "worker", widest).unwrap();
    assert!(insert(&conn, "g-idle", "worker", (1, 604_800_001, 1)).is_err());
    assert!(insert(&conn, "g-spawn", "worker", (4_294_967_296, 1, 1)).is_err());
    assert!(insert(&conn, "g-answer", "worker", (1, 1, 4_294_967_296)).is_err());
}
