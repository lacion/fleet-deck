//! Tests of `omarchy screensaver on | off | rebaseline` and of the restore
//! (`omarchy-admin.md` sections 3.4, 3.6 and 3.7). The helpers are the ones
//! of `screensaver_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use serde_json::{Value, json};

use super::tests::{
    W, backup_file, code, enable, fail, fleet, home, install, ok, put_original, read,
    screensaver_file, summary, tick, written,
};
use super::*;

// ---- the commands: on, off, rebaseline ----

#[test]
fn screensaver_commands_need_the_lock_file_and_a_current_record() {
    let (_dir, home) = home();
    // No lock file: "install first".
    for run in [on, off, rebaseline] {
        assert_eq!(code(run(&home, W).unwrap()), fail());
    }
    // A lock file and no record.
    drop(lock::take(&home, Duration::ZERO, true).unwrap());
    for run in [on, off, rebaseline] {
        assert_eq!(code(run(&home, W).unwrap()), fail());
    }
    // A version 1 record is outdated.
    let mut record = Record::new();
    record.v = 1;
    install(&home, &record);
    for run in [on, off, rebaseline] {
        assert_eq!(code(run(&home, W).unwrap()), fail());
    }
    assert_eq!(record::load(&home).unwrap().unwrap(), record);
}

#[test]
fn screensaver_on_sets_the_switch_and_clears_restored() {
    let (_dir, home) = home();
    let mut record = Record::new();
    record.screensaver.restored = Some(Restored::Backup);
    install(&home, &record);
    assert_eq!(code(on(&home, W).unwrap()), ok());
    let record = record::load(&home).unwrap().unwrap();
    assert!(record.screensaver.enabled);
    assert_eq!(record.screensaver.restored, None);
}

#[test]
fn screensaver_off_puts_the_backup_back_and_keeps_the_backup_file() {
    let (dir, home) = home();
    written(&home, &dir, Some("mine\n"));
    assert_eq!(code(off(&home, W).unwrap()), ok());
    assert_eq!(read(screensaver_file(&dir)), "mine\n");
    assert_eq!(read(backup_file(&dir)), "mine\n", "the backup is kept");
    let record = record::load(&home).unwrap().unwrap();
    assert!(!record.screensaver.enabled);
    assert_eq!(record.screensaver.restored, Some(Restored::Backup));
    assert_eq!(record.screensaver.last_written_sha256, None);
    assert_eq!(record.screensaver.backup_sha256, None);
    assert!(!record.screensaver.had_original);
    assert!(!record.screensaver.first_write_refused);
}

#[test]
fn screensaver_off_without_an_original_removes_the_written_file() {
    let (dir, home) = home();
    written(&home, &dir, None);
    assert!(screensaver_file(&dir).exists());
    assert_eq!(code(off(&home, W).unwrap()), ok());
    assert!(!screensaver_file(&dir).exists());
    let record = record::load(&home).unwrap().unwrap();
    assert!(!record.screensaver.enabled);
    assert_eq!(record.screensaver.restored, Some(Restored::Removed));
}

#[test]
fn screensaver_off_keeps_a_file_that_differs_from_the_last_written_text() {
    let (dir, home) = home();
    written(&home, &dir, Some("mine\n"));
    std::fs::write(screensaver_file(&dir), "edited by the user\n").unwrap();
    assert_eq!(code(off(&home, W).unwrap()), ok());
    assert_eq!(read(screensaver_file(&dir)), "edited by the user\n");
    assert_eq!(read(backup_file(&dir)), "mine\n");
    let record = record::load(&home).unwrap().unwrap();
    assert!(!record.screensaver.enabled);
    assert_eq!(record.screensaver.restored, None);
    // The cache is cleared, so a later `on` goes through the first-write rules.
    assert_eq!(record.screensaver.last_written_sha256, None);
    assert!(!record.screensaver.had_original);
}

