//! MCP P4a deck tests 1, 6, 7, 8, 9, 11, 12 and 13 at the store level: the
//! decision and its transaction, the retries, the end of a worker grant, the
//! bounds, rule 3, the confinement of a worker's authority and the
//! migrations. (The routes are in `tests/worker_grant.rs`.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::config_worker_grant::WorkerGrantConfig;
use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{
    DeckCommandState, GrantKind, GrantScope, RETRY_WINDOW_MS, SessionCommand, SessionSpec,
    UserInput,
};
use fleetdeck_proto::event::{Event, EventBody, WorkerOutcome};
use fleetdeck_proto::link::{Scope, StationInfo};
use fleetdeck_proto::mutation::{
    CoordinatorBinding, CoordinatorCall, CoordinatorCallEnvelope, CoordinatorOp,
};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{
    CommandId, EventEnvelope, Receipt, Reject, RejectCode, SessionId, SessionSummary, StationId,
};
use fleetdeck_testkit::Det;
use serde_json::json;

use super::*;
use crate::link::CommandKind;
use crate::store::Settle;
use crate::store::coordinator_ops::mode_within;
use crate::store::grants::{Grant, GrantClass, GrantState, NewGrant};
use crate::store::outbox::OutboxItem;
use crate::store::stations::NewStation;

const MIN: u64 = 60_000;
const HOUR: u64 = 60 * MIN;
const DAY: u64 = 24 * HOUR;

struct World {
    det: Det,
    s: DeckStore,
    a: StationId,
    b: StationId,
}

fn info(name: &str) -> StationInfo {
    let p = |i: &str| json!({"instance": i, "provider": i, "version": "1", "auth": "signed_in"});
    serde_json::from_value(json!({"name": name, "incarnation": "inc", "head": 0,
        "live_sessions": 2, "providers": [p("claude"), p("codex")]}))
    .unwrap()
}

async fn pin(s: &DeckStore, name: &str) -> StationId {
    let key = Identity::generate().unwrap().public_key();
    let scopes = BTreeSet::from([Scope::Read, Scope::Coordinate]);
    let new = NewStation {
        key,
        name: name.to_owned(),
        scopes,
        dial_url: None,
    };
    s.pin_station(new).await.unwrap().station_id
}

fn card(id: &str) -> SessionSummary {
    SessionSummary::new(id.into())
}

/// Two stations that both negotiated `worker_grant.v1` and `coordinator.v1`
/// (and v2), with an incarnation, `claude` and `codex`.
async fn world() -> World {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let (a, b) = (pin(&s, "alpha").await, pin(&s, "beta").await);
    for (st, token) in [(&a, 1), (&b, 2)] {
        s.apply_snapshot(
            st.clone(),
            "inc".to_owned().into(),
            0,
            ["w1", "worker-1", "owner-1", "coord-1"].map(card).to_vec(),
        )
        .await
        .unwrap();
        s.set_reachable(st, true);
        s.set_station_info(st, info(st.as_str()));
        let features = [
            feature::WORKER_GRANT,
            feature::COORDINATOR,
            feature::COORDINATOR_V2,
        ];
        s.set_link_features(st, token, features.map(str::to_owned).to_vec());
    }
    s.set_fleet_control(true);
    World { det, s, a, b }
}

fn start(cwd: &str) -> SessionCommand {
    SessionCommand::Start {
        spec: SessionSpec {
            provider_instance: "claude".into(),
            cwd: cwd.to_owned(),
            mode: None,
            model: None,
            title: None,
            coordinator: None,
            launch: None,
            extra_env: None,
        },
        prompt: Some(UserInput::new("go")),
        workspace: None,
        setup_cmd: None,
        batch: None,
        issue: None,
        origin_text: false,
        fleet_git: None,
    }
}

fn req(w: &World, id: &str, cwd: &str, setting: &WorkerGrantConfig) -> WorkerStart {
    WorkerStart {
        station: w.a.clone(),
        command: start(cwd),
        command_id: CommandId::from(id),
        provenance: Provenance::Owner,
        author: Some("owner".to_owned()),
        setting: setting.clone(),
    }
}

async fn submit(w: &World, id: &str, setting: &WorkerGrantConfig) -> SubmitOutcome {
    let r = req(w, id, "/w", setting);
    w.s.submit_start_with_worker_decision(r).await.unwrap()
}

fn on() -> WorkerGrantConfig {
    WorkerGrantConfig::default()
}

fn off() -> WorkerGrantConfig {
    WorkerGrantConfig {
        enabled: false,
        ..WorkerGrantConfig::default()
    }
}

