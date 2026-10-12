//! The read-only file service (PLAN §6.6, M3 DL-088): list a directory,
//! read a file, and search names or content below a root.
//!
//! The walls:
//!
//! - the root is canonical and inside the allowed roots (the caller resolves
//!   it); every query path is relative and validated lexically first
//!   ([`fleetdeck_proto::files::path_segments`]);
//! - no symlink is ever followed, also when one is swapped in while a query
//!   runs: the root opens once as a directory descriptor (walked from `/`
//!   component by component, so a swapped ancestor is refused too), and every
//!   component below it opens relative to its parent's descriptor with
//!   `O_NOFOLLOW` (directories with `O_DIRECTORY`, files with `O_NONBLOCK`,
//!   so a FIFO never blocks). A path is never resolved again by name, so a
//!   directory replaced by a symlink after a check cannot redirect a later
//!   step (M3 round 12; the review found that the earlier `lstat` walk
//!   protected only the final component);
//! - `.git` and credential directories are never listed, opened or searched,
//!   and neither is a root inside one ([`private_root`]; finding 2 of
//!   sa-13931f53);
//! - every walk and read is bounded, and each cut is reported.

use std::ffi::CStr;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;

use fleetdeck_proto::files::{
    FileContent, FileEntry, FileKind, FileListing, MAX_HIT_CHARS, MAX_LIST_ENTRIES, MAX_READ_BYTES,
    SearchHit, code, hidden_name, hidden_path, path_segments,
};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat};

/// The most directory entries read from one directory.
pub const MAX_SCANNED: usize = 20_000;
/// The first bytes that decide whether a file is binary.
pub const BINARY_SNIFF_BYTES: usize = 8000;
/// Walk search: the deepest directory level.
pub const WALK_DEPTH_MAX: usize = 12;
/// Walk search: the most directory entries looked at.
pub const WALK_ENTRY_MAX: usize = 20_000;
/// Walk search: the most files opened for content.
pub const WALK_FILE_MAX: usize = 2000;
/// Walk search: larger files are not read for content.
pub const WALK_FILE_BYTES: u64 = 1024 * 1024;
/// Content search: the most hits from one file.
pub const PER_FILE_HITS: usize = 5;
/// Directories that a search does not enter (generated or dependency
/// trees); list and read still show them.
pub const SEARCH_PRUNED: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "coverage",
    ".next",
    ".turbo",
];

/// Why the file service refuses a query.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct FileError {
    /// One of [`fleetdeck_proto::files::code`].
    pub code: &'static str,
    pub message: String,
}

impl FileError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn not_found(path: &str) -> FileError {
    FileError::new(code::NOT_FOUND, format!("`{path}` is not found"))
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn file_flags() -> OFlags {
    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY
}

/// Opens the canonical, absolute `root` as a directory descriptor without
/// following a symlink at any level: the walk starts at `/` and opens each
/// component relative to its parent's descriptor with `O_NOFOLLOW`, so an
/// ancestor of the root swapped for a symlink is refused too (M3 round 13,
/// DL-092). A root that is not absolute or has `.`/`..` components is
/// refused; callers pass the canonical root from the allowed-roots policy.
///
/// # Errors
/// The `openat` error of the first component that is missing, not a
/// directory or a symlink; `EINVAL` for a root that is not canonical.
pub fn open_root(root: &Path) -> rustix::io::Result<OwnedFd> {
    use std::path::Component;
    if !root.is_absolute() {
        return Err(rustix::io::Errno::INVAL);
    }
    let mut dir = rustix::fs::open("/", dir_flags(), Mode::empty())?;
    for component in root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                dir = rustix::fs::openat(&dir, name, dir_flags(), Mode::empty())?;
            }
            Component::Prefix(_) | Component::CurDir | Component::ParentDir => {
                return Err(rustix::io::Errno::INVAL);
            }
        }
    }
    Ok(dir)
}

/// A path that names exactly the directory `dir` for a child process's
/// working directory: `/proc/self/fd/<n>`. The child resolves it before
/// `exec`, while it still holds the inherited descriptor (close-on-exec
/// closes it only at `exec`), so a rename of the root or of one of its
/// ancestors cannot move the child elsewhere. The caller keeps `dir` open
/// until the child has started. `None` where no such path exists (macOS):
/// the caller then must not run a child in the directory by name.
#[must_use]
pub fn pinned_dir(dir: &OwnedFd) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        Some(std::path::PathBuf::from(format!(
            "/proc/self/fd/{}",
            dir.as_raw_fd()
        )))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = dir;
        None
    }
}

