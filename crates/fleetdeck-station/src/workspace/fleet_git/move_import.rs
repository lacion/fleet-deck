//! The target side of the transcript transfer (FG-9, design §12.3 steps 3 and
//! 4, ADR-009 §9, ADR-010 §5.2):
//!
//! - [`check_manifest`]: the checks of `session.move_prepare`.
//! - [`Receiver`]: the staging of the upload. It writes the files under
//!   [`staged_name`](super::staged_name) in a new folder (mode 0700, files
//!   mode 0600, `O_CREAT | O_EXCL | O_NOFOLLOW`), counts the bytes and checks
//!   each SHA-256.
//! - [`place_files`]: places the staged files in the provider home with
//!   `import_files`. Each placed file is recorded in its own transaction
//!   before it gets its final name; a failure deletes the placed files by
//!   their recorded identity.
//! - [`delete_file_by_identity`], [`delete_folder_by_identity`]: the DL-150
//!   pattern. A file or folder whose identity changed is kept and logged.

use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use fleetdeck_proto::command::check_native_session_id;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{MOVE_MAX_BYTES, MOVE_MAX_FILES, TranscriptManifest, code};
use ring::digest;
use rustix::fs::{Mode, OFlags};

use super::move_export::{MoveFileError, TOO_LARGE, hex};
use super::move_layout::{MoveSupport, import_files, staged_name, support};
use crate::store::Store;
use crate::store::move_runs::ImportedFile;

/// The longest `rel` of a manifest entry.
const REL_MAX: usize = 1024;

fn checksum() -> MoveFileError {
    MoveFileError::new(
        code::MOVE_CHECKSUM,
        "The conversation files did not arrive complete. Try again.",
    )
}

fn invalid(why: &str) -> MoveFileError {
    MoveFileError::new(
        "invalid_params",
        format!("The manifest is not valid: {why}"),
    )
}

fn internal(what: &str, e: &dyn std::fmt::Display) -> MoveFileError {
    MoveFileError::new("internal", format!("{what}: {e}"))
}

