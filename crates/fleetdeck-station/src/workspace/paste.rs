//! `files.write_temp` (DL-147): a pasted image goes to the station's paste
//! directory and the board gets its path. The `upload_id` is the idempotency
//! key: a retry with the same id and the same bytes returns the same path.
//! The caller holds [`LOCK`] around [`write`], so the quota steps and the
//! write never overlap.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_proto::link::{PasteImage, PastedImage};
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};

/// Serializes every paste on this station.
pub static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The largest decoded image: 700 KiB.
pub const MAX_BYTES: usize = 716_800;
/// The files that stay in the directory before a write (one more follows).
pub const KEEP_FILES: usize = 49;
/// The bytes that stay in the directory before a write: 24 MiB.
pub const KEEP_BYTES: u64 = 24 * 1024 * 1024;
/// A file older than this is deleted.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// The free space that must remain after a write: 512 MiB.
pub const MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;

/// A refused or failed paste. `code` is the link error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteError {
    pub code: &'static str,
    pub message: String,
}

impl PasteError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn internal(what: &str, e: impl std::fmt::Display) -> Self {
        Self::new("internal", format!("{what}: {e}"))
    }
}

/// Stores the image in `dir` and returns its absolute path.
///
/// # Errors
/// `invalid_params`, `too_large`, `conflict`, `no_space` or `internal`.
pub fn write(dir: &Path, req: &PasteImage, now: SystemTime) -> Result<PastedImage, PasteError> {
    check_upload_id(&req.upload_id)?;
    let ext = check_ext(&req.ext)?;
    let bytes = decode(&req.data_base64)?;
    if !magic_matches(ext, &bytes) {
        return Err(PasteError::new(
            "invalid_params",
            format!("the data is not a {ext} image"),
        ));
    }
    check_dir(dir)?;
    let path = dir.join(file_name(&req.upload_id, ext));
    if let Some(existing) = read_existing(&path)? {
        return if existing == bytes {
            Ok(answer(&path))
        } else {
            Err(PasteError::new(
                "conflict",
                "this upload id was used for another image",
            ))
        };
    }
    enforce_quota(dir, now)?;
    check_free_space(dir, bytes.len() as u64)?;
    store(dir, &path, &bytes)?;
    Ok(answer(&path))
}

fn answer(path: &Path) -> PastedImage {
    PastedImage {
        path: path.to_string_lossy().into_owned(),
    }
}

fn check_upload_id(id: &str) -> Result<(), PasteError> {
    let ok = (8..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(PasteError::new(
            "invalid_params",
            "`upload_id` has 8 to 64 characters of A-Z, a-z, 0-9, `_` and `-`",
        ))
    }
}

fn check_ext(ext: &str) -> Result<&'static str, PasteError> {
    ["png", "jpg", "gif", "webp"]
        .into_iter()
        .find(|e| *e == ext)
        .ok_or_else(|| PasteError::new("invalid_params", "`ext` is png, jpg, gif or webp"))
}

fn too_large() -> PasteError {
    PasteError::new(
        "too_large",
        format!("an image has 1 byte to {} KiB", MAX_BYTES / 1024),
    )
}

fn decode(data: &str) -> Result<Vec<u8>, PasteError> {
    // The longest valid text for MAX_BYTES; a longer text is refused before
    // it is decoded.
    if data.len() > MAX_BYTES.div_ceil(3) * 4 {
        return Err(too_large());
    }
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| PasteError::new("invalid_params", "the data is not standard base64"))?;
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

fn magic_matches(ext: &str, b: &[u8]) -> bool {
    match ext {
        "png" => b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "jpg" => b.starts_with(&[0xFF, 0xD8, 0xFF]),
        "gif" => b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a"),
        "webp" => b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP",
        _ => false,
    }
}

