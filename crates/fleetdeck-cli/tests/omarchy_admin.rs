//! The CLI side of the Omarchy management from the board
//! (`omarchy-admin.md` sections 3.5 to 3.7): the record version 2, the
//! Omarchy lock, the conversion by install, the refusals for a version 1
//! record, and the new keys of `omarchy status --json`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
#![allow(dead_code, reason = "the rig of the installer tests is shared")]

#[path = "omarchy_install/rig.rs"]
mod rig;

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use rig::{Rig, err, out};

const OUTDATED: &str = "The Omarchy integration is outdated: run fleetdeck omarchy install.";
const BUSY: &str = "Another Omarchy change is running.";

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn record_path(rig: &Rig) -> PathBuf {
    rig.fd_dir().join("omarchy-install.json")
}

/// Changes the record on disk (the rig writes what the installer wrote).
fn patch(rig: &Rig, change: impl FnOnce(&mut Value)) {
    let mut record = rig.record();
    change(&mut record);
    std::fs::write(record_path(rig), record.to_string()).unwrap();
}

fn status(rig: &Rig) -> Value {
    let o = rig.run(&["omarchy", "status", "--json"]);
    serde_json::from_str(out(&o).trim()).unwrap()
}

fn code(rig: &Rig, args: &[&str]) -> (i32, String, String) {
    let o = rig.run(args);
    (o.status.code().unwrap(), out(&o), err(&o))
}

/// An install that is older than two minutes, as the board sees it.
fn settled(rig: &Rig) {
    assert_eq!(rig.install().status.code(), Some(0));
    patch(rig, |r| r["converted_at"] = json!(now_ms() - 600_000));
}

fn hold_lock(rig: &Rig) -> std::fs::File {
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(rig.fd_dir().join("omarchy.lock"))
        .unwrap();
    lock.lock().unwrap();
    lock
}

// ---- the record and the lock file ----

#[test]
fn omarchy_admin_install_makes_a_version_2_record_and_a_0600_lock_file() {
    let rig = Rig::new();
    assert_eq!(rig.install().status.code(), Some(0));
    let record = rig.record();
    assert_eq!(record["v"], 2);
    assert!(record["converted_at"].is_null());
    assert_eq!(rig::mode(&rig.fd_dir().join("omarchy.lock")), 0o600);
    assert_eq!(rig::mode(&rig.fd_dir()), 0o755);
}

#[test]
fn omarchy_admin_install_converts_a_version_1_record() {
    let rig = Rig::new();
    assert_eq!(rig.install().status.code(), Some(0));
    patch(&rig, |r| {
        r["v"] = json!(1);
        let object = r.as_object_mut().unwrap();
        object.remove("converted_at");
        object["screensaver"]
            .as_object_mut()
            .unwrap()
            .remove("restored");
        object["screensaver"]
            .as_object_mut()
            .unwrap()
            .remove("first_write_refused");
    });
    let before = now_ms();
    assert_eq!(rig.install().status.code(), Some(0));
    let record = rig.record();
    assert_eq!(record["v"], 2);
    let at = record["converted_at"].as_i64().unwrap();
    assert!(at >= before && at <= now_ms(), "{at}");
    // A second install keeps the time of the conversion.
    assert_eq!(rig.install().status.code(), Some(0));
    assert_eq!(rig.record()["converted_at"].as_i64(), Some(at));
}

#[test]
fn omarchy_admin_install_adopts_a_file_with_a_stale_hash_and_keeps_the_flag() {
    let rig = Rig::new();
    assert_eq!(rig.install().status.code(), Some(0));
    let want = rig.record()["files"][0]["sha256"].clone();
    patch(&rig, |r| {
        r["files"][0]["sha256"] = json!("0".repeat(64));
        r["files"][0]["existed_before"] = json!(true);
    });
    assert_eq!(status(&rig)["files"][0]["state"], "changed");
    assert_eq!(rig.install().status.code(), Some(0));
    let record = rig.record();
    assert_eq!(record["files"][0]["sha256"], want);
    assert_eq!(record["files"][0]["existed_before"], true);
    assert_eq!(status(&rig)["files"][0]["state"], "ok");
}

// ---- the refusals for a version 1 record ----

#[test]
fn omarchy_admin_a_version_1_record_is_refused_by_on_off_rebaseline_and_uninstall() {
    let rig = Rig::new();
    assert_eq!(rig.install().status.code(), Some(0));
    patch(&rig, |r| r["v"] = json!(1));
    let before = std::fs::read(record_path(&rig)).unwrap();
    let commands: [&[&str]; 4] = [
        &["omarchy", "screensaver", "on"],
        &["omarchy", "screensaver", "off"],
        &["omarchy", "screensaver", "rebaseline"],
        &["omarchy", "uninstall"],
    ];
    for args in commands {
        let (c, o, e) = code(&rig, args);
        assert_eq!((c, o.as_str()), (1, ""), "{args:?}: {e}");
        assert!(e.contains(OUTDATED), "{args:?}: {e}");
    }
    assert_eq!(std::fs::read(record_path(&rig)).unwrap(), before);
    assert!(
        rig.omarchy_dir().join(rig::MANIFEST).exists(),
        "uninstall removed nothing"
    );
}

