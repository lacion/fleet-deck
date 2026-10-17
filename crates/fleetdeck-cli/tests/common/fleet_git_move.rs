//! Helpers of the Fleet Git FG-9 tests on loopback (T4.6b, design §12.9): the
//! transcript files of a native move (the fake agents write none), a Codex
//! provider instance, the move routes of the board, the probe of "this
//! session accepts a prompt", and the watch that checks the rule "at no step
//! do the source and the target both accept a prompt" while a move runs.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::fleet_git::PushState;
use fleetdeck_testkit::FakeAgent;
use serde_json::{Value, json};

use super::fleet_git::{Rig, commit_in};

/// The longest a whole move may run in a test (the Claude liveness alone is 5 s).
pub const MOVE_WAIT: Duration = Duration::from_secs(40);

/// The user line of a stdin log of the fake Claude.
pub const USER_FRAME: &str = "\"type\":\"user\"";

/// Claude's directory name of a working directory.
#[must_use]
pub fn encode_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The `station.toml` tail with the fake Codex as one more provider instance.
#[must_use]
pub fn codex_tail() -> String {
    let fake = FakeAgent::Codex.locate().unwrap();
    format!(
        "\n[[provider_instance]]\nid = \"{}\"\ndriver = \"codex\"\nbinary = \"{}\"\n\
         env = {{ FLEETDECK_FAKE_SCENARIO = \"sim\" }}\n",
        FakeAgent::Codex.instance_id(),
        fake.display()
    )
}

