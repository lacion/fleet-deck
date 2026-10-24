//! MCP P4b deck test 8 at the store level (the routes are P4b-3c): the give
//! of an owner grant, its row and its queued set, `has_grant`, the worker
//! grant that a give replaces, and what the receipts of the set and of a
//! start do to the grant and the row. The helpers are shared with the other
//! owner grant tests.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{CommandResult, CoordinatorSpec, DeckCommandState, GrantKind};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{
    CommandEnvelope, CommandId, Receipt, Reject, RejectCode, SessionId, SessionSummary, StationId,
};
use fleetdeck_testkit::Det;
use rusqlite::params;
use serde_json::json;

use super::*;
use crate::store::stations::NewStation;
use crate::store::{DeckCommand, Settle};

pub(super) struct World {
    pub det: Det,
    pub s: DeckStore,
    pub a: StationId,
}

/// One station that negotiated `coordinator_live.v1`, with an incarnation and
/// the sessions `owner-1` to `owner-6`, `w1` and `w2`.
pub(super) async fn world() -> World {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let a = s
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "alpha".to_owned(),
            scopes: BTreeSet::from([Scope::Read, Scope::Coordinate]),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id;
    let sessions = (1..=6)
        .map(|n| format!("owner-{n}"))
        .chain(["w1".to_owned(), "w2".to_owned()])
        .map(|id| SessionSummary::new(id.into()))
        .collect();
    s.apply_snapshot(a.clone(), "inc".to_owned().into(), 0, sessions)
        .await
        .unwrap();
    s.set_reachable(&a, true);
    let features = [
        feature::COORDINATOR,
        feature::COORDINATOR_V2,
        feature::COORDINATOR_LIVE,
    ];
    s.set_link_features(&a, 1, features.map(str::to_owned).to_vec());
    World { det, s, a }
}

pub(super) fn new_grant(w: &World, session: &str) -> NewGrant {
    NewGrant::new(w.a.clone(), session.into())
}

/// A worker grant of `session`, in effect.
pub(super) async fn worker_grant(w: &World, session: &str, start: Option<&str>) -> Grant {
    let mut g = new_grant(w, session);
    g.kind = GrantKind::Worker;
    g.classes = BTreeSet::from([
        GrantClass::Read,
        GrantClass::Mail,
        GrantClass::Drive,
        GrantClass::Spawn,
    ]);
    g.max_modes = BTreeMap::from([("claude".to_owned(), "plan".to_owned())]);
    g.start_command_id = start.map(str::to_owned);
    w.s.create_grant(g).await.unwrap()
}

pub(super) async fn give(w: &World, session: &str, g: NewGrant) -> Result<Grant, GiveError> {
    w.s.give(w.a.clone(), session.into(), g).await.unwrap()
}

/// The row of a session.
pub(super) async fn row_of(w: &World, session: &str) -> Option<OwnerRow> {
    let (a, session) = (w.a.clone(), SessionId::from(session));
    call(&w.s.inner.db, move |conn| row(conn, &a, &session))
        .await
        .unwrap()
}

/// One stored `session.coordinator_set`.
#[derive(Debug)]
pub(super) struct SetCmd {
    pub id: CommandId,
    pub seq: u64,
    pub grant_id: String,
    pub spec: Option<CoordinatorSpec>,
    pub state: DeckCommandState,
    pub reason: Option<String>,
}

/// The sets of a session, oldest first.
pub(super) async fn sets(w: &World, session: &str) -> Vec<SetCmd> {
    let all: Vec<DeckCommand> = w.s.commands().await.unwrap();
    all.into_iter()
        .filter(|c| c.method == "session.coordinator_set")
        .filter_map(|c| {
            let env: CommandEnvelope = serde_json::from_value(c.envelope.clone()).unwrap();
            match env.command {
                fleetdeck_proto::command::SessionCommand::CoordinatorSet {
                    session_id,
                    seq,
                    grant_id,
                    spec,
                } if session_id.0 == session => Some(SetCmd {
                    id: c.command_id.clone(),
                    seq,
                    grant_id,
                    spec: spec.map(|s| *s),
                    state: c.state,
                    reason: c.reason.clone(),
                }),
                _ => None,
            }
        })
        .collect()
}

