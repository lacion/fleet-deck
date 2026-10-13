//! Failure points of a save, of a secret write, of the store directory and
//! of orphan collection (design 1.3, T1, U1, U2).

use std::sync::Arc;

use fleetdeck_proto::settings::{Durability, SecretSet};
use serde_json::json;

use super::testfs::{FaultFs, Rule};
use super::{Fixture, inline_station, ok_prepare};
use crate::config::StationConfig;
use crate::settings::SaveError;

const KEY: &str = "provider_instance.work.env.ANTHROPIC_API_KEY";

fn set(value: &str) -> SecretSet {
    SecretSet {
        key: KEY.to_owned(),
        value: value.to_owned(),
    }
}

fn resolves(fx: &Fixture, text: &str) -> bool {
    StationConfig::parse_with_store(text, &fx.env, Some(&fx.store)).is_ok()
}

#[test]
fn each_failure_point_of_a_save_leaves_the_old_file() {
    for op in ["create", "write", "fsync", "rename"] {
        let fx = Fixture::new(Some("max_sessions = 4\n"));
        let fs = Arc::new(FaultFs::default());
        let w = fx.writer_with(fs.clone());
        let before = fx.text();
        fs.set_rule(Some(Rule::fail(op, "station.toml")));
        let err = w
            .save(&fx.change(json!({ "max_sessions": 9 })), false, ok_prepare)
            .unwrap_err();
        assert_eq!(err, SaveError::SettingsWriteFailed, "{op}");
        assert_eq!(err.code(), "settings_write_failed");
        assert_eq!(fx.text(), before, "{op}");
        assert!(fx.leftovers().is_empty(), "{op}: {:?}", fx.leftovers());
        // The holder still works afterwards.
        fs.set_rule(None);
        let (saved, _, ()) = w
            .save(&fx.change(json!({ "max_sessions": 9 })), false, ok_prepare)
            .unwrap();
        assert_eq!(saved.durability, Durability::Confirmed);
    }
}

#[test]
fn each_failure_point_of_a_secret_write_leaves_no_reference() {
    let points = [
        ("create", "secrets"),
        ("write", "secrets"),
        ("fsync", "secrets"),
        ("rename", "secrets"),
        ("fsync_dir", "secrets"),
        ("create", "station.toml"),
        ("write", "station.toml"),
        ("fsync", "station.toml"),
        ("rename", "station.toml"),
    ];
    for (op, path) in points {
        let fx = Fixture::new(Some(&inline_station()));
        let fs = Arc::new(FaultFs::default());
        let w = fx.writer_with(fs.clone());
        let before = fx.text();
        fs.set_rule(Some(Rule::fail(op, path)));
        let err = w
            .set_secret(&set("new-value"), &fx.revision(), true, ok_prepare)
            .unwrap_err();
        assert_eq!(err, SaveError::SettingsWriteFailed, "{op} {path}");
        assert_eq!(fx.text(), before, "{op} {path}");
        assert!(fx.secrets().is_empty(), "{op} {path}: {:?}", fx.secrets());
        assert!(fx.leftovers().is_empty(), "{op} {path}");
        assert!(resolves(&fx, &fx.text()));
        fs.set_rule(None);
        w.set_secret(&set("new-value"), &fx.revision(), true, ok_prepare)
            .unwrap();
    }
}

/// Saves a first secret, then a second one with a failing sync of the
/// config directory after the rename.
fn uncertain_save(fx: &Fixture, fs: &Arc<FaultFs>) -> (String, Vec<String>) {
    let w = fx.writer_with(fs.clone());
    w.set_secret(&set("value-a"), &fx.revision(), true, ok_prepare)
        .unwrap();
    let pre_rename = fx.text();
    let old_names = fx.secrets();
    fs.set_rule(Some(Rule::fail("fsync_dir", "/cfg")));
    let (saved, new, ()) = w
        .set_secret(&set("value-b"), &fx.revision(), true, ok_prepare)
        .unwrap();
    assert_eq!(saved.durability, Durability::Uncertain);
    assert_eq!(
        new.provider_instances[0].env["ANTHROPIC_API_KEY"].expose(),
        "value-b"
    );
    assert_eq!(saved.restart_needed, vec!["provider_instance"]);
    assert_eq!(saved.revision, fx.revision());
    assert_ne!(fx.text(), pre_rename);
    (pre_rename, old_names)
}

