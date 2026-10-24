//! Tests of the install plan facts (`board-gaps.md` §3.2 and §3.3).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::{PermissionsExt as _, symlink};

use fleetdeck_proto::omarchy_repair::{OmarchyAssetId as Id, PlanBlock, PlanClassification};

use super::*;
use crate::omarchy::assets::{MENU_PATH, asset_for};
use crate::omarchy::record::FileEntry;

const MANIFEST: &str = "plugins/fleetdeck.fleet/manifest.json";

fn home() -> (tempfile::TempDir, Home) {
    let dir = tempfile::Builder::new().prefix("plan").tempdir().unwrap();
    let home = Home::open(dir.path()).unwrap();
    (dir, home)
}

/// Every asset written as this build has it, and recorded.
fn installed(home: &Home) -> Record {
    let mut record = Record::new();
    for asset in FILES {
        home.write(
            &rel(asset.base, asset.path),
            asset.text.as_bytes(),
            asset.mode,
        )
        .unwrap();
        record.upsert(FileEntry {
            path: asset.path.to_owned(),
            base: asset.base,
            sha256: sha256_hex(asset.text.as_bytes()),
            existed_before: false,
        });
    }
    record::save(home, &record).unwrap();
    record
}

fn path_of(id: Id) -> std::path::PathBuf {
    let asset = asset_for(id).unwrap();
    rel(asset.base, asset.path)
}

#[test]
fn a_clean_install_has_no_files_and_a_stable_build_hash() {
    let (_dir, home) = home();
    let record = installed(&home);
    let a = facts(&home, &record).unwrap();
    let b = facts(&home, &record).unwrap();
    assert!(a.files.is_empty());
    assert_eq!(a, b);
    assert_eq!(a.build_hash.len(), 64);
    assert_eq!(a.build_hash, build_hash());
    let raw = std::fs::read(home.abs(Path::new(record::RECORD_PATH))).unwrap();
    assert_eq!(a.record_sha256, Some(sha256_hex(&raw)));
}

#[test]
fn the_build_hash_covers_the_id_a_nul_and_the_text_of_each_asset_but_not_the_menu() {
    let mut bytes = Vec::new();
    for asset in FILES.iter().filter(|a| a.path != MENU_PATH) {
        bytes.extend_from_slice(asset.id.unwrap().as_str().as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(asset.text.as_bytes());
    }
    assert_eq!(build_hash(), sha256_hex(&bytes));
    assert_eq!(FILES.iter().filter(|a| a.id.is_none()).count(), 1);
    assert!(asset_for(Id::FleetBindings).is_some());
}

#[test]
fn no_record_gives_a_null_record_hash_and_an_unrecorded_conflict() {
    let (_dir, home) = home();
    home.write(&Path::new("omarchy").join(MANIFEST), b"theirs\n", 0o644)
        .unwrap();
    let out = facts(&home, &Record::new()).unwrap();
    assert_eq!(out.record_sha256, None);
    assert_eq!(out.files.len(), 1);
    let file = &out.files[0];
    assert_eq!(file.asset_id, Id::FleetPluginManifest);
    assert_eq!(file.classification, PlanClassification::UnrecordedConflict);
    assert_eq!(
        file.path,
        home.abs(&path_of(Id::FleetPluginManifest))
            .display()
            .to_string()
    );
    assert_eq!(file.rel, MANIFEST);
    assert_eq!(file.sha256, Some(sha256_hex(b"theirs\n")));
    assert_eq!(file.size, Some(7));
    assert_eq!(file.mode, Some(0o644));
    assert!(file.dev.is_some() && file.ino.is_some());
    assert!(file.replaceable);
    assert_eq!(file.blocked, None);
}

#[test]
fn a_recorded_file_that_the_owner_edited_is_a_recorded_edit() {
    let (_dir, home) = home();
    let mut record = installed(&home);
    home.write(&path_of(Id::HookThemeSet), b"#!/bin/sh\n# mine\n", 0o755)
        .unwrap();
    home.write(&path_of(Id::FleetPluginModel), b"mine\n", 0o644)
        .unwrap();
    // The model file differs from the asset and its record holds the hash
    // of an older build; the hook differs from its record too.
    record.upsert(FileEntry {
        path: "plugins/fleetdeck.fleet/Model.js".to_owned(),
        base: Base::Omarchy,
        sha256: sha256_hex(b"an older build\n"),
        existed_before: false,
    });
    let out = facts(&home, &record).unwrap();
    let kinds: Vec<_> = out
        .files
        .iter()
        .map(|f| (f.asset_id, f.classification))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (Id::FleetPluginModel, PlanClassification::RecordedEdit),
            (Id::HookThemeSet, PlanClassification::RecordedEdit),
        ]
    );
    assert_eq!(out.files[1].mode, Some(0o755));
}

