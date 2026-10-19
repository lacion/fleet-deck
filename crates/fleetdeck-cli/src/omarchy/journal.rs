//! The write-ahead journal of an installer repair (`board-gaps.md` §3.5.1,
//! revision 3): `<config home>/fleetdeck/omarchy-repair/<repair_id>.json`,
//! mode 0600 in a directory of mode 0700, synced. The journal records the
//! plan before any file operation and the intent and the completion of each
//! step. A completion record is a hint, never proof: the files decide.
//! Journals are never deleted.

use std::path::{Path, PathBuf};

use fleetdeck_proto::omarchy_repair::OmarchyAssetId;
use serde::{Deserialize, Serialize};

use super::assets::{Asset, asset_for};
use super::fsx::{FsError, Home, random_bytes_hex};
use super::record::{rel, sha256_hex};

/// The folder of the journals, relative to the config home.
pub const DIR: &str = "fleetdeck/omarchy-repair";
/// The most files that one journal holds.
pub const MAX_FILES: usize = 64;
/// The format version of the journal.
const VERSION: u32 = 1;

/// One step of a file in a repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    BackingUp,
    BackedUp,
    Staging,
    Staged,
    Publishing,
    Published,
    Restoring,
    Restored,
}

/// One file of a repair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalFile {
    pub asset_id: OmarchyAssetId,
    /// The target path, relative to the config home.
    pub target: String,
    /// The hash of the target content that the plan approved; `null` for a
    /// target that did not exist.
    pub original_hash: Option<String>,
    pub asset_hash: String,
    /// The name of the kept copy, in the directory of the target.
    pub backup_name: String,
    /// The name of the staging file, in the directory of the target.
    pub staging_name: String,
    /// The step that began and has no completion record yet.
    pub intent: Option<Step>,
    /// The steps that ended.
    pub completed: Vec<Step>,
}

/// The journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    pub v: u32,
    pub repair_id: String,
    pub build_hash: String,
    /// Unix milliseconds.
    pub created_at: i64,
    pub files: Vec<JournalFile>,
}

/// Why a journal could not be loaded.
#[derive(Debug)]
pub enum LoadError {
    Fs(FsError),
    /// The file is not a journal that this build understands.
    Invalid,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fs(e) => write!(f, "{e}"),
            Self::Invalid => f.write_str("the repair journal is not valid."),
        }
    }
}

impl std::error::Error for LoadError {}

/// True for 32 lower-case hexadecimal characters.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A new repair ID: 128 random bits as 32 lower-case hex characters.
///
/// # Errors
/// The system has no random bytes.
pub fn new_id() -> std::io::Result<String> {
    random_bytes_hex(16)
}

/// The path of a journal, relative to the config home.
#[must_use]
pub fn path_of(id: &str) -> PathBuf {
    Path::new(DIR).join(format!("{id}.json"))
}

/// The repair ID in a journal file name; `None` for any other name.
#[must_use]
pub fn id_of_name(name: &str) -> Option<&str> {
    let id = name.strip_suffix(".json")?;
    valid_id(id).then_some(id)
}

/// The leaf of a path, as text.
fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The relative path of a file of the target's directory.
fn sibling(target: &str, name: &str) -> PathBuf {
    Path::new(target).with_file_name(name)
}

impl JournalFile {
    /// The asset of this file.
    #[must_use]
    pub fn asset(&self) -> Option<&'static Asset> {
        asset_for(self.asset_id)
    }

    /// The target path, relative to the config home.
    #[must_use]
    pub fn target_rel(&self) -> PathBuf {
        PathBuf::from(&self.target)
    }

    /// The kept copy, relative to the config home.
    #[must_use]
    pub fn backup_rel(&self) -> PathBuf {
        sibling(&self.target, &self.backup_name)
    }

    /// The staging file, relative to the config home.
    #[must_use]
    pub fn staging_rel(&self) -> PathBuf {
        sibling(&self.target, &self.staging_name)
    }
}