/// Opens the directory `segments` below `base`, one component at a time,
/// each relative to its parent's descriptor and without following a
/// symlink.
fn descend(base: &OwnedFd, segments: &[&str]) -> rustix::io::Result<OwnedFd> {
    let mut dir = rustix::fs::openat(base, c".", dir_flags(), Mode::empty())?;
    for segment in segments {
        dir = rustix::fs::openat(&dir, *segment, dir_flags(), Mode::empty())?;
    }
    Ok(dir)
}

/// The `lstat` of `name` in the directory `dir`.
fn stat_at(dir: &OwnedFd, name: &str) -> rustix::io::Result<Stat> {
    rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)
}

fn kind_of(file_type: FileType) -> FileKind {
    match file_type {
        FileType::Symlink => FileKind::Symlink,
        FileType::Directory => FileKind::Dir,
        FileType::RegularFile => FileKind::File,
        _ => FileKind::Other,
    }
}

fn kind_of_stat(stat: &Stat) -> FileKind {
    kind_of(FileType::from_raw_mode(stat.st_mode))
}

fn size_of(stat: &Stat) -> u64 {
    u64::try_from(stat.st_size).unwrap_or(0)
}

/// Splits a validated query path into its segments.
fn segments_of(path: &str) -> Result<Vec<&str>, FileError> {
    path_segments(path).map_err(|c| {
        if c == code::NOT_FOUND {
            not_found(path)
        } else {
            FileError::new(c, "a path is relative, without `.` or `..`, NUL or `\\`")
        }
    })
}

/// Opens the parent directory of the last segment and checks the last
/// segment's kind without following it. `None` for the root itself.
fn parent_and_last<'p>(
    root: &OwnedFd,
    segments: &[&'p str],
    path: &str,
) -> Result<Option<(OwnedFd, &'p str, FileKind)>, FileError> {
    let Some((last, parents)) = segments.split_last() else {
        return Ok(None);
    };
    let parent = descend(root, parents).map_err(|_| not_found(path))?;
    let stat = stat_at(&parent, last).map_err(|_| not_found(path))?;
    match kind_of_stat(&stat) {
        FileKind::Symlink => Err(not_found(path)),
        kind => Ok(Some((parent, last, kind))),
    }
}

/// The names in the directory `dir` (without `.` and `..` and without
/// names that are not UTF-8), at most `max + 1` of them; true when the
/// directory has more.
fn names_in(dir: &OwnedFd, max: usize) -> (Vec<String>, bool) {
    let Ok(entries) = Dir::read_from(dir) else {
        return (Vec::new(), false);
    };
    let mut names = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name: &CStr = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        if names.len() >= max {
            return (names, true);
        }
        if let Ok(name) = name.to_str() {
            names.push(name.to_owned());
        }
    }
    (names, false)
}

/// Lists the directory `path` below `root`: directories first, then by
/// name. Hidden names and names that are not UTF-8 are left out.
///
/// # Errors
/// [`FileError`] with `invalid_path`, `not_found` or `wrong_kind`.
pub fn list(root: &Path, path: &str) -> Result<FileListing, FileError> {
    let segments = segments_of(path)?;
    let base = open_root(root).map_err(|_| not_found(path))?;
    let dir = match parent_and_last(&base, &segments, path)? {
        None => descend(&base, &[]).map_err(|_| not_found(path))?,
        Some((_, _, kind)) if kind != FileKind::Dir => {
            return Err(FileError::new(
                code::WRONG_KIND,
                format!("`{path}` is not a directory"),
            ));
        }
        Some((parent, last, _)) => descend(&parent, &[last]).map_err(|_| not_found(path))?,
    };
    let (names, mut truncated) = names_in(&dir, MAX_SCANNED);
    let mut entries = Vec::new();
    for name in names {
        if hidden_name(&name) {
            continue;
        }
        let Ok(stat) = stat_at(&dir, &name) else {
            continue;
        };
        let kind = kind_of_stat(&stat);
        entries.push(FileEntry {
            name,
            kind,
            size: (kind == FileKind::File).then(|| size_of(&stat)),
        });
    }
    entries.sort_by(|a, b| {
        (a.kind != FileKind::Dir)
            .cmp(&(b.kind != FileKind::Dir))
            .then_with(|| a.name.cmp(&b.name))
    });
    if entries.len() > MAX_LIST_ENTRIES {
        entries.truncate(MAX_LIST_ENTRIES);
        truncated = true;
    }
    Ok(FileListing {
        root: root.to_string_lossy().into_owned(),
        path: path.trim_end_matches('/').to_owned(),
        entries,
        truncated,
    })
}

