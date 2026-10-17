//! Fleet Git FG-5 on loopback (T2.5, design §8.9, owner acceptance A2): one
//! project on many stations. A project of a V24 deck database keeps working
//! after the migration, "add home" clones on a second station or adopts an
//! existing complete clone, a removed and added home gets a new generation,
//! an interrupted clone is never adopted, and a station without
//! `allow_local_remotes` refuses a local remote.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::WAIT;
use common::fleet_git::{Options, Rig, StationSetup, git};
use fleetdeck_core::fleet_git_digest::home_folder_name;
use rusqlite::Connection;
use serde_json::{Value, json};

/// A legacy project row of a V24 database.
struct Legacy {
    id: &'static str,
    name: &'static str,
    station: String,
    path: PathBuf,
    created_at: i64,
}

/// Replaces `db` (the deck is down) by a V24 database: the tables of the
/// current database that V24 has are copied (the pairing of the stations, the
/// cards), and the legacy projects are added. The migrations V25 and V26 run
/// at the next deck start.
fn write_v24(db: &Path, legacy: &[Legacy]) {
    let fresh = db.with_extension("v24");
    let mut conn = Connection::open(&fresh).unwrap();
    fleetdeck_deck::store::schema::migrations()
        .to_version(&mut conn, 24)
        .unwrap();
    conn.execute("ATTACH DATABASE ?1 AS old", [db.to_str().unwrap()])
        .unwrap();
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM main.sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for table in tables {
        let known: i64 = conn
            .query_row(
                "SELECT count(*) FROM old.sqlite_master WHERE type = 'table' AND name = ?1",
                [&table],
                |r| r.get(0),
            )
            .unwrap();
        if known == 0 {
            continue;
        }
        let columns: Vec<String> = conn
            .prepare(&format!("PRAGMA main.table_info(\"{table}\")"))
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let list = columns
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        conn.execute(
            &format!("INSERT INTO main.\"{table}\" ({list}) SELECT {list} FROM old.\"{table}\""),
            [],
        )
        .unwrap();
    }
    for p in legacy {
        conn.execute(
            "INSERT INTO main.projects (project_id, name, station_id, path, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                p.id,
                p.name,
                p.station,
                p.path.to_str().unwrap(),
                p.created_at
            ],
        )
        .unwrap();
    }
    conn.execute("DETACH DATABASE old", []).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 24);
    drop(conn);
    for suffix in ["", "-wal", "-shm"] {
        let mut name = db.as_os_str().to_owned();
        name.push(suffix);
        let _ = std::fs::remove_file(name);
    }
    std::fs::rename(&fresh, db).unwrap();
}

/// A rig of `names` whose deck database was rewritten to V24 with `legacy`
/// projects (given the rig, to name the stations) and then migrated by the
/// next deck start.
fn migrated(names: &[&str], legacy: impl FnOnce(&Rig) -> Vec<Legacy>) -> Rig {
    let mut rig = Rig::new(names);
    let projects = legacy(&rig);
    rig.restart_deck_with(|db| write_v24(db, &projects));
    rig
}

fn legacy_of(rig: &Rig, i: usize, id: &'static str, name: &'static str, at: i64) -> Legacy {
    Legacy {
        id,
        name,
        station: rig.station_id(i),
        path: rig.stations[i].home.clone(),
        created_at: at,
    }
}

/// The project of a card, as `GET /api/sessions` lists it (the board fills
/// `project_id` from the homes).
fn card_project(rig: &Rig, session: &str) -> Option<String> {
    let r = rig.board("GET", "/api/sessions", None);
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["summary"]["session_id"] == session)
        .and_then(|c| c["project_id"].as_str().map(str::to_owned))
}

