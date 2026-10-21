//! The secret files of a project on the station (Fleet Sync FS-3, design
//! `docs/v2/design/fleet-sync.md` §3): `project.secret_scan`,
//! `project.secret_read` and `project.secret_write`.
//!
//! The station is the only judge of a path: no link is followed, Git must
//! ignore the file, and a content, digest or salt is never logged.

use std::collections::{HashMap, HashSet};
use std::fs::{File, Metadata};
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_core::launch_dir::open_root;
use fleetdeck_proto::fleet_git::{GitSettings, LocalRemotes, check_settings, remote_identity};
use fleetdeck_proto::fleet_git_secrets::{
    MAX_SECRET_BYTES, MAX_SECRET_FILES, SECRET_HIDDEN_TEXT, SECRET_PATH_TEXT, SecretFileKey,
    SecretFileState, SecretPathError, SecretPresence, SecretReadAnswer, SecretReadParams,
    SecretScanAnswer, SecretScanParams, SecretWriteAnswer, SecretWriteParams, check_secret_path,
};
use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags};
use rustix::io::Errno;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::identity::{READ_TIMEOUT, read, read_git};
use super::include::{dir_flags, ignored_among, open_chain};
use crate::manager::policy::Policy;
use crate::store::Store;
use crate::workspace::{GitOptions, PrivatePaths};

/// A refused call: the wire code and a fixed text (never a part of the params).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretError {
    pub code: &'static str,
    pub text: &'static str,
}

const fn err(code: &'static str, text: &'static str) -> SecretError {
    SecretError { code, text }
}

macro_rules! errors {
    ($($name:ident = $code:literal, $text:literal;)*) => {
        $(const $name: SecretError = err($code, $text);)*
    };
}

errors! {
    INVALID_PARAMS = "invalid_params", "The request does not parse.";
    NOT_A_HOME = "not_a_home", "This station has no home of the project.";
    STALE_GENERATION = "stale_generation", "The home of this project changed. Start again.";
    NOT_THIS_REPO = "not_this_repo", "This home does not point to the project's repository.";
    NOT_IGNORED = "not_ignored", "Git does not ignore this file here, or the file is tracked.";
    SYMLINK = "symlink", "A link or a file is in the path.";
    NOT_REGULAR = "not_regular", "Not a regular file.";
    TOO_LARGE = "too_large", "Larger than 512 KiB.";
    UNREADABLE = "unreadable", "The file cannot be read.";
    MISSING = "missing", "The file is missing.";
    PRIVATE_FILE = "private_file", "This file is a private file of the station, or it has more than one hard link.";
    CHANGED = "changed_meanwhile", "The file changed during the sync.";
    DIGEST_MISMATCH = "digest_mismatch", "The content does not match its digest.";
    BACKUP_FAILED = "backup_failed", "The station could not keep a copy of the old file.";
    WRITE_FAILED = "write_failed", "The station could not write the file.";
    INTERNAL = "internal", "The station could not answer.";
    UNKNOWN_METHOD = "unknown_method", "The station does not know this method.";
}

const KEEP_BACKUPS: usize = 5;
const URL_CAP: usize = 4 * 1024;

fn path_error(e: SecretPathError) -> SecretError {
    err(
        "invalid_path",
        match e {
            SecretPathError::Invalid => SECRET_PATH_TEXT,
            SecretPathError::NotMemberHidden => SECRET_HIDDEN_TEXT,
        },
    )
}

fn lower_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The 32 bytes of a salt of exactly 64 lower-case hex characters.
fn decode_salt(salt_hex: &str) -> Option<[u8; 32]> {
    let hex = |b: &u8| b.is_ascii_digit() || (b'a'..=b'f').contains(b);
    if salt_hex.len() != 64 || !salt_hex.as_bytes().iter().all(hex) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&salt_hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Lower-case hex of `HMAC-SHA256(salt, content)`; `None` for a bad salt.
#[must_use]
pub fn secret_digest(salt_hex: &str, content: &[u8]) -> Option<String> {
    let salt = decode_salt(salt_hex)?;
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &salt);
    Some(lower_hex(ring::hmac::sign(&key, content).as_ref()))
}