/// Opens the regular file `name` in the directory `dir` without following
/// a symlink and without blocking on a FIFO.
fn open_file(dir: &OwnedFd, name: &str, path: &str) -> Result<(File, u64), FileError> {
    let fd =
        rustix::fs::openat(dir, name, file_flags(), Mode::empty()).map_err(|_| not_found(path))?;
    let stat = rustix::fs::fstat(fd.as_fd()).map_err(|_| not_found(path))?;
    if kind_of_stat(&stat) != FileKind::File {
        return Err(FileError::new(
            code::WRONG_KIND,
            format!("`{path}` is not a regular file"),
        ));
    }
    Ok((File::from(fd), size_of(&stat)))
}

/// Reads at most `max_bytes` (capped at [`MAX_READ_BYTES`]) of the file
/// `path` below `root`.
///
/// # Errors
/// [`FileError`] with `invalid_path`, `not_found` or `wrong_kind`.
pub fn read(root: &Path, path: &str, max_bytes: Option<u64>) -> Result<FileContent, FileError> {
    if path.is_empty() {
        return Err(FileError::new(code::WRONG_KIND, "the root is a directory"));
    }
    let segments = segments_of(path)?;
    let base = open_root(root).map_err(|_| not_found(path))?;
    let Some((parent, last, kind)) = parent_and_last(&base, &segments, path)? else {
        return Err(FileError::new(code::WRONG_KIND, "the root is a directory"));
    };
    if kind != FileKind::File {
        return Err(FileError::new(
            code::WRONG_KIND,
            format!("`{path}` is not a regular file"),
        ));
    }
    let (file, size) = open_file(&parent, last, path)?;
    let limit = max_bytes.unwrap_or(MAX_READ_BYTES).min(MAX_READ_BYTES);
    let mut buf = Vec::new();
    file.take(limit)
        .read_to_end(&mut buf)
        .map_err(|e| FileError::new(code::NOT_FOUND, format!("`{path}`: {e}")))?;
    let binary = buf[..buf.len().min(BINARY_SNIFF_BYTES)].contains(&0);
    let truncated = size > buf.len() as u64;
    Ok(FileContent {
        path: path.to_owned(),
        size,
        binary,
        text: (!binary).then(|| String::from_utf8_lossy(&buf).into_owned()),
        truncated,
    })
}

/// Cuts a hit line to [`MAX_HIT_CHARS`] characters.
#[must_use]
pub fn clip(line: &str) -> String {
    let line = line.trim_end_matches(['\r', '\n']);
    match line.char_indices().nth(MAX_HIT_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    }
}

/// The result of a bounded search.
#[derive(Debug, Default)]
pub struct Found {
    pub hits: Vec<SearchHit>,
    pub truncated: bool,
}

impl Found {
    /// Adds a hit; false when the hit limit is reached.
    pub fn push(&mut self, hit: SearchHit, max: usize) -> bool {
        if self.hits.len() >= max {
            self.truncated = true;
            return false;
        }
        self.hits.push(hit);
        true
    }
}

/// Searches a directory that is not a Git worktree: a bounded walk that
/// never follows a symlink and never enters a hidden or pruned directory.
/// `needle` is lower case already.
pub fn walk_search(root: &Path, needle: &str, content: bool, max: usize) -> Found {
    let mut found = Found::default();
    let Ok(base) = open_root(root) else {
        return found;
    };
    let mut queue = std::collections::VecDeque::from([(String::new(), 0usize)]);
    let (mut seen, mut opened) = (0usize, 0usize);
    while let Some((rel, depth)) = queue.pop_front() {
        // Each directory opens again from the root descriptor, one
        // component at a time without following a symlink: a directory
        // that became a symlink after it was queued is skipped.
        let segments: Vec<&str> = if rel.is_empty() {
            Vec::new()
        } else {
            rel.split('/').collect()
        };
        let Ok(dir) = descend(&base, &segments) else {
            continue;
        };
        let (mut names, more) = names_in(&dir, WALK_ENTRY_MAX);
        names.sort();
        if more {
            found.truncated = true;
        }
        for name in names {
            seen += 1;
            if seen > WALK_ENTRY_MAX {
                found.truncated = true;
                return found;
            }
            if hidden_name(&name) {
                continue;
            }
            let Ok(stat) = stat_at(&dir, &name) else {
                continue;
            };
            let kind = kind_of_stat(&stat);
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if kind == FileKind::Dir {
                if SEARCH_PRUNED.iter().any(|p| p.eq_ignore_ascii_case(&name)) {
                    continue;
                }
                if depth + 1 >= WALK_DEPTH_MAX {
                    found.truncated = true;
                } else {
                    queue.push_back((path, depth + 1));
                }
                continue;
            }
            if kind != FileKind::File {
                continue;
            }
            if !content {
                if path.to_lowercase().contains(needle)
                    && !found.push(
                        SearchHit {
                            path,
                            line: None,
                            text: None,
                        },
                        max,
                    )
                {
                    return found;
                }
                continue;
            }
            opened += 1;
            if opened > WALK_FILE_MAX {
                found.truncated = true;
                return found;
            }
            if !grep_file(&dir, &name, &path, needle, max, &mut found) {
                return found;
            }
        }
    }
    found
}

