//! Tests of the journaled replacement, the resume and the restore
//! (`board-gaps.md` §3.5.1 and §3.6): every row of the decision table, and
//! a simulated crash at every seam, each followed by resume twice and by
//! restore twice, with a check of the files after each run.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use fleetdeck_proto::omarchy_repair::OmarchyAssetId as Id;

use super::*;
use crate::omarchy::assets::asset_for;
use crate::omarchy::record::rel;

const IDS: [Id; 2] = [Id::FleetPluginManifest, Id::HookThemeSet];

fn home() -> (tempfile::TempDir, Home) {
    let dir = tempfile::Builder::new().prefix("rep").tempdir().unwrap();
    let home = Home::open(dir.path()).unwrap();
    (dir, home)
}

fn original(id: Id) -> Vec<u8> {
    format!("original of {}\n", id.as_str()).into_bytes()
}

fn target(dir: &tempfile::TempDir, id: Id) -> PathBuf {
    let asset = asset_for(id).unwrap();
    dir.path().join(rel(asset.base, asset.path))
}

/// Writes an own file for each id and returns the replacements.
fn setup(home: &Home, ids: &[Id]) -> Vec<Replacement> {
    ids.iter()
        .map(|id| {
            let asset = asset_for(*id).unwrap();
            let bytes = original(*id);
            home.write(&rel(asset.base, asset.path), &bytes, asset.mode)
                .unwrap();
            Replacement {
                asset,
                id: *id,
                original_hash: Some(sha256_hex(&bytes)),
            }
        })
        .collect()
}

fn started(home: &Home, ids: &[Id]) -> Journal {
    let reps = setup(home, ids);
    start(home, &reps, &"b".repeat(64)).unwrap()
}

/// A complete replacement.
fn replaced(home: &Home, ids: &[Id]) -> Journal {
    let mut run = Run::new(home, started(home, ids));
    assert_eq!(run.advance_all(true).unwrap(), None);
    run.journal
}

fn bytes(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

fn backup(dir: &tempfile::TempDir, file: &JournalFile) -> PathBuf {
    dir.path().join(file.backup_rel())
}

fn staging(dir: &tempfile::TempDir, file: &JournalFile) -> PathBuf {
    dir.path().join(file.staging_rel())
}

/// No staging or kept-aside name is left under `dir` (only stage names).
fn no_staging(dir: &tempfile::TempDir) {
    fn walk(path: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(path).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, found);
            } else if p.to_string_lossy().contains(".fleetdeck-stage-") {
                found.push(p);
            }
        }
    }
    let mut found = Vec::new();
    walk(dir.path(), &mut found);
    assert!(found.is_empty(), "staging files left: {found:?}");
}

