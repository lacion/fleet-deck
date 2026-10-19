//! Tests of the PRE-PR round 1 fixes of the installer repair: kept names
//! that no hook dispatcher runs, an unsafe permission refused, a staging
//! file that is replaced is never published, a later edit that a restore
//! meets is never displaced, and a repair whose install did not finish
//! stays incomplete.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use fleetdeck_proto::omarchy_repair::OmarchyAssetId as Id;

use super::tests::{
    IDS, all, backup, bytes, home, original, reload, replaced, setup, staging, started, target,
};
use super::*;
use crate::omarchy::assets::{FILES, asset_for};
use crate::omarchy::record::rel;

const HOOK: Id = Id::HookPostBoot;
const HOOK_SCRIPT: &str = "#!/bin/bash\necho original >> \"$COUNTER\"\n";

fn chmod(path: &Path, bits: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
}

fn kept_abs(dir: &tempfile::TempDir, file: &JournalFile) -> PathBuf {
    dir.path().join(file.kept_rel().unwrap())
}

fn names(dir: &Path) -> Vec<String> {
    let mut all: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    all.sort();
    all
}

// ---- the kept names and the hook dispatcher ----

fn bash() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|d| d.join("bash"))
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("/bin/bash"))
}

/// The dispatcher of `omarchy-hook` (`omarchy-p4.md` line 38): every file
/// of `<name>.d/` in glob order, `bash "$file"`, and `*.sample` skipped. It
/// returns the names that it ran. The child has no `PATH`, so the real hook
/// asset finds no `fleetdeck`.
fn dispatch(dir: &Path, counter: &Path) -> Vec<String> {
    let ran: Vec<String> = names(dir)
        .into_iter()
        .filter(|n| !n.starts_with('.') && !n.ends_with(".sample"))
        .collect();
    for name in &ran {
        let status = Command::new(bash())
            .arg(dir.join(name))
            .env_clear()
            .env("PATH", "/nonexistent")
            .env("COUNTER", counter)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "{name}");
    }
    ran
}

fn originals_run(counter: &Path) -> usize {
    std::fs::read_to_string(counter)
        .unwrap_or_default()
        .lines()
        .filter(|l| *l == "original")
        .count()
}

#[test]
fn a_replacement_and_a_restore_run_the_original_hook_once_and_no_kept_copy() {
    let (dir, home) = home();
    let asset = asset_for(HOOK).unwrap();
    home.write(&rel(asset.base, asset.path), HOOK_SCRIPT.as_bytes(), 0o755)
        .unwrap();
    let reps = [Replacement {
        asset,
        id: HOOK,
        original_hash: Some(sha256_hex(HOOK_SCRIPT.as_bytes())),
    }];
    let journal = start(&home, &reps, &"b".repeat(64)).unwrap();
    let hooks = target(&dir, HOOK).parent().unwrap().to_path_buf();
    let counter = dir.path().join("counter");
    // Before: the original hook runs once.
    assert_eq!(dispatch(&hooks, &counter), vec!["fleetdeck".to_owned()]);
    assert_eq!(originals_run(&counter), 1);
    let _ = std::fs::remove_file(&counter);

    // After the replacement: the asset runs, the kept original does not.
    let mut run = Run::new(&home, journal);
    assert_eq!(run.advance_all(true).unwrap(), None);
    let journal = run.journal;
    assert!(journal.files[0].backup_name.ends_with(".sample"));
    assert!(backup(&dir, &journal.files[0]).is_file());
    assert_eq!(dispatch(&hooks, &counter), vec!["fleetdeck".to_owned()]);
    assert_eq!(originals_run(&counter), 0);

    // After the restore: the original runs once; the kept asset and the
    // backup do not.
    let mut run = Run::new(&home, journal);
    let (stopped, changed) = run.restore_all(&[0]).unwrap();
    assert_eq!((stopped, changed), (None, vec![0]));
    let journal = run.journal;
    let kept = journal.files[0].kept_name.clone().unwrap();
    assert!(kept.ends_with(".sample"), "{kept}");
    assert!(kept.starts_with("fleetdeck.fleetdeck-kept-"), "{kept}");
    assert_eq!(dispatch(&hooks, &counter), vec!["fleetdeck".to_owned()]);
    assert_eq!(originals_run(&counter), 1);
    // The kept names are on disk, and every one of them ends in `.sample`.
    let copies: Vec<String> = names(&hooks)
        .into_iter()
        .filter(|n| n.contains(".fleetdeck-"))
        .collect();
    assert_eq!(copies.len(), 2, "{copies:?}");
    assert!(copies.iter().all(|n| n.ends_with(".sample")), "{copies:?}");
}

