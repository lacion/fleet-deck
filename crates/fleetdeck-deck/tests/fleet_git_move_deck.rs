//! The session move on the deck (T4.4b, design §12.2, §12.3, §12.9; FG-9):
//! the route and its conditions, the steps in order with the pipe, the
//! abort (the target first), the finish with a lost or refused receipt, and
//! the pipe failures. Two fake stations answer the move methods from a
//! script; the move task runs as in the deck.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
#[path = "common/fleet_git_move_fake.rs"]
mod move_fake;
#[path = "common/fleet_git_move_world.rs"]
mod world;

use std::time::Duration;

use common::fleet_git_fake::{bearer, expect};
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::session_moves::MoveState;
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::link::Scope;
use move_fake::{OID, R};
use serde_json::{Value, json};
use world::{World, pushed};

/// The id of the new session in the answers of the fake.
const NEW: &str = "new-1";

fn moves(w: &World) -> Vec<String> {
    w.log.moves()
}

// ----- the route -----

#[tokio::test]
async fn the_route_is_for_the_owner_and_checks_its_body_and_its_targets() {
    let w = World::new(false).await;
    let c = &w.s.c;
    // A token with every scope is not the owner's login.
    let token = bearer(c, "t", &[Scope::Read, Scope::Admin]).await;
    let h = [(token.0.as_str(), token.1.as_str())];
    let r = c
        .post(
            &w.path("s1"),
            &h,
            json!({ "to_station": "bravo", "command_id": "c1" }),
        )
        .await;
    expect(&r, 403, "owner_only");
    let body = |b: Value| async { c.post(&w.path("s1"), &w.s.h(), b).await };
    expect(
        &body(json!({ "to_station": "bravo" })).await,
        400,
        "invalid_body",
    );
    expect(
        &body(json!({ "to_station": "bravo", "command_id": "c1", "kind": "native" })).await,
        400,
        "invalid_body",
    );
    expect(
        &body(json!({ "to_station": "bravo", "command_id": "bad id" })).await,
        400,
        "invalid_command_id",
    );
    expect(
        &body(json!({ "to_station": "ghost", "command_id": "c1" })).await,
        404,
        "unknown_station",
    );
    expect(&w.post_move("nope", "c1").await, 404, "unknown_session");
    // Refused requests write nothing; each one that reached a check has an
    // audit row (the owner check comes first).
    assert!(w.s.f.store.move_open().await.unwrap().is_empty());
    let refused = w.s.c.audited("session.move", AuditResult::Refused).await;
    assert_eq!(refused.len(), 5, "{refused:?}");
}

#[tokio::test]
async fn each_condition_of_the_design_is_a_409_with_its_text() {
    let w = World::new(false).await;
    let mut running = pushed("s2");
    running.state = SessionState::Running;
    running.current_turn = Some("t1".into());
    let mut unpushed = pushed("s3");
    unpushed.push = None;
    w.show(vec![pushed("s1"), running, unpushed]).await;
    for (session, text) in [
        ("s2", "A turn is running."),
        ("s3", "This session is not in a synced project."),
    ] {
        let r = w.post_move(session, "c1").await;
        expect(&r, 409, "move_not_ready");
        assert_eq!(r.body["message"], text);
    }
    // The target is the source.
    let r =
        w.s.c
            .post(
                &w.path("s1"),
                &w.s.h(),
                json!({ "to_station": "fake", "command_id": "c2" }),
            )
            .await;
    expect(&r, 409, "move_not_ready");
    assert_eq!(r.body["message"], "The session is already on this station.");
    // A second move of the session while one is open.
    let id = w.start().await;
    let r = w.post_move("s1", "c3").await;
    assert_eq!(r.body["message"], "The session is already moving.");
    // The same command id gives the same move.
    let again = w.post_move("s1", "c1").await;
    assert_eq!(
        (again.status, again.body["move_id"].as_str()),
        (202, Some(id.as_str()))
    );
    let ok = w.s.c.audited("session.move", AuditResult::Ok).await;
    assert_eq!(ok.len(), 2, "{ok:?}");
}

