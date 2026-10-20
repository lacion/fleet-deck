//! The fleet skill tree of MCP P3 (`docs/v2/design/mcp-p3.md` §4.1): the two
//! embedded files are published once at the start of the station into
//! `<station dir>/share/fleet-skill/<hash>/`, and every provider session gets
//! the absolute path (Claude `--plugin-dir`, Codex `skills/extraRoots/set`).
//!
//! The publisher never follows a link. It opens the station directory with
//! `O_NOFOLLOW`, checks each directory on the way (the station user's, no
//! group or other write bit), and then works only with `*at` calls relative
//! to the descriptor of `share/fleet-skill`, under an exclusive `flock` on
//! `.lock`. A tree that is already there is verified byte for byte; a tree
//! that does not match is renamed to `<hash>.bad-<unix ms>` and published
//! again. A new tree is written into `.tmp-<random>/`, flushed and renamed.

use std::ffi::OsStr;
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ring::digest::{Context, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use rustix::fs::{AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, Stat};
use rustix::io::Errno;

/// The bytes of `.claude-plugin/plugin.json`. `version` is the literal `"2"`:
/// `include_str!` cannot fill the package version.
const PLUGIN_JSON: &str = include_str!("fleet_skill/plugin.json");
/// The bytes of `skills/fleet/SKILL.md` (design §4.3).
const SKILL_MD: &str = include_str!("fleet_skill/SKILL.md");

/// The files of the tree, in the fixed order of the hash.
const FILES: [(&str, &str); 2] = [
    (".claude-plugin/plugin.json", PLUGIN_JSON),
    ("skills/fleet/SKILL.md", SKILL_MD),
];
/// The directories of the tree, parents first.
const DIRS: [&str; 3] = [".claude-plugin", "skills", "skills/fleet"];

/// The folder of the trees below the station directory.
const SHARE: &str = "share";
const TREES: &str = "fleet-skill";
const LOCK: &str = ".lock";
const TEMP_PREFIX: &str = ".tmp-";
const BAD_INFIX: &str = ".bad-";

/// How long the publisher waits for the lock (design §4.1).
const LOCK_WAIT: Duration = Duration::from_secs(10);
const LOCK_POLL: Duration = Duration::from_millis(20);

/// The most bytes read from a file of an existing tree: more than the
/// embedded content cannot be equal to it.
const READ_SLACK: u64 = 1;

/// Why a step of the publication failed. The text goes to the log.
#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error("{what}: {source}")]
    Io {
        what: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Unsafe(String),
    #[error("the lock was not free within the bound")]
    LockBusy,
    #[error("interrupted before the rename (test)")]
    Interrupted,
}

fn io(what: &str) -> impl FnOnce(Errno) -> Failure + '_ {
    move |errno| Failure::Io {
        what: what.to_owned(),
        source: errno.into(),
    }
}

fn io_std(what: &str) -> impl FnOnce(std::io::Error) -> Failure + '_ {
    move |source| Failure::Io {
        what: what.to_owned(),
        source,
    }
}

/// Test hooks and bounds of a publication.
#[derive(Default)]
pub(crate) struct Options<'a> {
    /// The wait for the lock; `None` is the 10 s of the design.
    pub lock_wait: Option<Duration>,
    /// Called under the lock after an existing tree was verified (or found
    /// absent).
    pub after_verify: Option<&'a dyn Fn()>,
    /// Called under the lock right before the rename; `true` stops the
    /// publication there and leaves the temporary directory.
    pub stop_before_rename: Option<&'a dyn Fn() -> bool>,
}

