//! The session move on the deck, part two (T4.4b, design §12.3 "Deck
//! restart", §12.4A, §13.3; FG-9): a deck restart in each state, "Undo move",
//! and the coordinator move with its authority. The fake stations are those
//! of `fleet_git_move_deck.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
#[path = "common/fleet_git_move_fake.rs"]
mod move_fake;
#[path = "common/fleet_git_move_world.rs"]
mod world;

use std::time::Duration;

use common::fleet::{bound_for, call};
use common::fleet_git_fake::{expect, station_scopes};
use fleetdeck_deck::store::session_moves::MoveState;
use fleetdeck_deck::store::session_moves_steps::MovePatch;
use fleetdeck_deck::store::{AuditResult, GrantClass};
use fleetdeck_proto::EventEnvelope;
use fleetdeck_proto::command::{ReceiptState, RejectCode};
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::fleet_git::{MoveInInfo, MoveInState};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::{CoordinatorBinding, CoordinatorOp};
use move_fake::R;
use serde_json::{Value, json};
use world::World;

/// Starts a move of `s1` before the driver runs, and walks the row to
/// `state` as the driver would have.
async fn row_in(w: &World, state: MoveState) -> String {
    let id = w.start().await;
    let s = &w.s.f.store;
    let path: &[(&'static [MoveState], MoveState)] = &[
        (&[MoveState::Holding], MoveState::Exporting),
        (&[MoveState::Exporting], MoveState::Importing),
        (&[MoveState::Importing], MoveState::Starting),
        (&[MoveState::Starting], MoveState::Committed),
        (&[MoveState::Committed], MoveState::Finishing),
        (&[MoveState::Finishing], MoveState::Releasing),
        (&[MoveState::Releasing], MoveState::Done),
        (&[MoveState::Done], MoveState::ResumeFailed),
    ];
    for (from, to) in path {
        if state == MoveState::Holding {
            break;
        }
        s.move_advance(id.clone(), from, *to, MovePatch::default())
            .await
            .unwrap();
        if *to == state {
            break;
        }
    }
    if matches!(
        state,
        MoveState::Committed | MoveState::Finishing | MoveState::ResumeFailed
    ) {
        s.move_set_dst(id.clone(), "new-1".to_owned())
            .await
            .unwrap();
    }
    id
}

fn event(session: &str, seq: u64, state: MoveInState, id: &str) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at: 1_000,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::SessionMoveIn(MoveInInfo {
            move_id: id.to_owned(),
            state,
            code: (state == MoveInState::ResumeFailed).then(|| "move_resume_failed".to_owned()),
            text: (state == MoveInState::ResumeFailed)
                .then(|| "Claude could not resume the conversation.".to_owned()),
        })),
    }
}

// ----- the restart -----

#[tokio::test]
async fn a_restart_aborts_a_move_that_ran_in_memory_and_sends_the_target_discard_first() {
    for state in [
        MoveState::Holding,
        MoveState::Exporting,
        MoveState::Importing,
    ] {
        let mut w = World::new(false).await;
        let id = row_in(&w, MoveState::Holding).await;
        if state != MoveState::Holding {
            let from: &'static [MoveState] = &[MoveState::Holding];
            w.s.f
                .store
                .move_advance(id.clone(), from, state, MovePatch::default())
                .await
                .unwrap();
        }
        w.drive();
        let row = w.wait_state(&id, MoveState::Failed).await;
        assert_eq!(row.code.as_deref(), Some("move_lost"), "{state:?}");
        assert_eq!(
            row.message.as_deref(),
            Some("The move stopped when the deck restarted. The session stays on fake.")
        );
        assert_eq!(
            w.log.moves(),
            ["bravo:session.move_discard", "fake:session.move_abort"],
            "{state:?}"
        );
    }
}

#[tokio::test]
async fn a_starting_move_after_a_restart_commits_when_the_target_resumed() {
    let mut w = World::new(false).await;
    let id = row_in(&w, MoveState::Starting).await;
    w.drive();
    w.wait_state(&id, MoveState::Done).await;
    assert_eq!(
        w.log.moves(),
        [
            "bravo:session.move_status",
            // The finish asks the target for the id of the new session.
            "bravo:session.move_status",
            "fake:session.move_finish",
            "bravo:session.move_release"
        ]
    );
}

#[tokio::test]
async fn a_starting_move_after_a_restart_aborts_when_the_target_did_not_start() {
    for answer in ["staged", "imported", "unknown", "discarded"] {
        let mut w = World::new(false).await;
        w.b.script(
            "session.move_status",
            vec![R::Value(json!({ "state": answer }))],
        );
        let id = row_in(&w, MoveState::Starting).await;
        w.drive();
        let row = w.wait_state(&id, MoveState::Failed).await;
        let expected = if answer == "discarded" {
            "move_resume_failed"
        } else {
            "move_lost"
        };
        assert_eq!(row.code.as_deref(), Some(expected), "{answer}");
        let log = w.log.moves();
        assert_eq!(
            log[log.len() - 2..],
            ["bravo:session.move_discard", "fake:session.move_abort"],
            "{answer}: {log:?}"
        );
    }
}

