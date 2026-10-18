//! Tests of the publish step of the install: the recheck after the prompt
//! (R13 P4 PRE-PR r1, finding 2).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use super::*;

const HOOK: &str = "hooks/post-update.d/fleetdeck";

fn home() -> (tempfile::TempDir, Home) {
    let dir = tempfile::Builder::new().prefix("inst").tempdir().unwrap();
    let home = Home::open(dir.path()).unwrap();
    (dir, home)
}

fn omarchy(dir: &tempfile::TempDir, path: &str) -> PathBuf {
    dir.path().join("omarchy").join(path)
}

/// A first install: every file written, the record saved.
fn installed(home: &Home) -> Record {
    let mut record = Record::new();
    let items = classify(home, &record).unwrap();
    assert!(publish(home, &mut record, &items, || {}).unwrap());
    record
}

#[test]
fn install_publish_writes_every_new_file_and_records_it() {
    let (dir, home) = home();
    let record = installed(&home);
    assert_eq!(record.files.len(), FILES.len());
    for asset in FILES {
        let path = rel(asset.base, asset.path);
        let data = home.read(&path).unwrap().unwrap();
        assert_eq!(data.bytes, asset.text.as_bytes(), "{}", asset.path);
        assert_eq!(data.mode, asset.mode, "{}", asset.path);
    }
    assert!(omarchy(&dir, HOOK).is_file());
}

#[test]
fn install_publish_keeps_a_recorded_file_that_changed_after_the_prompt() {
    let (dir, home) = home();
    let mut record = installed(&home);
    let items = classify(&home, &record).unwrap();
    assert!(items.iter().all(|i| i.plan == Plan::Replace));
    let hook = omarchy(&dir, HOOK);
    let ok = publish(&home, &mut record, &items, || {
        std::fs::write(&hook, "#!/bin/bash\n# mine\n").unwrap();
    })
    .unwrap();
    assert!(ok);
    assert_eq!(
        std::fs::read_to_string(&hook).unwrap(),
        "#!/bin/bash\n# mine\n"
    );
    // The record still holds the old hash of the kept file.
    let asset = FILES.iter().find(|a| a.path == HOOK).unwrap();
    let entry = record.entry(asset.base, asset.path).unwrap();
    assert_eq!(entry.sha256, sha256_hex(asset.text.as_bytes()));
}

#[test]
fn install_publish_keeps_a_recorded_file_that_vanished_after_the_prompt() {
    let (dir, home) = home();
    let mut record = installed(&home);
    let items = classify(&home, &record).unwrap();
    let hook = omarchy(&dir, HOOK);
    let ok = publish(&home, &mut record, &items, || {
        std::fs::remove_file(&hook).unwrap();
    })
    .unwrap();
    assert!(ok);
    assert!(!hook.exists());
}

#[test]
fn install_publish_stops_at_a_file_that_appeared_after_the_prompt() {
    let (dir, home) = home();
    let mut record = Record::new();
    let items = classify(&home, &record).unwrap();
    assert!(items.iter().all(|i| i.plan == Plan::Create));
    // The third file of the list.
    let third = &FILES[2];
    assert_eq!(third.base, Base::Omarchy);
    let theirs = omarchy(&dir, third.path);
    let ok = publish(&home, &mut record, &items, || {
        std::fs::create_dir_all(theirs.parent().unwrap()).unwrap();
        std::fs::write(&theirs, "theirs\n").unwrap();
    })
    .unwrap();
    assert!(!ok);
    assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "theirs\n");
    // The files before it are written and recorded; nothing after it is.
    assert_eq!(record.files.len(), 2);
    assert_eq!(record::load(&home).unwrap().unwrap().files.len(), 2);
    for asset in &FILES[3..] {
        let path = rel(asset.base, asset.path);
        assert!(home.read(&path).unwrap().is_none(), "{}", asset.path);
    }
}
