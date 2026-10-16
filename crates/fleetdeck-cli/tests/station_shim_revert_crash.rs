//! Real station crashes inside a revert of a Codex session under a shim
//! (PLAN §6.6, L13; ADR-005; DL-117). A `fleetdeck station serve` process
//! with `host = "shim"`, the fake Codex app-server and a Git workspace runs
//! a revert; `FLEETDECK_FAULT=after_step@<step>` aborts the station right
//! after the revert step commits (an uncatchable kill: no destructors, no
//! graceful stop). A new station process then recovers the revert from
//! its stored step:
//!
//! - after `rolled_back` (the provider confirmed the conversation
//!   rollback, no file restored yet) and after `restoring`, the files are
//!   restored and the revert succeeds; the rollback is never sent again;
//! - after `rollback_sent` (the rollback may or may not have reached the
//!   provider) the revert needs reconciliation, the files are unchanged,
//!   the rollback is never sent again and the session takes no input.
//!
//! In every case the shim session is reattached (`running`, one
//! `initialize`), never replaced.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;
mod shim_reaper;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Station, WAIT, open_store, stored_events};
use fleetdeck_proto::command::{ReceiptState, SessionCommand, SessionSpec, UserInput};
use fleetdeck_proto::event::{Event, RevertState, SessionState, TurnStatus};
use fleetdeck_proto::session::{Activity, SessionSummary};
use fleetdeck_proto::{InputId, Receipt, SessionId, TurnId};
use fleetdeck_station::local::{LocalClient, LocalRequest, LocalResponse, socket_path};
use fleetdeck_testkit::FakeAgent;
use fleetdeck_testkit::sim_claude::StdinLog;

/// SIGABRT: what the fault hook's `abort` ends the station with.
const SIGABRT: i32 = 6;

struct Reap(PathBuf);

impl Drop for Reap {
    fn drop(&mut self) {
        shim_reaper::reap(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

struct Rig {
    // Dropped first: the node's directory (with the shim journals) goes
    // away when `station` drops.
    _reap: Reap,
    station: Station,
    log: PathBuf,
}

impl Rig {
    /// A shim-host station with the fake Codex app-server and a Git
    /// workspace; nothing runs yet.
    fn new() -> Self {
        let station = Station::new();
        let node = &station.node;
        let bins = FakeAgent::bin_dir().unwrap();
        FakeAgent::Codex.locate().unwrap();
        let log = node.root().join("codex-stdin.log");
        let mut text = node.station_toml(&bins, &[FakeAgent::Codex]).replacen(
            "host = \"direct\"",
            "host = \"shim\"",
            1,
        );
        text.push_str(&format!(
            "env = {{ FLEETDECK_FAKE_SCENARIO = \"sim\", FLEETDECK_FAKE_STDIN_LOG = {:?} }}\n",
            log.to_str().unwrap()
        ));
        node.write_station_toml(&text).unwrap();
        let ws = &station.workspace;
        git(ws, &["init", "-q", "-b", "main"]);
        std::fs::write(ws.join("a.txt"), "one\n").unwrap();
        git(ws, &["add", "."]);
        git(ws, &["commit", "-q", "-m", "init"]);
        let reap = Reap(node.dirs().station().join("shims"));
        Self {
            station,
            log,
            _reap: reap,
        }
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.station.workspace.join("a.txt")).unwrap()
    }

    fn frames(&self, method: &str) -> usize {
        StdinLog::read(&self.log).count(&format!("\"method\":\"{method}\""))
    }

    /// The provider processes that started a session: each `initialize`
    /// that is not a station-start probe (a probe process sends
    /// `account/read` after its `initialize` and exits; DL-219).
    fn provider_starts(&self) -> usize {
        self.frames("initialize") - self.frames("account/read")
    }

    fn send(&self, command: SessionCommand) -> Receipt {
        let path = socket_path(&self.station.node.dirs().station());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut client = LocalClient::connect(&path).await.unwrap();
            let response = client
                .call(&LocalRequest::Command {
                    command_id: None,
                    command,
                })
                .await
                .unwrap();
            let LocalResponse::Receipt { receipt } = response else {
                panic!("unexpected answer: {response:?}")
            };
            receipt
        })
    }

