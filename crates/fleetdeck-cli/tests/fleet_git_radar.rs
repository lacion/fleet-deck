//! Fleet Git FG-7 on loopback (T3.5, design §10.7, owner acceptance A2): the
//! predictive conflict radar. One project with homes on three stations;
//! sessions on `bravo` and `charlie` push their branches (`push = on_turn`),
//! and the radar runs on the radar home `alpha` (the first home).
//!
//! The deck runs the radar 30 s after a push and every 5 minutes; a test does
//! not wait for that. A restart of the radar home starts the link of that
//! station, and the deck runs the radar of its projects at once (design
//! §10.2). A test calls it after all pushes.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::WAIT;
use common::fleet_git::{Rig, commit_in};
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::fleet_git::PushState;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

/// A rig of `n` stations and a project of `alpha` with `push = on_turn`,
/// with a ready home on every other station.
fn rig_of(n: usize) -> (Rig, String) {
    let names = ["alpha", "bravo", "charlie"];
    let rig = Rig::new(&names[..n]);
    let id = rig.project(0, "App", json!({ "push": "on_turn" }));
    for i in 1..n {
        let r = rig.add_home(&id, i);
        assert_eq!(r.status, 202, "{}", r.body);
        rig.wait_home_on(&id, i, "ready");
    }
    (rig, id)
}

/// A session of the home on station `i`, after its first turn.
fn idle_in(rig: &Rig, project: &str, i: usize) -> SessionSummary {
    let home = rig.home_on(project, i).unwrap();
    let folder = PathBuf::from(home["path"].as_str().unwrap());
    let s = rig.start_in(
        i,
        &folder,
        json!({ "kind": "worktree" }),
        json!({ "project_id": project }),
    );
    let id = s.session_id.as_str().to_owned();
    rig.wait_summary("the first turn", i, &id, |s| {
        s.base.is_some() && s.current_turn.is_none() && s.usage.input_tokens.is_some()
    })
}

/// Commits `file` in the worktree of the session, runs a turn and waits for
/// the push of the commit.
fn push_edit(rig: &Rig, i: usize, s: &SessionSummary, file: &str, content: &str) {
    let work = PathBuf::from(s.worktree.clone().unwrap());
    let oid = commit_in(&work, file, content, file);
    let id = s.session_id.as_str();
    rig.prompt(i, id, file);
    rig.wait_summary("the push", i, id, |s| {
        s.push
            .as_ref()
            .is_some_and(|p| p.state == PushState::Pushed && p.oid.as_deref() == Some(&oid))
    });
}

/// Starts the radar run: the link of the radar home starts again.
fn run_radar(rig: &mut Rig) {
    rig.restart_station(0, &[]);
}

/// `GET /api/radar`.
fn radar(rig: &Rig) -> Value {
    let r = rig.board("GET", "/api/radar", None);
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
}

/// The will-conflict rows of the project in `GET /api/radar`, with the group
/// that holds them.
fn will_conflicts(body: &Value, project: &str) -> Vec<(Value, Value)> {
    body["groups"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["project_id"] == project)
        .flat_map(|g| {
            g["will_conflict"]
                .as_array()
                .unwrap()
                .iter()
                .map(|w| (g.clone(), w.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Waits until `f` finds something in `GET /api/radar`.
fn wait_radar<T>(rig: &Rig, what: &str, f: impl Fn(&Value) -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        let body = radar(rig);
        if let Some(found) = f(&body) {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {body}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The rows of `radar_pairs` of the project, read from the file of the live
/// deck: `(a session, b session, conflict)`.
fn pair_rows(rig: &Rig, project: &str) -> Vec<(String, String, i64)> {
    let db = rig.deck.node.dirs().deck_db();
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT a_session, b_session, conflict FROM radar_pairs WHERE project_id = ?1
             ORDER BY a_session, b_session",
        )
        .unwrap();
    stmt.query_map([project], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn same_lines_will_conflict() {
    let (mut rig, project) = rig_of(3);
    let b = idle_in(&rig, &project, 1);
    let c = idle_in(&rig, &project, 2);
    // Both branches change the one line of README.md.
    push_edit(&rig, 1, &b, "README.md", "bravo\n");
    push_edit(&rig, 2, &c, "README.md", "charlie\n");
    run_radar(&mut rig);

    let (group, row) = wait_radar(&rig, "a will-conflict row", |body| {
        will_conflicts(body, &project)
            .into_iter()
            .find(|(_, w)| w["b"] != "default")
    });
    assert_eq!(row["files"], json!(["README.md"]), "{row}");
    let mut sessions = [
        row["a"]["session_id"].as_str().unwrap().to_owned(),
        row["b"]["session_id"].as_str().unwrap().to_owned(),
    ];
    sessions.sort();
    let mut want = [
        b.session_id.as_str().to_owned(),
        c.session_id.as_str().to_owned(),
    ];
    want.sort();
    assert_eq!(sessions, want, "{row}");
    assert!(row["checked_at"].as_i64().unwrap() > 0);
    // The row is grouped by project: the group names the repository.
    assert_eq!(group["project_id"], project.as_str());
    assert!(
        group["group_key"]
            .as_str()
            .unwrap()
            .starts_with("origin:localhost/"),
        "{group}"
    );
}

#[test]
fn a_branch_that_conflicts_with_the_default_branch_is_flagged() {
    let (mut rig, project) = rig_of(2);
    let b = idle_in(&rig, &project, 1);
    push_edit(&rig, 1, &b, "README.md", "bravo\n");
    // `main` changes the same line after the branch was pushed.
    rig.advance_remote("README.md", "main edit");
    run_radar(&mut rig);

    let (_, row) = wait_radar(&rig, "a row against the default branch", |body| {
        will_conflicts(body, &project)
            .into_iter()
            .find(|(_, w)| w["b"] == "default")
    });
    assert_eq!(row["a"]["session_id"], b.session_id.as_str(), "{row}");
    assert_eq!(row["files"], json!(["README.md"]), "{row}");
}

#[test]
fn different_files_of_one_file_set_do_not() {
    let (mut rig, project) = rig_of(3);
    // The same three files in the base; each branch changes another one.
    for f in ["f1.txt", "f2.txt", "f3.txt"] {
        rig.advance_remote(f, f);
    }
    let b = idle_in(&rig, &project, 1);
    let c = idle_in(&rig, &project, 2);
    push_edit(&rig, 1, &b, "f1.txt", "bravo\n");
    push_edit(&rig, 2, &c, "f2.txt", "charlie\n");
    run_radar(&mut rig);

    // A full run checked three pairs: each branch against `main`, and the two
    // branches. None of them conflicts.
    let deadline = Instant::now() + WAIT;
    let rows = loop {
        let rows = pair_rows(&rig, &project);
        if rows.len() == 3 {
            break rows;
        }
        assert!(Instant::now() < deadline, "the radar rows: {rows:?}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        rows.iter().all(|(_, _, conflict)| *conflict == 0),
        "{rows:?}"
    );
    assert!(
        will_conflicts(&radar(&rig), &project).is_empty(),
        "no will-conflict row"
    );
}
