//! `fleetdeck omarchy uninstall` (R13 P4 task C, `omarchy-p4.md` §3.4).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use fleetdeck_core::paths::NodeDirs;

use super::assets::{Base, HYPR_PATH};
use super::fsx::Home;
use super::lock::{self, Take};
use super::record::{self, LoadError, Record, rel, sha256_hex};
use super::{TOOL_LIMIT, hyprland, omarchy_env, open_home, screensaver, set_summary, tool};
use fleetdeck_proto::omarchy_admin::LOCK_BUSY_EXIT;

const DISABLE: &str = "omarchy plugin disable fleetdeck.fleet";

fn warn(text: &str) {
    eprintln!("fleetdeck: warning: {text}");
}

/// Removes the empty directories above a removed file, from the bottom. The
/// folders `plugins`, `hooks`, `themed` and `extensions` of Omarchy, and the
/// two base folders, stay.
fn remove_empty_parents(home: &Home, base: Base, path: &str) {
    let keep_depth = match base {
        Base::Omarchy => 2,
        Base::Fleetdeck => 1,
    };
    let mut dir: PathBuf = Path::new(path)
        .parent()
        .map(Path::to_owned)
        .unwrap_or_default();
    while dir.components().count() >= keep_depth {
        if !home.remove_empty_dir(&rel(base, &dir.to_string_lossy())) {
            break;
        }
        if !dir.pop() {
            break;
        }
    }
}

/// Removes the recorded files that are as recorded. A file that changed
/// stays and is named. Returns the entries that stay.
fn remove_files(home: &Home, record: &Record) -> Vec<record::FileEntry> {
    let mut kept = Vec::new();
    for entry in &record.files {
        let path = rel(entry.base, &entry.path);
        let shown = home.abs(&path).display().to_string();
        match home.read(&path) {
            Ok(None) => {}
            Ok(Some(data)) if sha256_hex(&data.bytes) == entry.sha256 => match home.remove(&path) {
                Ok(_) => remove_empty_parents(home, entry.base, &entry.path),
                Err(e) => {
                    warn(&format!("{shown} was not removed: {e}"));
                    kept.push(entry.clone());
                }
            },
            Ok(Some(_)) => {
                println!("kept (changed): {shown}");
                kept.push(entry.clone());
            }
            Err(e) => {
                warn(&format!("{shown} was kept: {e}"));
                kept.push(entry.clone());
            }
        }
    }
    kept
}

/// `fleetdeck omarchy uninstall`.
///
/// # Errors
/// The config home cannot be opened, or the record is not valid, or the
/// record cannot be written or removed. Exit 1 (no error) when
/// `fleet_summary` could not be turned off or the screensaver could not be
/// restored: the record stays. Exit 1 also for a version 1 record, and when
/// another Omarchy change runs.
pub async fn run(dirs: &NodeDirs) -> anyhow::Result<ExitCode> {
    let home = open_home()?;
    let nothing = || {
        println!("Nothing to remove: no install record was found.");
        Ok(ExitCode::SUCCESS)
    };
    let _guard = match lock::take(&home, lock::WAIT, false)? {
        Take::Held(guard) => guard,
        Take::Busy => {
            eprintln!("{}", lock::BUSY);
            return Ok(ExitCode::from(LOCK_BUSY_EXIT));
        }
        Take::NoLockFile => return nothing(),
    };
    let mut record = match record::load_current(&home) {
        Ok(Some(record)) => record,
        Ok(None) => return nothing(),
        Err(LoadError::Outdated) => {
            eprintln!("{}", record::OUTDATED);
            return Ok(ExitCode::from(1));
        }
        Err(e) => return Err(e.into()),
    };
    let env = omarchy_env();

    // The managed line first, so that Hyprland never loads a file that is gone.
    let hypr_abs = home.abs(&rel(Base::Fleetdeck, HYPR_PATH));
    if let Some(line) = hyprland::line_for(&hypr_abs) {
        let shown = home.abs(Path::new(hyprland::FILE)).display().to_string();
        match hyprland::uninstall(&home, &line, &record.hyprland_line) {
            hyprland::Removal::Nothing => {}
            hyprland::Removal::Removed => {
                println!("Hyprland: removed the managed line from {shown}.")
            }
            hyprland::Removal::KeptOthers => println!("kept your other changes: {shown}"),
            hyprland::Removal::Failed(why) => warn(&format!(
                "the managed line was not removed from {shown}: {why}"
            )),
        }
    }

    let kept = remove_files(&home, &record);

    let disabled = matches!(
        tool(&env, "omarchy", &["plugin", "disable", "fleetdeck.fleet"], TOOL_LIMIT).await,
        Ok(out) if out.code == 0
    );
    if !disabled {
        warn(&format!("the plugin was not disabled; run: {DISABLE}"));
    }

    if record.fleet_summary_set_by_install {
        match set_summary(dirs, false).await {
            Ok(_) => record.fleet_summary_set_by_install = false,
            Err(_) => warn(
                "the station is not running or did not accept the change, so fleet_summary stays on; run uninstall again when the station runs.",
            ),
        }
    }
    // The setting that install turned on is still on: the record stays.
    let pending = record.fleet_summary_set_by_install;

    // A failed restore counts like a pending revocation: the record stays,
    // with the screensaver part unchanged (the restore printed the error).
    let restore_failed = screensaver::restore(&home, &mut record).is_err();
    if !restore_failed {
        record.screensaver.enabled = false;
        screensaver::report_backup(&home);
    }

    record.files = kept;
    record.hyprland_line = record::HyprlandLine::default();
    record.plugin_enabled = !disabled && record.plugin_enabled;
    record.menu_file_written = record
        .entry(Base::Omarchy, super::assets::MENU_PATH)
        .is_some();
    if record.files.is_empty() && !pending && !restore_failed {
        home.remove(Path::new(record::RECORD_PATH))?;
        println!("Fleet Deck is removed from Omarchy.");
    } else {
        record::save(&home, &record)?;
        if !record.files.is_empty() {
            println!("Some files stay; the record stays with them.");
        }
    }
    // A pending revocation or a failed restore is an incomplete teardown.
    Ok(if pending || restore_failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}