#[test]
fn a_failed_directory_sync_after_the_rename_is_uncertain_and_deletes_nothing() {
    let fx = Fixture::new(Some(&inline_station()));
    let fs = Arc::new(FaultFs::default());
    let (pre_rename, old_names) = uncertain_save(&fx, &fs);
    let now = fx.secrets();
    assert_eq!(now.len(), 2, "the old secret stays: {now:?}");
    assert!(old_names.iter().all(|n| now.contains(n)));
    // A power loss restores the pre-rename file; its secret is still there.
    assert!(resolves(&fx, &pre_rename));
    assert!(resolves(&fx, &fx.text()));
}

#[test]
fn a_restart_after_an_uncertain_save_deletes_nothing_while_the_sync_fails() {
    let fx = Fixture::new(Some(&inline_station()));
    let fs = Arc::new(FaultFs::default());
    let (pre_rename, _) = uncertain_save(&fx, &fs);
    // The next start: the sync fails again, so no orphan is deleted.
    let fs2 = Arc::new(FaultFs::new(Some(Rule::fail("fsync_dir", "/cfg"))));
    let restarted = fx.writer_with(fs2);
    assert!(restarted.collect_orphans().is_empty());
    assert_eq!(fx.secrets().len(), 2);
    assert!(resolves(&fx, &pre_rename));
    // A sync that fails on the file does the same.
    let fs3 = Arc::new(FaultFs::new(Some(Rule::fail("fsync", "station.toml"))));
    assert!(fx.writer_with(fs3).collect_orphans().is_empty());
    assert_eq!(fx.secrets().len(), 2);
    // A start that confirms the disk deletes the old secret.
    let deleted = fx.writer().collect_orphans();
    assert_eq!(deleted.len(), 1);
    assert_eq!(fx.secrets().len(), 1);
    assert!(resolves(&fx, &fx.text()));
}

#[test]
fn a_later_confirmed_save_deletes_the_secret_of_an_uncertain_one() {
    let fx = Fixture::new(Some(&inline_station()));
    let fs = Arc::new(FaultFs::default());
    let w = fx.writer_with(fs.clone());
    w.set_secret(&set("value-a"), &fx.revision(), true, ok_prepare)
        .unwrap();
    fs.set_rule(Some(Rule::fail("fsync_dir", "/cfg")));
    w.set_secret(&set("value-b"), &fx.revision(), true, ok_prepare)
        .unwrap();
    assert_eq!(fx.secrets().len(), 2);
    fs.set_rule(None);
    let (saved, _, ()) = w
        .save(&fx.change(json!({ "max_sessions": 3 })), false, ok_prepare)
        .unwrap();
    assert_eq!(saved.durability, Durability::Confirmed);
    assert_eq!(fx.secrets().len(), 1, "{:?}", fx.secrets());
    assert!(resolves(&fx, &fx.text()));
}

/// Whether a sync of the config directory succeeded after the rename.
fn config_dir_synced(fs: &FaultFs) -> bool {
    let log = fs.log();
    let renamed = log.iter().position(|(op, p, ok)| {
        op == "rename" && *ok && p.to_string_lossy().ends_with("station.toml")
    });
    renamed.is_some_and(|i| {
        log[i..]
            .iter()
            .any(|(op, p, ok)| op == "fsync_dir" && *ok && p.to_string_lossy().ends_with("/cfg"))
    })
}