#[test]
fn every_asset_gets_a_backup_name_that_ends_in_sample_and_a_kept_name_too() {
    for asset in FILES {
        let Some(id) = asset.id else { continue };
        let file = journal::new_file(asset, id, None, 7).unwrap();
        assert!(file.backup_name.ends_with(".sample"), "{:?}", asset.path);
        assert!(file.kept_name.is_none());
        let kept = journal::new_kept_name(&file.target, 7).unwrap();
        let leaf = file.target.rsplit('/').next().unwrap();
        assert!(kept.starts_with(&format!("{leaf}.fleetdeck-kept-7-")));
        assert!(kept.ends_with(".sample"));
        // 16 hex characters between the seconds and the ending.
        let random = kept
            .strip_suffix(".sample")
            .unwrap()
            .rsplit('-')
            .next()
            .unwrap();
        assert_eq!(random.len(), 16);
    }
    // A replacement and a restore leave only names that end in `.sample`.
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    let mut run = Run::new(&home, journal.clone());
    run.restore_all(&all(&journal)).unwrap();
    for id in IDS {
        let copies: Vec<String> = names(target(&dir, id).parent().unwrap())
            .into_iter()
            .filter(|n| n.contains(".fleetdeck-conflict-") || n.contains(".fleetdeck-kept-"))
            .collect();
        assert_eq!(copies.len(), 2, "{id:?}: {copies:?}");
        assert!(copies.iter().all(|n| n.ends_with(".sample")), "{copies:?}");
    }
}

#[test]
fn a_kept_name_of_a_journal_must_be_a_single_name_of_the_target() {
    let (_dir, home) = home();
    let journal = replaced(&home, &IDS);
    let write = |j: &Journal| {
        home.write(
            &journal::path_of(&journal.repair_id),
            &serde_json::to_vec(j).unwrap(),
            0o600,
        )
        .unwrap();
    };
    for bad in ["x/../../y", "other-name", ""] {
        let mut changed = journal.clone();
        changed.files[0].kept_name = Some(bad.to_owned());
        write(&changed);
        assert!(
            matches!(
                journal::load(&home, &journal.repair_id),
                Err(journal::LoadError::Invalid)
            ),
            "{bad:?}"
        );
    }
    let mut good = journal.clone();
    good.files[0].kept_name = Some(journal::new_kept_name(&good.files[0].target, 9).unwrap());
    write(&good);
    assert_eq!(
        journal::load(&home, &journal.repair_id).unwrap().unwrap(),
        good
    );
}

// ---- unsafe permissions ----

fn untouched(dir: &tempfile::TempDir, journal: &Journal) {
    for f in &journal.files {
        assert_eq!(
            bytes(&dir.path().join(f.target_rel())),
            Some(original(f.asset_id)),
            "{:?}",
            f.asset_id
        );
        assert!(!backup(dir, f).exists());
        assert!(!staging(dir, f).exists());
    }
}

