//! Fleet Git FG-9 on loopback (T4.6b, design §12.9, owner acceptance A2 FG-9):
//! a real deck, real stations and real Git move a session from alpha to
//! bravo. A native Claude move and a native Codex move end `done` with the
//! conversation on bravo and the source read only; a branch-only move gets
//! the held first prompt; each resume fault ends as the design says, and the
//! undo gives the session back to alpha; a deck restart in the middle of a
//! move ends `done`; a coordinator moves a session, and a target outside its
//! grant is refused. At every step a probe checks that the source and the
//! target never both accept a prompt (`common::fleet_git_move::watch`). The
//! fake agents write no transcript, so the tests write it (the files of a
//! three-turn conversation). The helpers are in `common/fleet_git_move.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::WAIT;
use common::fleet_git::Rig;
use common::fleet_git_coord::{
    Coord, options as coord_options, project_with_homes, pushed_session, wait_deck_knows,
};
use common::fleet_git_land::options as land_options;
use common::fleet_git_move::{
    USER_FRAME, accepts_prompt, branch_only_session, codex_tail, history, move_view, native_id,
    pushed_codex, resume, start_move, undo, wait_state, watch, write_claude_transcript,
    write_codex_rollout,
};
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::fleet_git::MoveInState;
use fleetdeck_testkit::fake::{
    RESUME_EXIT_AT_START, RESUME_FAULT_ENV, RESUME_MISSING, RESUME_OTHER_ID,
};
use serde_json::{Value, json};

/// A rig with alpha and bravo, a synced project with a home on each (push
/// `on_turn`), and the folders of the homes. `fault` is the resume fault of
/// the fake Claude of bravo; `codex` adds the fake Codex to both stations.
fn world(fault: Option<&str>, codex: bool) -> (Rig, String, Vec<PathBuf>) {
    let mut options = land_options(&["alpha", "bravo"]);
    if let Some(fault) = fault {
        options
            .setup
            .get_mut("bravo")
            .unwrap()
            .instance_env
            .push((RESUME_FAULT_ENV.to_owned(), fault.to_owned()));
    }
    if codex {
        for setup in options.setup.values_mut() {
            setup.tail.push_str(&codex_tail());
        }
    }
    let rig = Rig::with(&["alpha", "bravo"], options);
    let (project, folders) = project_with_homes(&rig, 2, json!({ "push": "on_turn" }));
    (rig, project, folders)
}

fn id_of(s: &SessionSummary) -> &str {
    s.session_id.as_str()
}

/// A pushed Claude session on alpha with a conversation of three turns on
/// disk (the transcript files). Returns the session and the three prompts.
fn native_session(rig: &Rig, project: &str, folder: &Path) -> (SessionSummary, Vec<String>) {
    let (s, _oid) = pushed_session(rig, 0, folder, project, "a.txt");
    wait_deck_knows(rig, &[&s]);
    let native = native_id(rig, 0, id_of(&s));
    let prompts = write_claude_transcript(rig, 0, s.cwd.as_deref().unwrap(), &native);
    (s, prompts)
}

/// Waits until the summary of the session on station `i` holds `state` in
/// its `move_in`.
fn wait_move_in(rig: &Rig, i: usize, session: &str, state: MoveInState) -> SessionSummary {
    rig.wait_summary(&format!("move_in {state:?}"), i, session, |s| {
        s.move_in.as_ref().is_some_and(|m| m.state == state)
    })
}

/// The move ended as the design says for a session that moved: the source
/// is `moved_to` the target on bravo and takes no prompt.
fn assert_source_moved(rig: &Rig, src: &str, dst: &str) {
    let s = rig.summary(0, src);
    let to = s.moved_to.expect("the source has moved_to");
    assert_eq!(to.station_id.as_str(), rig.station_id(1));
    assert_eq!(to.session_id.as_str(), dst);
    assert!(
        !accepts_prompt(rig, 0, src),
        "the moved source took a prompt"
    );
}

/// The first prompt of the target is the probe of the watch.
fn probed(w: &common::fleet_git_move::Watched) -> String {
    assert!(w.target_accepted, "the target took no prompt: {}", w.state);
    w.dst.clone().unwrap()
}

