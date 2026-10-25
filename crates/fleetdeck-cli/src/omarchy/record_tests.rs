//! Tests of the install record, version 1 and 2.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use serde_json::{Value, json};

use super::*;

fn home() -> (tempfile::TempDir, Home) {
    let dir = tempfile::Builder::new().prefix("rec").tempdir().unwrap();
    let home = Home::open(dir.path()).unwrap();
    (dir, home)
}

fn put(dir: &tempfile::TempDir, value: &Value) {
    let path = dir.path().join(RECORD_PATH);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, value.to_string()).unwrap();
}

/// A record of the previous build: version 1, none of the new fields.
fn version_1() -> Value {
    json!({
        "v": 1,
        "fleetdeck_version": "0.0.1",
        "files": [],
        "plugin_enabled": true,
        "fleet_summary_set_by_install": false,
        "menu_file_written": false,
        "screensaver": {
            "enabled": true, "had_original": true,
            "backup_sha256": "ab", "last_written_sha256": "cd"
        },
        "hyprland_line": {"added": false, "before_sha256": null, "after_sha256": null}
    })
}

#[test]
fn record_new_is_version_2_without_a_conversion_time() {
    let record = Record::new();
    assert_eq!(record.v, 2);
    assert_eq!(record.converted_at, None);
    assert!(!record.screensaver.first_write_refused);
    assert_eq!(record.screensaver.restored, None);
}

#[test]
fn record_load_accepts_version_1_and_2_and_refuses_others() {
    let (dir, home) = home();
    assert!(load(&home).unwrap().is_none());
    put(&dir, &version_1());
    let old = load(&home).unwrap().unwrap();
    assert_eq!(old.v, 1);
    // The new fields take their defaults.
    assert_eq!(old.converted_at, None);
    assert!(!old.screensaver.first_write_refused);
    assert_eq!(old.screensaver.restored, None);
    assert!(old.screensaver.enabled);

    save(&home, &Record::new()).unwrap();
    assert_eq!(load(&home).unwrap().unwrap().v, 2);

    let mut three = version_1();
    three["v"] = json!(3);
    put(&dir, &three);
    assert!(matches!(load(&home), Err(LoadError::Invalid(_))));
    let mut zero = version_1();
    zero["v"] = json!(0);
    put(&dir, &zero);
    assert!(matches!(load(&home), Err(LoadError::Invalid(_))));
}

#[test]
fn record_load_current_refuses_version_1_with_the_text() {
    let (dir, home) = home();
    assert!(load_current(&home).unwrap().is_none());
    put(&dir, &version_1());
    let err = load_current(&home).unwrap_err();
    assert!(matches!(err, LoadError::Outdated));
    assert_eq!(
        err.to_string(),
        "The Omarchy integration is outdated: run fleetdeck omarchy install."
    );
    save(&home, &Record::new()).unwrap();
    assert_eq!(load_current(&home).unwrap().unwrap().v, 2);
}

#[test]
fn record_new_fields_survive_a_save_and_a_load() {
    let (_dir, home) = home();
    let mut record = Record::new();
    record.converted_at = Some(1_700_000_000_123);
    record.screensaver.first_write_refused = true;
    record.screensaver.restored = Some(Restored::Backup);
    save(&home, &record).unwrap();
    assert_eq!(load(&home).unwrap().unwrap(), record);
    record.screensaver.restored = Some(Restored::Removed);
    save(&home, &record).unwrap();
    assert_eq!(load(&home).unwrap().unwrap(), record);
}

#[test]
fn record_restored_is_written_in_snake_case() {
    let mut record = Record::new();
    record.screensaver.restored = Some(Restored::Backup);
    let value = serde_json::to_value(&record).unwrap();
    assert_eq!(value["screensaver"]["restored"], "backup");
    record.screensaver.restored = Some(Restored::Removed);
    let value = serde_json::to_value(&record).unwrap();
    assert_eq!(value["screensaver"]["restored"], "removed");
    assert!(value["converted_at"].is_null());
}
