//! Helpers of the Fleet Git phase 3 tests on loopback (T3.5, FG-7, FG-8): the
//! station options for a land (a committer identity, `allow_check_cmd`), a
//! `git` wrapper that can block one `push` (apply it and hang, hang before it,
//! or wait on a FIFO), a fake `glab` and `gh`, and the land routes of the
//! board. The scripts live in `<rig root>/fx` (an allowed root) and reach
//! every station through `Options.scripts` (`PATH`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::WAIT;
use super::fleet_git::{Options, Rig, StationSetup, commit_in};
use fleetdeck_proto::SessionSummary;

/// The `git` wrapper. `fx/push.log` gets the arguments of every `push`. When
/// `fx/block` exists (line 1: the mode, line 2: a text that the arguments of
/// the push must hold), a matching push writes its pid to `fx/wrapper.pid`,
/// touches `fx/entered`, and then: `after` runs the real push and sleeps,
/// `before` sleeps without a push, `fifo` waits until someone writes to the
/// FIFO `fx/release` and then runs the real push.
const GIT_WRAPPER: &str = r#"#!/bin/sh
FX={FX}
REAL={REAL_GIT}
case " $* " in
*" push "*)
  echo "$*" >> "$FX/push.log"
  if [ -f "$FX/block" ]; then
    mode=$(sed -n 1p "$FX/block")
    pat=$(sed -n 2p "$FX/block")
    case "$*" in
    *"$pat"*)
      echo $$ > "$FX/wrapper.pid"
      case "$mode" in
      after) "$REAL" "$@" || exit $?; touch "$FX/entered"; exec sleep 120;;
      before) touch "$FX/entered"; exec sleep 120;;
      fifo) touch "$FX/entered"; read _line < "$FX/release";;
      esac;;
    esac
  fi;;
esac
exec "$REAL" "$@"
"#;

/// The fake forge CLI (`glab` and `gh`): the project read answers the id 42; a
/// lookup answers the merge requests of `fx/prs` (an empty list at first); a
/// create writes `fx/prs` and logs one line to `fx/creates`. Every merge
/// request comes from project 42 and goes to it. Every call is logged to
/// `fx/forge_calls`.
const FORGE_CLI: &str = r#"#!/bin/sh
FX={FX}
echo "$@" >> "$FX/forge_calls"
case "$*" in
*"-X GET"*"/merge_requests?"*)
  printf 'HTTP/2.0 200 OK\n\n'
  if [ -s "$FX/prs" ]; then cat "$FX/prs"; else echo '[]'; fi;;
*"-X GET"*)
  printf 'HTTP/2.0 200 OK\n\n{"id":42}';;
*"-X POST"*)
  cat > /dev/null
  echo post >> "$FX/creates"
  echo '[{"iid":5,"web_url":"https://forge.example/mr/5","source_project_id":42,"target_project_id":42}]' > "$FX/prs"
  printf 'HTTP/2.0 201 Created\n\n{"iid":5,"web_url":"https://forge.example/mr/5"}';;
esac
"#;