/// A plain relative path: no empty part, no `.`, no `..`, no NUL or `\`.
fn rel_ok(rel: &str) -> bool {
    !rel.is_empty()
        && rel.len() <= REL_MAX
        && !rel.contains(['\0', '\\'])
        && !rel.starts_with('/')
        && rel
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The checks of `session.move_prepare` (design §12.3 step 3): the provider
/// is supported, there are 1 to 256 files, the sizes add up to `total`, which
/// is at most 64 MiB, each `rel` is a plain relative path, each SHA-256 is 64
/// lower-case hex characters, and the native id is valid. The names that
/// depend on the target worktree path are checked later by `import_files`.
///
/// # Errors
/// [`MoveFileError`]: `move_too_large`, `move_checksum` (a bad SHA-256), or
/// `invalid_params`.
pub fn check_manifest(manifest: &TranscriptManifest) -> Result<(), MoveFileError> {
    if support(manifest.provider) != MoveSupport::Native {
        return Err(invalid("the provider is not supported"));
    }
    if check_native_session_id(&manifest.native_session_id).is_err() {
        return Err(invalid("the conversation id is not valid"));
    }
    if manifest.files.is_empty() || manifest.files.len() > MOVE_MAX_FILES {
        return Err(invalid("the number of files is not valid"));
    }
    let mut total = 0_u64;
    for file in &manifest.files {
        total = total.saturating_add(file.size);
    }
    if total > MOVE_MAX_BYTES || manifest.total > MOVE_MAX_BYTES {
        return Err(MoveFileError::new(code::MOVE_TOO_LARGE, TOO_LARGE));
    }
    if total != manifest.total {
        return Err(invalid("the sizes do not add up to the total"));
    }
    for file in &manifest.files {
        if !rel_ok(&file.rel) {
            return Err(invalid("a file name is not valid"));
        }
        let sha_ok = file.sha256.len() == 64
            && file
                .sha256
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !sha_ok {
            return Err(checksum());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- the staging

/// The identity of a staging folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
}

/// The staging of one upload: the files arrive back to back in manifest
/// order. [`Self::feed`] takes the bytes as they come; [`Self::finish`] checks
/// that every file is complete.
pub struct Receiver {
    folder: Folder,
    manifest: TranscriptManifest,
    /// The index of the file that is written now.
    index: usize,
    current: Option<Current>,
    received: u64,
}

struct Current {
    file: File,
    written: u64,
    hash: digest::Context,
}

impl Receiver {
    /// Makes the staging folder `<moves>/<move_id>` (mode 0700; `<moves>` is
    /// made when it is missing). A folder that exists already is refused.
    ///
    /// # Errors
    /// [`MoveFileError`] `internal` when the folder cannot be made.
    pub fn create(
        moves: &Path,
        move_id: &str,
        manifest: &TranscriptManifest,
    ) -> Result<Self, MoveFileError> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(moves)
            .map_err(|e| internal("the staging root", &e))?;
        let path = moves.join(move_id);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| internal("the staging folder", &e))?;
        let meta =
            std::fs::symlink_metadata(&path).map_err(|e| internal("the staging folder", &e))?;
        Ok(Self {
            folder: Folder {
                path,
                dev: meta.dev(),
                ino: meta.ino(),
            },
            manifest: manifest.clone(),
            index: 0,
            current: None,
            received: 0,
        })
    }

    /// The staging folder and its identity.
    #[must_use]
    pub fn folder(&self) -> &Folder {
        &self.folder
    }

    fn open_next(&mut self) -> Result<(), MoveFileError> {
        let path = self.folder.path.join(staged_name(self.index));
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = rustix::fs::open(&path, flags, Mode::RUSR | Mode::WUSR)
            .map_err(|e| internal("a staged file", &e))?;
        self.current = Some(Current {
            file: File::from(fd),
            written: 0,
            hash: digest::Context::new(&digest::SHA256),
        });
        Ok(())
    }

    /// Ends the current file: syncs it and checks its SHA-256.
    fn end_file(&mut self) -> Result<(), MoveFileError> {
        let Some(current) = self.current.take() else {
            return Ok(());
        };
        current
            .file
            .sync_all()
            .map_err(|e| internal("a staged file", &e))?;
        let sha = hex(current.hash.finish().as_ref());
        let wanted = self
            .manifest
            .files
            .get(self.index)
            .map(|f| f.sha256.as_str());
        if wanted != Some(sha.as_str()) {
            return Err(checksum());
        }
        self.index += 1;
        Ok(())
    }

    /// Writes received bytes into the staged files.
    ///
    /// # Errors
    /// `move_too_large` when more than `manifest.total` bytes come,
    /// `move_checksum` for a hash that does not match, `internal` for an I/O
    /// error.
    pub fn feed(&mut self, mut bytes: &[u8]) -> Result<(), MoveFileError> {
        while !bytes.is_empty() {
            if self.received >= self.manifest.total {
                return Err(MoveFileError::new(code::MOVE_TOO_LARGE, TOO_LARGE));
            }
            let Some(entry) = self.manifest.files.get(self.index) else {
                return Err(MoveFileError::new(code::MOVE_TOO_LARGE, TOO_LARGE));
            };
            let size = entry.size;
            if self.current.is_none() {
                self.open_next()?;
            }
            let Some(current) = self.current.as_mut() else {
                return Err(checksum());
            };
            let room = usize::try_from(size - current.written).unwrap_or(usize::MAX);
            let take = room.min(bytes.len());
            let (now, rest) = bytes.split_at(take);
            current
                .file
                .write_all(now)
                .map_err(|e| internal("a staged file", &e))?;
            current.hash.update(now);
            current.written += take as u64;
            self.received += take as u64;
            bytes = rest;
            if current.written == size {
                self.end_file()?;
            }
        }
        // A file of size 0 is complete when it opens.
        while self
            .manifest
            .files
            .get(self.index)
            .is_some_and(|f| f.size == 0)
        {
            self.open_next()?;
            self.end_file()?;
        }
        Ok(())
    }

    /// The upload ended: every file must be complete.
    ///
    /// # Errors
    /// `move_checksum` for a short stream.
    pub fn finish(mut self) -> Result<Folder, MoveFileError> {
        // Zero-size files at the start or the end of the list.
        self.feed(&[])?;
        if self.current.is_some()
            || self.index != self.manifest.files.len()
            || self.received != self.manifest.total
        {
            return Err(checksum());
        }
        Ok(self.folder)
    }
}

// ------------------------------------------------------------ delete by identity

/// What a delete by identity did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    Removed,
    /// Nothing at the path.
    Missing,
    /// Another file or folder is at the path now: kept and logged.
    Changed,
}

