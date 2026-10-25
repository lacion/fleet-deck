//! The write part of the secret files (Fleet Sync FS-3, design
//! `docs/v2/design/fleet-sync.md` §3, revisions R4 and R12): the temporary
//! file, the move aside, the backups and the install without a lost edit.
//!
//! The temporary file and the moved-aside file have a name that the default
//! member deny list hides (`secrets*`), so a member of a shared session
//! cannot list or read them (Fleet Sync pre-PR finding 1).

use std::fs::File;
use std::io::Write;
use std::os::fd::BorrowedFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags};
use rustix::io::Errno;

use super::{
    BACKUP_FAILED, CHANGED, Content, PRIVATE_FILE, SecretError, WRITE_FAILED, lower_hex, read_file,
};
use crate::workspace::PrivatePaths;

const KEEP_BACKUPS: usize = 5;

fn random_hex() -> Result<String, SecretError> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 8];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| WRITE_FAILED)?;
    Ok(lower_hex(&bytes))
}

/// What a write needs: the open folder of the file and the facts of the call.
pub(super) struct Job<'a> {
    pub(super) parent: BorrowedFd<'a>,
    pub(super) name: &'a str,
    pub(super) rel: &'a str,
    pub(super) salt: &'a str,
    pub(super) project_id: &'a str,
    pub(super) private: &'a PrivatePaths,
    pub(super) data_dir: Option<&'a Path>,
}

fn remove(parent: BorrowedFd<'_>, name: &str) {
    if let Err(e) = rustix::fs::unlinkat(parent, name, AtFlags::empty()) {
        tracing::debug!(code = ?e, "secret files: a temporary file stays");
    }
}

/// Renames `from` to `to`, never over an existing file.
fn restore(parent: BorrowedFd<'_>, from: &str, to: &str) -> Result<(), Errno> {
    rustix::fs::renameat_with(parent, from, parent, to, RenameFlags::NOREPLACE)
}

/// `<data>/secret-backups/<project>/<16 hex of SHA-256(path)>`; `None` for a
/// project id outside `^[A-Za-z0-9_-]{1,64}$`.
pub(super) fn backup_folder(data_dir: &Path, project_id: &str, path: &str) -> Option<PathBuf> {
    let id_ok = (1..=64).contains(&project_id.len())
        && project_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    let hash = ring::digest::digest(&ring::digest::SHA256, path.as_bytes());
    let dir = data_dir.join("secret-backups").join(project_id);
    id_ok.then(|| dir.join(&lower_hex(hash.as_ref())[..16]))
}

/// The sort key of a backup name: `<ms>` or `<ms>-<n>`. `None`: not a backup.
pub(super) fn backup_key(name: &str) -> Option<(u64, u64)> {
    let number = |s: &str| {
        if s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    };
    let (ms, n) = name.split_once('-').unwrap_or((name, "0"));
    Some((number(ms)?, number(n)?))
}

/// Keeps the five newest backups of a folder (decision 16).
fn prune_backups(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return tracing::debug!("secret files: the backup folder cannot be listed");
    };
    let mut found: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let key = backup_key(entry.file_name().to_str()?)?;
            let regular = std::fs::symlink_metadata(entry.path()).is_ok_and(|m| m.is_file());
            regular.then(|| (key, entry.path()))
        })
        .collect();
    found.sort();
    let old = found.len().saturating_sub(KEEP_BACKUPS);
    for (_, path) in found.into_iter().take(old) {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::debug!(code = ?e.kind(), "secret files: an old backup stays");
        }
    }
}

/// Keeps a copy of the old content in the data folder (decision 16).
fn backup(job: &Job<'_>, content: &[u8]) -> Result<(), SecretError> {
    let folder = job
        .data_dir
        .and_then(|dir| backup_folder(dir, job.project_id, job.rel))
        .ok_or(BACKUP_FAILED)?;
    let made = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder);
    made.map_err(|_| BACKUP_FAILED)?;
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    for attempt in 0..1000u32 {
        let name = if attempt == 0 {
            ms.to_string()
        } else {
            format!("{ms}-{attempt}")
        };
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(OFlags::NOFOLLOW.bits() as i32)
            .mode(0o600)
            .open(folder.join(&name));
        match opened {
            Ok(mut file) => {
                let saved = file.write_all(content).and_then(|()| file.sync_all());
                saved.map_err(|_| BACKUP_FAILED)?;
                prune_backups(&folder);
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => break,
        }
    }
    Err(BACKUP_FAILED)
}

/// Writes the new content to a temporary file next to the target.
fn write_temp(job: &Job<'_>, tmp: &str, content: &[u8]) -> Result<(), SecretError> {
    let fd = rustix::fs::openat(
        job.parent,
        tmp,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| WRITE_FAILED)?;
    let mut file = File::from(fd);
    let written = rustix::fs::fchmod(&file, Mode::from_raw_mode(0o600))
        .map_err(std::io::Error::from)
        .and_then(|()| file.write_all(content))
        .and_then(|()| file.sync_all());
    if written.is_err() {
        remove(job.parent, tmp);
        return Err(WRITE_FAILED);
    }
    Ok(())
}

