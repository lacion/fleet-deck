//! Tests of the RE-REVIEW round 2 fixes of the installer repair: the
//! recovery of an interrupted move aside before a resume or a restore
//! chooses its action, and the journal form without an earlier version.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use fleetdeck_proto::omarchy_repair::OmarchyAssetId as Id;

use super::tests::{IDS, all, backup, bytes, home, original, reload, replaced, target};
use super::*;

const EDIT: &[u8] = b"later edit\n";

fn arm(stop_at: Option<u32>) {
    STOP_AT.with(|s| s.set(stop_at));
    HITS.with(|h| h.set(0));
}

fn asset_bytes(id: Id) -> Vec<u8> {
    crate::omarchy::assets::asset_for(id)
        .unwrap()
        .text
        .as_bytes()
        .to_vec()
}

/// Runs the move aside of file 0 and crashes after its rename. Returns the
/// journal as saved and the absolute kept path.
fn crash_after_move(
    dir: &tempfile::TempDir,
    home: &Home,
    journal: Journal,
) -> (Journal, std::path::PathBuf) {
    let mut run = Run::new(home, journal);
    // The seam of `begin`, then the seam after the rename.
    arm(Some(2));
    let result = run.move_aside(0);
    arm(None);
    assert_eq!(result, Err(Halt::Injected));
    let saved = reload(home, &run.journal);
    assert_eq!(saved.files[0].intent, Some(Step::MovingAside));
    let kept = dir.path().join(saved.files[0].kept_rel().unwrap());
    assert!(!target(dir, IDS[0]).exists(), "the target moved");
    (saved, kept)
}

/// A published repair, an edit of the owner at the target, and a move aside
/// that a crash stops right after its rename: the edit sits under the kept
/// name, the target is absent and the intent is `MovingAside`.
fn crashed_with_edit() -> (tempfile::TempDir, Home, Journal, std::path::PathBuf) {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    std::fs::write(target(&dir, IDS[0]), EDIT).unwrap();
    let (journal, kept) = crash_after_move(&dir, &home, journal);
    assert_eq!(bytes(&kept), Some(EDIT.to_vec()));
    (dir, home, journal, kept)
}

fn assert_edit_back(dir: &tempfile::TempDir, home: &Home, run: &Run<'_>, kept: &std::path::Path) {
    // The edit is at the target again, the asset is not published, the kept
    // name is free and the intent is cleared.
    assert_eq!(bytes(&target(dir, IDS[0])), Some(EDIT.to_vec()));
    assert!(!kept.exists());
    assert_eq!(run.journal.files[0].intent, None);
    assert_eq!(reload(home, &run.journal).files[0].intent, None);
    assert_eq!(
        bytes(&backup(dir, &run.journal.files[0])),
        Some(original(IDS[0]))
    );
    assert_ne!(
        bytes(&target(dir, IDS[0])),
        Some(asset_bytes(IDS[0])),
        "the asset is not published over the edit"
    );
}

#[test]
fn a_resume_after_a_crashed_move_aside_puts_the_edit_back_and_stops_the_file() {
    let (dir, home, journal, kept) = crashed_with_edit();
    let mut run = Run::new(&home, journal);
    assert_eq!(
        run.advance_all(false).unwrap(),
        Some(code::RESTORE_CONFLICT)
    );
    assert_edit_back(&dir, &home, &run, &kept);
    // The other file is published and untouched.
    assert_eq!(
        bytes(&target(&dir, IDS[1])),
        Some(asset_bytes(IDS[1])),
        "the other file stays published"
    );
}

#[test]
fn a_restore_after_a_crashed_move_aside_puts_the_edit_back_and_stops_the_file() {
    let (dir, home, journal, kept) = crashed_with_edit();
    let mut run = Run::new(&home, journal.clone());
    let (stopped, changed) = run.restore_all(&all(&journal)).unwrap();
    assert_eq!(stopped, Some(code::RESTORE_CONFLICT));
    assert_eq!(changed, vec![1], "the other file is restored");
    assert_edit_back(&dir, &home, &run, &kept);
    assert_eq!(bytes(&target(&dir, IDS[1])), Some(original(IDS[1])));
    assert_eq!(result_files(&home, &run.journal, None)[0].kept_path, None);
}

#[test]
fn a_restore_after_a_crashed_move_aside_of_the_asset_copies_the_backup() {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    let (journal, kept) = crash_after_move(&dir, &home, journal);
    // No edit: the asset moved aside.
    assert_eq!(bytes(&kept), Some(asset_bytes(IDS[0])));
    let mut run = Run::new(&home, journal);
    let (stopped, changed) = run.restore_all(&[0]).unwrap();
    assert_eq!((stopped, changed), (None, vec![0]));
    assert_eq!(bytes(&target(&dir, IDS[0])), Some(original(IDS[0])));
    assert_eq!(bytes(&kept), Some(asset_bytes(IDS[0])));
    assert_eq!(run.journal.files[0].intent, None);
    let result = result_files(&home, &run.journal, None);
    assert_eq!(result[0].state, RepairFileState::Restored);
    assert_eq!(result[0].kept_path, None);
}