/// The native id that station `i` stored for the session (read only from
/// `station.db`); waits until there is one.
#[must_use]
pub fn native_id(rig: &Rig, i: usize, session: &str) -> String {
    let db = rig.stations[i].station.db();
    let deadline = Instant::now() + super::WAIT;
    loop {
        let found =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .ok()
                .and_then(|conn| {
                    conn.query_row(
                        "SELECT native_session_id FROM sessions WHERE session_id = ?1",
                        [session],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .ok()
                    .flatten()
                });
        if let Some(native) = found {
            return native;
        }
        assert!(Instant::now() < deadline, "no native id for {session}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn write_lines(file: &Path, lines: &[Value]) {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(file, text).unwrap();
}

/// Writes the transcript of a Claude conversation of three turns below the
/// provider home of station `i`, for the session `native` that ran in `cwd`.
/// Returns the three prompts. The fake Claude writes no transcript.
pub fn write_claude_transcript(rig: &Rig, i: usize, cwd: &str, native: &str) -> Vec<String> {
    let file = rig.stations[i]
        .station
        .node
        .home()
        .join(".claude/projects")
        .join(encode_cwd(cwd))
        .join(format!("{native}.jsonl"));
    let mut lines = Vec::new();
    let mut prompts = Vec::new();
    for n in 1..=3 {
        let prompt = format!("question {n}");
        lines.push(json!({
            "type": "user", "uuid": format!("u-{n}"), "sessionId": native,
            "timestamp": format!("2026-01-01T10:00:{:02}.000Z", n * 2),
            "message": { "role": "user", "content": prompt },
        }));
        lines.push(json!({
            "type": "assistant", "uuid": format!("a-{n}"), "sessionId": native,
            "timestamp": format!("2026-01-01T10:00:{:02}.000Z", n * 2 + 1),
            "message": { "role": "assistant", "content": [
                { "type": "text", "text": format!("answer {n}") }
            ] },
        }));
        prompts.push(prompt);
    }
    write_lines(&file, &lines);
    prompts
}

/// Writes the rollout of a Codex thread below the provider home of station
/// `i`. The fake Codex writes none.
pub fn write_codex_rollout(rig: &Rig, i: usize, cwd: &str, native: &str) {
    let file = rig.stations[i]
        .station
        .node
        .home()
        .join(".codex/sessions/2026/01/01")
        .join(format!("rollout-2026-01-01T10-00-00-{native}.jsonl"));
    write_lines(
        &file,
        &[
            json!({
                "type": "session_meta", "timestamp": "2026-01-01T10:00:00.000Z",
                "payload": { "id": native, "cwd": cwd },
            }),
            json!({
                "type": "response_item", "timestamp": "2026-01-01T10:00:01.000Z",
                "payload": { "type": "message", "role": "user", "content": [
                    { "type": "input_text", "text": "question 1" }
                ] },
            }),
        ],
    );
}

/// A Codex session (provider instance `fake-codex`) of the synced `project`
/// on station `i` whose commit of `file` was pushed. Returns the session and
/// the commit.
#[must_use]
pub fn pushed_codex(
    rig: &Rig,
    i: usize,
    cwd: &Path,
    project: &str,
    file: &str,
) -> (SessionSummary, String) {
    let spec = json!({ "provider_instance": FakeAgent::Codex.instance_id(), "cwd": cwd });
    let s = rig.start_in(
        i,
        cwd,
        json!({ "kind": "worktree" }),
        json!({ "project_id": project, "spec": spec }),
    );
    let id = s.session_id.as_str().to_owned();
    // The first turn ends with a push of the base commit.
    let s = rig.wait_summary("the first push", i, &id, |s| {
        s.push
            .as_ref()
            .is_some_and(|p| p.state == PushState::Pushed && p.oid.is_some())
    });
    let oid = commit_in(
        &PathBuf::from(s.worktree.clone().unwrap()),
        file,
        file,
        file,
    );
    rig.prompt(i, &id, file);
    let want = oid.clone();
    let s = rig.wait_summary("the push", i, &id, move |s| {
        s.push
            .as_ref()
            .is_some_and(|p| p.state == PushState::Pushed && p.oid.as_deref() == Some(&want))
    });
    (s, oid)
}

/// A Claude session of the synced `project` on station `i` that got no
/// prompt, so it has no native id. It committed `file`, was stopped, and its
/// branch was pushed (`Push now`, then `pushed` with the commit). Returns the
/// exited session and the commit.
#[must_use]
pub fn branch_only_session(
    rig: &Rig,
    i: usize,
    cwd: &Path,
    project: &str,
    file: &str,
) -> (SessionSummary, String) {
    let s = rig.start_in(
        i,
        cwd,
        json!({ "kind": "worktree" }),
        json!({ "project_id": project, "prompt": Value::Null }),
    );
    let id = s.session_id.as_str().to_owned();
    let s = rig.wait_summary("the worktree", i, &id, |s| s.worktree.is_some());
    // The summary names the worktree before Git has made it.
    let deadline = Instant::now() + super::WAIT;
    while !Path::new(s.worktree.as_deref().unwrap())
        .join(".git")
        .exists()
    {
        assert!(Instant::now() < deadline, "no worktree folder");
        std::thread::sleep(Duration::from_millis(50));
    }
    let oid = commit_in(
        &PathBuf::from(s.worktree.clone().unwrap()),
        file,
        file,
        file,
    );
    rig.stop_session(i, &id);
    let r = push_now(rig, i, &id);
    assert_eq!(r.status, 200, "{}", r.body);
    let want = oid.clone();
    let s = rig.wait_summary("the push", i, &id, move |s| {
        s.push
            .as_ref()
            .is_some_and(|p| p.state == PushState::Pushed && p.oid.as_deref() == Some(&want))
    });
    (s, oid)
}

/// `POST /api/sessions/{station}/{session}/push` (Push now).
pub fn push_now(rig: &Rig, i: usize, session: &str) -> super::deck::Reply {
    let path = format!("/api/sessions/{}/{session}/push", rig.stations[i].name);
    rig.board("POST", &path, None)
}

/// `POST /api/commands/session.resume` for a session of station `i`.
pub fn resume(rig: &Rig, i: usize, session: &str) {
    let r = rig.board(
        "POST",
        "/api/commands/session.resume",
        Some(json!({
            "station": rig.stations[i].name,
            "command_id": rig.command_id(),
            "command": { "op": "resume", "session_id": session },
        })),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["receipt"]["state"], "accepted", "{}", r.body);
}

/// Sends a follow-up and tells whether the station accepted it. A prompt that
/// is accepted runs a real turn.
#[must_use]
pub fn accepts_prompt(rig: &Rig, i: usize, session: &str) -> bool {
    let r = rig.board(
        "POST",
        "/api/commands/session.follow_up",
        Some(json!({
            "station": rig.stations[i].name,
            "command_id": rig.command_id(),
            "command": {
                "op": "follow_up",
                "session_id": session,
                "input": { "text": "probe" },
            },
        })),
    );
    r.status == 200 && r.body["receipt"]["state"] == "accepted"
}

/// `POST /api/sessions/{station}/{session}/move` from station `from` to
/// station `to`; returns the reply as it is.
pub fn move_reply(rig: &Rig, from: usize, session: &str, to: usize) -> super::deck::Reply {
    rig.board(
        "POST",
        &format!("/api/sessions/{}/{session}/move", rig.stations[from].name),
        Some(json!({
            "to_station": rig.stations[to].name,
            "command_id": rig.command_id(),
        })),
    )
}

/// A move that the deck must accept; returns its id.
pub fn start_move(rig: &Rig, from: usize, session: &str, to: usize) -> String {
    let r = move_reply(rig, from, session, to);
    assert_eq!(r.status, 202, "{}", r.body);
    r.body["move_id"].as_str().unwrap().to_owned()
}

/// The newest move of the session of station `from` (`GET .../move`), or
/// `None` when there is none.
#[must_use]
pub fn move_view(rig: &Rig, from: usize, session: &str) -> Option<Value> {
    let r = rig.board(
        "GET",
        &format!("/api/sessions/{}/{session}/move", rig.stations[from].name),
        None,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    Some(r.body["move"].clone()).filter(|m| !m.is_null())
}

/// Waits until `f` holds for the move of the session.
pub fn wait_move(
    rig: &Rig,
    from: usize,
    session: &str,
    what: &str,
    f: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + MOVE_WAIT;
    loop {
        let view = move_view(rig, from, session);
        if let Some(m) = view.clone().filter(|m| f(m)) {
            return m;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {view:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Waits until the move of the session is in the deck state `state`.
pub fn wait_state(rig: &Rig, from: usize, session: &str, state: &str) -> Value {
    wait_move(rig, from, session, state, |m| m["state"] == state)
}

/// `POST /api/moves/{move}/undo`.
pub fn undo(rig: &Rig, move_id: &str) -> super::deck::Reply {
    rig.board(
        "POST",
        &format!("/api/moves/{move_id}/undo"),
        Some(json!({ "command_id": rig.command_id() })),
    )
}

/// `GET /api/sessions/{station}/{session}/history` of station `i`.
pub fn history(rig: &Rig, i: usize, session: &str) -> super::deck::Reply {
    let path = format!("/api/sessions/{}/{session}/history", rig.stations[i].name);
    rig.board("GET", &path, None)
}

/// The deck states in which the target does not take input yet.
fn before_release(state: &str) -> bool {
    matches!(
        state,
        "holding" | "exporting" | "importing" | "starting" | "committed" | "finishing"
    )
}

/// What [`watch`] saw at its end.
pub struct Watched {
    /// The deck state at the end.
    pub state: String,
    /// The id of the new session, once the deck knew it.
    pub dst: Option<String>,
    /// The target took one prompt (the probe): it is the first prompt of the
    /// moved session.
    pub target_accepted: bool,
}

/// Polls a move and checks, at every step, that the source and the target
/// never both accept a prompt (design §12.9). The source takes no prompt
/// from the deck state `exporting` on. The target takes none, and its fake
/// provider gets no user frame, until the deck sent the release; the first
/// prompt that it takes ends the watch (`target_accepted`), and then the
/// source must be `moved` already. The watch also ends when `until` holds
/// for the deck state.
pub fn watch(
    rig: &Rig,
    from: usize,
    session: &str,
    to: usize,
    until: impl Fn(&str) -> bool,
) -> Watched {
    let deadline = Instant::now() + MOVE_WAIT;
    loop {
        let view = move_view(rig, from, session);
        let state = view
            .as_ref()
            .and_then(|m| m["state"].as_str())
            .unwrap_or("none")
            .to_owned();
        let dst = view
            .as_ref()
            .and_then(|m| m["dst_session"].as_str())
            .map(str::to_owned);
        if until(&state) {
            return Watched {
                state,
                dst,
                target_accepted: false,
            };
        }
        if state != "none" && state != "holding" {
            assert!(
                !accepts_prompt(rig, from, session),
                "the source took a prompt in {state}"
            );
        }
        if let Some(dst) = &dst {
            let users = rig.stations[to].station.stdin().count(USER_FRAME);
            let accepted = accepts_prompt(rig, to, dst);
            // The state is read after the probe: it only moves forward.
            let after = move_view(rig, from, session)
                .and_then(|m| m["state"].as_str().map(str::to_owned))
                .unwrap_or_default();
            if users > 0 || accepted {
                assert!(
                    !before_release(&after),
                    "the target took input in {after} (user frames {users}, accepted {accepted})"
                );
                assert!(
                    rig.summary(from, session).moved_to.is_some(),
                    "the target took input before the source was moved"
                );
            }
            if accepted {
                return Watched {
                    state: after,
                    dst: Some(dst.clone()),
                    target_accepted: true,
                };
            }
        }
        assert!(
            Instant::now() < deadline,
            "the move did not end in time (state {state})"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