/// Waits until the card of `session` has the project `want`.
fn wait_card_project(rig: &Rig, session: &str, want: &str) {
    let deadline = Instant::now() + WAIT;
    loop {
        if card_project(rig, session).as_deref() == Some(want) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the card of {session} has {:?}, not {want}",
            card_project(rig, session)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn the_migration_keeps_every_existing_project_working() {
    let mut rig = Rig::new(&["alpha"]);
    // A plain session in the home, made before the migration.
    let before = rig.start_with(0, Value::Null, Value::Null);
    let before = before.session_id.as_str().to_owned();
    let legacy = vec![legacy_of(&rig, 0, "p-legacy", "Legacy app", 1_000)];
    rig.restart_deck_with(|db| write_v24(db, &legacy));

    // The project moved into one ready home and kept its fields.
    let p = rig.project_of("p-legacy");
    assert_eq!(p["name"], "Legacy app");
    assert_eq!(p["git"]["sync"], false);
    let homes = p["homes"].as_array().unwrap();
    assert_eq!(homes.len(), 1, "{p}");
    assert_eq!(homes[0]["state"], "ready");
    assert_eq!(homes[0]["station_id"], rig.station_id(0));
    assert_eq!(homes[0]["path"], rig.stations[0].home.to_str().unwrap());

    // The old session keeps its project, and a new start works and joins it.
    wait_card_project(&rig, &before, "p-legacy");
    let started = rig.start_with(0, json!({ "kind": "worktree" }), Value::Null);
    wait_card_project(&rig, started.session_id.as_str(), "p-legacy");

    // The project can turn sync on and then starts as a synced session.
    let r = rig.save_git("p-legacy", json!({ "sync": true }));
    assert_eq!(r.status, 200, "{}", r.body);
    rig.wait_home("p-legacy", "ready");
    let synced = rig.start_with(0, json!({ "kind": "worktree" }), Value::Null);
    assert!(
        synced
            .branch
            .as_deref()
            .is_some_and(|b| b.starts_with("fd/alpha/")),
        "{:?}",
        synced.branch
    );
}

#[test]
fn duplicate_legacy_homes_keep_one_sync_owner() {
    let rig = migrated(&["alpha"], |rig| {
        vec![
            legacy_of(rig, 0, "p-old", "Old twin", 1_000),
            legacy_of(rig, 0, "p-new", "New twin", 2_000),
        ]
    });
    // Both projects kept their home on the one folder.
    for id in ["p-old", "p-new"] {
        let p = rig.project_of(id);
        assert_eq!(p["homes"].as_array().unwrap().len(), 1, "{p}");
        assert_eq!(p["homes"][0]["state"], "ready", "{p}");
        assert_eq!(p["homes"][0]["sync_owner"], false, "{p}");
    }
    // The oldest project turns sync on and owns the folder.
    let r = rig.save_git("p-old", json!({ "sync": true }));
    assert_eq!(r.status, 200, "{}", r.body);
    rig.wait_home("p-old", "ready");
    assert_eq!(rig.project_of("p-old")["homes"][0]["sync_owner"], true);
    // The other one cannot, and keeps its sync-off home.
    let r = rig.save_git("p-new", json!({ "sync": true }));
    assert_eq!(r.status, 400, "{}", r.body);
    assert_eq!(r.body["error"], "invalid_git_settings");
    assert_eq!(r.body["field"], "sync");
    assert_eq!(
        r.body["message"],
        "Another synced project uses this folder on this station."
    );
    let other = rig.project_of("p-new");
    assert_eq!(other["homes"][0]["state"], "ready", "{other}");
    assert_eq!(other["homes"][0]["sync_owner"], false, "{other}");
    // When the owner turns sync off, the other one can take the folder.
    let r = rig.save_git("p-old", json!({ "sync": false }));
    assert_eq!(r.status, 200, "{}", r.body);
    let r = rig.save_git("p-new", json!({ "sync": true }));
    assert_eq!(r.status, 200, "{}", r.body);
}

/// The reserved or real folder of the home on station `i`.
fn folder_of(home: &Value) -> PathBuf {
    let path = home["path"].as_str().or(home["reserved_path"].as_str());
    PathBuf::from(path.unwrap_or_else(|| panic!("a home without a folder: {home}")))
}

/// The remote HEAD commit of `main`.
fn remote_main(rig: &Rig) -> String {
    rig.bare_refs()["refs/heads/main"].clone()
}

#[test]
fn add_home_clones_on_a_second_station() {
    let rig = Rig::new(&["alpha", "bravo"]);
    let id = rig.project(0, "App", json!({}));
    let r = rig.add_home(&id, 1);
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.body["generation"], 1);
    let home = rig.wait_home_on(&id, 1, "ready");
    let folder = folder_of(&home);
    assert!(folder.starts_with(rig.stations[1].home.parent().unwrap()));
    assert_eq!(
        folder.file_name().unwrap().to_str().unwrap(),
        home_folder_name("App")
    );
    // A real clone of the rig remote with the remote `origin`.
    assert_eq!(git(&folder, &["rev-parse", "HEAD"]), remote_main(&rig));
    assert_eq!(
        git(&folder, &["remote", "get-url", "origin"]),
        rig.remote_url()
    );
    // The second home starts a synced session under its own station name.
    let s = rig.start_in(
        1,
        &folder,
        json!({ "kind": "worktree" }),
        json!({ "project_id": id }),
    );
    assert!(
        s.branch
            .as_deref()
            .is_some_and(|b| b.starts_with("fd/bravo/")),
        "{:?}",
        s.branch
    );
}

/// The modification time of a path.
fn mtime(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn add_home_adopts_an_existing_clone() {
    let rig = Rig::new(&["alpha", "bravo", "charlie"]);
    let id = rig.project(0, "App", json!({}));
    // Station charlie has a complete clone in the folder that "add home"
    // would use.
    // (The default clone folder is the first allowed root + `/projects`.)
    let existing = rig.stations[2]
        .home
        .parent()
        .unwrap()
        .join("projects")
        .join(home_folder_name("App"));
    git(
        &rig.root,
        &["clone", "-q", &rig.remote_url(), existing.to_str().unwrap()],
    );
    let existing = existing.canonicalize().unwrap();
    std::fs::write(existing.join("sentinel.txt"), "mine\n").unwrap();
    let (git_dir, head) = (existing.join(".git"), existing.join(".git/HEAD"));
    let (dir_time, head_time) = (mtime(&git_dir), mtime(&head));
    let head_oid = git(&existing, &["rev-parse", "HEAD"]);

    let r = rig.add_home(&id, 2);
    assert_eq!(r.status, 202, "{}", r.body);
    let home = rig.wait_home_on(&id, 2, "ready");
    // The home is that folder, and it was adopted: nothing in it changed (no
    // clone ran).
    assert_eq!(folder_of(&home), existing);
    assert_eq!(
        std::fs::read_to_string(existing.join("sentinel.txt")).unwrap(),
        "mine\n"
    );
    assert_eq!(mtime(&git_dir), dir_time, "the .git folder was touched");
    assert_eq!(mtime(&head), head_time);
    assert_eq!(git(&existing, &["rev-parse", "HEAD"]), head_oid);
}

#[test]
fn remove_and_readd_drops_the_late_answer_of_the_old_generation() {
    let rig = Rig::new(&["alpha", "bravo"]);
    let id = rig.project(0, "App", json!({}));
    assert_eq!(rig.add_home(&id, 1).status, 202);
    let first = rig.wait_home_on(&id, 1, "ready");
    assert_eq!(first["generation"], 1);
    let folder = folder_of(&first);
    std::fs::write(folder.join("sentinel.txt"), "kept\n").unwrap();

    // Remove: the deck deletes the row, the station invalidates its attempt
    // and keeps the folder.
    let r = rig.remove_home(&id, 1);
    assert_eq!(r.status, 204, "{}", r.body);
    assert!(rig.home_on(&id, 1).is_none());
    assert!(folder.join("sentinel.txt").is_file(), "no file is deleted");

    // Add again: generation 2 with a new attempt. The old attempt does not
    // come back: the home is the one of generation 2 (adopting the kept
    // folder) and stays ready.
    let r = rig.add_home(&id, 1);
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.body["generation"], 2, "{}", r.body);
    let second = rig.wait_home_on(&id, 1, "ready");
    assert_eq!(second["generation"], 2);
    assert_ne!(second["attempt_id"], first["attempt_id"]);
    assert_eq!(folder_of(&second), folder);
    std::thread::sleep(Duration::from_millis(1500));
    let again = rig.home_on(&id, 1).unwrap();
    assert_eq!(again["state"], "ready", "{again}");
    assert_eq!(again["generation"], 2);
    assert!(folder.join("sentinel.txt").is_file());
}

/// A `git` program for the station: it runs the real Git, but after a
/// `clone` it writes its pid and a marker file, then waits (so the attempt
/// stays `cloning` while the folder holds a complete clone).
fn slow_git(dir: &Path) -> PathBuf {
    let real = std::env::var("PATH")
        .unwrap()
        .split(':')
        .map(|d| Path::new(d).join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("git");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             for a in \"$@\"; do\n\
             \x20 if [ \"$a\" = clone ]; then\n\
             \x20   '{real}' \"$@\" || exit $?\n\
             \x20   echo $$ > '{dir}/wrapper.pid'\n\
             \x20   touch '{dir}/clone.done'\n\
             \x20   exec sleep 300\n\
             \x20 fi\n\
             done\n\
             exec '{real}' \"$@\"\n",
            real = real.display(),
            dir = dir.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Waits until the home on station `i` has the failure `code`.
fn wait_code(rig: &Rig, project: &str, i: usize, code: &str) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(h) = rig.home_on(project, i).filter(|h| h["code"] == code) {
            return h;
        }
        assert!(
            Instant::now() < deadline,
            "the home is not failed with {code}: {}",
            rig.project_of(project)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn an_interrupted_clone_is_never_adopted() {
    let mut rig = Rig::new(&["alpha", "bravo"]);
    let id = rig.project(0, "App", json!({}));
    let work = rig.root.join("slow-git");
    std::fs::create_dir_all(&work).unwrap();
    let bin = slow_git(&work);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    rig.restart_station(1, &[("PATH", &path)]);

    // The clone finishes, but the station is not told: the attempt stays
    // `cloning`. Then the station dies.
    assert_eq!(rig.add_home(&id, 1).status, 202);
    let deadline = Instant::now() + WAIT;
    while !work.join("clone.done").exists() {
        assert!(Instant::now() < deadline, "the clone did not run");
        std::thread::sleep(Duration::from_millis(50));
    }
    let cloning = rig.wait_home_on(&id, 1, "cloning");
    let folder = folder_of(&cloning);
    rig.kill_station(1);
    let pid = std::fs::read_to_string(work.join("wrapper.pid")).unwrap();
    let _ = std::process::Command::new("kill").arg(pid.trim()).status();

    // The restart marks the attempt `interrupted`; the deck polls it once.
    rig.start_station(1, &[]);
    let failed = wait_code(&rig, &id, 1, "clone_interrupted");
    assert_eq!(failed["state"], "failed");
    assert!(folder.join(".git").is_dir(), "the partial folder stays");

    // A retry with the folder present is refused, and the folder is never
    // adopted (the clone is complete, but a known interrupted attempt owns it).
    std::fs::write(folder.join("sentinel.txt"), "untouched\n").unwrap();
    let r = rig.retry_home(&id, 1);
    assert_eq!(r.status, 202, "{}", r.body);
    let refused = wait_code(&rig, &id, 1, "incomplete_folder");
    assert_eq!(refused["state"], "failed");
    assert!(refused["path"].is_null(), "{refused}");
    assert_eq!(
        std::fs::read_to_string(folder.join("sentinel.txt")).unwrap(),
        "untouched\n"
    );

    // With the folder removed, the retry clones again.
    std::fs::remove_dir_all(&folder).unwrap();
    let r = rig.retry_home(&id, 1);
    assert_eq!(r.status, 202, "{}", r.body);
    let ready = rig.wait_home_on(&id, 1, "ready");
    assert_eq!(folder_of(&ready), folder);
    assert_eq!(git(&folder, &["rev-parse", "HEAD"]), remote_main(&rig));
    assert!(!folder.join("sentinel.txt").exists());
}

#[test]
fn a_local_remote_is_refused_without_the_setting() {
    let mut rig = Rig::with(&["alpha"], Options::default());
    let id = rig.project(0, "App", json!({}));
    // A fourth station of the rig family: no `allow_local_remotes` key.
    rig.add(
        "delta",
        &StationSetup {
            allow_local_remotes: false,
            ..StationSetup::default()
        },
    );
    let info = rig.wait_info("the station.info of delta", 1, |i| i.fleet_git.is_some());
    assert!(!info.fleet_git.unwrap().allow_local_remotes);

    let r = rig.add_home(&id, 1);
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(r.body["error"], "remote_local_refused");
    assert_eq!(
        r.body["message"],
        "This station does not allow a local remote."
    );
    // Nothing was made: one home, and no clone folder on delta.
    assert_eq!(rig.project_of(&id)["homes"].as_array().unwrap().len(), 1);
    assert!(
        !rig.stations[1]
            .home
            .parent()
            .unwrap()
            .join("projects")
            .exists()
    );
}
