//! Fleet Git FG-4 on loopback (T1.7, owner acceptance A2): the station copies
//! the ignored files of `worktree_include` from the home into a new worktree
//! (regular files only, 1 MiB each, mode kept, O14); a link and a too-large
//! file are refused and the session still starts; the deck refuses a `..`
//! path; `worktree_env` reaches the provider process (O15).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use common::fleet_git::{Options, Rig};
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::event::Event;
use fleetdeck_testkit::FakeAgent;
use serde_json::{Value, json};

/// Makes `names` ignored in the main checkout (untracked and ignored).
fn ignore(home: &Path, names: &[&str]) {
    let exclude = home.join(".git/info/exclude");
    let mut text = std::fs::read_to_string(&exclude).unwrap_or_default();
    for name in names {
        text.push_str(&format!("{name}\n"));
    }
    std::fs::write(exclude, text).unwrap();
}

fn write_mode(path: &Path, content: &[u8], mode: u32) {
    std::fs::write(path, content).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Starts a worktree session and waits until its first turn is over: the
/// start effect (fetch, worktree, include files, provider) has finished.
fn started(rig: &Rig, i: usize) -> SessionSummary {
    let id = rig.start(i).session_id.as_str().to_owned();
    rig.wait_summary("the first turn", i, &id, |s| {
        s.base.is_some() && s.current_turn.is_none() && s.usage.input_tokens.is_some()
    })
}

/// The skipped files of the include step of `session`, as `(path, reason)`.
async fn skipped(rig: &mut Rig, i: usize, session: &str) -> Vec<(String, String)> {
    let events = rig.events_after_stop(i).await;
    let mut all: Vec<_> = events
        .into_iter()
        .filter(|(id, _)| id == session)
        .filter_map(|(_, e)| match e {
            Event::WorkspaceIncludeSkipped { files } => Some(files),
            _ => None,
        })
        .collect();
    assert!(all.len() <= 1, "one event at most: {all:?}");
    all.pop()
        .unwrap_or_default()
        .into_iter()
        .map(|f| (f.path, f.reason))
        .collect()
}

#[tokio::test]
async fn include_files_are_copied_with_their_mode() {
    let mut rig = Rig::new(&["alpha"]);
    let home = rig.stations[0].home.clone();
    ignore(&home, &[".env", "config/app.local"]);
    write_mode(&home.join(".env"), b"SECRET=1\n", 0o600);
    std::fs::create_dir_all(home.join("config")).unwrap();
    write_mode(&home.join("config/app.local"), b"local=1\n", 0o640);
    rig.project(
        0,
        "App",
        json!({ "worktree_include": [".env", "config/*.local"] }),
    );

    let s = started(&rig, 0);
    let work = PathBuf::from(s.worktree.clone().unwrap());
    assert_eq!(std::fs::read(work.join(".env")).unwrap(), b"SECRET=1\n");
    assert_eq!(mode_of(&work.join(".env")), 0o600);
    assert_eq!(
        std::fs::read(work.join("config/app.local")).unwrap(),
        b"local=1\n"
    );
    assert_eq!(mode_of(&work.join("config/app.local")), 0o640);
    // The source is untouched.
    assert_eq!(std::fs::read(home.join(".env")).unwrap(), b"SECRET=1\n");
    // Nothing was skipped.
    assert_eq!(skipped(&mut rig, 0, s.session_id.as_str()).await, []);
}

#[tokio::test]
async fn a_symlink_is_refused() {
    let mut rig = Rig::new(&["alpha"]);
    let home = rig.stations[0].home.clone();
    ignore(&home, &[".env", "link.env"]);
    write_mode(&home.join(".env"), b"SECRET=1\n", 0o600);
    symlink(home.join(".env"), home.join("link.env")).unwrap();
    rig.project(
        0,
        "App",
        json!({ "worktree_include": [".env", "link.env"] }),
    );

    let s = started(&rig, 0);
    let work = PathBuf::from(s.worktree.clone().unwrap());
    assert!(work.join(".env").is_file());
    assert!(
        std::fs::symlink_metadata(work.join("link.env")).is_err(),
        "the link was not copied"
    );
    assert_eq!(
        skipped(&mut rig, 0, s.session_id.as_str()).await,
        [("link.env".to_owned(), "symlink".to_owned())]
    );
}

#[tokio::test]
async fn a_too_large_file_is_refused() {
    let mut rig = Rig::new(&["alpha"]);
    let home = rig.stations[0].home.clone();
    ignore(&home, &[".env", "big.env"]);
    write_mode(&home.join(".env"), b"SECRET=1\n", 0o600);
    write_mode(&home.join("big.env"), &vec![b'x'; 1024 * 1024 + 1], 0o600);
    rig.project(0, "App", json!({ "worktree_include": [".env", "big.env"] }));

    let s = started(&rig, 0);
    let work = PathBuf::from(s.worktree.clone().unwrap());
    assert!(work.join(".env").is_file());
    assert!(
        !work.join("big.env").exists(),
        "the big file was not copied"
    );
    assert_eq!(
        skipped(&mut rig, 0, s.session_id.as_str()).await,
        [("big.env".to_owned(), "too_large".to_owned())]
    );
}

#[test]
fn a_dotdot_path_is_refused_by_the_deck() {
    let rig = Rig::new(&["alpha"]);
    let id = rig.create_project(0, "App");
    for path in ["../outside", "a/../../b", "/etc/passwd"] {
        let r = rig.save_git(&id, json!({ "sync": true, "worktree_include": [path] }));
        assert_eq!(r.status, 400, "{path}: {}", r.body);
        assert_eq!(r.body["error"], "invalid_git_settings", "{}", r.body);
        assert_eq!(r.body["field"], "worktree_include", "{}", r.body);
    }
    // Nothing was saved.
    assert_eq!(rig.project_of(&id)["git_revision"], 0);
}

/// A provider program that writes its environment to `provider-env.txt`
/// next to itself, then runs the fake Claude (as in the station tests of
/// T1.3b).
fn env_dump_program(dir: &Path) -> PathBuf {
    let fake = FakeAgent::Claude.locate().unwrap();
    let script = dir.join("provider.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nenv > \"$(dirname \"$0\")/provider-env.txt\"\nexec '{}' \"$@\"\n",
            fake.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

#[test]
fn worktree_env_reaches_the_provider() {
    let dir = tempfile::tempdir().unwrap();
    let program = env_dump_program(dir.path());
    let rig = Rig::with(
        &["alpha"],
        Options {
            provider: Some(program),
            ..Options::default()
        },
    );
    let settings: Value = json!({ "worktree_env": { "FG_TEST_FLAG": "sccache" } });
    rig.project(0, "App", settings);
    started(&rig, 0);

    let seen = std::fs::read_to_string(dir.path().join("provider-env.txt")).unwrap();
    let lines: Vec<&str> = seen.lines().collect();
    assert!(lines.contains(&"FG_TEST_FLAG=sccache"), "{lines:?}");
}
