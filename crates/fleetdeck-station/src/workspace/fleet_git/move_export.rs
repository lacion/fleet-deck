//! The export side of a move: the open files, their manifest and the cap
//! (FG-9, design §12.3 step 2, ADR-010 §5.1).
//!
//! The provider layouts (`move_layout.rs`) find the files and hand them here
//! as candidates. Every candidate is an open descriptor of a regular file
//! that the trusted open of `history::locate` checked. This file applies the
//! cap of 64 MiB and builds the manifest with the size and the SHA-256 of each
//! file, read from that descriptor.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{
    MOVE_MAX_BYTES, MOVE_MAX_FILES, ManifestFile, TranscriptManifest, code,
};
use ring::digest;

/// The text of `move_too_large` (design §12.7).
pub const TOO_LARGE: &str =
    "The conversation files are larger than 64 MiB. Use a branch-only move.";
/// The text of a session whose conversation files cannot be used.
pub const NOT_FOUND: &str = "The conversation files of this session were not found.";

/// The size of one read from a file.
const CHUNK: usize = 64 * 1024;

/// A failure of the file work of a move: a link error `code` and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveFileError {
    pub code: &'static str,
    pub message: String,
}

impl MoveFileError {
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The files were not found or are not trusted (`invalid_state`).
    #[must_use]
    pub fn not_found() -> Self {
        Self::new("invalid_state", NOT_FOUND)
    }
}

impl std::fmt::Display for MoveFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// A file that a layout found and opened, before the cap.
#[derive(Debug)]
pub struct Candidate {
    pub key: String,
    pub rel: String,
    pub file: File,
    pub size: u64,
}

/// An open file with its manifest entry.
#[derive(Debug)]
pub struct OpenFile {
    pub entry: ManifestFile,
    pub file: File,
}

/// The files of a native move: the manifest and the open files in manifest
/// order.
#[derive(Debug)]
pub struct Exported {
    pub manifest: TranscriptManifest,
    pub files: Vec<OpenFile>,
}

/// The lower-case hex text of `bytes`.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The SHA-256 of the first `size` bytes of `file`, read by position.
///
/// # Errors
/// An I/O error, or `UnexpectedEof` when the file is shorter than `size`.
pub fn sha256_of(file: &File, size: u64) -> io::Result<String> {
    let mut context = digest::Context::new(&digest::SHA256);
    let mut buf = vec![0_u8; CHUNK];
    let mut at = 0_u64;
    while at < size {
        let want = usize::try_from((size - at).min(CHUNK as u64)).unwrap_or(CHUNK);
        let n = file.read_at(&mut buf[..want], at)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        context.update(&buf[..n]);
        at += n as u64;
    }
    Ok(hex(context.finish().as_ref()))
}

fn io_error(e: &io::Error) -> MoveFileError {
    MoveFileError::new(
        "internal",
        format!("a conversation file cannot be read: {e}"),
    )
}

/// Applies the cap and builds the manifest (design §12.3 step 2, ADR-010
/// §5.1). The required file alone over the cap is `move_too_large`. When the
/// optional files make the total, or the file count, pass the cap, they are
/// all dropped and the move goes on with the required file.
///
/// # Errors
/// [`MoveFileError`] with `move_too_large`, or `internal` for a read error.
pub fn assemble(
    provider: ProviderKind,
    native: &str,
    required: Candidate,
    optional: Vec<Candidate>,
) -> Result<Exported, MoveFileError> {
    if required.size > MOVE_MAX_BYTES {
        return Err(MoveFileError::new(code::MOVE_TOO_LARGE, TOO_LARGE));
    }
    let extra: u64 = optional.iter().map(|c| c.size).sum();
    let optional = if required.size.saturating_add(extra) > MOVE_MAX_BYTES
        || optional.len() + 1 > MOVE_MAX_FILES
    {
        tracing::info!(
            dropped = optional.len(),
            "the optional conversation files are dropped from a move (over the cap)"
        );
        Vec::new()
    } else {
        optional
    };
    let mut files = Vec::with_capacity(optional.len() + 1);
    let mut total = 0_u64;
    for candidate in std::iter::once(required).chain(optional) {
        let sha256 = sha256_of(&candidate.file, candidate.size).map_err(|e| io_error(&e))?;
        total += candidate.size;
        files.push(OpenFile {
            entry: ManifestFile {
                key: candidate.key,
                rel: candidate.rel,
                size: candidate.size,
                sha256,
            },
            file: candidate.file,
        });
    }
    Ok(Exported {
        manifest: TranscriptManifest {
            provider,
            native_session_id: native.to_owned(),
            files: files.iter().map(|f| f.entry.clone()).collect(),
            total,
        },
        files,
    })
}