#[test]
fn screensaver_off_with_an_absent_file_clears_the_cache() {
    let (dir, home) = home();
    written(&home, &dir, None);
    std::fs::remove_file(screensaver_file(&dir)).unwrap();
    assert_eq!(code(off(&home, W).unwrap()), ok());
    let record = record::load(&home).unwrap().unwrap();
    assert!(!record.screensaver.enabled);
    assert_eq!(record.screensaver.last_written_sha256, None);
    assert_eq!(record.screensaver.restored, None);
}

#[test]
fn screensaver_off_with_the_switch_off_changes_nothing_else() {
    let (dir, home) = home();
    written(&home, &dir, Some("mine\n"));
    assert_eq!(code(off(&home, W).unwrap()), ok());
    // The user's own file now; a second `off` must not touch it.
    std::fs::write(screensaver_file(&dir), "other\n").unwrap();
    let before = record::load(&home).unwrap().unwrap();
    assert_eq!(code(off(&home, W).unwrap()), ok());
    assert_eq!(read(screensaver_file(&dir)), "other\n");
    assert_eq!(record::load(&home).unwrap().unwrap(), before);
}

#[test]
fn screensaver_on_after_off_writes_again_at_the_next_tick() {
    let (dir, home) = home();
    let s = summary(1, json!([]), Value::Null);
    put_original(&dir, "mine\n");
    enable(&home);
    assert_eq!(tick(&home, Some(&s)), ok());
    let first = read(screensaver_file(&dir));
    assert_eq!(code(off(&home, W).unwrap()), ok());
    assert_eq!(read(screensaver_file(&dir)), "mine\n");
    assert_eq!(code(on(&home, W).unwrap()), ok());
    // The same rendering as before: the cache was cleared, so it is written.
    assert_eq!(tick(&home, Some(&s)), ok());
    assert_eq!(read(screensaver_file(&dir)), first);
    assert_eq!(read(backup_file(&dir)), "mine\n");
    let record = record::load(&home).unwrap().unwrap();
    assert!(record.screensaver.enabled);
    assert!(record.screensaver.had_original);
}

#[test]
fn screensaver_a_failed_restore_keeps_the_record_and_a_second_off_restores() {
    let (dir, home) = home();
    let before = written(&home, &dir, Some("mine\n"));
    // A link in place of the file: the restore cannot read it.
    let outside = tempfile::Builder::new().prefix("out").tempdir().unwrap();
    std::fs::write(outside.path().join("victim"), "keep").unwrap();
    let ours = read(screensaver_file(&dir));
    std::fs::remove_file(screensaver_file(&dir)).unwrap();
    std::os::unix::fs::symlink(outside.path().join("victim"), screensaver_file(&dir)).unwrap();
    assert_eq!(code(off(&home, W).unwrap()), fail());
    assert_eq!(read(outside.path().join("victim")), "keep");
    assert_eq!(record::load(&home).unwrap().unwrap(), before);
    // The link goes; our text is back: the second `off` restores.
    std::fs::remove_file(screensaver_file(&dir)).unwrap();
    std::fs::write(screensaver_file(&dir), ours).unwrap();
    assert_eq!(code(off(&home, W).unwrap()), ok());
    assert_eq!(read(screensaver_file(&dir)), "mine\n");
    let record = record::load(&home).unwrap().unwrap();
    assert!(!record.screensaver.enabled);
    assert_eq!(record.screensaver.restored, Some(Restored::Backup));
}

#[test]
fn screensaver_restore_changes_no_field_on_an_error() {
    let (dir, home) = home();
    let mut record = written(&home, &dir, Some("mine\n"));
    let outside = tempfile::Builder::new().prefix("out").tempdir().unwrap();
    std::fs::write(outside.path().join("victim"), "keep").unwrap();
    std::fs::remove_file(screensaver_file(&dir)).unwrap();
    std::os::unix::fs::symlink(outside.path().join("victim"), screensaver_file(&dir)).unwrap();
    let before = record.clone();
    assert!(restore(&home, &mut record).is_err());
    assert_eq!(record, before);
    assert_eq!(read(outside.path().join("victim")), "keep");
}