#[test]
fn a_native_claude_move_ends_done_with_the_conversation_on_bravo() {
    let (rig, project, folders) = world(None, false);
    let (src, prompts) = native_session(&rig, &project, &folders[0]);
    let id = id_of(&src);
    let move_id = start_move(&rig, 0, id, 1);

    // The target takes its first prompt only after the release; that prompt
    // confirms the conversation.
    let w = watch(&rig, 0, id, 1, |s| matches!(s, "failed" | "resume_failed"));
    let dst = probed(&w);
    let done = wait_state(&rig, 0, id, "done");
    assert_eq!(done["kind"], "native", "{done}");
    assert_eq!(done["move_id"], move_id.as_str(), "{done}");
    let t = wait_move_in(&rig, 1, &dst, MoveInState::Confirmed);
    assert_eq!(t.branch, src.branch, "the same local branch");
    assert_source_moved(&rig, id, &dst);
    assert!(Path::new(src.worktree.as_deref().unwrap()).exists());

    // No input reached the fake Claude of bravo before the release: its only
    // user frame is the first prompt.
    let log = rig.stations[1].station.stdin();
    assert_eq!(log.count(USER_FRAME), 1, "{:?}", log.lines);
    assert_eq!(log.count("probe"), 1, "{:?}", log.lines);

    // The conversation is on bravo: its history lists the three turns.
    let deadline = Instant::now() + WAIT;
    let items = loop {
        let r = history(&rig, 1, &dst);
        assert_eq!(r.status, 200, "{}", r.body);
        let items = r.body["items"].as_array().cloned().unwrap_or_default();
        if !items.is_empty() {
            break items;
        }
        assert!(Instant::now() < deadline, "no history: {}", r.body);
        std::thread::sleep(Duration::from_millis(100));
    };
    let users: Vec<&str> = items
        .iter()
        .filter(|i| i["kind"] == "user")
        .map(|i| i["text"].as_str().unwrap())
        .collect();
    assert_eq!(users, prompts, "{items:?}");
    assert_eq!(items.len(), 6, "{items:?}");

    // A move that is not `resume_failed` cannot be undone.
    let r = undo(&rig, &move_id);
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(r.body["error"], "move_not_undoable", "{}", r.body);
}

#[test]
fn a_native_codex_move_ends_done_with_the_conversation_on_bravo() {
    let (rig, project, folders) = world(None, true);
    let (src, _oid) = pushed_codex(&rig, 0, &folders[0], &project, "c.txt");
    wait_deck_knows(&rig, &[&src]);
    let id = id_of(&src);
    let native = native_id(&rig, 0, id);
    write_codex_rollout(&rig, 0, src.cwd.as_deref().unwrap(), &native);
    let move_id = start_move(&rig, 0, id, 1);

    let w = watch(&rig, 0, id, 1, |s| matches!(s, "failed" | "resume_failed"));
    let dst = probed(&w);
    let done = wait_state(&rig, 0, id, "done");
    assert_eq!(done["kind"], "native", "{done}");
    assert_eq!(done["move_id"], move_id.as_str(), "{done}");
    // Codex confirms its thread at the commit point; the release stores it.
    wait_move_in(&rig, 1, &dst, MoveInState::Confirmed);
    assert_source_moved(&rig, id, &dst);
}