#[test]
fn a_moving_aside_intent_without_a_kept_file_is_cleared_and_the_action_goes_on() {
    let seed = || {
        let (dir, home) = home();
        let mut journal = replaced(&home, &IDS);
        let kept = journal::new_kept_name(&journal.files[0].target, 5).unwrap();
        journal.files[0].kept_name = Some(kept);
        journal.files[0].intent = Some(Step::MovingAside);
        journal::save(&home, &journal).unwrap();
        (dir, home, journal)
    };
    // A resume: the target is the published asset, nothing is left to do.
    let (dir, home, journal) = seed();
    let mut run = Run::new(&home, journal);
    assert_eq!(run.advance_all(false).unwrap(), None);
    assert_eq!(run.journal.files[0].intent, None);
    assert_eq!(reload(&home, &run.journal).files[0].intent, None);
    assert_eq!(bytes(&target(&dir, IDS[0])), Some(asset_bytes(IDS[0])));
    // A restore: the file is restored as without the intent.
    let (dir, home, journal) = seed();
    let mut run = Run::new(&home, journal.clone());
    let (stopped, changed) = run.restore_all(&all(&journal)).unwrap();
    assert_eq!((stopped, changed), (None, vec![0, 1]));
    assert_eq!(run.journal.files[0].intent, None);
    assert_eq!(bytes(&target(&dir, IDS[0])), Some(original(IDS[0])));
    assert_eq!(bytes(&target(&dir, IDS[1])), Some(original(IDS[1])));
}

#[test]
fn an_edit_that_cannot_go_back_stays_under_its_kept_name_after_a_crash() {
    for restore in [true, false] {
        let (dir, home, journal, kept) = crashed_with_edit();
        // The owner made a new file at the target in the meantime.
        std::fs::write(target(&dir, IDS[0]), "new file\n").unwrap();
        let mut run = Run::new(&home, journal.clone());
        if restore {
            let (stopped, changed) = run.restore_all(&[0]).unwrap();
            assert_eq!((stopped, changed.len()), (Some(code::RESTORE_CONFLICT), 0));
        } else {
            assert_eq!(
                run.advance_all(false).unwrap(),
                Some(code::RESTORE_CONFLICT)
            );
        }
        assert_eq!(bytes(&kept), Some(EDIT.to_vec()), "restore {restore}");
        assert_eq!(
            bytes(&target(&dir, IDS[0])),
            Some(b"new file\n".to_vec()),
            "restore {restore}"
        );
        assert_eq!(run.journal.files[0].intent, None);
        let result = result_files(&home, &run.journal, Some(&[0]));
        assert_eq!(
            result[0].kept_path.as_deref(),
            Some(kept.display().to_string().as_str()),
            "restore {restore}"
        );
        // A second run still refuses and still reports the edit.
        let mut again = Run::new(&home, reload(&home, &run.journal));
        let (stopped, changed) = again.restore_all(&[0]).unwrap();
        assert_eq!((stopped, changed.len()), (Some(code::RESTORE_CONFLICT), 0));
        assert_eq!(bytes(&kept), Some(EDIT.to_vec()));
    }
}

// ---- the journal form ----

fn write_value(home: &Home, journal: &Journal, value: &serde_json::Value) {
    home.write(
        &journal::path_of(&journal.repair_id),
        &serde_json::to_vec(value).unwrap(),
        0o600,
    )
    .unwrap();
}

#[test]
fn a_journal_of_version_1_is_not_valid() {
    let (_dir, home) = home();
    let journal = replaced(&home, &IDS);
    assert_eq!(journal.v, 2);
    let mut value = serde_json::to_value(&journal).unwrap();
    value["v"] = 1.into();
    write_value(&home, &journal, &value);
    assert!(matches!(
        journal::load(&home, &journal.repair_id),
        Err(journal::LoadError::Invalid)
    ));
}

#[test]
fn a_journal_of_version_2_without_finished_is_not_valid() {
    let (_dir, home) = home();
    let journal = replaced(&home, &IDS);
    let mut value = serde_json::to_value(&journal).unwrap();
    value.as_object_mut().unwrap().remove("finished");
    write_value(&home, &journal, &value);
    assert!(matches!(
        journal::load(&home, &journal.repair_id),
        Err(journal::LoadError::Invalid)
    ));
}

#[test]
fn a_backup_name_or_a_kept_name_without_sample_is_not_valid() {
    let (_dir, home) = home();
    let journal = replaced(&home, &IDS);
    let load = || journal::load(&home, &journal.repair_id);
    let mut bad = journal.clone();
    let name = bad.files[0].backup_name.clone();
    bad.files[0].backup_name = name.strip_suffix(".sample").unwrap().to_owned();
    write_value(&home, &journal, &serde_json::to_value(&bad).unwrap());
    assert!(matches!(load(), Err(journal::LoadError::Invalid)));

    let mut bad = journal.clone();
    let kept = journal::new_kept_name(&bad.files[0].target, 5).unwrap();
    bad.files[0].kept_name = Some(kept.strip_suffix(".sample").unwrap().to_owned());
    write_value(&home, &journal, &serde_json::to_value(&bad).unwrap());
    assert!(matches!(load(), Err(journal::LoadError::Invalid)));

    // The same journal with the endings is valid.
    let mut good = journal.clone();
    good.files[0].kept_name = Some(kept);
    write_value(&home, &journal, &serde_json::to_value(&good).unwrap());
    assert_eq!(load().unwrap().unwrap(), good);
}