fn sent_spec(c: &DeckCommand) -> Option<serde_json::Value> {
    let spec = c.envelope["command"]["spec"].get("coordinator").cloned();
    spec.filter(|v| !v.is_null())
}

async fn grants(w: &World) -> Vec<Grant> {
    w.s.grants(1000).await.unwrap()
}

async fn rows(w: &World, sql: impl Into<String>) -> i64 {
    let sql = sql.into();
    call(&w.s.inner.db, move |conn| {
        Ok(conn.query_row(&sql, [], |r| r.get(0))?)
    })
    .await
    .unwrap()
}

/// The newest grant of a session.
async fn caller(w: &World, session: &str) -> Grant {
    let (station, session) = (w.a.clone(), SessionId::from(session));
    call(&w.s.inner.db, move |conn| {
        Ok(crate::store::grants::caller_grant(conn, &station, &session)?.expect("a grant"))
    })
    .await
    .unwrap()
}

fn event(w: &World, seq: u64, session: &str, event: Event) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at: w.s.now_ms(),
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(event),
    }
}

fn outcome(grant: &Grant, command: &str) -> Event {
    Event::CoordinatorWorkerOutcome {
        grant_id: grant.grant_id.clone(),
        start_command_id: CommandId::from(command),
        outcome: WorkerOutcome::Skipped,
        reason: "handoff_unverified".to_owned(),
    }
}

async fn apply(w: &World, st: &StationId, seq: u64, session: &str, e: Event) {
    let env = event(w, seq, session, e);
    w.s.apply_events(st.clone(), "inc".to_owned().into(), vec![env])
        .await
        .unwrap();
}

async fn state_of(w: &World, g: &Grant) -> (GrantState, Option<String>) {
    let got = w.s.grant(g.grant_id.clone()).await.unwrap().unwrap();
    (got.state, got.revoked_reason)
}

/// Test 1 at the store level: the grant, the audit row and the command.
#[tokio::test]
async fn an_owner_board_start_gets_a_worker_grant_with_the_setting_values() {
    let w = world().await;
    let out = submit(&w, "c1", &on()).await;
    assert!(out.new);
    let all = grants(&w).await;
    assert_eq!(all.len(), 1);
    let g = &all[0];
    let classes = BTreeSet::from([
        GrantClass::Read,
        GrantClass::Mail,
        GrantClass::Drive,
        GrantClass::Spawn,
    ]);
    assert_eq!(g.kind, GrantKind::Worker);
    assert_eq!((g.classes.clone(), g.scope), (classes, GrantScope::Lineage));
    assert_eq!(g.stations, BTreeSet::from([w.a.clone(), w.b.clone()]));
    assert_eq!((g.spawn_cap, g.idle_ttl_ms), (2, 30 * 60_000));
    assert_eq!(g.created_at, i64::try_from(w.det.now_ms()).unwrap());
    assert_eq!(g.expires_at - g.created_at, 12 * 3_600_000);
    let modes = [
        ("claude".to_owned(), "acceptEdits".to_owned()),
        ("codex".to_owned(), "on-request/workspace-write".to_owned()),
    ];
    assert_eq!(g.max_modes, modes.into_iter().collect());
    assert_eq!(
        (g.station_id.clone(), g.session_id.0.as_str()),
        (w.a.clone(), "")
    );
    let spec = sent_spec(&out.command).expect("the command carries the grant");
    assert_eq!(spec["kind"], "worker");
    assert_eq!(spec["grant_id"], g.grant_id.as_str());
    assert_eq!(spec["classes"], json!(["read", "mail", "drive", "spawn"]));
    assert!(spec.get("scope").is_none() && spec.get("stations").is_none());
    assert_eq!(out.command.author.as_deref(), Some("owner"));
    assert_eq!(out.command.origin, "board");
    assert_eq!(
        out.command.retry_until,
        i64::try_from(w.det.now_ms()).unwrap() + RETRY_WINDOW_MS
    );
    let audit = w.s.audit_log(10).await.unwrap();
    let row = audit
        .iter()
        .find(|r| r.action == "coordinator.worker_grant_created")
        .expect("the audit row");
    assert_eq!(row.actor, "owner");
    assert_eq!(row.target.as_deref(), Some(g.grant_id.as_str()));
    assert_eq!(row.command_id, Some(CommandId::from("c1")));
    assert!(row.detail.as_deref().unwrap().contains("spawn_cap=2"));
    assert_eq!(rows(&w, "SELECT count(*) FROM worker_attach").await, 1);
    // The derived `coordinate` scope follows in the same transaction.
    let alpha = w.s.station(w.a.clone()).await.unwrap().unwrap();
    assert!(alpha.scopes.contains(&Scope::Coordinate));
}