#[test]
fn a_branch_only_move_gets_the_held_first_prompt_with_the_summary() {
    let (rig, project, folders) = world(None, false);
    let (src, oid) = branch_only_session(&rig, 0, &folders[0], &project, "b.txt");
    wait_deck_knows(&rig, &[&src]);
    let id = id_of(&src);
    let branch = src.branch.clone().unwrap();
    start_move(&rig, 0, id, 1);

    let w = watch(&rig, 0, id, 1, |s| matches!(s, "failed" | "resume_failed"));
    let dst = probed(&w);
    let done = wait_state(&rig, 0, id, "done");
    assert_eq!(done["kind"], "branch_only", "{done}");
    assert_source_moved(&rig, id, &dst);

    // The held prompt reaches the fake Claude after the release.
    let want = format!(
        "The branch {branch} starts at its last pushed commit {}",
        &oid[..12]
    );
    let deadline = Instant::now() + WAIT;
    let line = loop {
        let log = rig.stations[1].station.stdin();
        let found = log
            .lines
            .iter()
            .find(|l| l.contains("continues the work of the session"))
            .cloned();
        if let Some(line) = found {
            break line;
        }
        assert!(Instant::now() < deadline, "no held prompt: {:?}", log.lines);
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(line.contains(&want), "{line}");
    assert!(line.contains("<untrusted source=\\\"session:"), "{line}");
    assert!(line.contains("Continue from there."), "{line}");
    wait_move_in(&rig, 1, &dst, MoveInState::Confirmed);
}

/// A native move whose fake Claude of bravo has the resume `fault`: returns
/// the rig, the source session and the move id.
fn faulty_move(fault: &str) -> (Rig, SessionSummary, String) {
    let (rig, project, folders) = world(Some(fault), false);
    let (src, _prompts) = native_session(&rig, &project, &folders[0]);
    let move_id = start_move(&rig, 0, id_of(&src), 1);
    (rig, src, move_id)
}

/// The source session is resumable after an undo or an abort, and takes a
/// prompt.
fn assert_source_released(rig: &Rig, src: &str) {
    assert!(rig.summary(0, src).moved_to.is_none());
    resume(rig, 0, src);
    rig.wait_summary("the source runs again", 0, src, |s| {
        s.state != SessionState::Exited
    });
    rig.prompt(0, src, "back on alpha");
}

/// The first prompt of a moved Claude session fails (`missing`: the process
/// exits; `other_id`: another conversation starts); the move ends
/// `resume_failed`, the target stays stopped and held, and the undo gives
/// the session back to alpha.
fn resume_failed_then_undo(fault: &str, text: Option<&str>) {
    let (rig, src, move_id) = faulty_move(fault);
    let id = id_of(&src);
    let w = watch(&rig, 0, id, 1, |s| s == "failed");
    let dst = probed(&w);
    let row = wait_state(&rig, 0, id, "resume_failed");
    assert_eq!(row["kind"], "native", "{row}");
    let t = wait_move_in(&rig, 1, &dst, MoveInState::ResumeFailed);
    let info = t.move_in.unwrap();
    assert_eq!(info.code.as_deref(), Some("move_resume_failed"), "{info:?}");
    if let Some(text) = text {
        assert!(info.text.as_deref().unwrap().contains(text), "{info:?}");
    }
    // The source stays `moved` and the target is stopped and held.
    assert_source_moved(&rig, id, &dst);
    assert!(
        !accepts_prompt(&rig, 1, &dst),
        "the failed target took a prompt"
    );

    let r = undo(&rig, &move_id);
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.body["state"], "undoing", "{}", r.body);
    wait_state(&rig, 0, id, "undone");
    wait_move_in(&rig, 1, &dst, MoveInState::Failed);
    assert!(
        !accepts_prompt(&rig, 1, &dst),
        "the undone target took a prompt"
    );
    assert_source_released(&rig, id);
}

#[test]
fn a_failed_claude_resume_can_be_undone_missing() {
    resume_failed_then_undo(RESUME_MISSING, None);
}

#[test]
fn a_failed_claude_resume_can_be_undone_other_id() {
    resume_failed_then_undo(RESUME_OTHER_ID, Some("Claude started conversation"));
}

// The fake Claude of bravo exits at once: the move ends before its commit
// point, the source is released by the abort, and the target never took a
// prompt (the watch probes it at every step).
#[test]
fn a_claude_exit_at_start_aborts_the_move_and_releases_the_source() {
    let (rig, src, _move_id) = faulty_move(RESUME_EXIT_AT_START);
    let id = id_of(&src);
    let w = watch(&rig, 0, id, 1, |s| matches!(s, "failed" | "resume_failed"));
    assert!(!w.target_accepted);
    assert_eq!(w.state, "failed", "the move aborts before its commit point");
    let row = move_view(&rig, 0, id).unwrap();
    assert_eq!(row["code"], "move_resume_failed", "{row}");
    let dst = w.dst.expect("the deck knew the new session");
    let t = wait_move_in(&rig, 1, &dst, MoveInState::Abandoned);
    assert_eq!(
        t.move_in.unwrap().code.as_deref(),
        Some("move_resume_failed")
    );
    assert!(
        !accepts_prompt(&rig, 1, &dst),
        "the abandoned target took a prompt"
    );
    assert_source_released(&rig, id);
}

