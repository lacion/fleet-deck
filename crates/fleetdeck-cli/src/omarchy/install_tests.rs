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

const OLD: &[u8] = b"old text of a previous build\n";

/// An install of an older build: every file holds old text, and the record
/// holds the hash of that old text, so that each file classifies `Replace`.
fn installed_old(home: &Home) -> Record {
    let mut record = installed(home);
    for asset in FILES {
        let path = rel(asset.base, asset.path);
        home.write(&path, OLD, asset.mode).unwrap();
        record.upsert(FileEntry {
            path: asset.path.to_owned(),
            base: asset.base,
            sha256: sha256_hex(OLD),
            existed_before: false,
        });
    }
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
    let mut record = installed_old(&home);
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
    assert_eq!(entry.sha256, sha256_hex(OLD));
}

#[test]
fn install_publish_keeps_a_recorded_file_that_vanished_after_the_prompt() {
    let (dir, home) = home();
    let mut record = installed_old(&home);
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

// ---- adopt (omarchy-admin.md section 3.7) ----

#[test]
fn install_adopts_a_recorded_file_with_the_bytes_of_this_build_and_repairs_its_hash() {
    let (_dir, home) = home();
    let mut record = installed_old(&home);
    // A stale save of an old job: the file has this build's bytes, the record
    // still has the old hash; `existed_before` is true for the manifest.
    let asset = &FILES[0];
    home.write(
        &rel(asset.base, asset.path),
        asset.text.as_bytes(),
        asset.mode,
    )
    .unwrap();
    record.upsert(FileEntry {
        path: asset.path.to_owned(),
        base: asset.base,
        sha256: sha256_hex(OLD),
        existed_before: true,
    });
    let items = classify(&home, &record).unwrap();
    assert_eq!(items[0].plan, Plan::Adopt);
    assert!(items[1..].iter().all(|i| i.plan == Plan::Replace));
    assert!(publish(&home, &mut record, &items, || {}).unwrap());
    let entry = record.entry(asset.base, asset.path).unwrap();
    assert_eq!(entry.sha256, sha256_hex(asset.text.as_bytes()));
    assert!(entry.existed_before, "the flag of the entry is kept");
    let saved = record::load(&home).unwrap().unwrap();
    assert_eq!(saved.entry(asset.base, asset.path), Some(entry));
}

#[test]
fn install_adopts_a_file_that_is_not_recorded_as_existing_before() {
    let (_dir, home) = home();
    let asset = &FILES[0];
    home.write(
        &rel(asset.base, asset.path),
        asset.text.as_bytes(),
        asset.mode,
    )
    .unwrap();
    let mut record = Record::new();
    let items = classify(&home, &record).unwrap();
    assert_eq!(items[0].plan, Plan::Adopt);
    assert!(publish(&home, &mut record, &items, || {}).unwrap());
    let entry = record.entry(asset.base, asset.path).unwrap();
    assert!(entry.existed_before);
    assert_eq!(entry.sha256, sha256_hex(asset.text.as_bytes()));
}

#[test]
fn install_a_recorded_file_that_matches_neither_the_record_nor_the_asset_is_kept() {
    let (_dir, home) = home();
    let mut record = installed_old(&home);
    let asset = &FILES[0];
    home.write(&rel(asset.base, asset.path), b"theirs\n", asset.mode)
        .unwrap();
    let items = classify(&home, &record).unwrap();
    assert_eq!(items[0].plan, Plan::Keep);
    assert!(publish(&home, &mut record, &items, || {}).unwrap());
    assert_eq!(
        record.entry(asset.base, asset.path).unwrap().sha256,
        sha256_hex(OLD)
    );
}