fn random_hex() -> Result<String, SecretError> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 8];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| WRITE_FAILED)?;
    Ok(lower_hex(&bytes))
}

type FileLocks = Mutex<HashMap<(String, String), Arc<tokio::sync::Mutex<()>>>>;
static LOCKS: LazyLock<FileLocks> = LazyLock::new(FileLocks::default);

/// Locks the files in sorted order, so two calls cannot wait for each other.
async fn lock_files(
    project_id: &str,
    mut paths: Vec<String>,
) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
    paths.sort();
    paths.dedup();
    let locks: Vec<Arc<tokio::sync::Mutex<()>>> = {
        let mut map = LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
        // A lock that no call holds or awaits is not needed.
        map.retain(|_, lock| Arc::strong_count(lock) > 1);
        let key = |path| (project_id.to_owned(), path);
        paths
            .into_iter()
            .map(|path| Arc::clone(map.entry(key(path)).or_default()))
            .collect()
    };
    let mut guards = Vec::with_capacity(locks.len());
    for lock in locks {
        guards.push(lock.lock_owned().await);
    }
    guards
}

struct Env<'a> {
    store: &'a Store,
    policy: &'a Policy,
    private: Arc<PrivatePaths>,
    options: &'a GitOptions,
    data_dir: Option<&'a Path>,
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, SecretError> {
    serde_json::from_value(params).map_err(|_| INVALID_PARAMS)
}

fn check_remote_name(name: &str) -> Result<(), SecretError> {
    let settings = GitSettings {
        push_remote: name.to_owned(),
        ..GitSettings::default()
    };
    check_settings(&settings).map_err(|_| INVALID_PARAMS)
}

impl Env<'_> {
    /// The home folder (decision 11) and the repository check (R3).
    async fn home(
        &self,
        project_id: &str,
        generation: u64,
        dest: &str,
        remote_name: &str,
    ) -> Result<PathBuf, SecretError> {
        let stored = self
            .store
            .station_home(project_id.to_owned())
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "secret files: the home list");
                INTERNAL
            })?;
        let (stored_generation, path) = stored.ok_or(NOT_A_HOME)?;
        if stored_generation != generation {
            return Err(STALE_GENERATION);
        }
        let home = self.policy.workspace(&path).map_err(|_| NOT_A_HOME)?;
        let git = read_git(self.options, &home, READ_TIMEOUT);
        let args = ["remote", "get-url", "--push", "--all", "--", remote_name];
        let same = match read(&git, &args, URL_CAP).await {
            Ok((0, out, _)) => {
                let lines: Vec<&str> = out
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .collect();
                matches!(lines.as_slice(), [url]
                    if remote_identity(url, LocalRemotes::Allow).is_ok_and(|id| id.dest == dest))
            }
            _ => false,
        };
        if same { Ok(home) } else { Err(NOT_THIS_REPO) }
    }
}

/// Opens the folders `parents` below `root` one at a time, without following
/// a link and without making a folder. `Ok(None)`: a folder does not exist.
fn open_parent(root: BorrowedFd<'_>, parents: &[&str]) -> Result<Option<OwnedFd>, SecretError> {
    let flags = dir_flags();
    let mut dir = rustix::fs::openat(root, ".", flags, Mode::empty()).map_err(|_| UNREADABLE)?;
    for name in parents {
        dir = match rustix::fs::openat(&dir, *name, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(Errno::LOOP | Errno::NOTDIR) => return Err(SYMLINK),
            Err(_) => return Err(UNREADABLE),
        };
    }
    Ok(Some(dir))
}

fn split(path: &str) -> (Vec<&str>, &str) {
    let mut parts: Vec<&str> = path.split('/').collect();
    let name = parts.pop().unwrap_or(path);
    (parts, name)
}

struct Content {
    bytes: Vec<u8>,
    mtime_ms: i64,
    digest: String,
}

fn mtime_ms(meta: &Metadata) -> i64 {
    meta.mtime()
        .saturating_mul(1000)
        .saturating_add(meta.mtime_nsec() / 1_000_000)
}

