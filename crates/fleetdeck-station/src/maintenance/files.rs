//! The files of a maintenance run (`board-gaps.md` §2.9): the private
//! folder, the spec (the durable admission record), the state and the
//! output. Every file is 0600 in a 0700 folder that the effective user owns.
//! A read never follows a link and checks the owner, the type, the mode and
//! the size of the opened file.

use std::fmt;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};

use fleetdeck_proto::desktop_tools::{PostCheck, ResolvedPackage, is_hex_id, size_ok};
use rustix::fs::{CWD, FileType, Mode, OFlags, RenameFlags, fstat, renameat_with};
use serde::{Deserialize, Serialize};

use crate::desktop_tools::mapping::{by_id, target};

/// The most bytes of a spec file.
pub const SPEC_MAX: u64 = 16 * 1024;
/// The most bytes of a state file.
pub const STATE_MAX: u64 = 4 * 1024;
/// The most bytes of an output file.
pub const OUTPUT_MAX: u64 = 256 * 1024;

/// The version of the spec and of the state.
const VERSION: u32 = 1;

fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn denied(text: String) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, text)
}

/// Checks that `dir` is a directory (not a link) that the effective user
/// owns, with no group or other permission.
///
/// # Errors
/// `PermissionDenied` for a failed check; any `lstat` error.
pub fn check_dir(dir: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(denied(format!("{} is not a directory", dir.display())));
    }
    if meta.uid() != euid() {
        return Err(denied(format!("{} belongs to another user", dir.display())));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(denied(format!(
            "{} is open to other users (mode {:o})",
            dir.display(),
            meta.mode() & 0o777
        )));
    }
    Ok(())
}

/// Creates `dir` with mode 0700 when it is missing (its parent must exist),
/// then runs [`check_dir`].
///
/// # Errors
/// Any error of the creation or of the check.
pub fn ensure_dir(dir: &Path) -> io::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    check_dir(dir)
}

/// Why a spec, a state or an output file was refused. The text names no
/// path and no secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecError {
    /// The maintenance ID is not 32 lowercase hex characters.
    Id,
    /// The folder failed its check.
    Folder,
    /// The file does not exist.
    Missing,
    /// The file is a link, not regular, not owned by this user or not 0600.
    Unsafe,
    /// The file is larger than its limit.
    TooLarge,
    /// The file could not be read.
    Io,
    /// The file is not valid JSON of the expected shape.
    Parse,
    /// A field has a value that is not allowed.
    Invalid(&'static str),
}

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id => f.write_str("the maintenance id is not valid"),
            Self::Folder => f.write_str("the maintenance folder is not private"),
            Self::Missing => f.write_str("the file does not exist"),
            Self::Unsafe => f.write_str("the file is not a private regular file"),
            Self::TooLarge => f.write_str("the file is too large"),
            Self::Io => f.write_str("the file cannot be read"),
            Self::Parse => f.write_str("the file is not valid"),
            Self::Invalid(field) => write!(f, "the field {field} is not valid"),
        }
    }
}

impl std::error::Error for SpecError {}

impl From<SpecError> for io::Error {
    fn from(e: SpecError) -> Self {
        let kind = match e {
            SpecError::Missing => io::ErrorKind::NotFound,
            SpecError::Id => io::ErrorKind::InvalidInput,
            SpecError::Folder | SpecError::Unsafe => io::ErrorKind::PermissionDenied,
            SpecError::TooLarge | SpecError::Parse | SpecError::Invalid(_) => {
                io::ErrorKind::InvalidData
            }
            SpecError::Io => io::ErrorKind::Other,
        };
        Self::new(kind, e.to_string())
    }
}

/// The spec of a run: written once by the station, read once by the runner.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    /// The version: 1.
    pub v: u32,
    pub maintenance_id: String,
    pub request_id: String,
    /// The fingerprint of the deck that asked.
    pub deck: String,
    pub plan_id: String,
    /// The tool IDs to install.
    pub tools: Vec<String>,
    /// The expected transaction.
    pub transaction: Vec<ResolvedPackage>,
    /// SHA-256 of the pacman configuration (64 lowercase hex characters).
    pub conf_sha256: String,
    pub cols: u16,
    pub rows: u16,
    /// The token that the client sends first.
    pub station_token: String,
    /// The token that the runner answers with.
    pub runner_token: String,
    /// Unix milliseconds.
    pub created_at: i64,
}