#[tokio::test]
async fn the_board_reads_the_move_and_the_stations_of_the_dialog() {
    let w = World::new(false).await;
    let r = w.s.c.get(&w.path("s1"), &w.s.h()).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["move"], Value::Null);
    let targets = r.body["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert_eq!(targets[0]["name"], "bravo");
    assert_eq!(targets[0]["ready"], true, "{targets:?}");
    assert_eq!(targets[0]["kind"], "native");
    let id = w.start().await;
    let r = w.s.c.get(&w.path("s1"), &w.s.h()).await;
    assert_eq!(r.body["move"]["move_id"], id.as_str());
    assert_eq!(r.body["move"]["state"], "holding");
    // A token is not the owner.
    let token = bearer(&w.s.c, "t", &[Scope::Read]).await;
    let r =
        w.s.c
            .get(&w.path("s1"), &[(token.0.as_str(), token.1.as_str())])
            .await;
    expect(&r, 403, "owner_only");
}

// ----- the steps -----

#[tokio::test]
async fn a_native_move_runs_its_steps_in_order_and_pipes_the_files() {
    let w = World::new(true).await;
    let id = w.start().await;
    let row = w.wait_state(&id, MoveState::Done).await;
    assert_eq!(
        moves(&w),
        [
            "fake:session.move_out",
            "fake:session.move_export",
            "bravo:session.move_prepare",
            "bravo:session.move_in",
            "bravo:session.move_status",
            "fake:session.move_finish",
            "bravo:session.move_release",
        ]
    );
    // The files arrive complete, and the upload ended well.
    assert_eq!(*w.b.imported.lock().unwrap(), b"the moved conversation");
    assert_eq!(*w.b.import_ends.lock().unwrap(), ["closed"]);
    // The row.
    assert_eq!(row.kind, "native");
    assert_eq!(row.dst_session.as_deref(), Some(NEW));
    assert_eq!(row.bytes, Some(22));
    assert_eq!(row.publication.as_ref().map(|p| p.oid.as_str()), Some(OID));
    assert!(row.committed_at.is_some() && row.ended_at.is_some());
    // The commands carry the fixed ids and the origin of the board.
    let out = &w.a.requests("session.move_out")[0];
    assert_eq!(out["origin"], "board");
    assert_eq!(out["command"]["to_station"], w.bravo.as_str());
    assert_eq!(
        w.a.command_ids("session.move_out"),
        [row.out_command_id.as_str()]
    );
    let move_in = &w.b.requests("session.move_in")[0];
    assert_eq!(
        w.b.command_ids("session.move_in"),
        [row.in_command_id.as_str()]
    );
    let command = &move_in["command"];
    assert_eq!(command["kind"], "native");
    assert_eq!(command["provider_instance"], "claude");
    assert_eq!(command["callsign_hint"], "s1");
    assert_eq!(command["title"], "Work of s1");
    assert_eq!(command["from"]["publication"]["oid"], OID);
    assert_eq!(command["from"]["station_id"], w.s.f.station.as_str());
    assert_eq!(command["fleet_git"]["kind"], "worktree");
    assert_eq!(command["fleet_git"]["slug"], "bravo");
    assert_eq!(command["fleet_git"]["home_path"], "/srv/app");
    assert_eq!(move_in["authority"], Value::Null);
    let finish = &w.a.requests("session.move_finish")[0]["command"];
    assert_eq!(
        (finish["to_session"].as_str(), finish["to_station"].as_str()),
        (Some(NEW), Some(w.bravo.as_str()))
    );
    assert_eq!(
        w.a.command_ids("session.move_finish"),
        [row.finish_command_id]
    );
    assert_eq!(
        w.b.requests("session.move_release")[0]["move_id"],
        id.as_str()
    );
}