#[test]
fn screensaver_restore_does_nothing_when_no_text_was_written() {
    let (dir, home) = home();
    put_original(&dir, "mine\n");
    let mut record = enable(&home);
    let before = record.clone();
    restore(&home, &mut record).unwrap();
    assert_eq!(record, before);
    assert_eq!(read(screensaver_file(&dir)), "mine\n");
}

#[test]
fn screensaver_restore_with_a_missing_backup_is_an_error_and_changes_no_field() {
    let (dir, home) = home();
    let mut record = written(&home, &dir, Some("mine\n"));
    let ours = read(screensaver_file(&dir));
    std::fs::remove_file(backup_file(&dir)).unwrap();
    let before = record.clone();
    let e = restore(&home, &mut record).unwrap_err();
    assert!(e.to_string().contains("the backup is missing"), "{e}");
    assert_eq!(read(screensaver_file(&dir)), ours);
    assert_eq!(record, before);
    // The backup comes back: the same record restores.
    std::fs::write(backup_file(&dir), "mine\n").unwrap();
    restore(&home, &mut record).unwrap();
    assert_eq!(read(screensaver_file(&dir)), "mine\n");
    assert_eq!(record.screensaver.restored, Some(Restored::Backup));
    assert_eq!(record.screensaver.last_written_sha256, None);
}

#[test]
fn screensaver_off_with_a_missing_backup_fails_and_a_second_off_restores() {
    let (dir, home) = home();
    let record = written(&home, &dir, Some("mine\n"));
    let ours = read(screensaver_file(&dir));
    std::fs::remove_file(backup_file(&dir)).unwrap();
    assert_eq!(code(off(&home, W).unwrap()), fail());
    assert_eq!(record::load(&home).unwrap().unwrap(), record);
    assert_eq!(read(screensaver_file(&dir)), ours);
    std::fs::write(backup_file(&dir), "mine\n").unwrap();
    assert_eq!(code(off(&home, W).unwrap()), ok());
    assert_eq!(read(screensaver_file(&dir)), "mine\n");
    let record = record::load(&home).unwrap().unwrap();
    assert!(!record.screensaver.enabled);
    assert_eq!(record.screensaver.restored, Some(Restored::Backup));
}

#[test]
fn screensaver_restore_keeps_the_previous_restored_value_when_the_file_is_kept() {
    let (dir, home) = home();
    let mut record = written(&home, &dir, Some("mine\n"));
    record.screensaver.restored = Some(Restored::Removed);
    std::fs::write(screensaver_file(&dir), "edited\n").unwrap();
    restore(&home, &mut record).unwrap();
    assert_eq!(record.screensaver.restored, Some(Restored::Removed));
    assert_eq!(read(backup_file(&dir)), "mine\n");
}

#[test]
fn screensaver_commands_with_the_lock_held_by_another_change_do_nothing() {
    let (dir, home) = home();
    let mut record = written(&home, &dir, Some("mine\n"));
    record.screensaver.enabled = false;
    record::save(&home, &record).unwrap();
    let ours = read(screensaver_file(&dir));
    let other = Home::open(dir.path()).unwrap();
    let guard = lock::take(&other, Duration::ZERO, false).unwrap();
    assert!(matches!(guard, Take::Held(_)));
    let wait = Duration::from_millis(150);
    for run in [on, off, rebaseline] {
        let started = std::time::Instant::now();
        assert_eq!(code(run(&home, wait).unwrap()), fail());
        assert!(started.elapsed() >= wait);
    }
    assert_eq!(record::load(&home).unwrap().unwrap(), record);
    assert_eq!(read(screensaver_file(&dir)), ours);
    assert_eq!(read(backup_file(&dir)), "mine\n");
    drop(guard);
    assert_eq!(code(on(&home, W).unwrap()), ok());
}