/// The SHA-256 of the two relative paths and contents, in a fixed order, as
/// lowercase hex.
#[must_use]
pub fn tree_hash() -> String {
    let mut context = Context::new(&SHA256);
    for (path, content) in FILES {
        context.update(path.as_bytes());
        context.update(&[0]);
        context.update(content.as_bytes());
        context.update(&[0]);
    }
    context
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Publishes the fleet skill tree and returns the absolute path of
/// `<station dir>/share/fleet-skill/<hash>/`. `None` (with a log line) when a
/// check fails, the lock is not free within 10 s, or a write fails: no
/// session of this station run gets the skill then.
///
/// It blocks (file I/O and the lock wait): call it from a blocking context.
#[must_use]
pub fn publish(station_dir: &Path) -> Option<PathBuf> {
    publish_with(station_dir, &Options::default())
}

/// [`publish`] with the test hooks.
#[must_use]
pub(crate) fn publish_with(station_dir: &Path, options: &Options<'_>) -> Option<PathBuf> {
    match try_publish(station_dir, options) {
        Ok(path) => Some(path),
        Err(error) => {
            tracing::error!(%error, "the fleet skill tree was not published; sessions start without it");
            None
        }
    }
}

/// The publication of the station start: [`publish`], then [`prune_old`]
/// only when no session of the station is live (`live_sessions` is 0). A
/// shim-hosted provider that survived a restart may still read an old tree.
#[must_use]
pub fn publish_at_start(station_dir: &Path, live_sessions: usize) -> Option<PathBuf> {
    let path = publish(station_dir)?;
    if live_sessions == 0 {
        let removed = prune_old(station_dir, &path);
        if removed > 0 {
            tracing::info!(removed, "old fleet skill trees removed");
        }
    } else {
        tracing::info!(
            live_sessions,
            "old fleet skill trees stay: a session of this station is live"
        );
    }
    Some(path)
}

/// Removes the trees of other hashes (and their quarantined copies) below
/// `keep`'s parent, under the lock. The caller runs it at the start of the
/// station and only when no session of the station is live (design §4.1).
/// Returns the number of removed entries; a failure is logged.
pub fn prune_old(station_dir: &Path, keep: &Path) -> usize {
    let Some(current) = keep.file_name().and_then(OsStr::to_str) else {
        return 0;
    };
    let result = with_lock(station_dir, None, |trees| {
        let mut removed = 0;
        for name in names(trees)? {
            let old_tree = name.split(BAD_INFIX).next().is_some_and(|head| {
                head.len() == 64 && head.bytes().all(|b| b.is_ascii_hexdigit())
            });
            let is_current = name == current;
            let current_quarantine = name.starts_with(&format!("{current}{BAD_INFIX}"));
            if old_tree && !is_current && !current_quarantine {
                remove_tree(trees, &name)?;
                removed += 1;
            }
        }
        Ok(removed)
    });
    match result {
        Ok(removed) => removed,
        Err(error) => {
            tracing::warn!(%error, "old fleet skill trees were not removed");
            0
        }
    }
}

fn try_publish(station_dir: &Path, options: &Options<'_>) -> Result<PathBuf, Failure> {
    let absolute = std::path::absolute(station_dir).map_err(io_std("the station directory"))?;
    let hash = tree_hash();
    with_lock(station_dir, options.lock_wait, |trees| {
        // A crash before a rename leaves a `.tmp-*` directory: remove it.
        for name in names(trees)? {
            if name.starts_with(TEMP_PREFIX) {
                remove_tree(trees, &name)?;
            }
        }
        match rustix::fs::statat(trees, hash.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => {
                if !verified(trees, &hash, &stat) {
                    quarantine(trees, &hash)?;
                }
            }
            Err(Errno::NOENT) => {}
            Err(errno) => return Err(io("the tree")(errno)),
        }
        if let Some(hook) = options.after_verify {
            hook();
        }
        if rustix::fs::statat(trees, hash.as_str(), AtFlags::SYMLINK_NOFOLLOW).is_err() {
            write_tree(trees, &hash, options)?;
        }
        Ok(absolute.join(SHARE).join(TREES).join(&hash))
    })
}

// ---------------------------------------------------------------------------
// The directories and the lock.
// ---------------------------------------------------------------------------

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// A directory that is real, the station user's and not writable by group or
/// other.
fn check_dir(fd: &OwnedFd, what: &str) -> Result<(), Failure> {
    let stat = rustix::fs::fstat(fd).map_err(io(what))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Err(Failure::Unsafe(format!("{what} is not a directory")));
    }
    if stat.st_uid != euid() {
        return Err(Failure::Unsafe(format!(
            "{what} is not owned by the station user"
        )));
    }
    if stat.st_mode & 0o022 != 0 {
        return Err(Failure::Unsafe(format!(
            "{what} is writable by group or other"
        )));
    }
    Ok(())
}

/// Opens `name` below `parent` as a real directory, creating it with mode
/// 0700 when it is missing, and checks it.
fn open_checked(parent: &OwnedFd, name: &str) -> Result<OwnedFd, Failure> {
    match rustix::fs::mkdirat(parent, name, Mode::from_bits_truncate(0o700)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(errno) => return Err(io(name)(errno)),
    }
    let fd = rustix::fs::openat(parent, name, dir_flags(), Mode::empty()).map_err(|errno| {
        if errno == Errno::LOOP || errno == Errno::NOTDIR {
            Failure::Unsafe(format!("{name} is not a real directory"))
        } else {
            io(name)(errno)
        }
    })?;
    check_dir(&fd, name)?;
    Ok(fd)
}