fn place_new(job: &Job<'_>, tmp: &str) -> Result<bool, SecretError> {
    match restore(job.parent, tmp, job.name) {
        Ok(()) => sync_folder(job).map(|()| false),
        Err(e) => {
            remove(job.parent, tmp);
            Err(refusal(e, Errno::EXIST))
        }
    }
}

/// `CHANGED` when the rename failed with `busy` (the file is not as it was),
/// else `WRITE_FAILED`.
fn refusal(e: Errno, busy: Errno) -> SecretError {
    if e == busy { CHANGED } else { WRITE_FAILED }
}

fn sync_folder(job: &Job<'_>) -> Result<(), SecretError> {
    rustix::fs::fsync(job.parent).map_err(|_| WRITE_FAILED)
}

/// `expect = Some(digest)` (R4): the old file moves aside first, so the compare
/// cannot lose an edit. `between` runs after the move and `mid` after the first
/// backup, before the install (test hooks).
fn replace(
    job: &Job<'_>,
    tmp: &str,
    digest: &str,
    between: impl FnOnce(),
    mid: impl FnOnce(),
) -> Result<bool, SecretError> {
    let aside = format!("secrets.fleetdeck-old-{}", random_hex()?);
    match rustix::fs::renameat(job.parent, job.name, job.parent, aside.as_str()) {
        Ok(()) => {}
        Err(e) => {
            remove(job.parent, tmp);
            return Err(refusal(e, Errno::NOENT));
        }
    }
    between();
    let current = read_file(job.parent, &aside, job.rel, job.private, job.salt);
    if let Ok(Some(old)) = &current
        && old.digest == digest
    {
        return replace_matching(job, tmp, &aside, old, mid);
    }
    // The file is not the one that the scan saw: it goes back.
    let mut outcome = match &current {
        Err(e) if e.code == "private_file" => PRIVATE_FILE,
        _ => CHANGED,
    };
    match restore(job.parent, &aside, job.name) {
        Ok(()) => {}
        Err(Errno::EXIST) => {
            // An editor made a new file in the window: keep the old content.
            let kept = current.as_ref().ok().and_then(Option::as_ref);
            match kept.map(|old| backup(job, &old.bytes)) {
                Some(Ok(())) => remove(job.parent, &aside),
                _ => tracing::debug!("secret files: the old file stays aside"),
            }
        }
        Err(e) => {
            tracing::debug!(code = ?e, "secret files: the old file stays aside");
            outcome = WRITE_FAILED;
        }
    }
    remove(job.parent, tmp);
    Err(outcome)
}

/// Reads the moved-aside file again after the install, then unlinks it. An
/// editor that kept the file open can write to it after the first read: new
/// bytes get one more backup. A failed read or backup keeps the file aside;
/// only the code is logged.
fn drop_aside(job: &Job<'_>, aside: &str, first: &Content) {
    match read_file(job.parent, aside, job.rel, job.private, job.salt) {
        Ok(Some(late)) if late.digest != first.digest => {
            if let Err(e) = backup(job, &late.bytes) {
                return tracing::debug!(code = e.code, "secret files: a late write stays aside");
            }
        }
        Ok(_) => {}
        Err(e) => {
            return tracing::debug!(code = e.code, "secret files: the old file stays aside");
        }
    }
    remove(job.parent, aside);
}

fn replace_matching(
    job: &Job<'_>,
    tmp: &str,
    aside: &str,
    old: &Content,
    mid: impl FnOnce(),
) -> Result<bool, SecretError> {
    let put_back = |code: SecretError| {
        if let Err(e) = restore(job.parent, aside, job.name) {
            tracing::debug!(code = ?e, "secret files: the old file stays aside");
        }
        remove(job.parent, tmp);
        Err(code)
    };
    if let Err(e) = backup(job, &old.bytes) {
        return put_back(e);
    }
    mid();
    match restore(job.parent, tmp, job.name) {
        Ok(()) => {
            drop_aside(job, aside, old);
            sync_folder(job).map(|()| true)
        }
        Err(Errno::EXIST) => {
            // An editor made a new file in the window; the old content is in
            // the backup.
            remove(job.parent, tmp);
            remove(job.parent, aside);
            Err(CHANGED)
        }
        Err(_) => put_back(WRITE_FAILED),
    }
}

/// Writes `content` as the file `job.name`: `expect = None` makes a new file,
/// `Some(digest)` replaces the file that has this digest. The staging files
/// are `secrets.fleetdeck-tmp-<hex>` and `secrets.fleetdeck-old-<hex>` in the
/// folder of the file: the default member deny list hides them.
/// `between` and `mid` are test hooks (see [`replace`]).
pub(super) fn commit(
    job: &Job<'_>,
    content: &[u8],
    expect: Option<&str>,
    between: impl FnOnce(),
    mid: impl FnOnce(),
) -> Result<bool, SecretError> {
    let tmp = format!("secrets.fleetdeck-tmp-{}", random_hex()?);
    write_temp(job, &tmp, content)?;
    match expect {
        None => place_new(job, &tmp),
        Some(digest) => replace(job, &tmp, digest, between, mid),
    }
}