#[test]
fn omarchy_admin_a_tick_writes_nothing_for_a_version_1_record() {
    let rig = Rig::new();
    assert_eq!(
        code(&rig, &["omarchy", "install", "--yes", "--screensaver"]).0,
        0
    );
    patch(&rig, |r| r["v"] = json!(1));
    let (c, o, e) = code(&rig, &["omarchy", "screensaver-tick"]);
    assert_eq!((c, o.as_str(), e.as_str()), (0, "", ""));
    assert!(!rig.omarchy_dir().join("branding/screensaver.txt").exists());
}

// ---- the lock ----

#[test]
fn omarchy_admin_install_with_the_lock_held_waits_then_exits_1() {
    let rig = Rig::new();
    assert_eq!(rig.install().status.code(), Some(0));
    let _lock = hold_lock(&rig);
    let started = Instant::now();
    let (c, o, e) = code(&rig, &["omarchy", "install", "--yes"]);
    assert_eq!((c, o.as_str()), (1, ""), "{e}");
    assert!(e.contains(BUSY), "{e}");
    assert!(
        started.elapsed() >= Duration::from_secs(9),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn omarchy_admin_screensaver_on_with_the_lock_held_waits_then_exits_1() {
    let rig = Rig::new();
    assert_eq!(rig.install().status.code(), Some(0));
    let before = std::fs::read(record_path(&rig)).unwrap();
    let _lock = hold_lock(&rig);
    let started = Instant::now();
    let (c, _, e) = code(&rig, &["omarchy", "screensaver", "on"]);
    assert_eq!(c, 1, "{e}");
    assert!(e.contains(BUSY), "{e}");
    assert!(
        started.elapsed() >= Duration::from_secs(9),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(std::fs::read(record_path(&rig)).unwrap(), before);
}

#[test]
fn omarchy_admin_uninstall_with_the_lock_held_exits_1_and_changes_nothing() {
    let rig = Rig::new();
    assert_eq!(rig.install().status.code(), Some(0));
    let before = std::fs::read(record_path(&rig)).unwrap();
    let _lock = hold_lock(&rig);
    let (c, o, e) = code(&rig, &["omarchy", "uninstall"]);
    assert_eq!((c, o.as_str()), (1, ""), "{e}");
    assert!(e.contains(BUSY), "{e}");
    assert_eq!(std::fs::read(record_path(&rig)).unwrap(), before);
    assert!(rig.omarchy_dir().join(rig::MANIFEST).exists());
}

#[test]
fn omarchy_admin_uninstall_without_a_lock_file_has_nothing_to_remove() {
    let rig = Rig::new();
    let (c, o, _) = code(&rig, &["omarchy", "uninstall"]);
    assert_eq!(c, 0);
    assert!(
        o.contains("Nothing to remove: no install record was found."),
        "{o}"
    );
}

// ---- status ----

#[test]
fn omarchy_admin_status_json_of_a_settled_install() {
    let rig = Rig::new();
    settled(&rig);
    let v = status(&rig);
    assert_eq!(v["record_version"], 2);
    assert_eq!(v["outdated"], false);
    assert_eq!(
        v["screensaver"],
        json!({"enabled": false, "file_restored": null, "blocked": null})
    );
    // The old keys are still there.
    for key in [
        "installed",
        "files",
        "plugin_enabled",
        "summary",
        "hyprland_line",
    ] {
        assert!(v.get(key).is_some(), "{key}");
    }
    let o = rig.run(&["omarchy", "status"]);
    let text = out(&o);
    assert!(text.contains("record: version 2\n"), "{text}");
    assert!(text.contains("outdated: no\n"), "{text}");
    assert!(text.contains("screensaver: off\n"), "{text}");
    assert!(!text.contains("screensaver file:"), "{text}");
    assert!(!text.contains("screensaver blocked:"), "{text}");
}

#[test]
fn omarchy_admin_status_is_outdated_for_a_version_1_record() {
    let rig = Rig::new();
    settled(&rig);
    patch(&rig, |r| r["v"] = json!(1));
    let v = status(&rig);
    assert_eq!(v["record_version"], 1);
    assert_eq!(v["outdated"], true);
    assert_eq!(v["screensaver"]["enabled"], false, "status reads version 1");
    let o = rig.run(&["omarchy", "status"]);
    assert!(out(&o).contains("record: version 1\n"), "{}", out(&o));
    assert!(out(&o).contains("outdated: yes\n"), "{}", out(&o));
}

#[test]
fn omarchy_admin_status_is_outdated_for_an_old_plugin_file() {
    let rig = Rig::new();
    settled(&rig);
    // The record and the file agree (an old build wrote both), but the bytes
    // differ from this build's plugin.
    let widget = rig.omarchy_dir().join("plugins/fleetdeck.fleet/Widget.qml");
    std::fs::write(&widget, "// the widget of an old build\n").unwrap();
    let hash = sha256_of(b"// the widget of an old build\n");
    patch(&rig, |r| {
        for f in r["files"].as_array_mut().unwrap() {
            if f["path"] == "plugins/fleetdeck.fleet/Widget.qml" {
                f["sha256"] = json!(hash);
            }
        }
    });
    let v = status(&rig);
    assert_eq!(v["outdated"], true);
    // A changed hook is not a plugin file.
    let rig = Rig::new();
    settled(&rig);
    std::fs::write(rig.omarchy_dir().join(rig::HOOKS[0]), "changed\n").unwrap();
    assert_eq!(status(&rig)["outdated"], false);
}

#[test]
fn omarchy_admin_status_is_outdated_for_two_minutes_after_the_conversion() {
    let rig = Rig::new();
    settled(&rig);
    patch(&rig, |r| r["converted_at"] = json!(now_ms() - 60_000));
    assert_eq!(status(&rig)["outdated"], true);
    patch(&rig, |r| r["converted_at"] = json!(now_ms() - 130_000));
    assert_eq!(status(&rig)["outdated"], false);
}

#[test]
fn omarchy_admin_status_after_a_conversion_is_outdated_at_once() {
    let rig = Rig::new();
    assert_eq!(rig.install().status.code(), Some(0));
    patch(&rig, |r| r["v"] = json!(1));
    assert_eq!(rig.install().status.code(), Some(0));
    let v = status(&rig);
    assert_eq!(v["record_version"], 2);
    assert_eq!(v["outdated"], true, "less than 120 s since the conversion");
}

#[test]
fn omarchy_admin_status_screensaver_while_on_has_no_file_restored() {
    let rig = Rig::new();
    settled(&rig);
    let (c, _, e) = code(&rig, &["omarchy", "screensaver", "on"]);
    assert_eq!(c, 0, "{e}");
    assert_eq!(
        status(&rig)["screensaver"],
        json!({"enabled": true, "file_restored": null, "blocked": null})
    );
    let o = rig.run(&["omarchy", "status"]);
    assert!(out(&o).contains("screensaver: on\n"), "{}", out(&o));
}

#[test]
fn omarchy_admin_status_file_restored_follows_the_files() {
    let rig = Rig::new();
    settled(&rig);
    let branding = rig.omarchy_dir().join("branding");
    let file = branding.join("screensaver.txt");
    let backup = branding.join("screensaver.txt.fleetdeck-backup");
    std::fs::create_dir_all(&branding).unwrap();
    std::fs::write(&file, "original\n").unwrap();
    let on = || assert_eq!(code(&rig, &["omarchy", "screensaver", "on"]).0, 0);
    let off = || assert_eq!(code(&rig, &["omarchy", "screensaver", "off"]).0, 0);
    on();
    assert_eq!(code(&rig, &["omarchy", "screensaver-tick"]).0, 0);
    off();
    // restored = backup: true while the file equals the backup.
    assert_eq!(status(&rig)["screensaver"]["file_restored"], true);
    let text = out(&rig.run(&["omarchy", "status"]));
    assert!(text.contains("screensaver file: restored\n"), "{text}");
    // An old job writes the Fleet Deck text again: false.
    std::fs::write(&file, "Fleet Deck text of an old job\n").unwrap();
    assert_eq!(status(&rig)["screensaver"]["file_restored"], false);
    let text = out(&rig.run(&["omarchy", "status"]));
    assert!(text.contains("screensaver file: not restored\n"), "{text}");
    // The backup is gone and the file is as the backup was: false as well.
    std::fs::write(&file, "original\n").unwrap();
    std::fs::remove_file(&backup).unwrap();
    assert_eq!(status(&rig)["screensaver"]["file_restored"], false);
    // restored = removed: true when the file is absent.
    std::fs::remove_file(&file).unwrap();
    patch(&rig, |r| r["screensaver"]["restored"] = json!("removed"));
    assert_eq!(status(&rig)["screensaver"]["file_restored"], true);
    std::fs::write(&file, "something\n").unwrap();
    assert_eq!(status(&rig)["screensaver"]["file_restored"], false);
    // No restore recorded: null.
    patch(&rig, |r| r["screensaver"]["restored"] = Value::Null);
    assert_eq!(status(&rig)["screensaver"]["file_restored"], Value::Null);
}

#[test]
fn omarchy_admin_status_blocked_follows_first_write_refused() {
    let rig = Rig::new();
    settled(&rig);
    patch(&rig, |r| {
        r["screensaver"]["first_write_refused"] = json!(true)
    });
    assert_eq!(
        status(&rig)["screensaver"]["blocked"],
        "kept_original_differs"
    );
    let text = out(&rig.run(&["omarchy", "status"]));
    assert!(
        text.contains("screensaver blocked: kept_original_differs\n"),
        "{text}"
    );
    patch(&rig, |r| {
        r["screensaver"]["first_write_refused"] = json!(false)
    });
    assert_eq!(status(&rig)["screensaver"]["blocked"], Value::Null);
}

/// Lower-case hex SHA-256.
fn sha256_of(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