/// Test 6.
#[tokio::test]
async fn a_retry_reuses_the_stored_decision_across_setting_and_feature_changes() {
    let w = world().await;
    let first = submit(&w, "r1", &on()).await;
    let none = submit(&w, "r2", &off()).await;
    assert!(first.new && none.new);
    assert!(sent_spec(&none.command).is_none());
    // Setting on to off, and a lost feature: the grant stays.
    w.s.set_link_features(&w.a, 3, vec![feature::COORDINATOR.to_owned()]);
    let again = submit(&w, "r1", &off()).await;
    assert!(!again.new);
    assert_eq!(again.command.command_id, first.command.command_id);
    assert!(sent_spec(&again.command).is_some());
    // Setting off to on, and the features back: still no grant.
    w.s.clear_link_features(&w.a, 3);
    let again = submit(&w, "r2", &on()).await;
    assert!(!again.new);
    assert!(sent_spec(&again.command).is_none());
    assert_eq!(grants(&w).await.len(), 1);
    // Another original with the same ID is `command_id_reused`.
    let other = req(&w, "r1", "/another", &on());
    let e =
        w.s.submit_start_with_worker_decision(other)
            .await
            .unwrap_err();
    assert!(matches!(e, DeckError::CommandIdReused(_)), "{e}");
    let e =
        w.s.submit_start_with_worker_decision(req(&w, "r2", "/another", &off()))
            .await
            .unwrap_err();
    assert!(matches!(e, DeckError::CommandIdReused(_)), "{e}");
    // The reasons of the decisions.
    let reasons = call(&w.s.inner.db, |conn| {
        let mut stmt =
            conn.prepare("SELECT command_id, decision, reason FROM worker_attach ORDER BY 1")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .await
    .unwrap();
    let want = [("r1", "grant", "owner"), ("r2", "none", "setting_off")];
    assert_eq!(
        reasons,
        want.map(|(a, b, c)| (a.to_owned(), b.to_owned(), c.to_owned()))
    );
    // No link that negotiated both features: no grant, reason `link_features`.
    w.s.set_link_features(&w.a, 4, vec![feature::COORDINATOR.to_owned()]);
    let none = submit(&w, "r3", &on()).await;
    assert!(sent_spec(&none.command).is_none());
    let reason: String = call(&w.s.inner.db, |conn| {
        Ok(conn.query_row(
            "SELECT reason FROM worker_attach WHERE command_id = 'r3'",
            [],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap();
    assert_eq!(reason, "link_features");
    // A start that is not a start, or carries a coordinator request, is refused
    // before anything is written.
    let before = rows(&w, "SELECT count(*) FROM commands").await;
    let mut bad = req(&w, "r4", "/w", &on());
    bad.command = SessionCommand::Interrupt {
        session_id: "w1".into(),
    };
    assert!(w.s.submit_start_with_worker_decision(bad).await.is_err());
    assert_eq!(rows(&w, "SELECT count(*) FROM commands").await, before);
    // A crash point between the decision and the command leaves nothing: an
    // unsynced station fails the call, and no row of any table is written.
    let c = pin(&w.s, "gamma").await;
    w.s.set_link_features(&c, 9, vec![feature::WORKER_GRANT.to_owned()]);
    let mut unsynced = req(&w, "r5", "/w", &on());
    unsynced.station = c;
    let e =
        w.s.submit_start_with_worker_decision(unsynced)
            .await
            .unwrap_err();
    assert!(matches!(e, DeckError::NotSynced(_)), "{e}");
    for table in [
        "worker_attach WHERE command_id = 'r5'",
        "commands WHERE command_id = 'r5'",
    ] {
        assert_eq!(rows(&w, format!("SELECT count(*) FROM {table}")).await, 0);
    }
    assert_eq!(grants(&w).await.len(), 1);
}

/// Test 7 at the store level: an expired queued start revokes its grant; a
/// start that was sent does not.
#[tokio::test]
async fn a_queued_worker_start_waits_for_a_compatible_link_and_expires_with_its_grant_revoked() {
    let w = world().await;
    let queued = submit(&w, "q1", &on()).await;
    let g = grants(&w).await.remove(0);
    assert_eq!(state_of(&w, &g).await.0, GrantState::Active);
    w.det.advance(Duration::from_millis(
        u64::try_from(RETRY_WINDOW_MS).unwrap() + 1,
    ));
    let ended = w.s.expire_commands().await.unwrap();
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].reason.as_deref(), Some("not_sent"));
    assert_eq!(
        state_of(&w, &g).await,
        (GrantState::Revoked, Some("undelivered".to_owned()))
    );
    let alpha = w.s.station(w.a.clone()).await.unwrap().unwrap();
    assert!(!alpha.scopes.contains(&Scope::Coordinate));
    assert_eq!(queued.command.state, DeckCommandState::Pending);
    // A start that was sent and has no receipt is ambiguous: nothing is revoked.
    submit(&w, "q2", &on()).await;
    let g2 = grants(&w).await.remove(0);
    w.s.mark_sent(CommandId::from("q2")).await.unwrap().unwrap();
    w.det.advance(Duration::from_millis(
        u64::try_from(RETRY_WINDOW_MS).unwrap() + 1,
    ));
    let ended = w.s.expire_commands().await.unwrap();
    assert_eq!(ended[0].reason.as_deref(), Some("no_receipt"));
    assert_eq!(state_of(&w, &g2).await.0, GrantState::Active);
}

/// Test 8.
#[tokio::test]
async fn a_skipped_or_failed_start_revokes_its_worker_grant_at_once_and_a_late_bound_event_revokes_the_station_binding()
 {
    let w = world().await;
    // An outcome after an admission skip, a launch skip or a shim row failure
    // is the same event: the grant of that start is revoked at once.
    submit(&w, "s1", &on()).await;
    let g1 = grants(&w).await.remove(0);
    apply(&w, &w.a, 1, "w1", outcome(&g1, "s1")).await;
    assert_eq!(
        state_of(&w, &g1).await,
        (GrantState::Revoked, Some("handoff_skipped".to_owned()))
    );
    // A replay of the same event (a reconnect) changes nothing.
    let again = event(&w, 1, "w1", outcome(&g1, "s1"));
    w.s.apply_events(w.a.clone(), "inc".to_owned().into(), vec![again])
        .await
        .unwrap();
    assert_eq!(state_of(&w, &g1).await.0, GrantState::Revoked);
    // A forged outcome: another station's grant, another start command, an
    // owner grant, an unknown grant.
    submit(&w, "s2", &on()).await;
    let g2 = grants(&w).await.remove(0);
    apply(&w, &w.b, 1, "w1", outcome(&g2, "s2")).await;
    apply(&w, &w.a, 2, "w1", outcome(&g2, "not-s2")).await;
    let mut owner = NewGrant::new(w.a.clone(), "coord".into());
    owner.start_command_id = Some("owner-start".to_owned());
    let og = w.s.create_grant(owner).await.unwrap();
    apply(&w, &w.a, 3, "w1", outcome(&og, "owner-start")).await;
    let ghost = Event::CoordinatorWorkerOutcome {
        grant_id: "nope".to_owned(),
        start_command_id: "s2".into(),
        outcome: WorkerOutcome::Skipped,
        reason: "x".to_owned(),
    };
    apply(&w, &w.a, 4, "w1", ghost).await;
    assert_eq!(state_of(&w, &g2).await.0, GrantState::Active);
    assert_eq!(state_of(&w, &og).await.0, GrantState::Active);
    // A start that fails: a refusal of the link layer, and a rejected receipt.
    submit(&w, "f1", &on()).await;
    submit(&w, "f2", &on()).await;
    submit(&w, "f3", &on()).await;
    let by_start = |id: &str| {
        let all = grants_by(&w);
        let id = id.to_owned();
        async move { all.await.into_iter().find(|(c, _)| *c == id).unwrap().1 }
    };
    for id in ["f1", "f2", "f3"] {
        w.s.mark_sent(CommandId::from(id)).await.unwrap().unwrap();
    }
    let refused = Settle::Refused {
        code: "forbidden".to_owned(),
        message: "no".to_owned(),
    };
    // After one send the same refusal is ambiguous: f3 stays.
    w.s.mark_sent(CommandId::from("f3")).await.unwrap().unwrap();
    w.s.settle_command(CommandId::from("f1"), refused.clone())
        .await
        .unwrap();
    let rejected = Receipt::rejected(
        CommandId::from("f2"),
        Reject::new(RejectCode::InvalidCommand, "bad"),
        w.s.now_ms(),
    );
    w.s.settle_command(CommandId::from("f2"), Settle::Receipt(rejected))
        .await
        .unwrap();
    w.s.settle_command(CommandId::from("f3"), refused)
        .await
        .unwrap();
    for id in ["f1", "f2"] {
        let g = by_start(id).await;
        assert_eq!(
            state_of(&w, &g).await.1.as_deref(),
            Some("start_failed"),
            "{id}"
        );
    }
    let g3 = by_start("f3").await;
    assert_eq!(state_of(&w, &g3).await.0, GrantState::Active);
    // A late `coordinator.bound` of a revoked, never-bound worker grant keeps
    // it revoked and queues `session.coordinator_revoke` for the session.
    let bound = Event::CoordinatorBound {
        binding_id: "b-1".to_owned(),
        grant_id: g1.grant_id.clone(),
        generation: 1,
        provider_instance: "claude".to_owned(),
        native_id: "native-1".to_owned(),
    };
    apply(&w, &w.a, 5, "late-session", bound).await;
    let after = w.s.grant(g1.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(after.state, GrantState::Revoked);
    assert!(after.binding.is_none());
    let queued = w.s.outbox().await.unwrap();
    let revokes: Vec<_> = queued
        .iter()
        .filter_map(|i| match serde_json::from_value(i.payload.clone()) {
            Ok(CommandKind::Session {
                command:
                    SessionCommand::CoordinatorRevoke {
                        session_id,
                        grant_id,
                    },
            }) => Some((i.origin.clone(), i.station_id.clone(), session_id, grant_id)),
            _ => None,
        })
        .collect();
    assert_eq!(
        revokes,
        [(
            "board".to_owned(),
            w.a.clone(),
            "late-session".into(),
            g1.grant_id.clone()
        )]
    );
    // No outcome and no bound event at all: the idle time counts from creation.
    submit(&w, "i1", &on()).await;
    let idle = by_start("i1").await;
    w.det.advance(Duration::from_millis(29 * MIN));
    w.s.sweep_grants().await.unwrap();
    assert_eq!(state_of(&w, &idle).await.0, GrantState::Active);
    w.det.advance(Duration::from_millis(2 * MIN));
    w.s.sweep_grants().await.unwrap();
    assert_eq!(
        state_of(&w, &idle).await,
        (GrantState::Expired, Some("idle".to_owned()))
    );
    // A used grant counts its idle time from the last use (the first bound).
    submit(&w, "i2", &on()).await;
    let used = by_start("i2").await;
    let bound = Event::CoordinatorBound {
        binding_id: "b-2".to_owned(),
        grant_id: used.grant_id.clone(),
        generation: 1,
        provider_instance: "claude".to_owned(),
        native_id: "native-2".to_owned(),
    };
    apply(&w, &w.a, 6, "used-session", bound).await;
    w.det.advance(Duration::from_millis(29 * MIN));
    w.s.sweep_grants().await.unwrap();
    assert_eq!(state_of(&w, &used).await.0, GrantState::Active);
    let after = w.s.grant(used.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(after.session_id.0, "used-session");
}

async fn grants_by(w: &World) -> Vec<(String, Grant)> {
    let all = grants(w).await;
    let mut out = Vec::new();
    for g in all {
        let id: Option<String> = call(&w.s.inner.db, {
            let grant_id = g.grant_id.clone();
            move |conn| {
                Ok(conn.query_row(
                    "SELECT start_command_id FROM tokens WHERE grant_id = ?1",
                    [grant_id],
                    |r| r.get(0),
                )?)
            }
        })
        .await
        .unwrap();
        out.push((id.unwrap_or_default(), g));
    }
    out
}

/// Test 9.
#[tokio::test]
async fn the_deck_wide_cap_and_the_retention_bound_worker_grants() {
    let w = world().await;
    for n in 0..WORKER_GRANT_CAP {
        let out = submit(&w, &format!("c{n}"), &on()).await;
        assert!(sent_spec(&out.command).is_some(), "start {n}");
    }
    let over = submit(&w, "over", &on()).await;
    assert!(sent_spec(&over.command).is_none());
    assert_eq!(grants(&w).await.len(), WORKER_GRANT_CAP);
    let skipped: Vec<_> =
        w.s.audit_log(1000)
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.action == "coordinator.worker_grant_skipped")
            .collect();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].detail.as_deref(), Some("worker_cap"));
    let reason: String = call(&w.s.inner.db, |conn| {
        Ok(conn.query_row(
            "SELECT reason FROM worker_attach WHERE command_id = 'over'",
            [],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap();
    assert_eq!(reason, "worker_cap");
    // An idle-expired grant frees its slot.
    w.det.advance(Duration::from_millis(31 * MIN));
    w.s.sweep_grants().await.unwrap();
    let freed = submit(&w, "freed", &on()).await;
    assert!(sent_spec(&freed.command).is_some());
    // 8 days later the terminal grants, their decisions and their lineage go;
    // a stale retry is answered by the retained command row, not dispatched.
    w.det.advance(Duration::from_millis(12 * HOUR + 8 * DAY));
    w.s.sweep_grants().await.unwrap();
    assert_eq!(rows(&w, "SELECT count(*) FROM tokens").await, 0);
    assert_eq!(rows(&w, "SELECT count(*) FROM worker_attach").await, 0);
    let stale = submit(&w, "c0", &on()).await;
    assert!(!stale.new, "a stale retry is not stored or sent again");
    assert!(sent_spec(&stale.command).is_some());
    assert_eq!(grants(&w).await.len(), 0);
    let e =
        w.s.submit_start_with_worker_decision(req(&w, "c0", "/another", &on()))
            .await
            .unwrap_err();
    assert!(matches!(e, DeckError::CommandIdReused(_)), "{e}");
    // Sustained automation with the setting off writes `none` rows, which the
    // retention prunes after 7 days; a replay is then answered by the command.
    for n in 0..30 {
        submit(&w, &format!("auto{n}"), &off()).await;
    }
    assert_eq!(rows(&w, "SELECT count(*) FROM worker_attach").await, 30);
    w.det.advance(Duration::from_millis(7 * DAY - HOUR));
    w.s.sweep_grants().await.unwrap();
    assert_eq!(rows(&w, "SELECT count(*) FROM worker_attach").await, 30);
    w.det.advance(Duration::from_millis(2 * HOUR));
    w.s.sweep_grants().await.unwrap();
    assert_eq!(rows(&w, "SELECT count(*) FROM worker_attach").await, 0);
    let replay = submit(&w, "auto3", &on()).await;
    assert!(!replay.new);
    assert!(sent_spec(&replay.command).is_none());
    assert_eq!(grants(&w).await.len(), 0);
    assert_eq!(rows(&w, "SELECT count(*) FROM worker_attach").await, 0);
}

/// Binds the worker grant of start `id` to `session`.
async fn bind_worker(w: &World, id: &str, session: &str, seq: u64) -> (Grant, CoordinatorBinding) {
    let g = grants_by(w)
        .await
        .into_iter()
        .find(|(c, _)| c == id)
        .unwrap()
        .1;
    let e = Event::CoordinatorBound {
        binding_id: format!("b-{session}"),
        grant_id: g.grant_id.clone(),
        generation: 1,
        provider_instance: "claude".to_owned(),
        native_id: format!("native-{session}"),
    };
    apply(w, &w.a, seq, session, e).await;
    let g = w.s.grant(g.grant_id).await.unwrap().unwrap();
    let b = g.binding.clone().expect("bound");
    (g, b)
}

async fn call_as(w: &World, b: &CoordinatorBinding, id: &str, op: CoordinatorOp) -> Receipt {
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
    w.s.decide_coordinator(w.a.clone(), envelope).await.unwrap()
}

fn code(r: &Receipt) -> Option<RejectCode> {
    r.reject.as_ref().map(|j| j.code)
}

fn fleet_grant(w: &World, session: &str) -> NewGrant {
    let mut g = NewGrant::new(w.a.clone(), session.into());
    g.classes = BTreeSet::from([
        GrantClass::Read,
        GrantClass::Mail,
        GrantClass::Drive,
        GrantClass::Spawn,
        GrantClass::Inspect,
        GrantClass::Manage,
        GrantClass::Answer,
    ]);
    g.scope = GrantScope::Fleet;
    g.spawn_cap = 5;
    g.max_modes = [("claude".to_owned(), "acceptEdits".to_owned())].into();
    g
}

async fn fleet_binding(w: &World, session: &str) -> CoordinatorBinding {
    let stored = w.s.create_grant(fleet_grant(w, session)).await.unwrap();
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

/// Test 11: a fleet coordinator may act on a session that holds a worker
/// grant, and never on a session that holds an owner grant.
#[tokio::test]
async fn rule_3_ignores_worker_grants_and_keeps_owner_grants() {
    let w = world().await;
    submit(&w, "k1", &on()).await;
    let (_, _) = bind_worker(&w, "k1", "worker-1", 1).await;
    let owner =
        w.s.create_grant(NewGrant::new(w.a.clone(), "owner-1".into()))
            .await
            .unwrap();
    w.s.bind_grant(CoordinatorBinding {
        binding_id: "b-owner".to_owned(),
        grant_id: owner.grant_id,
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "native-owner-1".to_owned(),
    })
    .await
    .unwrap();
    let fleet = fleet_binding(&w, "coord-1").await;
    let interrupt = |s: &str| CoordinatorOp::Interrupt {
        station: w.a.clone(),
        session_id: s.into(),
    };
    let r = call_as(&w, &fleet, "k-ok", interrupt("worker-1")).await;
    assert!(code(&r).is_none(), "{r:?}");
    let r = call_as(&w, &fleet, "k-no", interrupt("owner-1")).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
    assert!(r.reject.unwrap().message.contains("coordinator"));
    // The station-facing flag still says that a worker binding is bound.
    let g = caller(&w, "worker-1").await;
    assert!(g.binding.is_some());
}

/// Test 12: a worker's authority stays inside its grant.
#[tokio::test]
async fn fleet_control_approval_of_a_worker_spawn_stays_inside_the_worker_grant() {
    let w = world().await;
    let narrow = WorkerGrantConfig {
        stations: vec!["alpha".to_owned()],
        spawn_cap: 1,
        ..on()
    };
    submit(&w, "w12", &narrow).await;
    let (g, wb) = bind_worker(&w, "w12", "worker-1", 1).await;
    assert_eq!(g.stations, BTreeSet::from([w.a.clone()]));
    let spawn = |station: &StationId, mode: Option<&str>| CoordinatorOp::Spawn {
        station: station.clone(),
        spec: SessionSpec {
            provider_instance: "claude".into(),
            cwd: "/w/app".to_owned(),
            mode: mode.map(str::to_owned),
            model: None,
            title: None,
            coordinator: None,
            launch: None,
            extra_env: None,
        },
        prompt: Some(UserInput::new("do")),
    };
    // The ceiling and the grant's stations hold.
    let r = call_as(&w, &wb, "sp-auto", spawn(&w.a, Some("auto"))).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
    let r = call_as(&w, &wb, "sp-beta", spawn(&w.b, None)).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
    // One spawn charges the worker's cap and the child gets no grant.
    let r = call_as(&w, &wb, "sp-1", spawn(&w.a, Some("acceptEdits"))).await;
    assert!(code(&r).is_none(), "{r:?}");
    let r = call_as(&w, &wb, "sp-2", spawn(&w.a, None)).await;
    assert_eq!(code(&r), Some(RejectCode::SpawnCapReached), "{r:?}");
    let g = w.s.grant(g.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(g.spawn_used, 1);
    let starts = |items: Vec<OutboxItem>| -> Vec<SessionCommand> {
        items
            .into_iter()
            .filter_map(|i| match serde_json::from_value(i.payload) {
                Ok(CommandKind::Session { command }) => Some(command),
                _ => None,
            })
            .filter(|c| matches!(c, SessionCommand::Start { .. }))
            .collect()
    };
    let child = starts(w.s.outbox().await.unwrap());
    assert_eq!(child.len(), 1);
    assert!(matches!(&child[0], SessionCommand::Start { spec, .. } if spec.coordinator.is_none()));
    // A fleet coordinator acts on the worker with its own grant: its spawns
    // charge its own cap, and the child has no grant either.
    let fleet = fleet_binding(&w, "coord-1").await;
    let assign = CoordinatorOp::AssignAuto {
        task: "t".to_owned(),
        requirements: fleetdeck_proto::mutation::AssignRequirements::default(),
    };
    let r = call_as(&w, &fleet, "fa-1", assign).await;
    assert!(code(&r).is_none(), "{r:?}");
    let fleet_grant = caller(&w, "coord-1").await;
    let worker_after = w.s.grant(g.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(
        worker_after.spawn_used, 1,
        "the worker's cap is not charged"
    );
    for c in starts(w.s.outbox().await.unwrap()) {
        assert!(matches!(&c, SessionCommand::Start { spec, .. } if spec.coordinator.is_none()));
    }
    assert!(fleet_grant.spawn_used <= fleet_grant.spawn_cap);
    // An owner grant's session stays a refused target.
    let owner =
        w.s.create_grant(NewGrant::new(w.a.clone(), "owner-1".into()))
            .await
            .unwrap();
    w.s.bind_grant(CoordinatorBinding {
        binding_id: "b-o".to_owned(),
        grant_id: owner.grant_id,
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "native-owner-1".to_owned(),
    })
    .await
    .unwrap();
    let r = call_as(
        &w,
        &fleet,
        "fa-owner2",
        CoordinatorOp::Interrupt {
            station: w.a.clone(),
            session_id: "owner-1".into(),
        },
    )
    .await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
    // After the revocation the worker's calls are refused.
    assert!(
        w.s.revoke_grant(g.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    let r = call_as(&w, &wb, "sp-3", spawn(&w.a, None)).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
}

/// The core copy of `mode_within` equals the deck's for every pair.
#[test]
fn the_core_copy_of_mode_within_equals_the_deck_function() {
    use fleetdeck_core::config_worker_grant as core;
    let claude: Vec<&str> = vec![
        "default",
        "plan",
        "acceptEdits",
        "dontAsk",
        "auto",
        "bypassPermissions",
        "junk",
    ];
    for a in &claude {
        for b in &claude {
            assert_eq!(
                mode_within("claude", Some(a), b),
                core::mode_within("claude", Some(a), b),
                "{a} {b}"
            );
        }
        assert_eq!(
            mode_within("claude", None, a),
            core::mode_within("claude", None, a)
        );
    }
    let policies = ["untrusted", "on-request", "never", "x"];
    let sandboxes = ["read-only", "workspace-write", "danger-full-access", "y"];
    let mut codex: Vec<String> = Vec::new();
    for p in policies {
        for s in sandboxes {
            codex.push(format!("{p}/{s}"));
        }
    }
    codex.push("junk".to_owned());
    for a in &codex {
        for b in &codex {
            assert_eq!(
                mode_within("codex", Some(a), b),
                core::mode_within("codex", Some(a), b),
                "{a} {b}"
            );
        }
        assert_eq!(
            mode_within("codex", None, a),
            core::mode_within("codex", None, a)
        );
    }
    for family in ["claude", "codex", "other"] {
        assert_eq!(
            mode_within(family, Some("plan"), "plan"),
            core::mode_within(family, Some("plan"), "plan")
        );
    }
    // The registry options are exactly the valid ceilings.
    for m in core::CLAUDE_CEILINGS {
        assert!(crate::store::coordinator_ops::valid_ceiling("claude", m));
    }
    for m in core::CODEX_CEILINGS {
        assert!(crate::store::coordinator_ops::valid_ceiling("codex", m));
    }
}

/// Test 13: an upgraded database.
#[test]
fn the_migrations_add_the_grant_kind_the_schedule_author_and_the_attach_table() {
    use rusqlite::Connection;
    let migrations = crate::store::schema::migrations();
    let mut conn = Connection::open_in_memory().unwrap();
    migrations.to_version(&mut conn, 42).unwrap();
    let has = |conn: &Connection, table: &str, column: &str| -> bool {
        let sql = format!("SELECT count(*) FROM pragma_table_info('{table}') WHERE name = ?1");
        conn.query_row(&sql, [column], |r| r.get::<_, i64>(0))
            .unwrap()
            == 1
    };
    assert!(!has(&conn, "tokens", "kind") && !has(&conn, "schedules", "author"));
    conn.execute(
        "INSERT INTO tokens (grant_id, station_id, session_id, classes, stations, spawn_cap,
             max_modes, idle_ttl_ms, created_at, expires_at, state)
         VALUES ('g1', 's', 'x', '[]', '[]', 0, '{}', 1000, 1, 2, 'active')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO schedules (schedule_id, name, enabled, cron, timezone, station_id, path,
             provider_instance, prompt, reuse_session, created_at, armed_at)
         VALUES ('sc1', 'n', 1, '* * * * *', '+00:00', 'st', '/w', 'claude', 'p', 0, 1, 1)",
        [],
    )
    .unwrap();
    migrations.to_latest(&mut conn).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 45);
    let kind: String = conn
        .query_row("SELECT kind FROM tokens WHERE grant_id = 'g1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kind, "coordinator");
    let author: String = conn
        .query_row(
            "SELECT author FROM schedules WHERE schedule_id = 'sc1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(author, "unknown");
    assert!(has(&conn, "worker_attach", "original_sha256"));
    // The checks of the columns and the table.
    assert!(
        conn.execute("UPDATE tokens SET kind = 'other'", [])
            .is_err()
    );
    assert!(
        conn.execute("UPDATE schedules SET author = 'x'", [])
            .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO worker_attach (command_id, original_sha256, decision, reason, created_at)
             VALUES ('c', 'h', 'maybe', 'r', 1)",
            [],
        )
        .is_err()
    );
    let index: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'worker_attach_by_created'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(index, 1);
}