pub(super) async fn accept(w: &World, id: &CommandId, seq: Option<u64>) {
    let result = CommandResult {
        seq,
        ..CommandResult::default()
    };
    let receipt = Receipt::accepted(id.clone(), 1, result, w.s.now_ms());
    w.s.settle_command(id.clone(), Settle::Receipt(receipt))
        .await
        .unwrap();
}

pub(super) async fn reject(w: &World, id: &CommandId) {
    let reject = Reject::new(RejectCode::InvalidState, "no");
    let receipt = Receipt::rejected(id.clone(), reject, w.s.now_ms());
    w.s.settle_command(id.clone(), Settle::Receipt(receipt))
        .await
        .unwrap();
}

/// The link layer refuses the first send: the command is `failed`.
pub(super) async fn refuse(w: &World, id: &CommandId) {
    let refused = Settle::Refused {
        code: "forbidden".to_owned(),
        message: "no".to_owned(),
    };
    w.s.settle_command(id.clone(), refused).await.unwrap();
}

/// Writes the state and the reason of a grant.
pub(super) async fn set_state(w: &World, grant_id: &str, state: &str, reason: Option<&str>) {
    let (id, state, reason) = (
        grant_id.to_owned(),
        state.to_owned(),
        reason.map(str::to_owned),
    );
    call(&w.s.inner.db, move |conn| {
        conn.execute(
            "UPDATE tokens SET state = ?2, revoked_reason = ?3 WHERE grant_id = ?1",
            params![id, state, reason],
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

pub(super) async fn grant(w: &World, grant_id: &str) -> Grant {
    w.s.grant(grant_id.to_owned()).await.unwrap().unwrap()
}

/// MCP P4b test 8, the store part: the give and its row and set, `has_grant`
/// for a held grant (also idle-expired, also a grant without a row), the
/// overwrite of a terminal grant's row, the worker grant that a give
/// replaces, and the receipts of the set.
#[tokio::test]
async fn the_owner_gives_a_grant_to_a_running_or_hibernated_session() {
    let w = world().await;
    let g = give(&w, "owner-1", new_grant(&w, "owner-1")).await.unwrap();
    assert_eq!(g.session_id.0, "owner-1");
    assert_eq!(g.kind, GrantKind::Coordinator);
    assert_eq!(g.state, GrantState::Active);
    assert!(g.binding.is_none());
    let row_1 = OwnerRow {
        grant_id: g.grant_id.clone(),
        seq: 1,
        station_seq: 0,
    };
    assert_eq!(row_of(&w, "owner-1").await, Some(row_1));
    let queued = sets(&w, "owner-1").await;
    assert_eq!(queued.len(), 1);
    assert_eq!(
        (queued[0].seq, queued[0].state),
        (1, DeckCommandState::Pending)
    );
    assert_eq!(queued[0].grant_id, g.grant_id);
    let spec = queued[0].spec.as_ref().unwrap();
    assert_eq!(spec.grant_id, g.grant_id);
    assert_eq!(spec.classes, ["read", "mail"]);
    assert_eq!(spec.expires_at, g.expires_at);
    assert_eq!(spec.kind, GrantKind::Coordinator);
    assert!(spec.stations.is_empty() && spec.spawn_cap.is_none());

    // One owner grant per session: in effect, idle-expired, or active with its
    // idle time passed, the give is refused and writes nothing.
    assert_eq!(
        give(&w, "owner-1", new_grant(&w, "owner-1")).await,
        Err(GiveError::HasGrant)
    );
    set_state(&w, &g.grant_id, "expired", Some("idle")).await;
    assert_eq!(
        give(&w, "owner-1", new_grant(&w, "owner-1")).await,
        Err(GiveError::HasGrant)
    );
    set_state(&w, &g.grant_id, "active", None).await;
    let old = w.s.now_ms() - 2 * g.idle_ttl_ms;
    let id = g.grant_id.clone();
    call(&w.s.inner.db, move |conn| {
        conn.execute(
            "UPDATE tokens SET last_used_at = ?2 WHERE grant_id = ?1",
            params![id, old],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        give(&w, "owner-1", new_grant(&w, "owner-1")).await,
        Err(GiveError::HasGrant)
    );
    assert_eq!(sets(&w, "owner-1").await.len(), 1);
    assert_eq!(w.s.grants(100).await.unwrap().len(), 1);

    // A terminal grant's row is overwritten: the next sequence, the new
    // grant, and the older pending set is superseded.
    assert!(
        w.s.revoke_grant(g.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    let g2 = give(&w, "owner-1", new_grant(&w, "owner-1")).await.unwrap();
    assert_ne!(g2.grant_id, g.grant_id);
    let row = row_of(&w, "owner-1").await.unwrap();
    assert_eq!((row.grant_id.as_str(), row.seq), (g2.grant_id.as_str(), 2));
    let queued = sets(&w, "owner-1").await;
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0].state, DeckCommandState::Failed);
    assert_eq!(queued[0].reason.as_deref(), Some("superseded"));
    assert_eq!(
        (queued[1].seq, queued[1].state),
        (2, DeckCommandState::Pending)
    );

    // A start-time grant without a row also holds the session.
    let plain = w.s.create_grant(new_grant(&w, "owner-2")).await.unwrap();
    assert_eq!(
        give(&w, "owner-2", new_grant(&w, "owner-2")).await,
        Err(GiveError::HasGrant)
    );
    assert!(row_of(&w, "owner-2").await.is_none());
    w.s.revoke_grant(plain.grant_id, "owner".to_owned())
        .await
        .unwrap();
    give(&w, "owner-2", new_grant(&w, "owner-2")).await.unwrap();
    assert_eq!(row_of(&w, "owner-2").await.unwrap().seq, 1);

    // A worker grant in effect is replaced; the station ends it inside the
    // set, so the deck queues no worker revoke.
    let worker = worker_grant(&w, "w1", None).await;
    give(&w, "w1", new_grant(&w, "w1")).await.unwrap();
    let after = grant(&w, &worker.grant_id).await;
    assert_eq!(after.state, GrantState::Revoked);
    assert_eq!(after.revoked_reason.as_deref(), Some("replaced"));
    let all = w.s.commands().await.unwrap();
    assert!(all.iter().all(|c| c.method != "session.coordinator_revoke"));
    assert!(w.s.outbox().await.unwrap().is_empty());

    // A value out of range is an error and writes nothing.
    let mut bad = new_grant(&w, "owner-3");
    bad.spawn_cap = 99;
    let err =
        w.s.give(w.a.clone(), "owner-3".into(), bad)
            .await
            .unwrap_err();
    assert!(matches!(err, crate::store::DeckError::Invalid(_)));
    assert!(row_of(&w, "owner-3").await.is_none());
    assert!(sets(&w, "owner-3").await.is_empty());
}

/// The receipts of a set: a rejected or failed give revokes the grant
/// (`attach_failed`); an accepted one moves `station_seq`; a later failure
/// changes nothing; an ambiguous set revokes nothing.
#[tokio::test]
async fn the_receipts_of_a_give_revoke_it_or_move_the_station_sequence() {
    let w = world().await;
    let rejected = give(&w, "owner-3", new_grant(&w, "owner-3")).await.unwrap();
    let id = sets(&w, "owner-3").await[0].id.clone();
    reject(&w, &id).await;
    let after = grant(&w, &rejected.grant_id).await;
    assert_eq!(after.state, GrantState::Revoked);
    assert_eq!(after.revoked_reason.as_deref(), Some("attach_failed"));
    assert!(row_of(&w, "owner-3").await.is_some());

    let failed = give(&w, "owner-5", new_grant(&w, "owner-5")).await.unwrap();
    refuse(&w, &sets(&w, "owner-5").await[0].id).await;
    let after = grant(&w, &failed.grant_id).await;
    assert_eq!(after.revoked_reason.as_deref(), Some("attach_failed"));

    // The next give overwrites the row of the revoked grant.
    let again = give(&w, "owner-3", new_grant(&w, "owner-3")).await.unwrap();
    assert_eq!(
        row_of(&w, "owner-3").await.unwrap().grant_id,
        again.grant_id
    );

    let ok = give(&w, "owner-4", new_grant(&w, "owner-4")).await.unwrap();
    accept(&w, &sets(&w, "owner-4").await[0].id, Some(1)).await;
    assert_eq!(row_of(&w, "owner-4").await.unwrap().station_seq, 1);
    assert_eq!(grant(&w, &ok.grant_id).await.state, GrantState::Active);
    // The station never lowers it.
    let wider = Change {
        classes: BTreeSet::from([GrantClass::Read, GrantClass::Mail, GrantClass::Drive]),
        spawn_cap: None,
        max_modes: None,
        answer_cap: None,
        allow_widening: true,
        now: 0,
    };
    w.s.change_access(ok.grant_id.clone(), wider)
        .await
        .unwrap()
        .unwrap();
    let second = sets(&w, "owner-4").await[1].id.clone();
    accept(&w, &second, Some(1)).await;
    assert_eq!(row_of(&w, "owner-4").await.unwrap().station_seq, 1);
    // The station acknowledged a set before, so a later failure of a set
    // changes nothing: the reconcile repeats the latest state.
    let change = Change {
        classes: BTreeSet::from([GrantClass::Read, GrantClass::Mail]),
        spawn_cap: None,
        max_modes: None,
        answer_cap: None,
        allow_widening: false,
        now: 0,
    };
    w.s.change_access(ok.grant_id.clone(), change)
        .await
        .unwrap()
        .unwrap();
    let third = sets(&w, "owner-4").await[2].id.clone();
    reject(&w, &third).await;
    assert_eq!(grant(&w, &ok.grant_id).await.state, GrantState::Active);

    // An ambiguous set revokes nothing.
    let maybe = give(&w, "owner-6", new_grant(&w, "owner-6")).await.unwrap();
    let id = sets(&w, "owner-6").await[0].id.clone();
    w.s.mark_sent(id.clone()).await.unwrap().unwrap();
    w.s.mark_sent(id.clone()).await.unwrap().unwrap();
    refuse(&w, &id).await;
    let cmd = w.s.command(id).await.unwrap().unwrap();
    assert_eq!(cmd.state, DeckCommandState::Ambiguous);
    assert_eq!(grant(&w, &maybe.grant_id).await.state, GrantState::Active);
}

/// A `session.start` with an owner grant: the accepted receipt that names
/// `seq = 1` and the session writes the row `{seq 1, station_seq 1}`; no
/// `seq`, or a worker grant, writes none.
#[tokio::test]
async fn an_accepted_start_receipt_with_seq_one_writes_the_row_of_an_owner_start() {
    let w = world().await;
    let mut cases = Vec::new();
    for (n, seq, worker) in [(1, Some(1), false), (2, None, false), (3, Some(1), true)] {
        let command = format!("start-{n}");
        let grant = if worker {
            worker_grant(&w, "", Some(&command)).await
        } else {
            let mut g = new_grant(&w, "");
            g.start_command_id = Some(command.clone());
            w.s.create_grant(g).await.unwrap()
        };
        let id = CommandId::from(command.as_str());
        let envelope = json!({"command_id": command, "target_incarnation": null,
            "retry_until": w.s.now_ms() + 60_000, "origin": "board",
            "command": {"op": "interrupt", "session_id": "x"}});
        w.s.insert_command(NewCommand {
            command_id: id.clone(),
            station_id: w.a.clone(),
            method: "session.start".to_owned(),
            origin: "board".to_owned(),
            envelope,
            retry_until: w.s.now_ms() + 60_000,
            author: None,
        })
        .await
        .unwrap();
        let session = format!("started-{n}");
        let result = CommandResult {
            session_id: Some(session.as_str().into()),
            seq,
            ..CommandResult::default()
        };
        let receipt = Receipt::accepted(id.clone(), 1, result, w.s.now_ms());
        w.s.settle_command(id, Settle::Receipt(receipt))
            .await
            .unwrap();
        cases.push((session, grant.grant_id));
    }
    let first = row_of(&w, "started-1").await.unwrap();
    assert_eq!(
        first,
        OwnerRow {
            grant_id: cases[0].1.clone(),
            seq: 1,
            station_seq: 1
        }
    );
    assert!(row_of(&w, "started-2").await.is_none());
    assert!(row_of(&w, "started-3").await.is_none());
}

#[path = "owner_change_tests.rs"]
mod change_tests;
#[path = "owner_lifecycle_tests.rs"]
mod lifecycle_tests;
#[path = "owner_sequence_tests.rs"]
mod sequence_tests;