#[tokio::test]
async fn a_starting_move_that_is_still_launching_is_asked_again_and_then_aborted() {
    let mut w = World::new(false).await;
    w.b.script(
        "session.move_status",
        vec![
            R::Value(json!({ "state": "launching" })),
            R::Value(json!({ "state": "launching" })),
            R::Value(json!({ "state": "resumed", "session_id": "new-1" })),
        ],
    );
    let id = row_in(&w, MoveState::Starting).await;
    w.drive();
    w.wait_state(&id, MoveState::Done).await;
    // Three answers, and one more for the id of the new session.
    assert_eq!(w.b.requests("session.move_status").len(), 4);
    assert_eq!(w.log.count("bravo:session.move_discard"), 0);
}

#[tokio::test]
async fn a_restart_resends_the_finish_and_the_release_with_their_fixed_ids() {
    let mut w = World::new(false).await;
    let id = row_in(&w, MoveState::Finishing).await;
    w.drive();
    let row = w.wait_state(&id, MoveState::Done).await;
    assert_eq!(
        w.a.command_ids("session.move_finish"),
        [row.finish_command_id]
    );
    assert_eq!(w.log.count("bravo:session.move_release"), 1);
    // A move in `releasing` sends only the release.
    let mut w = World::new(false).await;
    let id = row_in(&w, MoveState::Releasing).await;
    w.drive();
    w.wait_state(&id, MoveState::Done).await;
    assert_eq!(w.log.moves(), ["bravo:session.move_release"]);
}

// ----- Undo move -----

#[tokio::test]
async fn a_failed_claude_resume_can_be_undone_target_first_and_in_one_command() {
    let w = World::new(true).await;
    let id = w.start().await;
    w.wait_state(&id, MoveState::Done).await;
    let undo = format!("/api/moves/{id}/undo");
    let post = |command: &str| {
        let body = json!({ "command_id": command });
        let (c, h, undo) = (&w.s.c, w.s.h(), &undo);
        async move { c.post(undo, &h, body).await }
    };
    // Refused while the move is not `resume_failed`.
    let r = post("u0").await;
    expect(&r, 409, "move_not_undoable");
    assert_eq!(
        r.body["message"],
        "Only a move whose new session could not resume can be undone."
    );
    // The target could not resume after the release.
    w.s.f
        .store
        .apply_events(
            w.bravo.clone(),
            "bravo-db".into(),
            vec![
                event("new-1", 1, MoveInState::Confirmed, &id),
                event("new-1", 2, MoveInState::ResumeFailed, &id),
            ],
        )
        .await
        .unwrap();
    let row = w.wait_state(&id, MoveState::ResumeFailed).await;
    assert_eq!(row.code.as_deref(), Some("move_resume_failed"));
    let r = post("u1").await;
    assert_eq!(
        (r.status, r.body["state"].as_str()),
        (202, Some("undoing")),
        "{}",
        r.body
    );
    let row = w.wait_state(&id, MoveState::Undone).await;
    // The same command id gives the first answer; another one is refused.
    let again = post("u1").await;
    assert_eq!(
        (again.status, again.body["move_id"].as_str()),
        (202, Some(id.as_str()))
    );
    expect(&post("u2").await, 409, "move_not_undoable");
    expect(
        &w.s.c
            .post(
                "/api/moves/ghost/undo",
                &w.s.h(),
                json!({ "command_id": "u3" }),
            )
            .await,
        404,
        "unknown_move",
    );
    // The target first, then one command to the source.
    let log = w.log.moves();
    assert_eq!(
        log[log.len() - 2..],
        ["bravo:session.move_fail", "fake:session.move_undo"],
        "{log:?}"
    );
    let undo_id = row.undo_command_id.clone().unwrap();
    assert!(undo_id.starts_with("moveundo-"));
    assert_eq!(w.a.command_ids("session.move_undo"), [undo_id]);
    let command = &w.a.requests("session.move_undo")[0]["command"];
    assert_eq!(
        (command["session_id"].as_str(), command["move_id"].as_str()),
        (Some("s1"), Some(id.as_str()))
    );
    let audited = w.s.c.audited("session.move_undo", AuditResult::Ok).await;
    assert_eq!(audited.len(), 2, "{audited:?}");
    // The source session is free for a new move.
    assert_eq!(w.post_move("s1", "c9").await.status, 202);
}