/// Stops the process after the call `k` of a secret write, then checks the
/// files. `replace`: the file already holds a reference. Returns the number
/// of calls of a clean run when `k` is `None`.
fn crash_run(replace: bool, k: Option<usize>) -> usize {
    let fx = Fixture::new(Some(&inline_station()));
    let fs = Arc::new(FaultFs::default());
    let start_rule = if replace { None } else { k.map(Rule::crash_at) };
    fs.set_rule(start_rule);
    let w = fx.writer_with(fs.clone());
    if replace {
        w.set_secret(&set("value-a"), &fx.revision(), true, ok_prepare)
            .unwrap();
        fs.set_rule(k.map(Rule::crash_at));
    }
    let start = fs.log().len();
    let pre = fx.text();
    let result = w.set_secret(&set("value-b"), &fx.revision(), true, ok_prepare);
    let calls = fs.log().len() - start;
    let Some(k) = k else {
        result.unwrap();
        return if replace { calls } else { fs.log().len() };
    };
    let current = fx.text();
    // The file that can be on disk after a power loss resolves: the new one
    // always, the old one while the sync of the config directory is missing.
    assert!(
        resolves(&fx, &current),
        "crash {k} replace {replace}: new file"
    );
    if !config_dir_synced(&fs) {
        assert!(resolves(&fx, &pre), "crash {k} replace {replace}: old file");
    }
    // A restart collects the orphans and must keep what is referenced.
    let restarted = fx.writer();
    restarted.collect_orphans();
    assert!(
        fx.load().is_ok(),
        "crash {k} replace {replace}: after the restart"
    );
    calls
}

#[test]
fn a_crash_after_any_call_keeps_a_complete_secret() {
    for replace in [false, true] {
        let total = crash_run(replace, None);
        assert!(total > 10, "{total} calls");
        for k in 0..=total {
            crash_run(replace, Some(k));
        }
    }
}

#[test]
fn the_store_directory_is_made_durable_before_a_reference() {
    let fx = Fixture::new(Some(&inline_station()));
    let fs = Arc::new(FaultFs::new(Some(Rule::fail("mkdir", "secrets"))));
    let w = fx.writer_with(fs.clone());
    let before = fx.text();
    let err = w.set_secret(&set("v"), &fx.revision(), true, ok_prepare);
    // The retry inside the save also fails while the rule stands (one shot:
    // it fails the first mkdir only), so this save may succeed; the rule
    // here fails the first call only.
    if let Err(e) = &err {
        assert_eq!(*e, SaveError::SecretStoreUnavailable);
        assert_eq!(fx.text(), before);
    }
    let fs = Arc::new(FaultFs::default());
    let fx = Fixture::new(Some(&inline_station()));
    // The sync of the first created directory fails at start.
    fs.set_rule(Some(Rule::fail("fsync_dir", "data")));
    let w = fx.writer_with(fs.clone());
    let before = fx.text();
    // A second failure keeps the store unavailable.
    fs.set_rule(Some(Rule {
        op: Some("fsync_dir"),
        path_has: "data",
        nth: 0,
        crash: true,
    }));
    let err = w
        .set_secret(&set("v"), &fx.revision(), true, ok_prepare)
        .unwrap_err();
    assert_eq!(err, SaveError::SecretStoreUnavailable);
    assert_eq!(err.code(), "secret_store_unavailable");
    assert_eq!(
        fx.text(),
        before,
        "no TOML points to a store that is not durable"
    );
    assert!(
        fx.secrets()
            .iter()
            .all(|n| n.starts_with('.') || n.is_empty())
            || fx.secrets().is_empty()
    );
    // The disk recovers: the retry syncs every directory that was created.
    fs.set_rule(None);
    let mark = fs.log().len();
    w.set_secret(&set("v"), &fx.revision(), true, ok_prepare)
        .unwrap();
    let synced: Vec<String> = fs.log()[mark..]
        .iter()
        .filter(|(op, _, ok)| op == "fsync_dir" && *ok)
        .map(|(_, p, _)| p.to_string_lossy().into_owned())
        .collect();
    let data = fx.dir.path().join("data").display().to_string();
    assert!(synced.contains(&data), "{synced:?}");
    assert!(fx.load().is_ok());
}