#[test]
fn a_target_that_a_group_or_other_user_can_write_is_not_replaced() {
    for bits in [0o664, 0o646, 0o666] {
        let (dir, home) = home();
        let journal = started(&home, &IDS);
        chmod(&target(&dir, IDS[0]), bits);
        let mut run = Run::new(&home, journal);
        assert_eq!(
            run.advance_all(true).unwrap(),
            Some(code::UNSAFE_PATH),
            "{bits:o}"
        );
        assert_eq!(
            bytes(&target(&dir, IDS[0])),
            Some(original(IDS[0])),
            "{bits:o}"
        );
        assert!(!backup(&dir, &run.journal.files[0]).exists());
        // The file that is safe is replaced by a run that goes on.
        assert_eq!(run.advance_all(false).unwrap(), Some(code::UNSAFE_PATH));
        assert_eq!(
            view(&home, &run.journal).files[1].state,
            ObservedState::Published
        );
    }
}

#[test]
fn a_directory_that_a_group_or_other_user_can_write_stops_every_step() {
    // The base folder, and the folder of the target.
    for dir_rel in [
        "omarchy",
        "omarchy/plugins",
        "omarchy/plugins/fleetdeck.fleet",
    ] {
        let (dir, home) = home();
        let journal = started(&home, &IDS[..1]);
        chmod(&dir.path().join(dir_rel), 0o775);
        let mut run = Run::new(&home, journal.clone());
        assert_eq!(
            run.advance_all(true).unwrap(),
            Some(code::UNSAFE_PATH),
            "{dir_rel}"
        );
        untouched(&dir, &journal);
        // A restore of a published file stops too.
        chmod(&dir.path().join(dir_rel), 0o755);
        let mut run = Run::new(&home, journal.clone());
        assert_eq!(run.advance_all(true).unwrap(), None);
        chmod(&dir.path().join(dir_rel), 0o757);
        let mut run = Run::new(&home, run.journal);
        let (stopped, changed) = run.restore_all(&[0]).unwrap();
        assert_eq!((stopped, changed.len()), (Some(code::UNSAFE_PATH), 0));
        assert_eq!(
            bytes(&target(&dir, IDS[0])),
            Some(asset_for(IDS[0]).unwrap().text.as_bytes().to_vec()),
            "{dir_rel}"
        );
        // The same run goes on when the folder is safe again.
        chmod(&dir.path().join(dir_rel), 0o755);
        let (stopped, changed) = run.restore_all(&[0]).unwrap();
        assert_eq!((stopped, changed), (None, vec![0]));
    }
}

#[test]
fn a_published_target_that_others_can_write_is_not_moved_aside() {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    chmod(&target(&dir, IDS[1]), 0o666);
    let mut run = Run::new(&home, journal.clone());
    let (stopped, changed) = run.restore_all(&all(&journal)).unwrap();
    assert_eq!((stopped, changed), (Some(code::UNSAFE_PATH), vec![0]));
    assert_eq!(
        bytes(&target(&dir, IDS[1])),
        Some(asset_for(IDS[1]).unwrap().text.as_bytes().to_vec())
    );
}