#[tokio::test]
async fn a_lost_undo_receipt_is_resent_with_the_same_command_id() {
    let mut w = World::new(false).await;
    w.a.script("session.move_undo", vec![R::Silent, R::Default]);
    let id = row_in(&w, MoveState::ResumeFailed).await;
    let r =
        w.s.c
            .post(
                &format!("/api/moves/{id}/undo"),
                &w.s.h(),
                json!({ "command_id": "u1" }),
            )
            .await;
    assert_eq!(r.status, 202, "{}", r.body);
    w.drive();
    let row = w.wait_state(&id, MoveState::Undone).await;
    let ids = w.a.command_ids("session.move_undo");
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert_eq!(ids[0], ids[1]);
    assert_eq!(Some(&ids[0]), row.undo_command_id.as_ref());
}

#[tokio::test]
async fn the_source_is_undone_only_after_the_target_answered_failed() {
    let mut w = World::new(false).await;
    w.b.script(
        "session.move_fail",
        vec![
            R::Value(json!({ "state": "confirmed" })),
            R::Err("internal"),
            R::Value(json!({ "state": "failed" })),
        ],
    );
    let id = row_in(&w, MoveState::ResumeFailed).await;
    let r =
        w.s.c
            .post(
                &format!("/api/moves/{id}/undo"),
                &w.s.h(),
                json!({ "command_id": "u1" }),
            )
            .await;
    assert_eq!(r.status, 202, "{}", r.body);
    w.drive();
    w.wait_state(&id, MoveState::Undone).await;
    // `move_undo` follows the third `move_fail` only; a restart in `undoing`
    // resent both steps in this order.
    let log = w.log.moves();
    assert_eq!(
        log,
        [
            "bravo:session.move_fail",
            "bravo:session.move_fail",
            "bravo:session.move_fail",
            "fake:session.move_undo"
        ],
        "{log:?}"
    );
    // Undo is refused in every state except `resume_failed`.
    for state in [
        MoveState::Holding,
        MoveState::Starting,
        MoveState::Finishing,
        MoveState::Done,
    ] {
        let w = World::new(false).await;
        let id = row_in(&w, state).await;
        let r =
            w.s.c
                .post(
                    &format!("/api/moves/{id}/undo"),
                    &w.s.h(),
                    json!({ "command_id": "u1" }),
                )
                .await;
        expect(&r, 409, "move_not_undoable");
    }
}

// ----- the coordinator -----

/// The world with a station that may coordinate, a grant over both stations
/// (`both`) or over the source only, and the binding of the coordinator.
async fn coordinated(both: bool) -> (World, CoordinatorBinding) {
    let mut scopes = station_scopes();
    scopes.insert(Scope::Coordinate);
    let w = World::with(scopes, false).await;
    w.s.f.store.set_fleet_control(true);
    let more = if both { vec![w.bravo.clone()] } else { vec![] };
    let b = bound_for(
        &w.s.f,
        &w.s.f.station,
        "coord",
        &[GrantClass::Manage],
        (true, &more),
    )
    .await;
    (w, b)
}

fn op(w: &World) -> CoordinatorOp {
    CoordinatorOp::Move {
        station: w.s.f.station.clone(),
        session_id: "s1".into(),
        to_station: w.bravo.clone(),
    }
}

#[tokio::test]
async fn a_grant_that_covers_only_the_source_station_is_refused_target_excluded() {
    let (w, b) = coordinated(false).await;
    let receipt = call(&w.s.f, &b, "mv1", op(&w)).await;
    assert_eq!(receipt.state, ReceiptState::Rejected);
    let reject = receipt.reject.unwrap();
    assert_eq!(reject.code, RejectCode::Forbidden);
    assert!(
        reject.message.starts_with("target_excluded:"),
        "{}",
        reject.message
    );
    assert!(w.s.f.store.move_open().await.unwrap().is_empty());
    // A condition of §12.2 is `invalid_state` with its code and text.
    let (w, b) = coordinated(true).await;
    let mut running = world::pushed("s1");
    running.current_turn = Some("t1".into());
    w.show(vec![running]).await;
    let receipt = call(&w.s.f, &b, "mv2", op(&w)).await;
    let reject = receipt.reject.unwrap();
    assert_eq!(reject.code, RejectCode::InvalidState);
    assert_eq!(reject.message, "move_not_ready: A turn is running.");
}