/// Deletes the regular file at `path` when it is the file `dev`/`ino`.
pub fn delete_file_by_identity(path: &Path, dev: u64, ino: u64) -> Removal {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Removal::Missing;
    };
    if !meta.is_file() || meta.dev() != dev || meta.ino() != ino {
        tracing::warn!(path = %path.display(), "a moved file changed; it is kept");
        return Removal::Changed;
    }
    match std::fs::remove_file(path) {
        Ok(()) => Removal::Removed,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "a moved file is not deleted");
            Removal::Changed
        }
    }
}

/// Deletes the folder at `path`, with its files, when it is the folder
/// `dev`/`ino` (not a link).
pub fn delete_folder_by_identity(path: &Path, dev: u64, ino: u64) -> Removal {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Removal::Missing;
    };
    if !meta.is_dir() || meta.dev() != dev || meta.ino() != ino {
        tracing::warn!(path = %path.display(), "a staging folder changed; it is kept");
        return Removal::Changed;
    }
    match std::fs::remove_dir_all(path) {
        Ok(()) => Removal::Removed,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "a staging folder is not deleted");
            Removal::Changed
        }
    }
}

/// Deletes the files of a discard by identity (the staging folder and each
/// imported file).
pub fn delete_cleanup(cleanup: &crate::store::move_discard::Cleanup) {
    if let Some(staging) = &cleanup.staging {
        delete_folder_by_identity(Path::new(&staging.path), staging.dev, staging.ino);
    }
    for file in &cleanup.imported {
        delete_file_by_identity(Path::new(&file.path), file.dev, file.ino);
    }
}

// ------------------------------------------------------------------ the import

/// Places the staged files of a native move in the provider `home` of the
/// target (design §12.3 step 4): `import_files` with a record callback that
/// stores each file in `move_in_runs.imported`, in a transaction of its own,
/// before the file gets its final name. On a failure the files placed so far
/// are deleted by their recorded identity (the failure is before `launching`).
///
/// Runs the blocking file work on the blocking pool.
///
/// # Errors
/// The [`MoveFileError`] of `import_files`, or `internal`.
pub async fn place_files(
    store: &Store,
    move_id: &str,
    home: PathBuf,
    kind: ProviderKind,
    target_cwd: String,
    manifest: TranscriptManifest,
    staged: PathBuf,
) -> Result<Vec<ImportedFile>, MoveFileError> {
    let handle = tokio::runtime::Handle::current();
    let store = store.clone();
    let move_id = move_id.to_owned();
    let task = tokio::task::spawn_blocking(move || {
        let mut recorded: Vec<ImportedFile> = Vec::new();
        let mut record = |file: &ImportedFile| -> Result<(), String> {
            match handle.block_on(store.move_imported_append(move_id.clone(), file.clone())) {
                Ok(true) => {
                    recorded.push(file.clone());
                    Ok(())
                }
                Ok(false) => Err("the move has no row".to_owned()),
                Err(e) => Err(e.to_string()),
            }
        };
        let done = import_files(&home, kind, &target_cwd, &manifest, &staged, &mut record);
        if done.is_err() {
            for file in &recorded {
                delete_file_by_identity(Path::new(&file.path), file.dev, file.ino);
            }
        }
        done
    });
    match task.await {
        Ok(done) => done,
        Err(e) => Err(internal("the import task", &e)),
    }
}

#[cfg(test)]
#[path = "move_import_tests.rs"]
mod tests;