#[tokio::test]
async fn a_branch_only_move_sends_no_files_and_the_last_summary_of_the_source() {
    let w = World::new(true).await;
    w.b.files.lock().unwrap().bytes.clear();
    w.a.files.lock().unwrap().branch_only = true;
    let mut s = pushed("s1");
    s.digest = Some("It fixed the build.".to_owned());
    w.show(vec![s]).await;
    let id = w.start().await;
    let row = w.wait_state(&id, MoveState::Done).await;
    assert_eq!(row.kind, "branch_only");
    assert!(
        !moves(&w).iter().any(|m| m.ends_with("move_prepare")),
        "{:?}",
        moves(&w)
    );
    let command = &w.b.requests("session.move_in")[0]["command"];
    assert_eq!(command["kind"], "branch_only");
    assert_eq!(command["from"]["summary"], "It fixed the build.");
    assert!(w.b.imported.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_large_conversation_arrives_complete_and_in_order() {
    let w = World::new(true).await;
    let bytes: Vec<u8> = (0..3 * 1024 * 1024_usize)
        .map(|i| (i % 251) as u8)
        .collect();
    w.a.files.lock().unwrap().bytes.clone_from(&bytes);
    let id = w.start().await;
    w.wait_state(&id, MoveState::Done).await;
    assert!(*w.b.imported.lock().unwrap() == bytes, "the bytes differ");
    assert_eq!(w.row(&id).await.bytes, Some(bytes.len() as u64));
}

#[tokio::test]
async fn a_session_without_an_instance_moves_with_the_instance_of_the_export_answer() {
    let w = World::new(true).await;
    let mut s = pushed("s1");
    s.provider_instance = None;
    w.show(vec![s]).await;
    let id = w.start().await;
    w.wait_state(&id, MoveState::Done).await;
    let command = &w.b.requests("session.move_in")[0]["command"];
    assert_eq!(command["provider_instance"], "claude");
}

#[tokio::test]
async fn a_missing_instance_after_the_export_aborts_before_the_prepare() {
    // The summary has no instance, so the request passes (FG-9).
    for (exported, text) in [
        (Some("ghost"), "bravo has no provider named ghost."),
        (None, "bravo has no provider for this session."),
    ] {
        let w = World::new(true).await;
        let mut s = pushed("s1");
        s.provider_instance = None;
        w.show(vec![s]).await;
        w.a.files.lock().unwrap().provider_instance = exported;
        let id = w.start().await;
        let row = w.wait_state(&id, MoveState::Failed).await;
        assert_eq!(row.code.as_deref(), Some("move_not_ready"));
        assert_eq!(row.message.as_deref(), Some(text));
        let log = moves(&w);
        assert!(
            !log.iter()
                .any(|m| m.ends_with("move_prepare") || m.ends_with("move_in")),
            "{log:?}"
        );
        assert_eq!(
            log.last().map(String::as_str),
            Some("fake:session.move_abort")
        );
    }
}

// ----- the abort -----

#[tokio::test]
async fn a_rejected_move_in_aborts_the_target_first_and_then_the_source() {
    let w = World::new(true).await;
    w.b.script(
        "session.move_in",
        vec![R::Reject(
            "invalid_state",
            "move_publication_mismatch: The branch is on x, but bravo pushes this project to y.",
        )],
    );
    let id = w.start().await;
    let row = w.wait_state(&id, MoveState::Failed).await;
    assert_eq!(row.code.as_deref(), Some("move_publication_mismatch"));
    assert_eq!(
        row.message.as_deref(),
        Some("The branch is on x, but bravo pushes this project to y.")
    );
    let log = moves(&w);
    let tail = &log[log.len() - 2..];
    assert_eq!(
        tail,
        ["bravo:session.move_discard", "fake:session.move_abort"],
        "{log:?}"
    );
    // The source was never finished, the target never released.
    assert!(
        !log.iter()
            .any(|m| m.ends_with("move_finish") || m.ends_with("move_release"))
    );
    assert!(row.ended_at.is_some());
    // The source session is free for a new move.
    let again = w.post_move("s1", "c2").await;
    assert_eq!(again.status, 202, "{}", again.body);
}

#[tokio::test]
async fn settling_is_not_an_acknowledgement_and_the_source_waits_for_the_target() {
    let w = World::new(true).await;
    w.b.script(
        "session.move_in",
        vec![R::Reject("invalid_state", "move_branch_exists: x")],
    );
    w.b.script(
        "session.move_discard",
        vec![
            R::Value(json!({ "state": "settling" })),
            R::Err("busy"),
            R::Value(json!({ "state": "abandoned", "effect": "settled" })),
        ],
    );
    let id = w.start().await;
    w.wait_state(&id, MoveState::Failed).await;
    let log = moves(&w);
    let discards = log.iter().filter(|m| m.ends_with("move_discard")).count();
    assert_eq!(discards, 3, "{log:?}");
    // The abort of the source came only after the acknowledgement.
    let abort = log.iter().position(|m| m.ends_with("move_abort")).unwrap();
    assert_eq!(abort, log.len() - 1, "{log:?}");
    assert_eq!(log.iter().filter(|m| m.ends_with("move_abort")).count(), 1);
}

#[tokio::test]
async fn a_discard_that_answers_resumed_makes_the_deck_finish_the_move() {
    let w = World::new(true).await;
    // The answer to `MoveIn` is lost: the move aborts, but the target had
    // committed.
    w.b.script("session.move_in", vec![R::Silent]);
    w.b.script(
        "session.move_discard",
        vec![R::Value(json!({ "state": "resumed", "effect": "none" }))],
    );
    let id = w.start().await;
    let row = w.wait_state(&id, MoveState::Done).await;
    assert_eq!(row.code, None, "the abort note is cleared");
    let log = moves(&w);
    assert!(log.iter().any(|m| m.ends_with("move_finish")), "{log:?}");
    assert!(!log.iter().any(|m| m.ends_with("move_abort")), "{log:?}");
    // The new session is learned from the status of the target.
    assert_eq!(row.dst_session.as_deref(), Some(NEW));
}

#[tokio::test]
async fn a_failure_before_the_commit_reads_the_code_of_the_event() {
    let w = World::new(true).await;
    w.b.script(
        "session.move_status",
        vec![R::Value(json!({
            "state": "abandoned", "session_id": NEW,
            "code": "identity_changed", "text": "Claude resumed another conversation."
        }))],
    );
    let id = w.start().await;
    let row = w.wait_state(&id, MoveState::Failed).await;
    assert_eq!(row.code.as_deref(), Some("identity_changed"));
    assert_eq!(
        row.message.as_deref(),
        Some("Claude resumed another conversation.")
    );
}

#[tokio::test]
async fn a_target_that_keeps_launching_ends_with_a_timeout_abort() {
    let w = World::new(true).await;
    w.b.script(
        "session.move_status",
        vec![R::Value(json!({ "state": "launching" }))],
    );
    let id = w.start().await;
    let row = w.wait_state(&id, MoveState::Failed).await;
    assert_eq!(row.code.as_deref(), Some("move_resume_timeout"));
    let polls = w.b.requests("session.move_status").len();
    assert_eq!(polls, 5, "polls_max of the test options");
}

#[tokio::test]
async fn a_lost_link_during_the_hold_aborts_with_the_station_that_was_lost() {
    let w = World::new(true).await;
    w.a.script("session.move_out", vec![R::Silent]);
    let id = w.start().await;
    let row = w.wait_state(&id, MoveState::Failed).await;
    assert_eq!(row.code.as_deref(), Some("station_unreachable"));
    assert_eq!(
        row.message.as_deref(),
        Some("Fleet Deck lost contact with fake during the move. The session stays on fake.")
    );
}

// ----- the finish -----

#[tokio::test]
async fn a_lost_finish_receipt_is_resent_with_the_same_command_id() {
    let w = World::new(true).await;
    w.a.script("session.move_finish", vec![R::Silent, R::Default]);
    let id = w.start().await;
    let row = w.wait_state(&id, MoveState::Done).await;
    let ids = w.a.command_ids("session.move_finish");
    assert_eq!(
        ids,
        [row.finish_command_id.clone(), row.finish_command_id],
        "{ids:?}"
    );
    // The release came once, after the accepted finish.
    assert_eq!(w.log.count("bravo:session.move_release"), 1);
}

#[tokio::test]
async fn a_source_that_runs_again_keeps_the_move_finishing_and_the_target_held() {
    let w = World::new(true).await;
    w.a.script(
        "session.move_finish",
        vec![R::Reject("invalid_state", "The session runs a turn.")],
    );
    let id = w.start().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let row = loop {
        let row = w.row(&id).await;
        if row.code.as_deref() == Some("source_running") {
            break row;
        }
        assert!(tokio::time::Instant::now() < deadline, "{row:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(row.state, MoveState::Finishing);
    assert_eq!(
        row.message.as_deref(),
        Some("The source session runs again. Stop it, then Fleet Deck finishes the move.")
    );
    assert!(
        w.b.requests("session.move_release").is_empty(),
        "the target stays held"
    );
    // The source stops: the finish is accepted and the move goes on.
    w.a.script("session.move_finish", vec![R::Default]);
    let row = w.wait_state(&id, MoveState::Done).await;
    assert_eq!(row.code, None);
}

#[tokio::test]
async fn a_release_that_is_refused_is_retried_and_never_aborts() {
    let w = World::new(true).await;
    w.b.script(
        "session.move_release",
        vec![R::Err("invalid_state"), R::Err("internal"), R::Default],
    );
    let id = w.start().await;
    w.wait_state(&id, MoveState::Done).await;
    assert_eq!(w.log.count("bravo:session.move_release"), 3);
    assert_eq!(w.log.count("fake:session.move_abort"), 0);
}

// ----- the pipe -----

async fn pipe_failure(
    configure: impl FnOnce(&World),
) -> (World, fleetdeck_deck::store::session_moves::MoveRow) {
    let w = World::new(true).await;
    configure(&w);
    let id = w.start().await;
    let row = w.wait_state(&id, MoveState::Failed).await;
    let log = moves(&w);
    assert!(
        !log.iter().any(|m| m.ends_with("session.move_in")),
        "{log:?}"
    );
    let tail = &log[log.len() - 2..];
    assert_eq!(
        tail,
        ["bravo:session.move_discard", "fake:session.move_abort"],
        "{log:?}"
    );
    (w, row)
}

#[tokio::test]
async fn a_source_that_resets_the_export_stream_aborts_the_move() {
    let (_, row) =
        pipe_failure(|w| w.a.files.lock().unwrap().export_reset = Some("move_aborted")).await;
    assert_eq!(row.code.as_deref(), Some("move_not_ready"));
    let (_, row) = pipe_failure(|w| w.a.files.lock().unwrap().export_reset = Some("move_io")).await;
    assert_eq!(row.code.as_deref(), Some("move_io"));
}

#[tokio::test]
async fn a_target_that_resets_the_upload_fails_with_its_code() {
    let (w, row) =
        pipe_failure(|w| w.b.files.lock().unwrap().import_reset = Some("move_checksum")).await;
    assert_eq!(row.code.as_deref(), Some("move_checksum"));
    assert_eq!(
        row.message.as_deref(),
        Some("The conversation files did not arrive complete. Try again.")
    );
    assert_eq!(*w.b.import_ends.lock().unwrap(), ["move_checksum"]);
    let (_, row) =
        pipe_failure(|w| w.b.files.lock().unwrap().import_reset = Some("move_discarded")).await;
    assert_eq!(row.code.as_deref(), Some("move_discarded"));
}

#[tokio::test]
async fn more_bytes_than_the_manifest_total_are_too_large_and_fewer_are_incomplete() {
    let (_, row) = pipe_failure(|w| w.a.files.lock().unwrap().total = Some(5)).await;
    assert_eq!(row.code.as_deref(), Some("move_too_large"));
    assert_eq!(
        row.message.as_deref(),
        Some("The conversation files are larger than 64 MiB. Use a branch-only move.")
    );
    let (_, row) = pipe_failure(|w| w.a.files.lock().unwrap().total = Some(5000)).await;
    assert_eq!(row.code.as_deref(), Some("move_checksum"));
}

#[tokio::test]
async fn a_station_error_of_the_export_or_the_prepare_is_the_code_of_the_failure() {
    let (_, row) = pipe_failure(|w| {
        w.a.script("session.move_export", vec![R::Err("move_too_large")])
    })
    .await;
    assert_eq!(row.code.as_deref(), Some("move_too_large"));
    let (_, row) = pipe_failure(|w| {
        w.b.script("session.move_prepare", vec![R::Err("move_discarded")])
    })
    .await;
    assert_eq!(row.code.as_deref(), Some("move_discarded"));
}