#[tokio::test]
async fn a_coordinator_move_carries_its_authority_and_a_revocation_after_the_commit_does_not_stop_it()
 {
    let (mut w, b) = coordinated(true).await;
    // The finish does not get through until the grant is revoked.
    w.a.script("session.move_finish", vec![R::Silent]);
    w.drive();
    let receipt = call(&w.s.f, &b, "mv1", op(&w)).await;
    assert_eq!(receipt.state, ReceiptState::Accepted, "{receipt:?}");
    let id = w.s.f.store.move_open().await.unwrap()[0].move_id.clone();
    let row = w.row(&id).await;
    assert_eq!(row.requested_by, format!("coordinator:{}", b.grant_id));
    w.wait_state(&id, MoveState::Finishing).await;
    w.s.f
        .store
        .revoke_grant(b.grant_id.clone(), "owner".to_owned())
        .await
        .unwrap();
    w.a.script("session.move_finish", vec![R::Default]);
    w.wait_state(&id, MoveState::Done).await;
    // `MoveOut` and `MoveIn` carry the authority and the origin of the
    // coordinator; the finish and the release carry none.
    let authority = |v: &Value| (v["origin"].clone(), v["authority"]["grant_id"].clone());
    let want = (json!("coordinator"), json!(b.grant_id));
    assert_eq!(authority(&w.a.requests("session.move_out")[0]), want);
    assert_eq!(authority(&w.b.requests("session.move_in")[0]), want);
    assert_eq!(
        authority(&w.a.requests("session.move_finish")[0]),
        (json!("board"), Value::Null)
    );
}

#[tokio::test]
async fn a_grant_that_is_lost_before_a_send_aborts_the_move_and_the_source_is_as_before() {
    for (case, code, text) in [
        (
            "revoked",
            "authority_lost",
            "The coordinator lost its permission during the move (grant_revoked).",
        ),
        (
            "off",
            "authority_lost",
            "The coordinator lost its permission during the move (fleet_control_off).",
        ),
        (
            "expired",
            "authority_expired",
            "The coordinator's move request expired before it started (10 minutes).",
        ),
    ] {
        let (mut w, b) = coordinated(true).await;
        // The task wakes up after a second: the grant is lost before it
        // sends anything.
        let mut opts = move_fake::options();
        opts.tick = Duration::from_secs(1);
        w.drive_with(opts);
        let receipt = call(&w.s.f, &b, "mv1", op(&w)).await;
        assert_eq!(receipt.state, ReceiptState::Accepted, "{case}");
        match case {
            "revoked" => {
                w.s.f
                    .store
                    .revoke_grant(b.grant_id.clone(), "owner".to_owned())
                    .await
                    .unwrap();
            }
            "off" => w.s.f.store.set_fleet_control(false),
            _ => w.s.f.det.advance(Duration::from_secs(601)),
        }
        let id = w.s.f.store.move_open().await.unwrap()[0].move_id.clone();
        let row = w.wait_state(&id, MoveState::Failed).await;
        assert_eq!(
            (row.code.as_deref(), row.message.as_deref()),
            (Some(code), Some(text)),
            "{case}"
        );
        // Nothing reached the source; the target discard comes first.
        assert_eq!(
            w.log.moves(),
            ["bravo:session.move_discard", "fake:session.move_abort"],
            "{case}"
        );
    }
}

#[tokio::test]
async fn a_grant_that_is_lost_between_the_hold_and_the_export_aborts_before_the_export() {
    let (mut w, b) = coordinated(true).await;
    // The answer to `MoveOut` is slow: the grant is revoked meanwhile.
    w.a.script("session.move_out", vec![R::After(400)]);
    w.drive();
    let receipt = call(&w.s.f, &b, "mv1", op(&w)).await;
    assert_eq!(receipt.state, ReceiptState::Accepted);
    let id = w.s.f.store.move_open().await.unwrap()[0].move_id.clone();
    w.a.wait_for("session.move_out", 1).await;
    w.s.f
        .store
        .revoke_grant(b.grant_id.clone(), "owner".to_owned())
        .await
        .unwrap();
    let row = w.wait_state(&id, MoveState::Failed).await;
    assert_eq!(row.code.as_deref(), Some("authority_lost"));
    // The hold was made, so the source is released by the abort; the
    // export never started.
    let log = w.log.moves();
    assert_eq!(
        log,
        [
            "fake:session.move_out",
            "bravo:session.move_discard",
            "fake:session.move_abort"
        ],
        "{log:?}"
    );
}

#[tokio::test]
async fn the_move_request_of_a_coordinator_is_idempotent_and_keeps_its_authority() {
    let (w, b) = coordinated(true).await;
    let a = call(&w.s.f, &b, "mv1", op(&w)).await;
    let again = call(&w.s.f, &b, "mv1", op(&w)).await;
    assert_eq!(
        (a.state, again.state),
        (ReceiptState::Accepted, ReceiptState::Accepted)
    );
    let open = w.s.f.store.move_open().await.unwrap();
    assert_eq!(open.len(), 1);
    let authority = open[0].authority.clone().unwrap();
    assert_eq!(authority.grant_id, b.grant_id);
    assert!(open[0].authority_expires_at.is_some());
}