/// Opens and reads the file `name` of `parent`. `Ok(None)`: it does not
/// exist. `rel` is the path below the home, for the private file check.
fn read_file(
    parent: BorrowedFd<'_>,
    name: &str,
    rel: &str,
    private: &PrivatePaths,
    salt: &str,
) -> Result<Option<Content>, SecretError> {
    // NONBLOCK: a FIFO that replaced the file cannot block the open.
    let fd = match rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(None),
        Err(Errno::LOOP) => return Err(SYMLINK),
        Err(_) => return Err(UNREADABLE),
    };
    let file = File::from(fd);
    let meta = file.metadata().map_err(|_| UNREADABLE)?;
    if !meta.is_file() {
        return Err(NOT_REGULAR);
    }
    if private.is_private_opened(&meta, rel) {
        return Err(PRIVATE_FILE);
    }
    if meta.len() > MAX_SECRET_BYTES {
        return Err(TOO_LARGE);
    }
    let mut bytes = Vec::new();
    file.take(MAX_SECRET_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UNREADABLE)?;
    if bytes.len() as u64 > MAX_SECRET_BYTES {
        return Err(TOO_LARGE);
    }
    let digest = secret_digest(salt, &bytes).ok_or(INVALID_PARAMS)?;
    Ok(Some(Content {
        bytes,
        mtime_ms: mtime_ms(&meta),
        digest,
    }))
}

fn state(path: &str, state: SecretPresence, code: Option<&str>) -> SecretFileState {
    SecretFileState {
        path: path.to_owned(),
        state,
        code: code.map(str::to_owned),
        digest: None,
        size: None,
        mtime_ms: None,
    }
}

fn refused(path: &str, code: &str) -> SecretFileState {
    state(path, SecretPresence::Refused, Some(code))
}

fn missing(path: &str) -> SecretFileState {
    state(path, SecretPresence::Missing, None)
}

/// A file before Git answered: its parent folder, or its refused state.
type Pre = Result<Option<OwnedFd>, SecretFileState>;

/// R13: the path rule and the folders come first; a refused path skips Git.
fn scan_first(root: BorrowedFd<'_>, files: &[SecretFileKey]) -> Vec<Pre> {
    let first = |file: &SecretFileKey| {
        check_secret_path(&file.path).map_err(|e| refused(&file.path, path_error(e).code))?;
        open_parent(root, &split(&file.path).0).map_err(|e| refused(&file.path, e.code))
    };
    files.iter().map(first).collect()
}

/// The states of the files after Git answered.
fn scan_second(
    pre: Vec<Pre>,
    files: &[SecretFileKey],
    ignored: &HashSet<String>,
    private: &PrivatePaths,
) -> Vec<SecretFileState> {
    let second = |(pre, file): (Pre, &SecretFileKey)| {
        let parent = match pre {
            Ok(parent) if ignored.contains(&file.path) => parent,
            Ok(_) => return refused(&file.path, NOT_IGNORED.code),
            Err(state) => return state,
        };
        let Some(parent) = parent else {
            return missing(&file.path);
        };
        let name = split(&file.path).1;
        match read_file(parent.as_fd(), name, &file.path, private, &file.salt) {
            Ok(None) => missing(&file.path),
            Ok(Some(c)) => SecretFileState {
                state: SecretPresence::Present,
                size: Some(c.bytes.len() as u64),
                mtime_ms: Some(c.mtime_ms),
                digest: Some(c.digest),
                ..state(&file.path, SecretPresence::Present, None)
            },
            Err(e) => refused(&file.path, e.code),
        }
    };
    pre.into_iter().zip(files).map(second).collect()
}

fn joined<T>(result: Result<T, tokio::task::JoinError>) -> Result<T, SecretError> {
    result.map_err(|_| INTERNAL)
}

fn answer(value: impl serde::Serialize) -> Result<Value, SecretError> {
    serde_json::to_value(value).map_err(|_| INTERNAL)
}

/// What every call starts with: the home and its repository, the open home
/// folder and the locks of the files.
type Start = (PathBuf, Arc<OwnedFd>, Vec<tokio::sync::OwnedMutexGuard<()>>);