#[test]
fn a_first_start_creates_and_syncs_the_chain_of_directories() {
    let fx = Fixture::new(Some(&inline_station()));
    let fs = Arc::new(FaultFs::default());
    let _w = fx.writer_with(fs.clone());
    let log = fs.log();
    let mkdirs = log
        .iter()
        .filter(|(op, _, ok)| op == "mkdir" && *ok)
        .count();
    assert_eq!(mkdirs, 3, "data, station, secrets: {log:?}");
    let synced: Vec<String> = log
        .iter()
        .filter(|(op, _, ok)| op == "fsync_dir" && *ok)
        .map(|(_, p, _)| p.display().to_string())
        .collect();
    for dir in [
        fx.store.clone(),
        fx.store.parent().unwrap().to_path_buf(),
        fx.dir.path().join("data"),
        fx.dir.path().to_path_buf(),
    ] {
        assert!(
            synced.contains(&dir.display().to_string()),
            "{dir:?} in {synced:?}"
        );
    }
}

#[test]
fn a_store_that_is_not_a_private_directory_is_refused() {
    let fx = Fixture::new(Some(&inline_station()));
    std::fs::create_dir_all(fx.store.parent().unwrap()).unwrap();
    std::fs::write(&fx.store, "not a directory").unwrap();
    let w = fx.writer();
    let before = fx.text();
    let err = w
        .set_secret(&set("v"), &fx.revision(), true, ok_prepare)
        .unwrap_err();
    assert_eq!(err, SaveError::SecretStoreUnavailable);
    assert_eq!(fx.text(), before);
    // A store that others can read is refused too.
    std::fs::remove_file(&fx.store).unwrap();
    std::fs::create_dir(&fx.store).unwrap();
    std::fs::set_permissions(
        &fx.store,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let err = fx
        .writer()
        .set_secret(&set("v"), &fx.revision(), true, ok_prepare)
        .unwrap_err();
    assert_eq!(err, SaveError::SecretStoreUnavailable);
}

#[test]
fn no_store_means_no_secret_write() {
    let fx = Fixture::new(Some(&inline_station()));
    let w = crate::settings::SettingsWriter::<crate::settings::StationHolder>::new(
        &fx.config, None, &fx.env,
    );
    let err = w
        .set_secret(&set("v"), &fx.revision(), true, ok_prepare)
        .unwrap_err();
    assert_eq!(err, SaveError::SecretStoreUnavailable);
}

#[test]
fn orphan_collection_without_a_config_file_deletes_nothing() {
    let fx = Fixture::new(None);
    let w = fx.writer();
    std::fs::write(fx.store.join("some-secret"), "x").unwrap();
    assert!(w.collect_orphans().is_empty());
    assert_eq!(fx.secrets(), vec!["some-secret".to_owned()]);
}

#[test]
fn a_save_that_keeps_references_needs_a_durable_store() {
    let fx = Fixture::new(Some(&inline_station()));
    fx.writer()
        .set_secret(&set("v"), &fx.revision(), true, ok_prepare)
        .unwrap();
    let fs = Arc::new(FaultFs::new(Some(Rule {
        op: Some("fsync_dir"),
        path_has: "secrets",
        nth: 0,
        crash: true,
    })));
    let w = fx.writer_with(fs.clone());
    let before = fx.text();
    let err = w
        .save(&fx.change(json!({ "max_sessions": 3 })), false, ok_prepare)
        .unwrap_err();
    assert_eq!(err, SaveError::SecretStoreUnavailable);
    assert_eq!(fx.text(), before);
    fs.set_rule(None);
    w.save(&fx.change(json!({ "max_sessions": 3 })), false, ok_prepare)
        .unwrap();
}