#[test]
fn the_menu_never_appears_in_the_plan() {
    let (_dir, home) = home();
    let record = installed(&home);
    home.write(&rel(Base::Omarchy, MENU_PATH), b"{ \"mine\": 1 }\n", 0o644)
        .unwrap();
    assert!(facts(&home, &record).unwrap().files.is_empty());
    assert!(facts(&home, &Record::new()).unwrap().files.is_empty());
}

#[test]
fn a_link_a_hard_link_a_directory_and_a_large_file_are_not_replaceable() {
    let (dir, home) = home();
    installed(&home);
    let outside = tempfile::Builder::new().prefix("out").tempdir().unwrap();
    let victim = outside.path().join("victim");
    std::fs::write(&victim, "victim\n").unwrap();
    // A link.
    let widget = dir.path().join(path_of(Id::FleetPluginWidget));
    std::fs::remove_file(&widget).unwrap();
    symlink(&victim, &widget).unwrap();
    // A hard link (the second name of an own file).
    let panel = dir.path().join(path_of(Id::FleetPluginPanel));
    std::fs::hard_link(&panel, dir.path().join("omarchy/other-name")).unwrap();
    // A directory.
    let service = dir.path().join(path_of(Id::FleetPluginService));
    std::fs::remove_file(&service).unwrap();
    std::fs::create_dir(&service).unwrap();
    // A file over 8 MiB.
    let model = dir.path().join(path_of(Id::FleetPluginModel));
    std::fs::write(&model, vec![b'x'; 8 * 1024 * 1024 + 1]).unwrap();
    std::fs::set_permissions(&model, std::fs::Permissions::from_mode(0o644)).unwrap();

    let record = record::load(&home).unwrap().unwrap();
    let out = facts(&home, &record).unwrap();
    let find = |id| out.files.iter().find(|f| f.asset_id == id).unwrap();
    for id in [
        Id::FleetPluginWidget,
        Id::FleetPluginPanel,
        Id::FleetPluginService,
        Id::FleetPluginModel,
    ] {
        let file = find(id);
        assert!(!file.replaceable, "{id:?}");
        assert_eq!(file.blocked, Some(PlanBlock::UnsafePath), "{id:?}");
        assert_eq!(file.sha256, None, "{id:?}");
        // A recorded file is a recorded edit without a byte comparison.
        assert_eq!(
            file.classification,
            PlanClassification::RecordedEdit,
            "{id:?}"
        );
    }
    // The identity is kept where it is known.
    assert!(find(Id::FleetPluginPanel).ino.is_some());
    assert_eq!(find(Id::FleetPluginModel).size, Some(8 * 1024 * 1024 + 1));
    assert_eq!(out.files.len(), 4);
    // The victim of the link is untouched.
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "victim\n");
}

#[test]
fn an_unrecorded_unsafe_file_is_an_unrecorded_conflict() {
    let (dir, home) = home();
    let outside = tempfile::Builder::new().prefix("out").tempdir().unwrap();
    let victim = outside.path().join("victim");
    std::fs::write(&victim, "victim\n").unwrap();
    let hook = dir.path().join(path_of(Id::HookPostBoot));
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    symlink(&victim, &hook).unwrap();
    let out = facts(&home, &Record::new()).unwrap();
    assert_eq!(out.files.len(), 1);
    assert_eq!(
        out.files[0].classification,
        PlanClassification::UnrecordedConflict
    );
    assert!(!out.files[0].replaceable);
    assert_eq!(out.files[0].blocked, Some(PlanBlock::UnsafePath));
}

#[test]
fn a_link_above_a_file_makes_it_not_replaceable() {
    let (dir, home) = home();
    let outside = tempfile::Builder::new().prefix("out").tempdir().unwrap();
    std::fs::create_dir_all(outside.path().join("fleetdeck.fleet")).unwrap();
    std::fs::write(
        outside.path().join("fleetdeck.fleet/manifest.json"),
        "theirs\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("omarchy")).unwrap();
    symlink(outside.path(), dir.path().join("omarchy/plugins")).unwrap();
    let out = facts(&home, &Record::new()).unwrap();
    assert!(
        out.files
            .iter()
            .any(|f| f.asset_id == Id::FleetPluginManifest)
    );
    assert!(out.files.iter().all(|f| !f.replaceable));
    // No identity is known through a link above the file.
    assert!(
        out.files
            .iter()
            .all(|f| f.sha256.is_none() && f.dev.is_none())
    );
}