    fn accepted(&self, command: SessionCommand) -> Receipt {
        let receipt = self.send(command);
        assert_eq!(receipt.state, ReceiptState::Accepted, "{receipt:?}");
        receipt
    }

    fn summary(&self, session: &SessionId) -> SessionSummary {
        self.station
            .sessions()
            .into_iter()
            .find(|s| &s.session_id == session)
            .unwrap()
    }

    fn wait(&self, what: &str, session: &SessionId, f: impl Fn(&SessionSummary) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            let s = self.summary(session);
            if f(&s) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {s:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A session started without a prompt, then one turn that edits
    /// `a.txt` while it runs; returns the session and the turn id.
    fn edit_turn(&self) -> (SessionId, TurnId) {
        let receipt = self.accepted(SessionCommand::Start {
            spec: SessionSpec {
                provider_instance: FakeAgent::Codex.instance_id().into(),
                cwd: self.station.workspace.to_string_lossy().into_owned(),
                mode: None,
                model: None,
                title: None,
                coordinator: None,
            },
            prompt: None,
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
        });
        let session = receipt.result.and_then(|r| r.session_id).unwrap();
        self.wait("the thread opens", &session, |s| {
            s.state == SessionState::Running && self.frames("thread/start") == 1
        });
        self.accepted(SessionCommand::Prompt {
            session_id: session.clone(),
            input: text("wait edit"),
        });
        self.wait("the turn runs", &session, |s| {
            s.activity == Activity::Working && s.current_turn.is_some()
        });
        let turn = self.summary(&session).current_turn.unwrap();
        std::fs::write(self.station.workspace.join("a.txt"), "one\nedited\n").unwrap();
        self.accepted(SessionCommand::Interrupt {
            session_id: session.clone(),
        });
        self.wait("the turn ends", &session, |s| {
            s.activity == Activity::Idle && s.current_turn.is_none()
        });
        (session, turn)
    }

    /// Submits the revert until it is admitted (the turn may still be
    /// finalizing its checkpoint).
    fn revert(&self, session: &SessionId, turn: &TurnId) {
        let deadline = Instant::now() + WAIT;
        loop {
            let receipt = self.send(SessionCommand::Revert {
                session_id: session.clone(),
                turn_id: turn.clone(),
            });
            if receipt.state == ReceiptState::Accepted {
                return;
            }
            assert!(Instant::now() < deadline, "never admitted: {receipt:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Runs two turns and a revert under a station armed to crash right
    /// after `step` commits; then starts a new station process.
    fn crash_after(&mut self, step: &str) -> SessionId {
        self.station.spawn(Some(&format!("after_step@{step}")));
        let (session, turn) = self.edit_turn();
        assert_eq!(self.file(), "one\nedited\n");
        self.revert(&session, &turn);
        assert_eq!(self.station.wait_crash(), SIGABRT, "the fault fired");
        self.station.spawn(None);
        session
    }
}

fn text(t: &str) -> UserInput {
    UserInput::new(t.to_owned())
}

fn revert_state(s: &SessionSummary) -> Option<RevertState> {
    s.revert.as_ref().map(|r| r.state)
}

/// The stdin frames of `method` whose line contains `needle`.
fn frames_with(rig: &Rig, method: &str, needle: &str) -> usize {
    StdinLog::read(&rig.log)
        .lines
        .iter()
        .filter(|l| l.contains(&format!("\"method\":\"{method}\"")) && l.contains(needle))
        .count()
}

/// After the restart the revert succeeds without a second rollback, and
/// the reattached session runs the next prompt: its `turn/start` frame
/// with the input's id reaches the same provider, and the turn of that
/// input completes with the provider's answer. Stops the station.
fn assert_completed(rig: &mut Rig, session: &SessionId) {
    rig.wait("the revert succeeds", session, |s| {
        revert_state(s) == Some(RevertState::Succeeded)
    });
    assert_eq!(rig.file(), "one\n", "the files match the turn's start");
    assert_eq!(rig.frames("thread/rollback"), 1, "never sent again");
    let s = rig.summary(session);
    assert_eq!(s.state, SessionState::Running, "reattached, not lost");
    let starts = rig.frames("turn/start");
    let receipt = rig.accepted(SessionCommand::Prompt {
        session_id: session.clone(),
        input: text("after the revert"),
    });
    let input = receipt
        .result
        .and_then(|r| r.input_id)
        .expect("an input id");
    // The provider read the input's frame.
    let deadline = Instant::now() + WAIT;
    while frames_with(rig, "turn/start", input.as_str()) == 0 {
        assert!(
            Instant::now() < deadline,
            "the prompt never reached the provider"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(frames_with(rig, "turn/start", "after the revert"), 1);
    assert_eq!(rig.frames("turn/start"), starts + 1, "one new turn/start");
    // The input's turn completes with the provider's answer (the station
    // still runs; SQLite WAL lets the test read its log).
    let (turn, events) = wait_input_turn_completed(rig, session, &input);
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::MessageCompleted { turn_id, text, .. }
                if *turn_id == turn && text.contains("after the revert")
        )),
        "{events:?}"
    );
    assert_eq!(rig.provider_starts(), 1, "no replacement provider");
    assert_eq!(rig.frames("thread/resume"), 0, "no replacement provider");
    rig.station.stop();
}

/// The stored events of `session` (read while the station runs).
fn session_events(rig: &Rig, session: &SessionId) -> Vec<Event> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async { stored_events(&open_store(&rig.station.db()).await).await })
        .into_iter()
        .filter(|(s, _)| s == session.as_str())
        .map(|(_, e)| e)
        .collect()
}

/// Waits until the turn that the provider opened for `input` (stored
/// `input.updated` with a turn id) has a stored `turn.completed` with
/// status `done`; returns the turn and the session's events.
fn wait_input_turn_completed(
    rig: &Rig,
    session: &SessionId,
    input: &InputId,
) -> (TurnId, Vec<Event>) {
    let deadline = Instant::now() + WAIT;
    loop {
        let events = session_events(rig, session);
        let turn = events.iter().find_map(|e| match e {
            Event::InputUpdated {
                input_id,
                turn_id: Some(turn),
                ..
            } if input_id == input => Some(turn.clone()),
            _ => None,
        });
        if let Some(turn) = turn && events.iter().any(|e| {
            matches!(
                e,
                Event::TurnCompleted { turn_id, status: TurnStatus::Done, .. } if *turn_id == turn
            )
        }) {
            return (turn, events);
        }
        assert!(
            Instant::now() < deadline,
            "the input's turn never completed: {events:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_station_killed_after_a_confirmed_rollback_restores_the_files_after_the_restart_without_a_second_rollback()
 {
    let mut rig = Rig::new();
    let session = rig.crash_after("rolled_back");
    assert_completed(&mut rig, &session);
}

#[test]
fn a_station_killed_during_the_file_restore_finishes_it_after_the_restart() {
    let mut rig = Rig::new();
    let session = rig.crash_after("restoring");
    assert_completed(&mut rig, &session);
}

#[test]
fn a_station_killed_after_the_rollback_was_claimed_requires_reconciliation_and_never_resends_it() {
    let mut rig = Rig::new();
    let session = rig.crash_after("rollback_sent");
    rig.wait("the revert needs reconciliation", &session, |s| {
        revert_state(s) == Some(RevertState::ReconciliationRequired)
    });
    assert_eq!(rig.file(), "one\nedited\n", "no file restored");
    // The crash came after the claim and before the frame: nothing was
    // sent, and recovery never sends it.
    assert_eq!(rig.frames("thread/rollback"), 0);
    let s = rig.summary(&session);
    assert_eq!(s.state, SessionState::Running, "reattached, not lost");
    let receipt = rig.send(SessionCommand::Prompt {
        session_id: session.clone(),
        input: text("blocked"),
    });
    assert_eq!(receipt.state, ReceiptState::Rejected, "input is blocked");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(rig.frames("thread/rollback"), 0);
    assert_eq!(rig.provider_starts(), 1);
    assert_eq!(rig.frames("thread/resume"), 0);
    rig.station.stop();
}