/// The real `git` on the PATH of the test.
fn real_git() -> String {
    std::env::var("PATH")
        .unwrap()
        .split(':')
        .map(|d| std::path::Path::new(d).join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH")
        .display()
        .to_string()
}

/// The station options of a land test: a committer identity (the station runs
/// Git without user configuration), `allow_check_cmd = true` on every named
/// station, and the scripts.
#[must_use]
pub fn options(stations: &[&str]) -> Options {
    let env = |k: &str, v: &str| (k.to_owned(), v.to_owned());
    let setup = StationSetup {
        toml: "allow_check_cmd = true\n".to_owned(),
        ..StationSetup::default()
    };
    Options {
        station_env: vec![
            env("GIT_AUTHOR_NAME", "rig"),
            env("GIT_AUTHOR_EMAIL", "rig@example.invalid"),
            env("GIT_COMMITTER_NAME", "rig"),
            env("GIT_COMMITTER_EMAIL", "rig@example.invalid"),
        ],
        setup: stations
            .iter()
            .map(|s| ((*s).to_owned(), setup.clone()))
            .collect::<BTreeMap<_, _>>(),
        scripts: vec![
            (
                "git".to_owned(),
                GIT_WRAPPER.replace("{REAL_GIT}", &format!("'{}'", real_git())),
            ),
            ("glab".to_owned(), FORGE_CLI.to_owned()),
            ("gh".to_owned(), FORGE_CLI.to_owned()),
        ],
        ..Options::default()
    }
}

/// The folder of the scripts and markers.
#[must_use]
pub fn fx(rig: &Rig) -> PathBuf {
    rig.root.join("fx")
}

/// A GitLab binding that matches the loopback remote (design §11.1: the
/// identity of a `file://` remote is `localhost/<path>`; a GitHub repository
/// has two segments, so only GitLab can name it).
#[must_use]
pub fn forge(rig: &Rig) -> Value {
    json!({
        "kind": "gitlab",
        "host": "localhost",
        "repo": rig.remote.to_str().unwrap().trim_start_matches('/'),
    })
}

/// Blocks the next `push` whose arguments hold `pattern`: `mode` is `after`,
/// `before` or `fifo` (see [`GIT_WRAPPER`]). `fifo` makes the FIFO.
pub fn block_push(rig: &Rig, mode: &str, pattern: &str) {
    let fx = fx(rig);
    let _ = std::fs::remove_file(fx.join("entered"));
    std::fs::write(fx.join("block"), format!("{mode}\n{pattern}\n")).unwrap();
    if mode == "fifo" && !fx.join("release").exists() {
        let ok = Command::new("mkfifo")
            .arg(fx.join("release"))
            .status()
            .unwrap();
        assert!(ok.success(), "mkfifo");
    }
}

/// Removes the block (a push that waits keeps waiting).
pub fn unblock(rig: &Rig) {
    let fx = fx(rig);
    let _ = std::fs::remove_file(fx.join("block"));
    let _ = std::fs::remove_file(fx.join("entered"));
}

/// Waits until a push entered the block.
pub fn wait_entered(rig: &Rig) {
    let marker = fx(rig).join("entered");
    let deadline = Instant::now() + WAIT;
    while !marker.exists() {
        assert!(Instant::now() < deadline, "no push entered the block");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Lets a `fifo` push go on.
pub fn release(rig: &Rig) {
    use std::io::Write;
    let mut fifo = std::fs::OpenOptions::new()
        .write(true)
        .open(fx(rig).join("release"))
        .unwrap();
    writeln!(fifo, "go").unwrap();
}

/// Stops the push that hangs (the station was killed; its child lives on).
pub fn kill_hung(rig: &Rig) {
    if let Ok(pid) = std::fs::read_to_string(fx(rig).join("wrapper.pid")) {
        let _ = Command::new("kill").arg(pid.trim()).status();
    }
}

/// The arguments of every `git push` of the stations, in order.
#[must_use]
pub fn pushes(rig: &Rig) -> Vec<String> {
    std::fs::read_to_string(fx(rig).join("push.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// How many pushes name `target` (a text such as `:refs/heads/main`).
#[must_use]
pub fn pushes_of(rig: &Rig, target: &str) -> usize {
    pushes(rig).iter().filter(|p| p.contains(target)).count()
}

/// The number of lines of an `fx` log (`creates`, `forge_calls`).
#[must_use]
pub fn log_lines(rig: &Rig, name: &str) -> usize {
    std::fs::read_to_string(fx(rig).join(name)).map_or(0, |t| t.lines().count())
}

/// A session of station `i` whose first turn is over.
pub fn idle(rig: &Rig, i: usize) -> SessionSummary {
    let id = rig.start(i).session_id.as_str().to_owned();
    rig.wait_summary("the first turn", i, &id, |s| {
        s.base.is_some() && s.current_turn.is_none() && s.usage.input_tokens.is_some()
    })
}

/// Commits `file` in the worktree of the session; returns the commit.
pub fn commit(s: &SessionSummary, file: &str, content: &str) -> String {
    commit_in(
        &PathBuf::from(s.worktree.clone().unwrap()),
        file,
        content,
        file,
    )
}

/// The ref that the station publishes for the session.
#[must_use]
pub fn publish_ref(s: &SessionSummary) -> String {
    format!("refs/heads/{}", s.branch.clone().unwrap())
}

static REQUESTS: AtomicU32 = AtomicU32::new(0);

/// `POST /api/sessions/{station}/{session}/land`; returns the reply.
pub fn land_reply(rig: &Rig, i: usize, session: &str) -> super::deck::Reply {
    let n = REQUESTS.fetch_add(1, Ordering::Relaxed) + 1;
    rig.board(
        "POST",
        &format!("/api/sessions/{}/{session}/land", rig.stations[i].name),
        Some(json!({ "command_id": format!("landreq-{n}") })),
    )
}

/// A land that must be queued; returns its id.
pub fn land(rig: &Rig, i: usize, session: &str) -> String {
    let r = land_reply(rig, i, session);
    assert_eq!(r.status, 202, "{}", r.body);
    r.body["land_id"].as_str().unwrap().to_owned()
}

/// The row of a land of the project, from the queue or the recent ends.
#[must_use]
pub fn land_row(rig: &Rig, project: &str, land: &str) -> Option<Value> {
    let r = rig.board("GET", &format!("/api/projects/{project}/lands"), None);
    assert_eq!(r.status, 200, "{}", r.body);
    ["queue", "recent"]
        .iter()
        .flat_map(|k| r.body[k].as_array().cloned().unwrap_or_default())
        .find(|row| row["land_id"] == land)
}

/// Waits until `f` holds for the row of the land.
pub fn wait_land(
    rig: &Rig,
    project: &str,
    land: &str,
    what: &str,
    f: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let row = land_row(rig, project, land);
        if let Some(row) = row.as_ref().filter(|r| f(r)) {
            return row.clone();
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {row:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Waits until the land has the state `state`.
pub fn wait_state(rig: &Rig, project: &str, land: &str, state: &str) -> Value {
    wait_land(rig, project, land, state, |r| r["state"] == state)
}

/// Moves `remote.git` back after [`Rig::remove_remote`].
pub fn restore_remote(rig: &Rig, gone: &std::path::Path) {
    std::fs::rename(gone, &rig.remote).unwrap();
}