/// `<first 32 hex of sha256(upload_id)>.<ext>`.
fn file_name(upload_id: &str, ext: &str) -> String {
    let d = digest(&SHA256, upload_id.as_bytes());
    format!("{}.{ext}", hex(&d.as_ref()[..16]))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The directory must be a real directory (not a link); a missing one is
/// created with mode 0700.
fn check_dir(dir: &Path) -> Result<(), PasteError> {
    let meta = match fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fleetdeck_core::paths::ensure_private_dir(dir)
                .map_err(|e| PasteError::internal("the paste directory cannot be created", e))?;
            fs::symlink_metadata(dir)
                .map_err(|e| PasteError::internal("the paste directory cannot be read", e))?
        }
        Err(e) => {
            return Err(PasteError::internal(
                "the paste directory cannot be read",
                e,
            ));
        }
    };
    if !meta.file_type().is_dir() {
        return Err(PasteError::new(
            "internal",
            "the paste directory is not a real directory",
        ));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| PasteError::internal("the paste directory mode cannot be set", e))?;
    }
    Ok(())
}

/// The bytes of the stored file; `None` when it does not exist.
fn read_existing(path: &Path) -> Result<Option<Vec<u8>>, PasteError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(PasteError::internal("a stored image cannot be read", e)),
    };
    if !meta.file_type().is_file() {
        return Err(PasteError::new(
            "internal",
            "a stored image is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|f| f.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes))
        .map_err(|e| PasteError::internal("a stored image cannot be read", e))?;
    Ok(Some(bytes))
}

/// Deletes the regular files older than [`MAX_AGE`], then the oldest files
/// until [`KEEP_FILES`] files and [`KEEP_BYTES`] bytes remain.
fn enforce_quota(dir: &Path, now: SystemTime) -> Result<(), PasteError> {
    let list_err = |e| PasteError::internal("the paste directory cannot be listed", e);
    let mut files: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    for entry in fs::read_dir(dir).map_err(list_err)? {
        let entry = entry.map_err(list_err)?;
        let meta = entry.metadata().map_err(list_err)?;
        if meta.file_type().is_file() {
            let modified = meta.modified().map_err(list_err)?;
            files.push((entry.path(), modified, meta.len()));
        }
    }
    files.sort_by_key(|(_, modified, _)| *modified);
    let mut keep = Vec::new();
    for file in files {
        let old = now.duration_since(file.1).is_ok_and(|age| age > MAX_AGE);
        if old {
            remove(&file.0)?;
        } else {
            keep.push(file);
        }
    }
    let mut total: u64 = keep.iter().map(|f| f.2).sum();
    let mut count = keep.len();
    for (path, _, len) in keep {
        if count <= KEEP_FILES && total <= KEEP_BYTES {
            break;
        }
        remove(&path)?;
        count -= 1;
        total -= len;
    }
    Ok(())
}

fn remove(path: &Path) -> Result<(), PasteError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(PasteError::internal("an old image cannot be deleted", e)),
    }
}

fn check_free_space(dir: &Path, new_bytes: u64) -> Result<(), PasteError> {
    let st = rustix::fs::statvfs(dir)
        .map_err(|e| PasteError::internal("the free space cannot be read", e))?;
    let free = st.f_bavail.saturating_mul(st.f_frsize);
    if free.saturating_sub(new_bytes) < MIN_FREE_BYTES {
        return Err(PasteError::new(
            "no_space",
            "the station disk has too little free space",
        ));
    }
    Ok(())
}

/// A temp file (mode 0600), `sync_all`, then `rename` to the final name.
fn store(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), PasteError> {
    let mut random = [0u8; 8];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| PasteError::new("internal", "no random bytes"))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dir.join(format!(".{name}.{}.tmp", hex(&random)));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if let Err(e) = result {
        // Best effort: the write error is the one that matters.
        let _ = fs::remove_file(&temp);
        return Err(PasteError::internal("the image cannot be written", e));
    }
    Ok(())
}

#[cfg(test)]
#[path = "paste_tests.rs"]
mod tests;