/// Runs `body` with the descriptor of `share/fleet-skill` while this process
/// holds the exclusive lock on `.lock`.
fn with_lock<T>(
    station_dir: &Path,
    wait: Option<Duration>,
    body: impl FnOnce(&OwnedFd) -> Result<T, Failure>,
) -> Result<T, Failure> {
    let root = rustix::fs::openat(rustix::fs::CWD, station_dir, dir_flags(), Mode::empty())
        .map_err(|errno| {
            if errno == Errno::LOOP || errno == Errno::NOTDIR {
                Failure::Unsafe("the station directory is not a real directory".to_owned())
            } else {
                io("the station directory")(errno)
            }
        })?;
    check_dir(&root, "the station directory")?;
    let share = open_checked(&root, SHARE)?;
    let trees = open_checked(&share, TREES)?;
    let lock = rustix::fs::openat(
        &trees,
        LOCK,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(io("the lock file"))?;
    let stat = rustix::fs::fstat(&lock).map_err(io("the lock file"))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_uid != euid() {
        return Err(Failure::Unsafe(
            "the lock file is not a regular file of the station user".to_owned(),
        ));
    }
    let deadline = Instant::now() + wait.unwrap_or(LOCK_WAIT);
    loop {
        match rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => break,
            Err(Errno::WOULDBLOCK) if Instant::now() < deadline => std::thread::sleep(LOCK_POLL),
            Err(Errno::WOULDBLOCK) => return Err(Failure::LockBusy),
            Err(errno) => return Err(io("the lock")(errno)),
        }
    }
    // The lock ends when `lock` closes, after the body.
    let result = body(&trees);
    drop(lock);
    result
}

/// The entry names of a directory (those that are UTF-8).
fn names(dir: &OwnedFd) -> Result<Vec<String>, Failure> {
    let entries = Dir::read_from(dir).map_err(io("a directory listing"))?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io("a directory entry"))?;
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        if let Ok(name) = name.to_str() {
            out.push(name.to_owned());
        }
    }
    Ok(out)
}

/// Removes `name` below `parent`: a directory with everything in it, or a
/// file or a link (not followed).
fn remove_tree(parent: &OwnedFd, name: &str) -> Result<(), Failure> {
    let stat = match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(()),
        Err(errno) => return Err(io(name)(errno)),
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return rustix::fs::unlinkat(parent, name, AtFlags::empty()).map_err(io(name));
    }
    let dir = rustix::fs::openat(parent, name, dir_flags(), Mode::empty()).map_err(io(name))?;
    // A directory without the owner's write and search bits cannot be emptied.
    rustix::fs::fchmod(&dir, Mode::from_bits_truncate(0o700)).map_err(io(name))?;
    for child in names(&dir)? {
        remove_tree(&dir, &child)?;
    }
    drop(dir);
    rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(io(name))
}

// ---------------------------------------------------------------------------
// Verification, quarantine and writing.
// ---------------------------------------------------------------------------

/// Whether `<hash>/` is exactly the embedded tree.
fn verified(trees: &OwnedFd, hash: &str, stat: &Stat) -> bool {
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return false;
    }
    let Ok(top) = rustix::fs::openat(trees, hash, dir_flags(), Mode::empty()) else {
        return false;
    };
    verify_dir(&top, "").unwrap_or(false)
}

/// The names that directory `at` (relative to the tree root) must hold.
fn expected_names(at: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for path in DIRS.iter().copied().chain(FILES.iter().map(|(p, _)| *p)) {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        if parent == at {
            out.push(name);
        }
    }
    out
}