async fn begin(
    env: &Env<'_>,
    (project, generation): (&str, u64),
    (dest, remote): (&str, &str),
    paths: Vec<String>,
) -> Result<Start, SecretError> {
    check_remote_name(remote)?;
    let home = env.home(project, generation, dest, remote).await?;
    let guards = lock_files(project, paths).await;
    let root = Arc::new(open_root(&home).map_err(|_| NOT_A_HOME)?);
    Ok((home, root, guards))
}

async fn scan(env: &Env<'_>, p: SecretScanParams) -> Result<Value, SecretError> {
    if p.files.len() > MAX_SECRET_FILES || p.files.iter().any(|f| decode_salt(&f.salt).is_none()) {
        return Err(INVALID_PARAMS);
    }
    let paths = p.files.iter().map(|f| f.path.clone()).collect();
    let (home, root, _guards) = begin(
        env,
        (&p.project_id, p.generation),
        (&p.dest, &p.remote_name),
        paths,
    )
    .await?;
    let files = Arc::new(p.files);
    let pre = {
        let (root, files) = (Arc::clone(&root), Arc::clone(&files));
        joined(tokio::task::spawn_blocking(move || scan_first(root.as_fd(), &files)).await)?
    };
    let batch: Vec<String> = pre
        .iter()
        .zip(files.iter())
        .filter(|(pre, _)| pre.is_ok())
        .map(|(_, file)| file.path.clone())
        .collect();
    let ignored = if batch.is_empty() {
        HashSet::new()
    } else {
        ignored_among(env.options, &home, &batch).await
    };
    let private = Arc::clone(&env.private);
    let states = joined(
        tokio::task::spawn_blocking(move || scan_second(pre, &files, &ignored, &private)).await,
    )?;
    answer(SecretScanAnswer { files: states })
}

/// The parent folder of `path` (`None`: missing): the path rule, the folders
/// without a link, then Git.
async fn readable_parent(
    env: &Env<'_>,
    home: &Path,
    root: &Arc<OwnedFd>,
    path: &str,
) -> Result<Option<OwnedFd>, SecretError> {
    check_secret_path(path).map_err(path_error)?;
    let parent = open_parent(root.as_fd(), &split(path).0)?;
    let ignored = ignored_among(env.options, home, &[path.to_owned()]).await;
    ignored.contains(path).then_some(parent).ok_or(NOT_IGNORED)
}

async fn read_call(env: &Env<'_>, p: &SecretReadParams) -> Result<Value, SecretError> {
    if decode_salt(&p.salt).is_none() {
        return Err(INVALID_PARAMS);
    }
    let (home, root, _guards) = begin(
        env,
        (&p.project_id, p.generation),
        (&p.dest, &p.remote_name),
        vec![p.path.clone()],
    )
    .await?;
    let Some(parent) = readable_parent(env, &home, &root, &p.path).await? else {
        return Err(MISSING);
    };
    let private = Arc::clone(&env.private);
    let (path, salt) = (p.path.clone(), p.salt.clone());
    let current = joined(
        tokio::task::spawn_blocking(move || {
            read_file(parent.as_fd(), split(&path).1, &path, &private, &salt)
        })
        .await,
    )??;
    let Some(content) = current else {
        return Err(MISSING);
    };
    if content.digest != p.digest {
        return Err(CHANGED);
    }
    answer(SecretReadAnswer {
        content_b64: STANDARD.encode(&content.bytes),
    })
}

struct Job<'a> {
    parent: BorrowedFd<'a>,
    name: &'a str,
    rel: &'a str,
    salt: &'a str,
    project_id: &'a str,
    private: &'a PrivatePaths,
    data_dir: Option<&'a Path>,
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
fn backup_folder(data_dir: &Path, project_id: &str, path: &str) -> Option<PathBuf> {
    let id_ok = (1..=64).contains(&project_id.len())
        && project_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
    let hash = ring::digest::digest(&ring::digest::SHA256, path.as_bytes());
    let dir = data_dir.join("secret-backups").join(project_id);
    id_ok.then(|| dir.join(&lower_hex(hash.as_ref())[..16]))
}