/// A new file entry for `asset` with its approved hash. The backup name has
/// the seconds and 64 random bits; the staging name has 64 random bits.
///
/// # Errors
/// The system has no random bytes.
pub fn new_file(
    asset: &Asset,
    id: OmarchyAssetId,
    original_hash: Option<String>,
    secs: i64,
) -> std::io::Result<JournalFile> {
    let target = rel(asset.base, asset.path);
    let target = target.to_string_lossy().into_owned();
    let name = leaf(&target).to_owned();
    Ok(JournalFile {
        asset_id: id,
        original_hash,
        asset_hash: sha256_hex(asset.text.as_bytes()),
        backup_name: format!("{name}.fleetdeck-conflict-{secs}-{}", random_bytes_hex(8)?),
        staging_name: format!(".{name}.fleetdeck-stage-{}", random_bytes_hex(8)?),
        target,
        intent: None,
        completed: Vec::new(),
    })
}

fn valid_file(file: &JournalFile) -> bool {
    let Some(asset) = file.asset() else {
        return false;
    };
    let expected = rel(asset.base, asset.path).to_string_lossy().into_owned();
    let name = leaf(&expected).to_owned();
    let single = |s: &str| !s.is_empty() && !s.contains(['/', '\0']);
    file.target == expected
        && single(&file.backup_name)
        && single(&file.staging_name)
        && file
            .backup_name
            .starts_with(&format!("{name}.fleetdeck-conflict-"))
        && file
            .staging_name
            .starts_with(&format!(".{name}.fleetdeck-stage-"))
        && valid_hash(&file.asset_hash)
        && file.original_hash.as_deref().is_none_or(valid_hash)
}

fn valid(journal: &Journal, id: &str) -> bool {
    journal.v == VERSION
        && journal.repair_id == id
        && valid_hash(&journal.build_hash)
        && journal.files.len() <= MAX_FILES
        && journal.files.iter().all(valid_file)
        && journal.files.iter().enumerate().all(|(i, f)| {
            journal.files[..i]
                .iter()
                .all(|other| other.asset_id != f.asset_id)
        })
}

/// Reads and checks the journal `id` (the caller checked [`valid_id`]).
/// `Ok(None)` when there is none.
///
/// # Errors
/// A link or a foreign owner on the path, an unreadable file, or content
/// that is not a journal of this build's format.
pub fn load(home: &Home, id: &str) -> Result<Option<Journal>, LoadError> {
    let Some(data) = home.read(&path_of(id)).map_err(LoadError::Fs)? else {
        return Ok(None);
    };
    match serde_json::from_slice::<Journal>(&data.bytes) {
        Ok(journal) if valid(&journal, id) => Ok(Some(journal)),
        _ => Err(LoadError::Invalid),
    }
}

/// Makes the folder of the journals (0700).
///
/// # Errors
/// The folder cannot be made safely.
pub fn make_dir(home: &Home) -> Result<(), FsError> {
    home.private_dir(Path::new(DIR))
}

/// Writes the journal atomically (mode 0600, synced).
///
/// # Errors
/// The writer failed.
pub fn save(home: &Home, journal: &Journal) -> Result<(), FsError> {
    let mut text = serde_json::to_vec_pretty(journal).map_err(std::io::Error::other)?;
    text.push(b'\n');
    home.write(&path_of(&journal.repair_id), &text, 0o600)
}

/// A new journal; `files` is in the order of the install.
#[must_use]
pub fn new_journal(
    repair_id: String,
    build_hash: String,
    created_at: i64,
    files: Vec<JournalFile>,
) -> Journal {
    Journal {
        v: VERSION,
        repair_id,
        build_hash,
        created_at,
        files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_32_lower_case_hex_characters() {
        assert!(valid_id(&"a".repeat(32)));
        assert!(valid_id("0123456789abcdef0123456789abcdef"));
        assert!(!valid_id(&"a".repeat(31)));
        assert!(!valid_id(&"A".repeat(32)));
        assert!(!valid_id(&"g".repeat(32)));
        assert!(!valid_id("../../etc/passwd"));
        assert_eq!(new_id().unwrap().len(), 32);
        assert!(valid_id(&new_id().unwrap()));
    }

    #[test]
    fn a_journal_name_gives_its_id() {
        let name = format!("{}.json", "b".repeat(32));
        assert_eq!(id_of_name(&name), Some("b".repeat(32).as_str()));
        assert_eq!(id_of_name("note.json"), None);
        assert_eq!(id_of_name(&"b".repeat(32)), None);
    }
}
