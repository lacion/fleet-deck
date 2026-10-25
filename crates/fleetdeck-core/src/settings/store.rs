//! The secret store `<station data>/secrets/` (design 1.1, T1, U1, U2).

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use ring::rand::{SecureRandom, SystemRandom};

use super::fs::Fs;
use super::refs::MAX_SECRET_NAME;

/// `n` random bytes as lower-case hex.
pub(super) fn random_hex(n: usize) -> io::Result<String> {
    let mut bytes = vec![0_u8; n];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| io::Error::other("no random bytes"))?;
    Ok(bytes.iter().fold(String::new(), |mut out, b| {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
        out
    }))
}

/// A fresh secret name: `provider-<id>-<var>-<8 hex>`, at most 96 chars.
pub(super) fn secret_name(instance: &str, var: &str) -> io::Result<String> {
    let clean = |s: &str| -> String {
        s.to_ascii_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_lowercase() || c.is_ascii_digit() {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    };
    let tail = format!("-{}", random_hex(4)?);
    let mut base = format!("provider-{}-{}", clean(instance), clean(var));
    base.truncate(MAX_SECRET_NAME - tail.len());
    Ok(base + &tail)
}

/// Creates the store directory (mode 0700) and makes it durable: it syncs
/// the directory and its parent, and every directory that was created, with
/// its parent. An existing directory must be a real directory without group
/// or other access. `pending` holds the directories that were created but
/// not yet synced; a retry after a failed sync takes them up again.
pub(super) fn ensure_store(fs: &dyn Fs, dir: &Path, pending: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut missing: Vec<PathBuf> = Vec::new();
    let mut cur = dir.to_path_buf();
    loop {
        match fs::symlink_metadata(&cur) {
            Ok(meta) => {
                if cur == dir && (!meta.is_dir() || meta.permissions().mode() & 0o077 != 0) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "the secret store is not a private directory",
                    ));
                }
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                missing.push(cur.clone());
                cur = cur
                    .parent()
                    .ok_or_else(|| io::Error::other("no parent directory"))?
                    .to_path_buf();
            }
            Err(e) => return Err(e),
        }
    }
    for path in missing.iter().rev() {
        fs.mkdir(path)?;
        pending.push(path.clone());
    }
    if pending.is_empty() {
        pending.push(dir.to_path_buf());
    }
    while let Some(path) = pending.first().cloned() {
        fs.fsync_dir(&path)?;
        if let Some(parent) = path.parent() {
            fs.fsync_dir(parent)?;
        }
        pending.remove(0);
    }
    Ok(())
}

/// Writes a new secret file: a 0600 temp file, write, fsync, rename to
/// `name`, fsync of the directory. The name must be unused. After an error
/// the caller removes `name` (it can exist when only the last sync failed).
pub(super) fn write_secret(fs: &dyn Fs, dir: &Path, name: &str, value: &str) -> io::Result<()> {
    let final_path = dir.join(name);
    if fs::symlink_metadata(&final_path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the secret name is in use",
        ));
    }
    let tmp = dir.join(format!(".tmp-{}", random_hex(8)?));
    let mut file = fs.create_excl_0600(&tmp)?;
    let result = fs
        .write(&mut file, &tmp, value.as_bytes())
        .and_then(|()| fs.fsync(&file, &tmp));
    drop(file);
    let result = result
        .and_then(|()| fs.rename(&tmp, &final_path))
        .and_then(|()| fs.fsync_dir(dir));
    if result.is_err() {
        // After a successful rename the temp name is gone.
        match fs.remove(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("settings: cannot remove a temp secret file ({})", e.kind()),
        }
    }
    result
}

/// The file names in the store.
pub(super) fn list(dir: &Path) -> io::Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for entry in fs::read_dir(dir)? {
        match entry?.file_name().into_string() {
            Ok(name) => {
                out.insert(name);
            }
            Err(_) => {
                tracing::warn!("settings: a file of the secret store has a name that is not text")
            }
        }
    }
    Ok(out)
}