#[test]
fn a_staging_file_that_is_not_the_one_written_is_never_published() {
    let asset = asset_for(IDS[0]).unwrap();
    let text = asset.text.as_bytes().to_vec();
    type Change = Box<dyn Fn(&Path)>;
    let variants: Vec<(&str, Change)> = vec![
        (
            "another file with other bytes",
            Box::new(|p: &Path| {
                std::fs::remove_file(p).unwrap();
                std::fs::write(p, b"planted\n").unwrap();
                chmod(p, 0o644);
            }),
        ),
        (
            "another file with the same bytes",
            Box::new({
                let text = text.clone();
                move |p: &Path| {
                    std::fs::remove_file(p).unwrap();
                    std::fs::write(p, &text).unwrap();
                    chmod(p, 0o644);
                }
            }),
        ),
        (
            "the same file with other bytes",
            Box::new(|p: &Path| std::fs::write(p, b"planted\n").unwrap()),
        ),
        ("a loose mode", Box::new(|p: &Path| chmod(p, 0o666))),
        (
            "a second name",
            Box::new(|p: &Path| std::fs::hard_link(p, p.with_extension("second")).unwrap()),
        ),
        (
            "a link",
            Box::new(|p: &Path| {
                std::fs::remove_file(p).unwrap();
                std::os::unix::fs::symlink("/etc/hostname", p).unwrap();
            }),
        ),
        (
            "no file",
            Box::new(|p: &Path| std::fs::remove_file(p).unwrap()),
        ),
    ];
    for (label, change) in variants {
        let (dir, home) = home();
        let journal = started(&home, &IDS[..1]);
        let mut run = Run::new(&home, journal);
        run.backup(0).unwrap();
        run.stage(
            0,
            asset.text.as_bytes(),
            asset.mode,
            (Step::Staging, Step::Staged),
        )
        .unwrap();
        change(&staging(&dir, &run.journal.files[0]));
        let halt = run.publish(0, (Step::Publishing, Step::Published));
        assert_eq!(halt, Err(Halt::Stop(code::REPAIR_INCOMPLETE)), "{label}");
        assert!(!target(&dir, IDS[0]).exists(), "{label}: nothing published");
        assert!(backup(&dir, &run.journal.files[0]).exists(), "{label}");
    }
}

#[test]
fn a_staging_file_that_is_the_one_written_is_published() {
    let (dir, home) = home();
    let journal = started(&home, &IDS[..1]);
    let mut run = Run::new(&home, journal);
    run.backup(0).unwrap();
    let asset = asset_for(IDS[0]).unwrap();
    run.stage(
        0,
        asset.text.as_bytes(),
        asset.mode,
        (Step::Staging, Step::Staged),
    )
    .unwrap();
    run.publish(0, (Step::Publishing, Step::Published)).unwrap();
    assert_eq!(
        bytes(&target(&dir, IDS[0])),
        Some(asset.text.as_bytes().to_vec())
    );
}

#[test]
fn a_publication_without_a_staging_file_of_this_run_stops() {
    let (dir, home) = home();
    let journal = started(&home, &IDS[..1]);
    let mut run = Run::new(&home, journal);
    run.backup(0).unwrap();
    // A staging file of an earlier run, under the name of the journal.
    std::fs::write(staging(&dir, &run.journal.files[0]), "stale").unwrap();
    assert_eq!(
        run.publish(0, (Step::Publishing, Step::Published)),
        Err(Halt::Stop(code::REPAIR_INCOMPLETE))
    );
    assert!(!target(&dir, IDS[0]).exists());
}

// ---- a later edit in a restore ----

/// A published repair, and an edit of the owner at the target.
fn edited() -> (tempfile::TempDir, Home, Journal) {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    std::fs::write(target(&dir, IDS[0]), "later edit\n").unwrap();
    (dir, home, journal)
}

fn no_kept(dir: &tempfile::TempDir, id: Id) -> bool {
    !names(target(dir, id).parent().unwrap())
        .iter()
        .any(|n| n.contains(".fleetdeck-kept-"))
}

#[test]
fn an_edit_between_the_observation_and_the_move_is_put_back_and_not_covered() {
    let (dir, home, journal) = edited();
    let mut run = Run::new(&home, journal);
    assert_eq!(run.move_aside(0), Err(Halt::Stop(code::RESTORE_CONFLICT)));
    // The edit is at the target again; nothing is kept under a kept name;
    // the backup was not published.
    assert_eq!(bytes(&target(&dir, IDS[0])), Some(b"later edit\n".to_vec()));
    assert!(no_kept(&dir, IDS[0]));
    assert_eq!(
        bytes(&backup(&dir, &run.journal.files[0])),
        Some(original(IDS[0]))
    );
    // The journal has the kept name (written before the move) and no intent.
    let saved = reload(&home, &run.journal);
    assert!(saved.files[0].kept_name.is_some());
    assert_eq!(saved.files[0].intent, None);
    assert_eq!(result_files(&home, &saved, None)[0].kept_path, None);
}