impl fmt::Debug for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The tokens never reach a log.
        f.debug_struct("Spec")
            .field("maintenance_id", &self.maintenance_id)
            .field("request_id", &self.request_id)
            .field("plan_id", &self.plan_id)
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

/// The package targets (`<repo>/<name>`) of the tools of `spec`, in the
/// order of the spec.
#[must_use]
pub fn targets(spec: &Spec) -> Vec<String> {
    spec.tools
        .iter()
        .filter_map(|id| by_id(id).and_then(target))
        .collect()
}

/// The kind of a recorded state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStateKind {
    Running,
    Exited,
    Lost,
}

/// The state file of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunState {
    /// The version: 1.
    pub v: u32,
    pub state: RunStateKind,
    pub exit_code: Option<i32>,
    pub post_check: Option<PostCheck>,
    /// Unix milliseconds.
    pub at: i64,
}

fn path_of(dir: &Path, id: &str, suffix: &str) -> PathBuf {
    dir.join(format!("{id}.{suffix}"))
}

/// The path of the spec of `id`.
#[must_use]
pub fn spec_path(dir: &Path, id: &str) -> PathBuf {
    path_of(dir, id, "spec.json")
}

/// The path of the state file of `id`.
#[must_use]
pub fn state_path(dir: &Path, id: &str) -> PathBuf {
    path_of(dir, id, "state.json")
}

/// The path of the output file of `id`.
#[must_use]
pub fn output_path(dir: &Path, id: &str) -> PathBuf {
    path_of(dir, id, "out")
}

/// The path of the socket of `id`.
#[must_use]
pub fn socket_path(dir: &Path, id: &str) -> PathBuf {
    path_of(dir, id, "sock")
}

/// The path of the lock file of `id`.
#[must_use]
pub fn lock_path(dir: &Path, id: &str) -> PathBuf {
    path_of(dir, id, "lock")
}

fn create_flags() -> OFlags {
    OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn create_new(path: &Path) -> io::Result<File> {
    let fd = rustix::fs::open(path, create_flags(), Mode::from_raw_mode(0o600))?;
    Ok(File::from(fd))
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Writes `bytes` to a new temp file and publishes it as `name` in `dir`.
/// `replace` false: an existing file gives `AlreadyExists` and the temp is
/// removed.
fn publish(dir: &Path, tmp: &str, name: &Path, bytes: &[u8], replace: bool) -> io::Result<()> {
    let tmp = dir.join(tmp);
    // A temp that a crashed writer left behind is removed first.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut file = create_new(&tmp)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let moved = written.and_then(|()| {
        if replace {
            std::fs::rename(&tmp, name)
        } else {
            renameat_with(CWD, &tmp, CWD, name, RenameFlags::NOREPLACE).map_err(io::Error::from)
        }
    });
    if let Err(e) = moved {
        // The first error is the one to report.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    sync_dir(dir)
}

fn check_id(id: &str) -> io::Result<()> {
    if is_hex_id(id) {
        Ok(())
    } else {
        Err(SpecError::Id.into())
    }
}

/// Writes the spec atomically: a temp file, then a rename that never
/// replaces. The spec is never changed afterwards.
///
/// # Errors
/// `AlreadyExists` when the spec exists; `InvalidInput` for a maintenance ID
/// that is not hex; `InvalidData` for a spec over 16 KiB; any I/O error.
pub fn write_spec(dir: &Path, spec: &Spec) -> io::Result<()> {
    check_id(&spec.maintenance_id)?;
    check_dir(dir)?;
    let id = &spec.maintenance_id;
    let bytes = serde_json::to_vec(spec).map_err(io::Error::other)?;
    if bytes.len() as u64 > SPEC_MAX {
        return Err(SpecError::TooLarge.into());
    }
    publish(
        dir,
        &format!(".{id}.spec.tmp"),
        &spec_path(dir, id),
        &bytes,
        false,
    )
}

/// Writes the state file atomically (a temp file, then a rename over the
/// state path).
///
/// # Errors
/// `InvalidInput` for an ID that is not hex; any I/O error.
pub fn write_state(dir: &Path, id: &str, state: &RunState) -> io::Result<()> {
    check_id(id)?;
    check_dir(dir)?;
    let bytes = serde_json::to_vec(state).map_err(io::Error::other)?;
    publish(
        dir,
        &format!(".{id}.state.tmp"),
        &state_path(dir, id),
        &bytes,
        true,
    )
}

/// Writes the output file once (create-new, 0600).
///
/// # Errors
/// `AlreadyExists` when it exists; `InvalidData` over 256 KiB; any I/O error.
pub fn write_output(dir: &Path, id: &str, bytes: &[u8]) -> io::Result<()> {
    check_id(id)?;
    check_dir(dir)?;
    if bytes.len() as u64 > OUTPUT_MAX {
        return Err(SpecError::TooLarge.into());
    }
    let mut file = create_new(&output_path(dir, id))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    sync_dir(dir)
}

/// Opens `path` without following a link and reads it once: a regular
/// file, owned by the effective user, mode 0600, at most `max` bytes.
/// `Ok(None)`: the file does not exist.
fn read_private(path: &Path, max: u64) -> Result<Option<Vec<u8>>, SpecError> {
    let fd = match rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        // A link gives `LOOP` with `NOFOLLOW`.
        Err(rustix::io::Errno::LOOP) => return Err(SpecError::Unsafe),
        Err(_) => return Err(SpecError::Io),
    };
    let st = fstat(&fd).map_err(|_| SpecError::Io)?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile
        || st.st_uid != euid()
        || st.st_mode & 0o777 != 0o600
    {
        return Err(SpecError::Unsafe);
    }
    if u64::try_from(st.st_size).map_err(|_| SpecError::Io)? > max {
        return Err(SpecError::TooLarge);
    }
    let mut bytes = Vec::new();
    File::from(fd)
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SpecError::Io)?;
    if bytes.len() as u64 > max {
        return Err(SpecError::TooLarge);
    }
    Ok(Some(bytes))
}

