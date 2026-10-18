//! The install record `<fd>/omarchy-install.json` (R13 P4 task C,
//! `omarchy-p4.md` §3.4, decisions P4-7 and P4-13). Mode 0600, rewritten
//! atomically after each file.

use std::path::{Path, PathBuf};

use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};

use super::assets::Base;
use super::fsx::{FsError, Home};

/// The record path, relative to the config home.
pub const RECORD_PATH: &str = "fleetdeck/omarchy-install.json";

/// One written file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative to `base`.
    pub path: String,
    pub base: Base,
    pub sha256: String,
    pub existed_before: bool,
}

/// The screensaver part (the job is task G).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screensaver {
    pub enabled: bool,
    pub had_original: bool,
    pub backup_sha256: Option<String>,
    pub last_written_sha256: Option<String>,
}

/// The managed line of `hyprland.lua` (decision P4-7).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HyprlandLine {
    pub added: bool,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    /// True when install wrote a `\n` before the line, because the file did
    /// not end with one (decision P4-13).
    #[serde(default)]
    pub newline_added: bool,
}

/// The record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub v: u32,
    pub fleetdeck_version: String,
    pub files: Vec<FileEntry>,
    pub plugin_enabled: bool,
    pub fleet_summary_set_by_install: bool,
    pub menu_file_written: bool,
    pub screensaver: Screensaver,
    #[serde(default)]
    pub hyprland_line: HyprlandLine,
}

impl Record {
    #[must_use]
    pub fn new() -> Self {
        Self {
            v: 1,
            fleetdeck_version: fleetdeck_core::build::version().to_owned(),
            files: Vec::new(),
            plugin_enabled: false,
            fleet_summary_set_by_install: false,
            menu_file_written: false,
            screensaver: Screensaver::default(),
            hyprland_line: HyprlandLine::default(),
        }
    }

    /// The entry of a file.
    #[must_use]
    pub fn entry(&self, base: Base, path: &str) -> Option<&FileEntry> {
        self.files.iter().find(|f| f.base == base && f.path == path)
    }

    /// Adds or replaces an entry.
    pub fn upsert(&mut self, entry: FileEntry) {
        match self
            .files
            .iter_mut()
            .find(|f| f.base == entry.base && f.path == entry.path)
        {
            Some(slot) => *slot = entry,
            None => self.files.push(entry),
        }
    }
}

impl Default for Record {
    fn default() -> Self {
        Self::new()
    }
}

/// Lower-case hex SHA-256.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .fold(String::with_capacity(64), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// The path of a file relative to the config home.
#[must_use]
pub fn rel(base: Base, path: &str) -> PathBuf {
    Path::new(base.dir()).join(path)
}

/// Why the record could not be loaded.
#[derive(Debug)]
pub enum LoadError {
    Fs(FsError),
    /// The file is not a record that this build understands.
    Invalid(PathBuf),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fs(e) => write!(f, "{e}"),
            Self::Invalid(path) => {
                write!(
                    f,
                    "{} is not a valid install record; change nothing.",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// Reads the record. `Ok(None)` when there is none.
///
/// # Errors
/// A link or a foreign owner, an unreadable file, or invalid content.
pub fn load(home: &Home) -> Result<Option<Record>, LoadError> {
    let path = Path::new(RECORD_PATH);
    let Some(data) = home.read(path).map_err(LoadError::Fs)? else {
        return Ok(None);
    };
    match serde_json::from_slice::<Record>(&data.bytes) {
        Ok(record) if record.v == 1 => Ok(Some(record)),
        _ => Err(LoadError::Invalid(home.abs(path))),
    }
}

/// Writes the record atomically, mode 0600.
///
/// # Errors
/// The writer failed.
pub fn save(home: &Home, record: &Record) -> Result<(), FsError> {
    let mut text = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    text.push(b'\n');
    home.write(Path::new(RECORD_PATH), &text, 0o600)
}