#[test]
fn an_edit_that_cannot_go_back_stays_under_its_kept_name_and_is_reported() {
    let (dir, home, journal) = edited();
    let mut journal = journal;
    // The state after the move: the edit under the kept name, and a new
    // file at the target.
    let kept = journal::new_kept_name(&journal.files[0].target, 5).unwrap();
    journal.files[0].kept_name = Some(kept);
    let kept_path = kept_abs(&dir, &journal.files[0]);
    std::fs::rename(target(&dir, IDS[0]), &kept_path).unwrap();
    std::fs::write(target(&dir, IDS[0]), "new file\n").unwrap();
    let mut run = Run::new(&home, journal);
    assert_eq!(run.settle_aside(0), Err(Halt::Stop(code::RESTORE_CONFLICT)));
    assert_eq!(bytes(&kept_path), Some(b"later edit\n".to_vec()));
    assert_eq!(
        bytes(&target(&dir, IDS[0])),
        Some(b"new file\n".to_vec()),
        "the file that is there is not replaced"
    );
    let result = result_files(&home, &run.journal, None);
    assert_eq!(
        result[0].kept_path.as_deref(),
        Some(kept_path.display().to_string().as_str())
    );
    assert_eq!(result[1].kept_path, None);
}

#[test]
fn a_displaced_file_that_cannot_be_hashed_counts_as_an_edit() {
    let (dir, home, journal) = edited();
    let mut journal = journal;
    let kept = journal::new_kept_name(&journal.files[0].target, 5).unwrap();
    journal.files[0].kept_name = Some(kept);
    let kept_path = kept_abs(&dir, &journal.files[0]);
    std::fs::rename(target(&dir, IDS[0]), &kept_path).unwrap();
    // A second name makes the file unreadable for the installer.
    std::fs::hard_link(&kept_path, dir.path().join("second-name")).unwrap();
    let mut run = Run::new(&home, journal);
    assert_eq!(run.settle_aside(0), Err(Halt::Stop(code::RESTORE_CONFLICT)));
    // The target is absent: the file went back.
    assert_eq!(bytes(&target(&dir, IDS[0])), Some(b"later edit\n".to_vec()));
    assert!(!kept_path.exists());
}

#[test]
fn a_kept_edit_stops_the_copy_of_the_backup_and_is_reported() {
    let (dir, home, journal) = edited();
    let mut journal = journal;
    let kept = journal::new_kept_name(&journal.files[0].target, 5).unwrap();
    journal.files[0].kept_name = Some(kept);
    let kept_path = kept_abs(&dir, &journal.files[0]);
    std::fs::rename(target(&dir, IDS[0]), &kept_path).unwrap();
    // The target is absent, the backup is the original: a copy would
    // normally follow.
    let mut run = Run::new(&home, journal.clone());
    let (stopped, changed) = run.restore_all(&all(&journal)).unwrap();
    assert_eq!(stopped, Some(code::RESTORE_CONFLICT));
    assert_eq!(changed, vec![1], "the other file is restored");
    assert!(
        !target(&dir, IDS[0]).exists(),
        "the backup is not published"
    );
    assert_eq!(bytes(&kept_path), Some(b"later edit\n".to_vec()));
    let result = result_files(&home, &run.journal, None);
    assert_eq!(
        result[0].kept_path.as_deref(),
        Some(kept_path.display().to_string().as_str())
    );
    // The owner takes the edit away: the restore goes on.
    std::fs::remove_file(&kept_path).unwrap();
    let mut run = Run::new(&home, run.journal);
    let (stopped, changed) = run.restore_all(&[0]).unwrap();
    assert_eq!((stopped, changed), (None, vec![0]));
    assert_eq!(bytes(&target(&dir, IDS[0])), Some(original(IDS[0])));
}