/// Reads and validates the spec of `id`.
///
/// # Errors
/// A [`SpecError`] for any check that fails.
pub fn read_spec(dir: &Path, id: &str) -> Result<Spec, SpecError> {
    if !is_hex_id(id) {
        return Err(SpecError::Id);
    }
    check_dir(dir).map_err(|_| SpecError::Folder)?;
    let bytes = read_private(&spec_path(dir, id), SPEC_MAX)?.ok_or(SpecError::Missing)?;
    let spec: Spec = serde_json::from_slice(&bytes).map_err(|_| SpecError::Parse)?;
    validate(&spec, id)?;
    Ok(spec)
}

fn validate(spec: &Spec, id: &str) -> Result<(), SpecError> {
    if spec.v != VERSION {
        return Err(SpecError::Invalid("v"));
    }
    if spec.maintenance_id != id {
        return Err(SpecError::Invalid("maintenance_id"));
    }
    for (name, value) in [
        ("request_id", &spec.request_id),
        ("plan_id", &spec.plan_id),
        ("station_token", &spec.station_token),
        ("runner_token", &spec.runner_token),
    ] {
        if !is_hex_id(value) {
            return Err(SpecError::Invalid(name));
        }
    }
    if !size_ok(spec.cols, spec.rows) {
        return Err(SpecError::Invalid("size"));
    }
    if spec.tools.is_empty()
        || spec
            .tools
            .iter()
            .any(|t| by_id(t).is_none_or(|tool| tool.package.is_none()))
    {
        return Err(SpecError::Invalid("tools"));
    }
    let hash = &spec.conf_sha256;
    if hash.len() != 64 || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(SpecError::Invalid("conf_sha256"));
    }
    Ok(())
}

/// Reads the state of `id`. `Ok(None)`: no state file yet.
///
/// # Errors
/// A [`SpecError`] for a file that fails a check.
pub fn read_state(dir: &Path, id: &str) -> Result<Option<RunState>, SpecError> {
    if !is_hex_id(id) {
        return Err(SpecError::Id);
    }
    check_dir(dir).map_err(|_| SpecError::Folder)?;
    let Some(bytes) = read_private(&state_path(dir, id), STATE_MAX)? else {
        return Ok(None);
    };
    let state: RunState = serde_json::from_slice(&bytes).map_err(|_| SpecError::Parse)?;
    if state.v != VERSION {
        return Err(SpecError::Invalid("v"));
    }
    Ok(Some(state))
}

/// Reads the output file of `id`. `Ok(None)`: it does not exist.
///
/// # Errors
/// `InvalidInput` for an ID that is not hex; `PermissionDenied` for a file
/// that is a link, is not owned by this user or is not 0600; `InvalidData`
/// over 256 KiB.
pub fn read_output(dir: &Path, id: &str) -> io::Result<Option<Vec<u8>>> {
    check_id(id)?;
    check_dir(dir).map_err(|_| SpecError::Folder)?;
    Ok(read_private(&output_path(dir, id), OUTPUT_MAX)?)
}
