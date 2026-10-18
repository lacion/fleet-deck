//! `fleetdeck omarchy status [--json]` (R13 P4 task C, `omarchy-p4.md`
//! §3.4). Exit 0 only when a record exists and no file is `changed` or
//! `missing`; P8 uses the exit code.

use std::path::PathBuf;
use std::process::ExitCode;

use fleetdeck_core::paths::NodeDirs;
use serde_json::json;

use super::assets::{Base, HYPR_PATH};
use super::fsx::Home;
use super::record::{self, Record, rel, sha256_hex};
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
