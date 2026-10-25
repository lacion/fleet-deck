//! `fleetdeck omarchy status [--json]` (R13 P4 task C, `omarchy-p4.md`
//! §3.4). Exit 0 only when a record exists and no file is `changed` or
//! `missing`; P8 uses the exit code.

use std::path::PathBuf;
use std::process::ExitCode;

use fleetdeck_core::paths::NodeDirs;
use serde_json::{Value, json};

use super::assets::{Base, FILES, HYPR_PATH};
use super::fsx::Home;
use super::record::{self, Record, Restored, rel, sha256_hex};
use super::screensaver;
use super::{STATUS_STATION_LIMIT, hyprland, open_home, summary_value};

/// The state of one recorded file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ok,
    Changed,
    Missing,
}

impl State {
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Changed => "changed",
            Self::Missing => "missing",
        }
    }
}

/// The state of every recorded file: its absolute path and its state. A
/// path that is a link or not yours counts as changed.
#[must_use]
pub fn file_states(home: &Home, record: &Record) -> Vec<(PathBuf, State)> {
    record
        .files
        .iter()
        .map(|entry| {
            let rel = rel(entry.base, &entry.path);
            let state = match home.read(&rel) {
                Ok(None) => State::Missing,
                Ok(Some(data)) if sha256_hex(&data.bytes) == entry.sha256 => State::Ok,
                Ok(Some(_)) | Err(_) => State::Changed,
            };
            (home.abs(&rel), state)
        })
        .collect()
}

/// Whether the managed line is in `hyprland.lua` now.
#[must_use]
pub fn line_present(home: &Home) -> bool {
    hyprland::line_for(&home.abs(&rel(Base::Fleetdeck, HYPR_PATH)))
        .is_some_and(|line| hyprland::present(home, &line))
}

/// The plugin folder, relative to the Omarchy base.
const PLUGIN_DIR: &str = "plugins/fleetdeck.fleet/";
/// A conversion is recent for this long (two cycles of an old job).
const RECENT_MS: i64 = 120_000;

/// Whether the integration needs `omarchy install` (or a short wait): the
/// record is version 1, or a recorded file of the plugin folder differs
/// from this build's asset of the same path, or the record was converted
/// less than 120 s ago. A file that is absent or unreadable does not count.
#[must_use]
pub fn outdated(home: &Home, record: &Record, now_ms: i64) -> bool {
    if record.v == 1 {
        return true;
    }
    if record
        .converted_at
        .is_some_and(|at| now_ms.saturating_sub(at) < RECENT_MS)
    {
        return true;
    }
    record
        .files
        .iter()
        .filter(|e| e.base == Base::Omarchy && e.path.starts_with(PLUGIN_DIR))
        .any(|entry| {
            let Some(asset) = FILES
                .iter()
                .find(|a| a.base == entry.base && a.path == entry.path)
            else {
                return false;
            };
            matches!(
                home.read(&rel(entry.base, &entry.path)),
                Ok(Some(data)) if data.bytes != asset.text.as_bytes()
            )
        })
}

/// `file_restored`: `None` while the screensaver is on or when no restore
/// is recorded.
fn file_restored(home: &Home, record: &Record) -> Option<bool> {
    if record.screensaver.enabled {
        return None;
    }
    let file = home.read(&rel(Base::Omarchy, screensaver::FILE));
    match record.screensaver.restored? {
        Restored::Removed => Some(matches!(file, Ok(None))),
        Restored::Backup => {
            let backup = home.read(&rel(Base::Omarchy, screensaver::BACKUP));
            Some(match (file, backup) {
                (Ok(Some(file)), Ok(Some(backup))) => file.bytes == backup.bytes,
                _ => false,
            })
        }
    }
}

/// The `blocked` word of the screensaver, if any.
fn blocked(record: &Record) -> Option<&'static str> {
    record
        .screensaver
        .first_write_refused
        .then_some("kept_original_differs")
}

fn summary_word(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "on",
        Some(false) => "off",
        None => "unknown",
    }
}

/// `fleetdeck omarchy status`.
///
/// # Errors
/// The config home cannot be opened or the record is not valid.
pub async fn run(dirs: &NodeDirs, json: bool) -> anyhow::Result<ExitCode> {
    let home = open_home()?;
    let record = record::load(&home)?;
    let summary = summary_word(summary_value(dirs, STATUS_STATION_LIMIT).await);
    let line = if line_present(&home) { "ok" } else { "missing" };
    let states = record
        .as_ref()
        .map(|r| file_states(&home, r))
        .unwrap_or_default();
    let clean = record.is_some() && states.iter().all(|(_, s)| *s == State::Ok);
    let plugin_enabled = record.as_ref().is_some_and(|r| r.plugin_enabled);
    let is_outdated = record
        .as_ref()
        .is_some_and(|r| outdated(&home, r, record::now_ms()));
    let restored = record.as_ref().and_then(|r| file_restored(&home, r));
    let blocked = record.as_ref().and_then(blocked);
    if json {
        let files: Vec<_> = states
            .iter()
            .map(|(path, state)| json!({"path": path.display().to_string(), "state": state.word()}))
            .collect();
        println!(
            "{}",
            json!({
                "installed": record.is_some(),
                "files": files,
                "plugin_enabled": plugin_enabled,
                "summary": summary,
                "hyprland_line": line,
                "record_version": record.as_ref().map(|r| r.v),
                "outdated": is_outdated,
                "screensaver": record.as_ref().map_or(Value::Null, |r| json!({
                    "enabled": r.screensaver.enabled,
                    "file_restored": restored,
                    "blocked": blocked,
                })),
            })
        );
    } else if record.is_none() {
        println!("not installed");
    } else {
        for (path, state) in &states {
            println!("{} {}", state.word(), path.display());
        }
        println!(
            "plugin: {}",
            if plugin_enabled {
                "enabled"
            } else {
                "not enabled"
            }
        );
        println!("summary: {summary}");
        println!("hyprland: line {line}");
        if let Some(r) = &record {
            println!("record: version {}", r.v);
            println!("outdated: {}", if is_outdated { "yes" } else { "no" });
            println!(
                "screensaver: {}",
                if r.screensaver.enabled { "on" } else { "off" }
            );
            if let Some(restored) = restored {
                println!(
                    "screensaver file: {}",
                    if restored { "restored" } else { "not restored" }
                );
            }
            if let Some(word) = blocked {
                println!("screensaver blocked: {word}");
            }
        }
    }
    Ok(if clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// The count of changed or missing files, for the doctor.
#[must_use]
pub fn drift(home: &Home, record: &Record) -> usize {
    file_states(home, record)
        .iter()
        .filter(|(_, s)| *s != State::Ok)
        .count()
}