/// The sort key of a backup name: `<ms>` or `<ms>-<n>`. `None`: not a backup.
fn backup_key(name: &str) -> Option<(u64, u64)> {
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
/// cannot lose an edit. `between` runs after the move (a test hook).
fn replace(
    job: &Job<'_>,
    tmp: &str,
    digest: &str,
    between: impl FnOnce(),
) -> Result<bool, SecretError> {
    let aside = format!(".{}.fleetdeck-old-{}", job.name, random_hex()?);
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
        return replace_matching(job, tmp, &aside, old);
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

fn replace_matching(
    job: &Job<'_>,
    tmp: &str,
    aside: &str,
    old: &Content,
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
    match restore(job.parent, tmp, job.name) {
        Ok(()) => {
            remove(job.parent, aside);
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

fn commit(
    job: &Job<'_>,
    content: &[u8],
    expect: Option<&str>,
    between: impl FnOnce(),
) -> Result<bool, SecretError> {
    let tmp = format!(".{}.fleetdeck-tmp-{}", job.name, random_hex()?);
    write_temp(job, &tmp, content)?;
    match expect {
        None => place_new(job, &tmp),
        Some(digest) => replace(job, &tmp, digest, between),
    }
}

async fn write_call(env: &Env<'_>, p: SecretWriteParams) -> Result<Value, SecretError> {
    if decode_salt(&p.salt).is_none() {
        return Err(INVALID_PARAMS);
    }
    let content = STANDARD
        .decode(p.content_b64.as_bytes())
        .map_err(|_| INVALID_PARAMS)?;
    if content.len() as u64 > MAX_SECRET_BYTES {
        return Err(TOO_LARGE);
    }
    if secret_digest(&p.salt, &content).as_deref() != Some(p.digest.as_str()) {
        return Err(DIGEST_MISMATCH);
    }
    let (home, root, _guards) = begin(
        env,
        (&p.project_id, p.generation),
        (&p.dest, &p.remote_name),
        vec![p.path.clone()],
    )
    .await?;
    readable_parent(env, &home, &root, &p.path).await?;
    let private = Arc::clone(&env.private);
    let data_dir = env.data_dir.map(Path::to_path_buf);
    let made = joined(
        tokio::task::spawn_blocking(move || {
            let (parents, name) = split(&p.path);
            let parent = open_chain(root.as_fd(), &parents, true).map_err(|e| match e {
                "exists" => SYMLINK,
                _ => UNREADABLE,
            })?;
            let job = Job {
                parent: parent.as_fd(),
                name,
                rel: &p.path,
                salt: &p.salt,
                project_id: &p.project_id,
                private: &private,
                data_dir: data_dir.as_deref(),
            };
            commit(&job, &content, p.expect.as_deref(), || {})
        })
        .await,
    )??;
    answer(SecretWriteAnswer { backup: made })
}

/// Runs one secret method and logs its outcome code (decision 17).
///
/// # Errors
/// A [`SecretError`] with the wire code and the text.
pub async fn secret_call(
    store: &Store,
    policy: &Policy,
    private: Arc<PrivatePaths>,
    options: &GitOptions,
    data_dir: Option<&Path>,
    method: &str,
    params: Value,
) -> Result<Value, SecretError> {
    let env = Env {
        store,
        policy,
        private,
        options,
        data_dir,
    };
    let (project, path, result) = match method {
        "project.secret_scan" => {
            let p: SecretScanParams = parse(params)?;
            (p.project_id.clone(), None, scan(&env, p).await)
        }
        "project.secret_read" => {
            let p: SecretReadParams = parse(params)?;
            let path = Some(p.path.clone());
            (p.project_id.clone(), path, read_call(&env, &p).await)
        }
        "project.secret_write" => {
            let p: SecretWriteParams = parse(params)?;
            let (project, path) = (p.project_id.clone(), Some(p.path.clone()));
            (project, path, write_call(&env, p).await)
        }
        _ => return Err(UNKNOWN_METHOD),
    };
    // The outcome code, never a content, a digest or a salt (decision 17).
    let outcome = result.as_ref().map_or_else(|e| e.code, |_| "ok");
    let path = path.unwrap_or_default();
    if method == "project.secret_write" {
        tracing::info!(project, path, method, outcome, "secret files");
    } else {
        tracing::debug!(project, path, method, outcome, "secret files");
    }
    result
}

#[cfg(test)]
#[path = "secrets_tests.rs"]
mod tests;