#[test]
fn an_earlier_kept_edit_is_never_hidden_by_a_new_kept_name() {
    let (dir, home) = home();
    let mut journal = replaced(&home, &IDS);
    let kept = journal::new_kept_name(&journal.files[0].target, 5).unwrap();
    journal.files[0].kept_name = Some(kept);
    let kept_path = kept_abs(&dir, &journal.files[0]);
    std::fs::write(&kept_path, "earlier edit\n").unwrap();
    // The target is the published asset again.
    let mut run = Run::new(&home, journal);
    let (stopped, changed) = run.restore_all(&[0]).unwrap();
    assert_eq!((stopped, changed.len()), (Some(code::RESTORE_CONFLICT), 0));
    assert_eq!(bytes(&kept_path), Some(b"earlier edit\n".to_vec()));
    assert!(target(&dir, IDS[0]).exists());
    assert_eq!(
        bytes(&target(&dir, IDS[0])),
        Some(asset_for(IDS[0]).unwrap().text.as_bytes().to_vec())
    );
}

#[test]
fn a_second_restore_gets_a_new_kept_name_and_keeps_the_first_copy() {
    let (dir, home) = home();
    let journal = replaced(&home, &IDS);
    let mut run = Run::new(&home, journal);
    run.restore_all(&[0]).unwrap();
    let first = run.journal.files[0].kept_name.clone().unwrap();
    // A resume of an interrupted repair publishes the asset again.
    std::fs::remove_file(target(&dir, IDS[0])).unwrap();
    assert_eq!(run.advance_all(false).unwrap(), None);
    let (stopped, changed) = run.restore_all(&[0]).unwrap();
    assert_eq!((stopped, changed), (None, vec![0]));
    let second = run.journal.files[0].kept_name.clone().unwrap();
    assert_ne!(first, second);
    let leaf_dir = target(&dir, IDS[0]).parent().unwrap().to_path_buf();
    assert!(leaf_dir.join(&first).exists() && leaf_dir.join(&second).exists());
    assert_eq!(bytes(&target(&dir, IDS[0])), Some(original(IDS[0])));
}

// ---- an unfinished repair ----

#[test]
fn a_repair_with_every_file_published_is_complete_only_when_it_is_finished() {
    let (dir, home) = home();
    let reps = setup(&home, &IDS);
    let journal = start(&home, &reps, &"b".repeat(64)).unwrap();
    assert!(!journal.finished, "a new journal is not finished");
    let mut run = Run::new(&home, journal);
    assert_eq!(run.advance_all(true).unwrap(), None);
    let saved = reload(&home, &run.journal);
    assert!(!saved.finished);
    assert!(all_published(&home, &saved));
    let v = view(&home, &saved);
    assert_eq!(v.state, JournalState::Incomplete);
    assert!(v.files.iter().all(|f| f.state == ObservedState::Published));
    // A resume finds nothing to do for the files.
    let mut run = Run::new(&home, saved);
    assert_eq!(run.advance_all(false).unwrap(), None);
    no_stage_names(&dir);
    // The rest of the install ran: the journal is saved as finished.
    let mut done = run.journal;
    done.finished = true;
    journal::save(&home, &done).unwrap();
    assert_eq!(
        view(&home, &reload(&home, &done)).state,
        JournalState::Complete
    );
}

fn no_stage_names(dir: &tempfile::TempDir) {
    for id in IDS {
        assert!(
            !names(target(dir, id).parent().unwrap())
                .iter()
                .any(|n| n.contains(".fleetdeck-stage-"))
        );
    }
}

#[test]
fn a_restored_journal_stays_restored_whether_it_is_finished_or_not() {
    let (_dir, home) = home();
    let mut journal = replaced(&home, &IDS);
    journal.finished = false;
    let mut run = Run::new(&home, journal.clone());
    run.restore_all(&all(&journal)).unwrap();
    assert_eq!(view(&home, &run.journal).state, JournalState::Restored);
}