fn verify_dir(dir: &OwnedFd, at: &str) -> Result<bool, Failure> {
    let own = rustix::fs::fstat(dir).map_err(io("a tree directory"))?;
    if own.st_uid != euid() || own.st_mode & 0o022 != 0 {
        return Ok(false);
    }
    let mut found = names(dir)?;
    found.sort();
    let mut wanted: Vec<String> = expected_names(at).into_iter().map(str::to_owned).collect();
    wanted.sort();
    if found != wanted {
        return Ok(false);
    }
    for name in found {
        let path = if at.is_empty() {
            name.clone()
        } else {
            format!("{at}/{name}")
        };
        let stat = rustix::fs::statat(dir, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(io("a tree entry"))?;
        if stat.st_uid != euid() || stat.st_mode & 0o022 != 0 {
            return Ok(false);
        }
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory if DIRS.contains(&path.as_str()) => {
                let child = rustix::fs::openat(dir, name.as_str(), dir_flags(), Mode::empty())
                    .map_err(io("a tree directory"))?;
                if !verify_dir(&child, &path)? {
                    return Ok(false);
                }
            }
            FileType::RegularFile => {
                let Some((_, content)) = FILES.iter().find(|(p, _)| *p == path) else {
                    return Ok(false);
                };
                if !file_equals(dir, &name, content.as_bytes())? {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}

fn file_equals(dir: &OwnedFd, name: &str, expected: &[u8]) -> Result<bool, Failure> {
    let fd = rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io("a tree file"))?;
    let mut file = std::fs::File::from(fd);
    let mut content = Vec::with_capacity(expected.len() + 1);
    let limit = expected.len() as u64 + READ_SLACK;
    std::io::Read::by_ref(&mut file)
        .take(limit)
        .read_to_end(&mut content)
        .map_err(io_std("a tree file"))?;
    Ok(content == expected)
}

/// Renames `<hash>` to `<hash>.bad-<unix ms>`.
fn quarantine(trees: &OwnedFd, hash: &str) -> Result<(), Failure> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    tracing::warn!(
        tree = hash,
        "the published fleet skill tree does not match; it is renamed"
    );
    for bump in 0..16_u128 {
        let target = format!("{hash}{BAD_INFIX}{}", ms + bump);
        match rustix::fs::renameat(trees, hash, trees, target.as_str()) {
            Ok(()) => return Ok(()),
            Err(Errno::EXIST | Errno::NOTEMPTY) => {}
            Err(errno) => return Err(io("the quarantine rename")(errno)),
        }
    }
    Err(Failure::Unsafe(
        "no free name for the quarantine rename".to_owned(),
    ))
}

fn random_hex() -> Result<String, Failure> {
    let mut bytes = [0_u8; 8];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Failure::Unsafe("no randomness for the temporary name".to_owned()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Writes the tree into `.tmp-<random>/` and renames it to `<hash>`.
fn write_tree(trees: &OwnedFd, hash: &str, options: &Options<'_>) -> Result<(), Failure> {
    let temp_name = format!("{TEMP_PREFIX}{}", random_hex()?);
    rustix::fs::mkdirat(trees, temp_name.as_str(), Mode::from_bits_truncate(0o700))
        .map_err(io("the temporary directory"))?;
    let temp = rustix::fs::openat(trees, temp_name.as_str(), dir_flags(), Mode::empty())
        .map_err(io("the temporary directory"))?;
    let mut opened: Vec<(&str, OwnedFd)> = Vec::new();
    for dir in DIRS {
        let (parent_path, name) = dir.rsplit_once('/').unwrap_or(("", dir));
        let parent = opened
            .iter()
            .find(|(p, _)| *p == parent_path)
            .map_or(&temp, |(_, fd)| fd);
        rustix::fs::mkdirat(parent, name, Mode::from_bits_truncate(0o700)).map_err(io(dir))?;
        let fd = rustix::fs::openat(parent, name, dir_flags(), Mode::empty()).map_err(io(dir))?;
        opened.push((dir, fd));
    }
    for (path, content) in FILES {
        let (parent_path, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = opened
            .iter()
            .find(|(p, _)| *p == parent_path)
            .map_or(&temp, |(_, fd)| fd);
        let fd = rustix::fs::openat(
            parent,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o644),
        )
        .map_err(io(path))?;
        rustix::fs::fchmod(&fd, Mode::from_bits_truncate(0o644)).map_err(io(path))?;
        let mut file = std::fs::File::from(fd);
        file.write_all(content.as_bytes()).map_err(io_std(path))?;
        file.sync_all().map_err(io_std(path))?;
    }
    // The directories become 0755 after the write; children first.
    for (path, fd) in opened.iter().rev() {
        rustix::fs::fchmod(fd, Mode::from_bits_truncate(0o755)).map_err(io(path))?;
        rustix::fs::fsync(fd).map_err(io(path))?;
    }
    rustix::fs::fchmod(&temp, Mode::from_bits_truncate(0o755)).map_err(io("the tree"))?;
    rustix::fs::fsync(&temp).map_err(io("the tree"))?;
    if options.stop_before_rename.is_some_and(|stop| stop()) {
        return Err(Failure::Interrupted);
    }
    rustix::fs::renameat(trees, temp_name.as_str(), trees, hash).map_err(io("the rename"))?;
    rustix::fs::fsync(trees).map_err(io("the trees directory"))?;
    Ok(())
}

#[cfg(test)]
#[path = "fleet_skill_tests.rs"]
mod tests;