/// True when the canonical `root` is a private directory or lies inside
/// one: any segment of the whole `root` path is a hidden name
/// (case-insensitive), including the segments of the allowed root that
/// holds it, so an allowed-root entry never masks a private directory
/// (finding 1 of sa-fbed2a6d). A root that no allowed root holds is
/// private too (fail closed).
pub fn private_root(root: &Path, allowed: &[std::path::PathBuf]) -> bool {
    if !allowed.iter().any(|a| root.starts_with(a)) {
        return true;
    }
    root.components().any(|c| match c {
        std::path::Component::Normal(name) => name.to_str().is_none_or(hidden_name),
        std::path::Component::RootDir => false,
        _ => true,
    })
}

/// Searches the content of candidate `paths` (for example the paths that
/// `git ls-files` listed) below the directory that `base` holds. Each file
/// opens from `base`, one component at a time, without following a
/// symlink, so a candidate whose directory became a symlink is skipped:
/// Git may name the candidates, but it never reads their content for the
/// station (finding 1 of sa-13931f53). Hidden paths are skipped; at most
/// [`WALK_FILE_MAX`] files open. `needle` is lower case already.
#[must_use]
pub fn grep_paths(base: &OwnedFd, paths: &[String], needle: &str, max: usize) -> Found {
    let mut found = Found::default();
    let mut opened = 0usize;
    for path in paths {
        if hidden_path(path) {
            continue;
        }
        let Ok(segments) = path_segments(path) else {
            continue;
        };
        let Ok(Some((parent, last, FileKind::File))) = parent_and_last(base, &segments, path)
        else {
            continue;
        };
        opened += 1;
        if opened > WALK_FILE_MAX {
            found.truncated = true;
            return found;
        }
        if !grep_file(&parent, last, path, needle, max, &mut found) {
            return found;
        }
    }
    found
}

/// Adds the content hits of one file; false when the hit limit is reached.
/// Only the first [`WALK_FILE_BYTES`] of a file are searched; a larger file
/// or a read error marks the search `truncated`, so a match past the limit
/// is never silently missing (finding 2 of sa-fbed2a6d). A binary file is
/// skipped (as `git grep -I` does).
fn grep_file(
    dir: &OwnedFd,
    name: &str,
    path: &str,
    needle: &str,
    max: usize,
    found: &mut Found,
) -> bool {
    let Ok((file, size)) = open_file(dir, name, path) else {
        return true;
    };
    let mut buf = Vec::new();
    if file.take(WALK_FILE_BYTES).read_to_end(&mut buf).is_err() {
        found.truncated = true;
        return true;
    }
    if buf[..buf.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
        return true;
    }
    let cut = size > WALK_FILE_BYTES || buf.len() as u64 >= WALK_FILE_BYTES;
    if cut {
        found.truncated = true;
        // The last line of the prefix may be cut: search complete lines.
        if let Some(end) = buf.iter().rposition(|b| *b == b'\n') {
            buf.truncate(end + 1);
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut in_file = 0;
    for (n, line) in text.lines().enumerate() {
        if !line.to_lowercase().contains(needle) {
            continue;
        }
        let hit = SearchHit {
            path: path.to_owned(),
            line: u32::try_from(n + 1).ok(),
            text: Some(clip(line)),
        };
        if !found.push(hit, max) {
            return false;
        }
        in_file += 1;
        if in_file >= PER_FILE_HITS {
            break;
        }
    }
    true
}

#[cfg(test)]
#[path = "files_race_tests.rs"]
mod race_tests;