fn assert_published(dir: &tempfile::TempDir, journal: &Journal) {
    for f in &journal.files {
        let asset = f.asset().unwrap();
        assert_eq!(
            bytes(&dir.path().join(f.target_rel())),
            Some(asset.text.as_bytes().to_vec()),
            "{:?}",
            f.asset_id
        );
        assert_eq!(bytes(&backup(dir, f)), Some(original(f.asset_id)));
        let mode = std::fs::metadata(dir.path().join(f.target_rel()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, asset.mode);
    }
}

fn assert_restored(dir: &tempfile::TempDir, journal: &Journal) {
    for f in &journal.files {
        assert_eq!(
            bytes(&dir.path().join(f.target_rel())),
            Some(original(f.asset_id)),
            "{:?}",
            f.asset_id
        );
        if let Some(kept) = bytes(&backup(dir, f)) {
            assert_eq!(kept, original(f.asset_id), "the kept copy stays");
        }
    }
}

fn all(journal: &Journal) -> Vec<usize> {
    (0..journal.files.len()).collect()
}

fn reload(home: &Home, journal: &Journal) -> Journal {
    journal::load(home, &journal.repair_id).unwrap().unwrap()
}

fn observed(home: &Home, journal: &Journal) -> Vec<ObservedState> {
    view(home, journal).files.iter().map(|f| f.state).collect()
}

fn actions(home: &Home, journal: &Journal) -> Vec<RestoreAction> {
    restore_facts(home, journal)
        .files
        .iter()
        .map(|f| f.action)
        .collect()
}

// ---- the journal ----

#[test]
fn the_journal_is_private_and_holds_the_plan_before_any_file_moves() {
    let (dir, home) = home();
    let journal = started(&home, &IDS);
    let path = dir.path().join(journal::path_of(&journal.repair_id));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(dir.path().join(journal::DIR))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(journal.repair_id.len(), 32);
    assert_eq!(journal.build_hash, "b".repeat(64));
    for f in &journal.files {
        assert_eq!(f.original_hash, Some(sha256_hex(&original(f.asset_id))));
        assert_eq!(f.asset_hash, sha256_hex(f.asset().unwrap().text.as_bytes()));
        let name = f.target.rsplit('/').next().unwrap();
        assert!(
            f.backup_name
                .starts_with(&format!("{name}.fleetdeck-conflict-"))
        );
        assert!(
            f.staging_name
                .starts_with(&format!(".{name}.fleetdeck-stage-"))
        );
        assert_eq!(f.intent, None);
        assert!(f.completed.is_empty());
    }
    // Nothing moved: the targets are the originals.
    for id in IDS {
        assert_eq!(bytes(&target(&dir, id)), Some(original(id)));
    }
    assert_eq!(reload(&home, &journal), journal);
}

#[test]
fn a_journal_that_is_not_valid_is_refused() {
    let (_dir, home) = home();
    let journal = replaced(&home, &IDS);
    let write = |j: &Journal| {
        let text = serde_json::to_vec(j).unwrap();
        home.write(&journal::path_of(&journal.repair_id), &text, 0o600)
            .unwrap();
    };
    let load = || journal::load(&home, &journal.repair_id);
    assert!(load().unwrap().is_some());

    let mut bad = journal.clone();
    bad.files[0].target = "../../escape".to_owned();
    write(&bad);
    assert!(matches!(load(), Err(journal::LoadError::Invalid)));

    let mut bad = journal.clone();
    bad.files[0].backup_name = "x/../../y".to_owned();
    write(&bad);
    assert!(matches!(load(), Err(journal::LoadError::Invalid)));

    let mut bad = journal.clone();
    bad.files[1].staging_name = "other-name".to_owned();
    write(&bad);
    assert!(matches!(load(), Err(journal::LoadError::Invalid)));

    let mut bad = journal.clone();
    bad.repair_id = "c".repeat(32);
    write(&bad);
    assert!(matches!(load(), Err(journal::LoadError::Invalid)));

    let mut bad = journal.clone();
    bad.files[1].asset_id = bad.files[0].asset_id;
    write(&bad);
    assert!(matches!(load(), Err(journal::LoadError::Invalid)));

    let mut bad = journal.clone();
    bad.files[0].asset_hash = "zz".to_owned();
    write(&bad);
    assert!(matches!(load(), Err(journal::LoadError::Invalid)));

    write(&journal);
    assert!(load().unwrap().is_some());
    assert!(journal::load(&home, &"d".repeat(32)).unwrap().is_none());
}

// ---- replace ----

#[test]
fn a_replacement_publishes_each_asset_and_keeps_each_original() {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    assert_published(&dir, &journal);
    no_staging(&dir);
    assert_eq!(observed(&home, &journal), vec![ObservedState::Published; 2]);
    let v = view(&home, &journal);
    assert_eq!(v.state, JournalState::Complete);
    assert!(v.files.iter().all(|f| f.backup_path.is_some()));
    let result = result_files(&home, &journal, None);
    assert!(result.iter().all(|f| f.state == RepairFileState::Replaced));
    assert!(all_published(&home, &journal));
    // The journal records the steps as hints.
    let saved = reload(&home, &journal);
    assert_eq!(
        saved.files[0].completed,
        vec![Step::BackedUp, Step::Staged, Step::Published]
    );
    assert_eq!(saved.files[0].intent, None);
}

#[test]
fn a_backup_name_that_exists_with_other_content_stops_with_repair_incomplete() {
    let (dir, home) = home();
    let journal = started(&home, &IDS);
    std::fs::write(backup(&dir, &journal.files[0]), "squatter").unwrap();
    let mut run = Run::new(&home, journal);
    assert_eq!(
        run.advance_all(true).unwrap(),
        Some(code::REPAIR_INCOMPLETE)
    );
    let journal = run.journal;
    for id in IDS {
        assert_eq!(bytes(&target(&dir, id)), Some(original(id)), "{id:?}");
    }
    assert_eq!(
        bytes(&backup(&dir, &journal.files[0])),
        Some(b"squatter".to_vec())
    );
    // The first file stopped the run: the second is not touched.
    assert_eq!(observed(&home, &journal)[1], ObservedState::NothingMoved);
}

#[test]
fn a_journal_folder_that_cannot_be_made_stops_with_backup_failed_before_any_move() {
    let (dir, home) = home();
    let reps = setup(&home, &IDS);
    std::fs::create_dir_all(dir.path().join("fleetdeck")).unwrap();
    std::fs::write(dir.path().join(journal::DIR), "a file in the way").unwrap();
    let err = start(&home, &reps, &"b".repeat(64)).unwrap_err();
    assert_eq!(err, Halt::Stop(code::BACKUP_FAILED));
    for id in IDS {
        assert_eq!(bytes(&target(&dir, id)), Some(original(id)), "{id:?}");
    }
}

#[test]
fn a_replacement_stops_at_the_first_stopped_file_and_a_resume_goes_on() {
    let (dir, home) = home();
    let journal = started(&home, &IDS);
    // A later owner edit of the first target: another hash.
    std::fs::write(target(&dir, IDS[0]), "later edit\n").unwrap();
    let mut run = Run::new(&home, journal);
    assert_eq!(run.advance_all(true).unwrap(), Some(code::RESTORE_CONFLICT));
    assert_eq!(
        observed(&home, &run.journal)[1],
        ObservedState::NothingMoved
    );
    // A resume stops only that file; the second file is replaced.
    let journal = run.journal;
    let mut run = Run::new(&home, journal);
    assert_eq!(
        run.advance_all(false).unwrap(),
        Some(code::RESTORE_CONFLICT)
    );
    let states = observed(&home, &run.journal);
    assert_eq!(
        states,
        vec![ObservedState::Conflict, ObservedState::Published]
    );
    assert_eq!(
        bytes(&target(&dir, IDS[0])),
        Some(b"later edit\n".to_vec()),
        "nothing is overwritten"
    );
    let result = result_files(&home, &run.journal, None);
    assert_eq!(result[0].state, RepairFileState::Unchanged);
    assert_eq!(result[1].state, RepairFileState::Replaced);
    assert!(!all_published(&home, &run.journal));
}

// ---- the decision table ----

#[test]
fn row_nothing_moved() {
    let (dir, home) = home();
    let journal = started(&home, &IDS);
    assert_eq!(
        observed(&home, &journal),
        vec![ObservedState::NothingMoved; 2]
    );
    assert_eq!(actions(&home, &journal), vec![RestoreAction::Nothing; 2]);
    // A stale staging file under the journal name goes away on a restore.
    std::fs::write(staging(&dir, &journal.files[0]), "stale").unwrap();
    let selected = all(&journal);
    let mut run = Run::new(&home, journal);
    let (stopped, changed) = run.restore_all(&selected).unwrap();
    assert_eq!((stopped, changed.len()), (None, 0));
    no_staging(&dir);
    assert_restored(&dir, &run.journal);
}

#[test]
fn row_backed_up_resumes_and_restores_by_copy() {
    let (dir, home) = home();
    let journal = started(&home, &IDS);
    for f in &journal.files {
        std::fs::rename(dir.path().join(f.target_rel()), backup(&dir, f)).unwrap();
        std::fs::write(staging(&dir, f), "stale").unwrap();
    }
    assert_eq!(observed(&home, &journal), vec![ObservedState::BackedUp; 2]);
    assert_eq!(actions(&home, &journal), vec![RestoreAction::CopyBackup; 2]);
    // Restore: copy; the kept copy stays; the mode is the original mode.
    let mut run = Run::new(&home, journal.clone());
    let (stopped, changed) = run.restore_all(&all(&journal)).unwrap();
    assert_eq!((stopped, changed), (None, vec![0, 1]));
    assert_restored(&dir, &journal);
    no_staging(&dir);
    for f in &journal.files {
        assert!(backup(&dir, f).exists());
        let mode = std::fs::metadata(dir.path().join(f.target_rel()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, f.asset().unwrap().mode);
    }
    assert_eq!(observed(&home, &journal), vec![ObservedState::Restored; 2]);
    assert_eq!(view(&home, &journal).state, JournalState::Restored);
}

#[test]
fn row_backed_up_resume_publishes() {
    let (dir, home) = home();
    let journal = started(&home, &IDS);
    for f in &journal.files {
        std::fs::rename(dir.path().join(f.target_rel()), backup(&dir, f)).unwrap();
        std::fs::write(staging(&dir, f), "stale").unwrap();
    }
    let mut run = Run::new(&home, journal);
    assert_eq!(run.advance_all(false).unwrap(), None);
    assert_published(&dir, &run.journal);
    no_staging(&dir);
}

#[test]
fn row_published_restores_by_moving_the_target_aside_and_copying() {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    assert_eq!(
        actions(&home, &journal),
        vec![RestoreAction::MoveAsideThenCopyBackup; 2]
    );
    let mut run = Run::new(&home, journal.clone());
    let (stopped, changed) = run.restore_all(&all(&journal)).unwrap();
    assert_eq!((stopped, changed), (None, vec![0, 1]));
    assert_restored(&dir, &journal);
    no_staging(&dir);
    // The published asset stays under a new kept name.
    for f in &journal.files {
        let leaf = f.target.rsplit('/').next().unwrap();
        let kept: Vec<_> = std::fs::read_dir(dir.path().join(f.target_rel()).parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{leaf}.fleetdeck-kept-"))
            })
            .collect();
        assert_eq!(kept.len(), 1, "{leaf}");
        assert_eq!(
            std::fs::read(kept[0].path()).unwrap(),
            f.asset().unwrap().text.as_bytes()
        );
    }
}

#[test]
fn row_published_resume_only_removes_a_staging_file() {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    std::fs::write(staging(&dir, &journal.files[0]), "stale").unwrap();
    let mut run = Run::new(&home, journal);
    assert_eq!(run.advance_all(false).unwrap(), None);
    no_staging(&dir);
    assert_published(&dir, &run.journal);
}

#[test]
fn rows_with_no_original_publish_and_restore_by_removing_the_target() {
    let (dir, home) = home();
    let asset = asset_for(Id::FleetPluginWidget).unwrap();
    let reps = [Replacement {
        asset,
        id: Id::FleetPluginWidget,
        original_hash: None,
    }];
    let journal = start(&home, &reps, &"b".repeat(64)).unwrap();
    // Absent, absent, `none`: nothing is published yet.
    assert_eq!(observed(&home, &journal), vec![ObservedState::NothingMoved]);
    assert_eq!(actions(&home, &journal), vec![RestoreAction::Nothing]);
    let mut run = Run::new(&home, journal);
    assert_eq!(run.advance_all(true).unwrap(), None);
    let journal = run.journal;
    let path = dir.path().join(journal.files[0].target_rel());
    assert_eq!(std::fs::read(&path).unwrap(), asset.text.as_bytes());
    assert!(!backup(&dir, &journal.files[0]).exists());
    // Asset, absent, `none`: published; a restore removes the target.
    assert_eq!(observed(&home, &journal), vec![ObservedState::Published]);
    assert_eq!(
        actions(&home, &journal),
        vec![RestoreAction::RemovePublished]
    );
    let mut run = Run::new(&home, journal.clone());
    let (stopped, changed) = run.restore_all(&[0]).unwrap();
    assert_eq!((stopped, changed), (None, vec![0]));
    assert!(!path.exists());
    // Again: nothing to restore.
    let (stopped, changed) = run.restore_all(&[0]).unwrap();
    assert_eq!((stopped, changed.len()), (None, 0));
    // A later owner edit of the published file is a conflict: not removed.
    let mut run = Run::new(&home, journal);
    assert_eq!(run.advance_all(false).unwrap(), None);
    std::fs::write(&path, "later edit\n").unwrap();
    let (stopped, _) = run.restore_all(&[0]).unwrap();
    assert_eq!(stopped, Some(code::RESTORE_CONFLICT));
    assert_eq!(std::fs::read(&path).unwrap(), b"later edit\n");
}

#[test]
fn row_published_and_the_backup_is_gone_stops_with_repair_incomplete() {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    std::fs::remove_file(backup(&dir, &journal.files[0])).unwrap();
    assert_eq!(observed(&home, &journal)[0], ObservedState::Incomplete);
    assert_eq!(
        actions(&home, &journal)[0],
        RestoreAction::StopRepairIncomplete
    );
    let mut run = Run::new(&home, journal.clone());
    assert_eq!(
        run.advance_all(false).unwrap(),
        Some(code::REPAIR_INCOMPLETE)
    );
    let (stopped, changed) = run.restore_all(&all(&journal)).unwrap();
    assert_eq!(stopped, Some(code::REPAIR_INCOMPLETE));
    // Only the file that stopped is not restored.
    assert_eq!(changed, vec![1]);
    assert_eq!(
        bytes(&target(&dir, IDS[0])),
        Some(asset_for(IDS[0]).unwrap().text.as_bytes().to_vec())
    );
}

#[test]
fn row_absent_absent_with_an_original_stops_with_repair_incomplete() {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    let f = &journal.files[0];
    std::fs::remove_file(dir.path().join(f.target_rel())).unwrap();
    std::fs::remove_file(backup(&dir, f)).unwrap();
    assert_eq!(observed(&home, &journal)[0], ObservedState::Incomplete);
    assert_eq!(
        actions(&home, &journal)[0],
        RestoreAction::StopRepairIncomplete
    );
    let mut run = Run::new(&home, journal);
    assert_eq!(
        run.advance_all(true).unwrap(),
        Some(code::REPAIR_INCOMPLETE)
    );
    assert!(!target(&dir, IDS[0]).exists());
}

#[test]
fn row_another_hash_stops_with_restore_conflict_and_nothing_is_overwritten() {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    std::fs::write(target(&dir, IDS[0]), "later edit\n").unwrap();
    assert_eq!(observed(&home, &journal)[0], ObservedState::Conflict);
    assert_eq!(
        actions(&home, &journal)[0],
        RestoreAction::StopRestoreConflict
    );
    let before = bytes(&backup(&dir, &journal.files[0]));
    let mut run = Run::new(&home, journal.clone());
    assert_eq!(
        run.advance_all(false).unwrap(),
        Some(code::RESTORE_CONFLICT)
    );
    let (stopped, _) = run.restore_all(&all(&journal)).unwrap();
    assert_eq!(stopped, Some(code::RESTORE_CONFLICT));
    assert_eq!(bytes(&target(&dir, IDS[0])), Some(b"later edit\n".to_vec()));
    assert_eq!(bytes(&backup(&dir, &journal.files[0])), before);
    // A link and a directory count as another hash.
    std::fs::remove_file(target(&dir, IDS[0])).unwrap();
    std::fs::create_dir(target(&dir, IDS[0])).unwrap();
    assert_eq!(observed(&home, &journal)[0], ObservedState::Conflict);
}

#[test]
fn row_the_backup_changed_stops_with_repair_incomplete() {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    std::fs::write(backup(&dir, &journal.files[1]), "changed\n").unwrap();
    assert_eq!(observed(&home, &journal)[1], ObservedState::Incomplete);
    assert_eq!(
        actions(&home, &journal)[1],
        RestoreAction::StopRepairIncomplete
    );
    let mut run = Run::new(&home, journal.clone());
    assert_eq!(
        run.advance_all(false).unwrap(),
        Some(code::REPAIR_INCOMPLETE)
    );
    let (stopped, _) = run.restore_all(&all(&journal)).unwrap();
    assert_eq!(stopped, Some(code::REPAIR_INCOMPLETE));
    assert_eq!(
        bytes(&backup(&dir, &journal.files[1])),
        Some(b"changed\n".to_vec())
    );
}

#[test]
fn a_staging_name_that_is_not_a_regular_file_stops_with_repair_incomplete() {
    let (dir, home) = home();
    let journal = started(&home, &IDS);
    std::fs::create_dir(staging(&dir, &journal.files[0])).unwrap();
    assert_eq!(observed(&home, &journal)[0], ObservedState::Incomplete);
    let mut run = Run::new(&home, journal);
    assert_eq!(
        run.advance_all(false).unwrap(),
        Some(code::REPAIR_INCOMPLETE)
    );
    assert_eq!(bytes(&target(&dir, IDS[0])), Some(original(IDS[0])));
}

#[test]
fn row_restored_is_skipped_by_a_resume() {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    let mut run = Run::new(&home, journal.clone());
    run.restore_all(&all(&journal)).unwrap();
    assert_eq!(observed(&home, &journal), vec![ObservedState::Restored; 2]);
    assert_eq!(actions(&home, &journal), vec![RestoreAction::Nothing; 2]);
    let mut run = Run::new(&home, journal.clone());
    assert_eq!(run.advance_all(false).unwrap(), None);
    assert_restored(&dir, &journal);
    assert!(!all_published(&home, &journal));
    let result = result_files(&home, &journal, None);
    assert!(result.iter().all(|f| f.state == RepairFileState::Restored));
}

#[test]
fn the_restore_facts_hold_the_hashes_and_paths() {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    let facts = restore_facts(&home, &journal);
    assert_eq!(facts.repair_id, journal.repair_id);
    for (fact, f) in facts.files.iter().zip(&journal.files) {
        assert_eq!(fact.asset_id, f.asset_id);
        assert_eq!(fact.target_sha256.as_deref(), Some(f.asset_hash.as_str()));
        assert_eq!(fact.backup_sha256, f.original_hash);
        assert_eq!(fact.backup_path, backup(&dir, f).display().to_string());
        assert_eq!(
            fact.path,
            dir.path().join(f.target_rel()).display().to_string()
        );
    }
    std::fs::remove_file(backup(&dir, &journal.files[0])).unwrap();
    let facts = restore_facts(&home, &journal);
    assert_eq!(facts.files[0].backup_sha256, None);
}

#[test]
fn the_view_derives_the_journal_state_from_the_files() {
    let (dir, home) = home();
    let journal = started(&home, &IDS);
    assert_eq!(view(&home, &journal).state, JournalState::Incomplete);
    let mut run = Run::new(&home, journal);
    run.advance_all(true).unwrap();
    assert_eq!(view(&home, &run.journal).state, JournalState::Complete);
    // The journal file says complete; a missing backup makes it incomplete.
    std::fs::remove_file(backup(&dir, &run.journal.files[0])).unwrap();
    assert_eq!(view(&home, &run.journal).state, JournalState::Incomplete);
}

// ---- crash seams ----

fn arm(stop_at: Option<u32>) {
    STOP_AT.with(|s| s.set(stop_at));
    HITS.with(|h| h.set(0));
}

/// A replacement that a crash stops at the seam `n`: `Some(journal)` when
/// it stopped, `None` when the run ended before the seam.
fn crashed(home: &Home, n: u32) -> Option<Journal> {
    let reps = setup(home, &IDS);
    let journal = start(home, &reps, &"b".repeat(64)).unwrap();
    arm(Some(n));
    let mut run = Run::new(home, journal);
    let result = run.advance_all(true);
    arm(None);
    match result {
        Err(Halt::Injected) => Some(reload(home, &run.journal)),
        Ok(None) => None,
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn a_crash_at_every_seam_is_recovered_by_a_resume_twice() {
    let mut seams = 0;
    for n in 1..500 {
        let (dir, home) = home();
        let Some(journal) = crashed(&home, n) else {
            break;
        };
        seams = n;
        // The status shows the table state, from the files: never a
        // conflict or an incomplete repair.
        for state in observed(&home, &journal) {
            assert!(
                matches!(
                    state,
                    ObservedState::NothingMoved
                        | ObservedState::BackedUp
                        | ObservedState::Published
                ),
                "seam {n}: {state:?}"
            );
        }
        for round in 0..2 {
            let mut run = Run::new(&home, reload(&home, &journal));
            assert_eq!(
                run.advance_all(false).unwrap(),
                None,
                "seam {n} round {round}"
            );
            assert_published(&dir, &run.journal);
            no_staging(&dir);
        }
        assert_eq!(view(&home, &journal).state, JournalState::Complete);
    }
    // Two files with 3 steps each (backup, stage, publish) and 3 seams in
    // each step, and the journal writes in between.
    assert!(seams >= 18, "{seams} seams");
}

#[test]
fn a_crash_at_every_seam_is_recovered_by_a_restore_twice() {
    let mut seams = 0;
    for n in 1..500 {
        let (dir, home) = home();
        let Some(journal) = crashed(&home, n) else {
            break;
        };
        seams = n;
        for round in 0..2 {
            let mut run = Run::new(&home, reload(&home, &journal));
            let (stopped, _) = run.restore_all(&all(&journal)).unwrap();
            assert_eq!(stopped, None, "seam {n} round {round}");
            assert_restored(&dir, &journal);
            no_staging(&dir);
        }
    }
    assert!(seams >= 18, "{seams} seams");
}

#[test]
fn a_crash_at_every_seam_of_a_restore_is_recovered_by_a_restore_twice_and_by_a_resume() {
    let mut seams = 0;
    for n in 1..500 {
        let (dir, home) = home();
        let journal = replaced(&home, &IDS);
        arm(Some(n));
        let mut run = Run::new(&home, journal.clone());
        let result = run.restore_all(&all(&journal));
        arm(None);
        match result {
            Err(Halt::Injected) => {}
            Ok(_) => break,
            Err(other) => panic!("unexpected: {other:?}"),
        }
        seams = n;
        for state in observed(&home, &journal) {
            assert!(
                matches!(
                    state,
                    ObservedState::Published | ObservedState::BackedUp | ObservedState::Restored
                ),
                "seam {n}: {state:?}"
            );
        }
        for round in 0..2 {
            let mut run = Run::new(&home, reload(&home, &journal));
            let (stopped, _) = run.restore_all(&all(&journal)).unwrap();
            assert_eq!(stopped, None, "seam {n} round {round}");
            assert_restored(&dir, &journal);
            no_staging(&dir);
        }
    }
    assert!(seams >= 10, "{seams} seams");
}

#[test]
fn a_crash_at_every_seam_of_a_restore_is_recovered_by_a_resume() {
    for n in 1..500 {
        let (dir, home) = home();
        let journal = replaced(&home, &IDS);
        arm(Some(n));
        let mut run = Run::new(&home, journal.clone());
        let result = run.restore_all(&all(&journal));
        arm(None);
        if result.is_ok() {
            break;
        }
        // A resume finishes the repair (a restored file is skipped, a file
        // between the states is published again).
        let mut run = Run::new(&home, reload(&home, &journal));
        let stopped = run.advance_all(false).unwrap();
        assert_eq!(stopped, None, "seam {n}");
        no_staging(&dir);
        for f in &journal.files {
            assert!(
                bytes(&dir.path().join(f.target_rel())).is_some(),
                "seam {n}: the target is present"
            );
        }
    }
}
