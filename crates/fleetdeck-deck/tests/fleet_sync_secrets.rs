//! The secret file sync of the deck (Fleet Sync FS-3): the store of the list,
//! the run with fake stations (scan, read, write, newest wins, the seen
//! digests, offline and refused stations), the run lock and the sync route.
//! The fake stations keep their files in memory and answer the three
//! `project.secret_*` methods.
//!
//! Not covered here: a captured log. The deck tests have no log capture
//! (`tracing-subscriber` is not a dependency of this crate), so the log rule
//! (no content, digest or salt) is kept by the code and read in review.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::fleet_git_fake::{FakeGit, FakeOptions, REMOTE, Reply, Setup, setup, spawn_sender};
use fleetdeck_deck::fleet_git_secrets::{SyncError, lock_project, sync_project};
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::GitHomeState;
use fleetdeck_proto::fleet_git_secrets::{SecretOutcome, SecretStationState};
use fleetdeck_proto::link::{Outcome, WireError};
use serde_json::{Value, json};
use tokio::sync::watch;

const ENV: &str = ".env";

/// The files of one fake station.
#[derive(Default)]
struct Fs {
    /// path -> (content, mtime ms)
    files: BTreeMap<String, (Vec<u8>, i64)>,
    /// path -> the refusal code of a scan
    refuse: BTreeMap<String, String>,
    /// The error code that a read answers.
    fail_read: Option<String>,
    /// The error code that a write answers.
    fail_write: Option<String>,
    /// The `(path, expect)` of every write.
    writes: Vec<(String, Option<String>)>,
    clock: i64,
}

type SharedFs = Arc<Mutex<Fs>>;