// The deck restarts while the target waits for its liveness check (`starting`).
// The restart table of design §12.3 goes on: the move ends `done`, and at no
// step do both stations accept a prompt.
#[test]
fn a_deck_restart_in_starting_ends_the_move_done() {
    let (mut rig, project, folders) = world(None, false);
    let (src, _prompts) = native_session(&rig, &project, &folders[0]);
    let id = id_of(&src);
    start_move(&rig, 0, id, 1);
    let w = watch(&rig, 0, id, 1, |s| {
        matches!(s, "starting" | "committed" | "failed" | "resume_failed")
    });
    assert!(!w.target_accepted);
    assert!(
        matches!(w.state.as_str(), "starting" | "committed"),
        "{}",
        w.state
    );
    rig.restart_deck_with(|_| {});

    let w = watch(&rig, 0, id, 1, |s| matches!(s, "failed" | "resume_failed"));
    let dst = probed(&w);
    wait_state(&rig, 0, id, "done");
    wait_move_in(&rig, 1, &dst, MoveInState::Confirmed);
    assert_source_moved(&rig, id, &dst);
}

/// The answer of a coordinator tool once it is not `pending` any more (a
/// call right after a deck restart can wait for the lifecycle fence).
fn settled(coord: &Coord, key: &str, args: &Value) -> common::fleet_git_coord::ToolAnswer {
    let mut args = args.clone();
    args["idempotency_key"] = json!(key);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let answer = coord.call("move_session", args.clone());
        eprintln!("move_session answered: {}", answer.text);
        if !answer.text.contains("\"pending\"") || Instant::now() > deadline {
            return answer;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// A rig with alpha, bravo and charlie, a coordinator on alpha whose grant
/// (`inspect`, `manage`) covers the stations `targets`, and a pushed session
/// on bravo that has a native transcript. The start request names the
/// target stations (grant target stations design §2.1). Returns the session,
/// its id and the arguments of `move_session` to charlie.
fn coord_move_world(
    targets: &[usize],
) -> (
    Rig,
    tempfile::TempDir,
    Coord,
    fleetdeck_proto::SessionSummary,
    String,
    Value,
) {
    let dir = tempfile::tempdir().unwrap();
    let names = ["alpha", "bravo", "charlie"];
    let rig = Rig::with(&names, coord_options(dir.path(), &names));
    let (project, folders) = project_with_homes(&rig, 3, json!({ "push": "on_turn" }));
    // The coordinator starts before any other session: every session of the
    // rig writes its own credential to the same file.
    let coord = Coord::start(&rig, dir.path(), 0, &["inspect", "manage"], targets);
    let (s, _oid) = pushed_session(&rig, 1, &folders[1], &project, "b.txt");
    wait_deck_knows(&rig, &[&s]);
    let id = id_of(&s).to_owned();
    let native = native_id(&rig, 1, &id);
    write_claude_transcript(&rig, 1, s.cwd.as_deref().unwrap(), &native);
    let args = json!({
        "station": rig.station_id(1),
        "session_id": id,
        "to_station": rig.station_id(2),
    });
    (rig, dir, coord, s, id, args)
}

// A grant over alpha and bravo only refuses charlie (`target_excluded`) and
// starts no move.
#[test]
fn a_coordinator_refuses_a_target_outside_its_grant() {
    let (rig, _dir, coord, _s, id, args) = coord_move_world(&[0, 1]);
    let refused = settled(&coord, "move-outside", &args);
    assert!(refused.is_error, "{}", refused.text);
    // The tool text keeps the reject class only; the deck test of the move
    // restart checks the `target_excluded` text.
    assert!(refused.text.starts_with("forbidden:"), "{}", refused.text);
    assert!(move_view(&rig, 1, &id).is_none(), "no move was started");
    assert!(rig.summary(1, &id).moved_to.is_none());
}

// A coordinator on alpha moves a session of bravo to charlie with `manage`:
// the grant covers all three stations.
#[test]
fn a_coordinator_moves_a_session_inside_its_grant() {
    let (rig, _dir, coord, s, id, args) = coord_move_world(&[0, 1, 2]);
    let accepted = settled(&coord, "move-inside", &args);
    assert!(!accepted.is_error, "{}", accepted.text);
    let done = common::fleet_git_move::wait_move(&rig, 1, &id, "done", |m| m["state"] == "done");
    assert_eq!(done["kind"], "native", "{done}");
    let dst = done["dst_session"].as_str().unwrap().to_owned();
    let to = rig
        .summary(1, &id)
        .moved_to
        .expect("the source has moved_to");
    assert_eq!(to.station_id.as_str(), rig.station_id(2));
    assert_eq!(to.session_id.as_str(), dst);
    // Claude confirms its conversation only at its first prompt.
    let t = rig.wait_summary("the target is released", 2, &dst, |s| {
        s.move_in
            .as_ref()
            .is_some_and(|m| m.state == MoveInState::Released)
    });
    assert_eq!(t.branch, s.branch);
}