#[test]
fn screensaver_rebaseline_renames_the_backup_and_the_next_tick_takes_a_new_one() {
    let (dir, home) = home();
    written(&home, &dir, Some("first original\n"));
    // The user restores a new screensaver of their own.
    assert_eq!(code(off(&home, W).unwrap()), ok());
    std::fs::write(screensaver_file(&dir), "second original\n").unwrap();
    assert_eq!(code(on(&home, W).unwrap()), ok());
    assert_eq!(code(tick_with(&home, None, &fleet()).unwrap()), ok());
    // The kept backup differs from the current file: refused.
    assert!(
        record::load(&home)
            .unwrap()
            .unwrap()
            .screensaver
            .first_write_refused
    );
    assert_eq!(code(rebaseline(&home, W).unwrap()), ok());
    assert!(!backup_file(&dir).exists());
    let kept: Vec<String> = std::fs::read_dir(backup_file(&dir).parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("screensaver.txt.fleetdeck-backup."))
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    let suffix = kept[0].trim_start_matches("screensaver.txt.fleetdeck-backup.");
    assert!(suffix.parse::<u64>().is_ok(), "{suffix}");
    assert_eq!(
        read(backup_file(&dir).with_file_name(&kept[0])),
        "first original\n"
    );
    let record = record::load(&home).unwrap().unwrap();
    assert!(record.screensaver.enabled, "enabled is kept");
    assert!(!record.screensaver.first_write_refused);
    assert_eq!(record.screensaver.last_written_sha256, None);
    assert_eq!(record.screensaver.backup_sha256, None);
    assert!(!record.screensaver.had_original);
    // The next tick takes a new backup of the current file.
    assert_eq!(code(tick_with(&home, None, &fleet()).unwrap()), ok());
    assert_eq!(read(backup_file(&dir)), "second original\n");
    assert!(read(screensaver_file(&dir)).starts_with("Fleet Deck"));
    assert_eq!(
        read(backup_file(&dir).with_file_name(&kept[0])),
        "first original\n",
        "the renamed file is never deleted"
    );
}

#[test]
fn screensaver_rebaseline_without_a_backup_only_clears_the_cache() {
    let (dir, home) = home();
    written(&home, &dir, None);
    assert_eq!(code(rebaseline(&home, W).unwrap()), ok());
    let record = record::load(&home).unwrap().unwrap();
    assert!(record.screensaver.enabled);
    assert_eq!(record.screensaver.last_written_sha256, None);
    assert!(screensaver_file(&dir).exists(), "the file stays");
}

#[test]
fn screensaver_rename_backup_adds_a_counter_when_the_name_exists_and_never_overwrites() {
    let (dir, home) = home();
    put_original(&dir, "x\n");
    let branding = backup_file(&dir).parent().unwrap().to_owned();
    std::fs::write(backup_file(&dir), "third\n").unwrap();
    std::fs::write(
        branding.join("screensaver.txt.fleetdeck-backup.1700000000"),
        "one\n",
    )
    .unwrap();
    std::fs::write(
        branding.join("screensaver.txt.fleetdeck-backup.1700000000-1"),
        "two\n",
    )
    .unwrap();
    let to = rename_backup(&home, 1_700_000_000).unwrap().unwrap();
    assert_eq!(
        to,
        branding.join("screensaver.txt.fleetdeck-backup.1700000000-2")
    );
    assert_eq!(read(to), "third\n");
    assert_eq!(
        read(branding.join("screensaver.txt.fleetdeck-backup.1700000000")),
        "one\n"
    );
    assert_eq!(
        read(branding.join("screensaver.txt.fleetdeck-backup.1700000000-1")),
        "two\n"
    );
    assert!(!backup_file(&dir).exists());
    // No backup: nothing to do.
    assert!(rename_backup(&home, 1_700_000_000).unwrap().is_none());
}