fn digest(salt_hex: &str, content: &[u8]) -> String {
    let salt: Vec<u8> = (0..salt_hex.len() / 2)
        .map(|i| u8::from_str_radix(&salt_hex[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &salt);
    ring::hmac::sign(&key, content)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn err(code: &str) -> Outcome {
    Outcome::Err(WireError::new(code, "the fake says no"))
}

/// Makes `fake` answer the secret methods from `fs`.
fn install(fake: &FakeGit, fs: &SharedFs, dest: &str) {
    let (fs, dest) = (Arc::clone(fs), dest.to_owned());
    *fake.secret.lock().unwrap() = Some(Box::new(move |method, p: &Value| {
        if p["dest"] != json!(dest) || p["remote_name"] != json!("origin") {
            return err("not_this_repo");
        }
        let mut fs = fs.lock().unwrap();
        match method {
            "project.secret_scan" => {
                let files: Vec<Value> = p["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| {
                        let (path, salt) =
                            (f["path"].as_str().unwrap(), f["salt"].as_str().unwrap());
                        if let Some(code) = fs.refuse.get(path) {
                            return json!({ "path": path, "state": "refused", "code": code });
                        }
                        match fs.files.get(path) {
                            Some((content, mtime)) => json!({
                                "path": path, "state": "present",
                                "digest": digest(salt, content),
                                "size": content.len(), "mtime_ms": mtime
                            }),
                            None => json!({ "path": path, "state": "missing" }),
                        }
                    })
                    .collect();
                Outcome::Ok(json!({ "files": files }))
            }
            "project.secret_read" => {
                if let Some(code) = &fs.fail_read {
                    return err(code);
                }
                let (path, salt) = (p["path"].as_str().unwrap(), p["salt"].as_str().unwrap());
                match fs.files.get(path) {
                    Some((content, _)) if digest(salt, content) == p["digest"] => {
                        Outcome::Ok(json!({ "content_b64": STANDARD.encode(content) }))
                    }
                    Some(_) => err("changed_meanwhile"),
                    None => err("missing"),
                }
            }
            "project.secret_write" => {
                if let Some(code) = &fs.fail_write {
                    return err(code);
                }
                let (path, salt) = (p["path"].as_str().unwrap(), p["salt"].as_str().unwrap());
                let content = STANDARD.decode(p["content_b64"].as_str().unwrap()).unwrap();
                if digest(salt, &content) != p["digest"] {
                    return err("digest_mismatch");
                }
                let expect = p["expect"].as_str().map(str::to_owned);
                let now = match (fs.files.get(path), &expect) {
                    (None, None) => true,
                    (Some((old, _)), Some(d)) => digest(salt, old) == *d,
                    _ => false,
                };
                if !now {
                    return err("changed_meanwhile");
                }
                fs.writes.push((path.to_owned(), expect));
                fs.clock += 1;
                let mtime = 1_000_000 + fs.clock;
                fs.files.insert(path.to_owned(), (content, mtime));
                Outcome::Ok(json!({ "backup": true }))
            }
            _ => err("unknown_method"),
        }
    }));
}

/// A project `App` with homes on `n` stations (position 0 is the fixture
/// station), the secret list `[.env]`, and a fake file system on each.
struct World {
    s: Setup,
    stations: Vec<StationId>,
    fakes: Vec<Arc<FakeGit>>,
    fs: Vec<SharedFs>,
    dest: String,
    _stop: watch::Sender<bool>,
}

impl World {
    fn project(&self) -> String {
        self.s.app.project_id.clone()
    }

    fn content(&self, i: usize, path: &str) -> Option<Vec<u8>> {
        self.fs[i]
            .lock()
            .unwrap()
            .files
            .get(path)
            .map(|f| f.0.clone())
    }

    fn put(&self, i: usize, path: &str, content: &str, mtime: i64) {
        self.fs[i]
            .lock()
            .unwrap()
            .files
            .insert(path.to_owned(), (content.as_bytes().to_vec(), mtime));
    }

    async fn run(&self) -> fleetdeck_proto::fleet_git_secrets::SecretSyncReport {
        sync_project(&self.s.f.store, &self.s.f.ctx, &self.project(), "owner")
            .await
            .unwrap()
    }

    async fn row(&self, path: &str) -> fleetdeck_deck::store::secret_files::SecretFileRow {
        self.s
            .f
            .store
            .secret_file(self.project(), path.to_owned())
            .await
            .unwrap()
            .unwrap()
    }
}

async fn world(n: usize, paths: &[&str]) -> World {
    let mut s = setup().await;
    let r = s.save(json!({ "sync": false }), Some(REMOTE)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let dest = s.app().await.remote_dest.expect("a destination");
    let mut stations = vec![s.f.station.clone()];
    let mut fakes = vec![FakeGit::connect_with(&mut s.f, "fake", FakeOptions::default()).await];
    for i in 1..n {
        let (id, fake) =
            FakeGit::connect_second(&mut s.f, &format!("st{i}"), FakeOptions::default()).await;
        let path = format!("/srv/projects/App{i}");
        *fake.ensure.lock().unwrap() = Reply::cloning(&path);
        *fake.status.lock().unwrap() = Reply::ready(&path);
        stations.push(id);
        fakes.push(fake);
    }
    let ids = stations.clone();
    s.f.wait("the station infos", |f| {
        let (store, ids) = (f.store.clone(), ids.clone());
        async move { ids.iter().all(|i| store.station_info(i).is_some()) }
    })
    .await;
    let (stop, _task) = spawn_sender(&s.f, true);
    let project = s.app.project_id.clone();
    for (i, id) in stations.iter().enumerate().skip(1) {
        let r =
            s.c.post(
                &format!("/api/projects/{project}/homes"),
                &s.h(),
                json!({ "station": id.as_str(), "command_id": format!("add-{i}") }),
            )
            .await;
        assert_eq!(r.status, 202, "{}", r.body);
        let (store, project, id) = (s.f.store.clone(), project.clone(), id.clone());
        s.f.wait("a ready home", move |_| {
            let (store, project, id) = (store.clone(), project.clone(), id.clone());
            async move {
                let p = store.project(project).await.unwrap().unwrap();
                p.homes
                    .iter()
                    .any(|h| h.station_id == id && h.state == GitHomeState::Ready)
            }
        })
        .await;
    }
    let p = s.app().await;
    assert!(
        p.homes.iter().all(|h| h.state == GitHomeState::Ready),
        "{:?}",
        p.homes
    );
    let fs: Vec<SharedFs> = fakes
        .iter()
        .map(|fake| {
            let fs = SharedFs::default();
            install(fake, &fs, &dest);
            fs
        })
        .collect();
    let paths: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
    s.f.store
        .secret_files_set(project, paths)
        .await
        .unwrap()
        .unwrap();
    World {
        s,
        stations,
        fakes,
        fs,
        dest,
        _stop: stop,
    }
}

fn station_states(
    report: &fleetdeck_proto::fleet_git_secrets::SecretFileReport,
) -> Vec<SecretStationState> {
    report.stations.iter().map(|s| s.state).collect()
}

// ----- the store -----

#[tokio::test]
async fn the_periodic_run_picks_projects_with_a_file_and_two_homes() {
    let w = world(2, &[ENV]).await;
    assert_eq!(w.s.f.store.secret_projects().await.unwrap(), [w.project()]);
    // Without a file, or with one home, a project is not picked.
    w.s.f
        .store
        .secret_files_set(w.project(), vec![])
        .await
        .unwrap();
    assert_eq!(
        w.s.f.store.secret_projects().await.unwrap(),
        Vec::<String>::new()
    );
    let one = setup().await;
    one.f
        .store
        .secret_files_set(one.app.project_id.clone(), vec![ENV.into()])
        .await
        .unwrap();
    assert_eq!(
        one.f.store.secret_projects().await.unwrap(),
        Vec::<String>::new()
    );
}

// ----- the run -----

#[tokio::test]
async fn a_run_copies_the_newest_copy_to_a_stale_and_a_missing_home() {
    let w = world(3, &[ENV]).await;
    w.put(0, ENV, "SENTINEL_VALUE=new\n", 300);
    w.put(1, ENV, "SENTINEL_VALUE=old\n", 100);
    let report = w.run().await;
    let file = &report.files[0];
    assert_eq!(file.path, ENV);
    assert_eq!(file.outcome, SecretOutcome::Copied);
    // No station had a seen digest and two copies differ: a conflict.
    assert!(file.conflict);
    assert_eq!(file.source.as_deref(), Some(w.stations[0].as_str()));
    assert!(file.synced_at.is_some());
    assert_eq!(
        station_states(file),
        [
            SecretStationState::Same,
            SecretStationState::Written,
            SecretStationState::Written
        ]
    );
    assert_eq!(w.content(1, ENV).unwrap(), b"SENTINEL_VALUE=new\n");
    assert_eq!(w.content(2, ENV).unwrap(), b"SENTINEL_VALUE=new\n");
    // `expect`: the stale digest for the stale home, nothing for the missing.
    let salt = hex(&w.row(ENV).await.salt);
    let stale = digest(&salt, b"SENTINEL_VALUE=old\n");
    assert_eq!(
        w.fs[1].lock().unwrap().writes,
        [(ENV.to_owned(), Some(stale))]
    );
    assert_eq!(w.fs[2].lock().unwrap().writes, [(ENV.to_owned(), None)]);
    assert!(w.fs[0].lock().unwrap().writes.is_empty());
    // The station calls carry the repository and the generation.
    let write = &w.fakes[2].requests("project.secret_write")[0];
    assert_eq!(write["dest"], json!(w.dest));
    assert_eq!(write["remote_name"], "origin");
    assert_eq!(write["project_id"], json!(w.project()));
    // The stored facts.
    let row = w.row(ENV).await;
    let winner = digest(&salt, b"SENTINEL_VALUE=new\n");
    assert_eq!(row.last_digest.as_deref(), Some(winner.as_str()));
    assert_eq!(row.last_source.as_deref(), Some(w.stations[0].as_str()));
    assert!(row.last_synced_at.is_some());
    assert_eq!(row.last_report, *file);
    let mut seen: Vec<&str> = row.seen.iter().map(|(s, _)| s.as_str()).collect();
    seen.sort_unstable();
    let mut all: Vec<&str> = w.stations.iter().map(StationId::as_str).collect();
    all.sort_unstable();
    assert_eq!(seen, all);
    assert!(row.seen.iter().all(|(_, d)| *d == winner));
    // One audit row for the file, with a digest prefix and no content.
    let rows = w.s.c.audited("project.secret_sync", AuditResult::Ok).await;
    assert_eq!(rows.len(), 1);
    let detail = rows[0].as_deref().unwrap();
    assert!(detail.starts_with(".env from "), "{detail}");
    assert!(
        detail.contains("written 2 failed 0 conflict true digest "),
        "{detail}"
    );
    assert!(detail.ends_with(&winner[..12]), "{detail}");
    // Nothing that the deck stored holds a content or a whole digest.
    let stored = serde_json::to_string(&report).unwrap();
    for hidden in ["SENTINEL", winner.as_str(), salt.as_str()] {
        assert!(!stored.contains(hidden), "{hidden}");
        assert!(!detail.contains(hidden), "{hidden}");
    }
    let audit = format!("{:?}", w.s.f.store.audit_log(100).await.unwrap());
    assert!(
        !audit.contains("SENTINEL") && !audit.contains(&winner),
        "{audit}"
    );
    let dump =
        w.s.f
            .store
            .db()
            .call(|conn| {
                let mut out = String::new();
                for table in ["secret_files", "secret_file_homes"] {
                    let mut stmt = conn.prepare(&format!("SELECT * FROM {table}"))?;
                    let n = stmt.column_count();
                    let rows = stmt.query_map([], |r| {
                        (0..n)
                            .map(|i| {
                                r.get_ref(i).map(|v| match v {
                                    rusqlite::types::ValueRef::Text(t) => {
                                        String::from_utf8_lossy(t).into_owned()
                                    }
                                    other => format!("{other:?}"),
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })?;
                    for row in rows {
                        out.push_str(&row?.join("|"));
                    }
                }
                Ok(out)
            })
            .await
            .unwrap();
    assert!(!dump.contains("SENTINEL"), "{dump}");
    // A second run finds the same copy everywhere: nothing is written.
    let again = w.run().await;
    assert_eq!(again.files[0].outcome, SecretOutcome::Same);
    assert!(!again.files[0].conflict);
    assert_eq!(w.fs[1].lock().unwrap().writes.len(), 1);
    let rows = w.s.c.audited("project.secret_sync", AuditResult::Ok).await;
    assert_eq!(rows.len(), 1, "a run without a write has no audit row");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn a_copy_that_one_home_changed_wins_without_a_conflict() {
    let w = world(3, &[ENV]).await;
    w.put(0, ENV, "V=1\n", 100);
    assert_eq!(w.run().await.files[0].outcome, SecretOutcome::Copied);
    // Only the third home is edited, and it is the oldest by its clock.
    w.put(2, ENV, "V=2\n", 50);
    w.put(0, ENV, "V=1\n", 500);
    w.put(1, ENV, "V=1\n", 600);
    let report = w.run().await;
    let file = &report.files[0];
    assert_eq!(file.outcome, SecretOutcome::Copied);
    assert!(!file.conflict);
    assert_eq!(file.source.as_deref(), Some(w.stations[2].as_str()));
    assert_eq!(w.content(0, ENV).unwrap(), b"V=2\n");
    assert_eq!(w.content(1, ENV).unwrap(), b"V=2\n");
    assert_eq!(
        station_states(file),
        [
            SecretStationState::Written,
            SecretStationState::Written,
            SecretStationState::Same
        ]
    );
}

#[tokio::test]
async fn an_offline_station_makes_the_run_partial_and_keeps_the_baseline() {
    let w = world(3, &[ENV]).await;
    w.put(0, ENV, "V=new\n", 300);
    w.put(1, ENV, "V=old\n", 100);
    // The third station answers nothing but an error: offline for the run.
    *w.fakes[2].secret.lock().unwrap() = None;
    let report = w.run().await;
    let file = &report.files[0];
    assert_eq!(file.outcome, SecretOutcome::Partial);
    assert_eq!(
        station_states(file),
        [
            SecretStationState::Same,
            SecretStationState::Written,
            SecretStationState::Offline
        ]
    );
    assert_eq!(file.source.as_deref(), Some(w.stations[0].as_str()));
    assert_eq!(file.synced_at, None);
    assert_eq!(w.content(1, ENV).unwrap(), b"V=new\n");
    // The baseline did not change; the scanned stations have their seen digest.
    let row = w.row(ENV).await;
    assert_eq!(row.last_digest, None);
    assert_eq!(row.last_synced_at, None);
    assert_eq!(row.seen.len(), 2);
    assert_eq!(row.last_report.outcome, SecretOutcome::Partial);
    let failed =
        w.s.c
            .audited("project.secret_sync", AuditResult::Failed)
            .await;
    assert_eq!(failed.len(), 1);
    // The station comes back: it is a missing home, and the run is complete.
    install(&w.fakes[2], &w.fs[2], &w.dest);
    let again = w.run().await;
    assert_eq!(again.files[0].outcome, SecretOutcome::Copied);
    assert_eq!(w.content(2, ENV).unwrap(), b"V=new\n");
    assert!(w.row(ENV).await.last_digest.is_some());
}

#[tokio::test]
async fn a_refused_station_gets_no_write_and_the_baseline_stays_open() {
    let w = world(3, &[ENV]).await;
    w.put(0, ENV, "V=1\n", 100);
    w.put(1, ENV, "OTHER\n", 10);
    w.fs[1]
        .lock()
        .unwrap()
        .refuse
        .insert(ENV.to_owned(), "not_ignored".to_owned());
    let report = w.run().await;
    let file = &report.files[0];
    assert_eq!(
        station_states(file),
        [
            SecretStationState::Same,
            SecretStationState::Refused,
            SecretStationState::Written
        ]
    );
    assert_eq!(file.stations[1].code.as_deref(), Some("not_ignored"));
    // Every target was written and nobody was offline.
    assert_eq!(file.outcome, SecretOutcome::Copied);
    assert_eq!(file.synced_at, None);
    assert_eq!(w.content(1, ENV).unwrap(), b"OTHER\n");
    let row = w.row(ENV).await;
    assert_eq!(row.last_digest, None);
    // The refused station has no seen digest.
    assert!(row.seen.iter().all(|(s, _)| s != w.stations[1].as_str()));
    // A newer value is never reverted when the station comes back with the
    // old copy: it has no seen digest, so it is not changed.
    w.fs[1].lock().unwrap().refuse.clear();
    w.put(1, ENV, "OLD-BUT-LOUD\n", 9_999);
    let again = w.run().await;
    assert_eq!(again.files[0].outcome, SecretOutcome::Copied);
    assert_eq!(w.content(1, ENV).unwrap(), b"V=1\n");
    assert_eq!(w.content(0, ENV).unwrap(), b"V=1\n");
}

#[tokio::test]
async fn a_failed_write_is_a_failed_station_and_a_partial_run() {
    let w = world(3, &[ENV]).await;
    w.put(0, ENV, "V=1\n", 100);
    w.fs[1].lock().unwrap().fail_write = Some("backup_failed".to_owned());
    let report = w.run().await;
    let file = &report.files[0];
    assert_eq!(file.outcome, SecretOutcome::Partial);
    assert_eq!(
        station_states(file),
        [
            SecretStationState::Same,
            SecretStationState::Failed,
            SecretStationState::Written
        ]
    );
    assert_eq!(file.stations[1].code.as_deref(), Some("backup_failed"));
    assert_eq!(
        file.stations[1].message.as_deref(),
        Some("the fake says no")
    );
    assert_eq!(w.row(ENV).await.last_digest, None);
    let rows =
        w.s.c
            .audited("project.secret_sync", AuditResult::Failed)
            .await;
    assert!(rows[0].as_deref().unwrap().contains("written 1 failed 1"));
}

#[tokio::test]
async fn a_failed_read_fails_every_target_with_its_code() {
    let w = world(3, &[ENV]).await;
    w.put(0, ENV, "V=1\n", 100);
    w.fs[0].lock().unwrap().fail_read = Some("changed_meanwhile".to_owned());
    let report = w.run().await;
    let file = &report.files[0];
    assert_eq!(file.outcome, SecretOutcome::Partial);
    assert_eq!(
        station_states(file),
        [
            SecretStationState::Same,
            SecretStationState::Failed,
            SecretStationState::Failed
        ]
    );
    assert!(
        file.stations[1..]
            .iter()
            .all(|s| s.code.as_deref() == Some("changed_meanwhile"))
    );
    assert!(w.fs[1].lock().unwrap().writes.is_empty());
}

#[tokio::test]
async fn an_answer_that_does_not_parse_is_offline_or_bad_answer() {
    let w = world(2, &[ENV, "k.pem"]).await;
    w.put(0, ENV, "V=1\n", 100);
    // A scan answer with other paths is unusable: the station is offline.
    *w.fakes[1].secret.lock().unwrap() = Some(Box::new(|_, _| {
        Outcome::Ok(json!({ "files": [
            { "path": "other", "state": "missing" },
            { "path": "k.pem", "state": "missing" }
        ] }))
    }));
    let report = w.run().await;
    assert_eq!(report.files[0].outcome, SecretOutcome::Partial);
    assert_eq!(
        report.files[0].stations[1].state,
        SecretStationState::Offline
    );
    // A present state without a digest is unusable too.
    *w.fakes[1].secret.lock().unwrap() = Some(Box::new(|_, p| {
        let files: Vec<Value> = p["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| json!({ "path": f["path"], "state": "present" }))
            .collect();
        Outcome::Ok(json!({ "files": files }))
    }));
    let report = w.run().await;
    assert_eq!(
        report.files[0].stations[1].state,
        SecretStationState::Offline
    );
    // An unknown state is refused with `unknown_state`.
    *w.fakes[1].secret.lock().unwrap() = Some(Box::new(|_, p| {
        let files: Vec<Value> = p["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| json!({ "path": f["path"], "state": "levitating" }))
            .collect();
        Outcome::Ok(json!({ "files": files }))
    }));
    let report = w.run().await;
    let st = &report.files[0].stations[1];
    assert_eq!(st.state, SecretStationState::Refused);
    assert_eq!(st.code.as_deref(), Some("unknown_state"));
    // A write answer that does not parse is `bad_answer`.
    let fs1 = SharedFs::default();
    install(&w.fakes[1], &fs1, &w.dest);
    let inner = w.fakes[1].secret.lock().unwrap().take().unwrap();
    *w.fakes[1].secret.lock().unwrap() = Some(Box::new(move |m, p| {
        if m == "project.secret_write" {
            Outcome::Ok(json!({ "surprise": true }))
        } else {
            inner(m, p)
        }
    }));
    let report = w.run().await;
    let st = &report.files[0].stations[1];
    assert_eq!(st.state, SecretStationState::Failed);
    assert_eq!(st.code.as_deref(), Some("bad_answer"));
    assert_eq!(
        st.message.as_deref(),
        Some("The station answer does not parse.")
    );
}

#[tokio::test]
async fn a_run_needs_a_remote_and_a_known_project() {
    let s = setup().await;
    // A project with a forge gets its remote from the forge (Fleet Sync §1.3).
    let mut bare = common::fleet_git_fake::input("Bare", "/work/bare");
    bare.forge = None;
    let bare =
        s.f.store
            .create_project(s.f.station.0.clone(), bare)
            .await
            .unwrap();
    let r = sync_project(&s.f.store, &s.f.ctx, &bare.project_id, "owner").await;
    assert!(matches!(r, Err(SyncError::NeedsRemote)));
    let r = sync_project(&s.f.store, &s.f.ctx, "nope", "owner").await;
    assert!(matches!(r, Err(SyncError::UnknownProject)));
}

#[tokio::test]
async fn a_file_that_leaves_the_list_is_not_run_and_a_new_salt_is_new() {
    let w = world(2, &[ENV]).await;
    w.put(0, ENV, "V=1\n", 100);
    let before = w.row(ENV).await.salt;
    // The list changes while no run holds the lock.
    w.s.f
        .store
        .secret_files_set(w.project(), vec![])
        .await
        .unwrap();
    let report = w.run().await;
    assert!(report.files.is_empty());
    assert!(w.fakes[1].requests("project.secret_scan").is_empty());
    w.s.f
        .store
        .secret_files_set(w.project(), vec![ENV.into()])
        .await
        .unwrap();
    assert_ne!(w.row(ENV).await.salt, before);
}

#[tokio::test]
async fn the_run_lock_serializes_runs_and_list_changes() {
    let w = world(2, &[ENV]).await;
    let guard = lock_project(&w.project()).await;
    let (store, ctx, project) = (w.s.f.store.clone(), Arc::clone(&w.s.f.ctx), w.project());
    let run = tokio::spawn(async move { sync_project(&store, &ctx, &project, "owner").await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!run.is_finished(), "the run waits for the lock");
    drop(guard);
    let done = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .unwrap()
        .unwrap();
    assert!(done.is_ok());
    // Another project is not blocked.
    let _other = lock_project("another").await;
    let _mine = tokio::time::timeout(Duration::from_secs(2), lock_project(&w.project()))
        .await
        .unwrap();
}

#[tokio::test]
async fn the_sync_route_runs_the_project_and_audits_it() {
    let w = world(2, &[ENV]).await;
    w.put(0, ENV, "V=1\n", 100);
    let path = format!("/api/projects/{}/secret-files/sync", w.project());
    let r =
        w.s.c
            .post(&path, &w.s.h(), json!({ "command_id": "s1" }))
            .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["files"][0]["outcome"], "copied");
    assert_eq!(r.body["files"][0]["path"], ENV);
    assert_eq!(w.content(1, ENV).unwrap(), b"V=1\n");
    let rows =
        w.s.c
            .audited("project.secret_sync_run", AuditResult::Ok)
            .await;
    assert_eq!(rows, [Some("1 files".to_owned())]);
    // The run audit row names the actor.
    let log = w.s.f.store.audit_log(100).await.unwrap();
    assert!(
        log.iter()
            .any(|e| e.action == "project.secret_sync" && e.actor == "owner")
    );
    // The view shows the report of the last run and the paths.
    let view =
        w.s.c
            .get(
                &format!("/api/projects/{}/secret-files", w.project()),
                &w.s.h(),
            )
            .await;
    assert_eq!(view.status, 200, "{}", view.body);
    assert_eq!(view.body["paths"], json!([ENV]));
    assert_eq!(view.body["files"][0]["outcome"], "copied");
}
