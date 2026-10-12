//! The read-only file service (PLAN §6.6, M3 DL-088): list a directory,
//! read a file, and search names or content below a root.
//!
//! The walls:
//!
//! - the root is canonical and inside the allowed roots (the caller resolves
//!   it); every query path is relative and validated lexically first
//!   ([`fleetdeck_proto::files::path_segments`]);
//! - no symlink is ever followed: every component below the root is checked
//!   with `lstat`, and a file opens with `O_NOFOLLOW | O_NONBLOCK`, so a
//!   symlink or a FIFO swapped in at the last moment is refused too;
//! - `.git` and credential directories are never listed, opened or searched;
//! - every walk and read is bounded, and each cut is reported.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use fleetdeck_proto::files::{
    FileContent, FileEntry, FileKind, FileListing, MAX_HIT_CHARS, MAX_LIST_ENTRIES, MAX_READ_BYTES,
    SearchHit, code, hidden_name, path_segments,
};
use rustix::fs::{CWD, Mode, OFlags};

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

/// Resolves a relative `path` below `root` without following a symlink.
/// Returns the absolute path and the `lstat` of its last component (`None`
/// for the root itself).
fn resolve(root: &Path, path: &str) -> Result<(PathBuf, Option<std::fs::Metadata>), FileError> {
    let segments = path_segments(path).map_err(|c| {
        if c == code::NOT_FOUND {
            not_found(path)
        } else {
            FileError::new(c, "a path is relative, without `.` or `..`, NUL or `\\`")
        }
    })?;
    let mut abs = root.to_path_buf();
    let mut last = None;
    for segment in segments {
        abs.push(segment);
        let meta = std::fs::symlink_metadata(&abs).map_err(|_| not_found(path))?;
        if meta.file_type().is_symlink() {
            return Err(not_found(path));
        }
        last = Some(meta);
    }
    Ok((abs, last))
}

fn kind_of(file_type: std::fs::FileType) -> FileKind {
    if file_type.is_symlink() {
        FileKind::Symlink
    } else if file_type.is_dir() {
        FileKind::Dir
    } else if file_type.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    }
}

/// Lists the directory `path` below `root`: directories first, then by
/// name. Hidden names and names that are not UTF-8 are left out.
///
/// # Errors
/// [`FileError`] with `invalid_path`, `not_found` or `wrong_kind`.
pub fn list(root: &Path, path: &str) -> Result<FileListing, FileError> {
    let (dir, meta) = resolve(root, path)?;
    if meta.is_some_and(|m| !m.is_dir()) {
        return Err(FileError::new(
            code::WRONG_KIND,
            format!("`{path}` is not a directory"),
        ));
    }
    let mut entries = Vec::new();
    let mut truncated = false;
    let items = std::fs::read_dir(&dir).map_err(|_| not_found(path))?;
    for (n, item) in items.enumerate() {
        if n >= MAX_SCANNED {
            truncated = true;
            break;
        }
        let Ok(item) = item else { continue };
        let Ok(name) = item.file_name().into_string() else {
            continue;
        };
        if hidden_name(&name) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(item.path()) else {
            continue;
        };
        let kind = kind_of(meta.file_type());
        entries.push(FileEntry {
            name,
            kind,
            size: (kind == FileKind::File).then_some(meta.len()),
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

/// Opens a regular file without following a symlink and without blocking
/// on a FIFO.
fn open_file(abs: &Path, path: &str) -> Result<(File, u64), FileError> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = rustix::fs::openat(CWD, abs, flags, Mode::empty()).map_err(|_| not_found(path))?;
    let file = File::from(fd);
    let meta = file.metadata().map_err(|_| not_found(path))?;
    if !meta.is_file() {
        return Err(FileError::new(
            code::WRONG_KIND,
            format!("`{path}` is not a regular file"),
        ));
    }
    Ok((file, meta.len()))
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
    let (abs, _) = resolve(root, path)?;
    let (file, size) = open_file(&abs, path)?;
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
    let mut queue = std::collections::VecDeque::from([(String::new(), 0usize)]);
    let (mut seen, mut opened) = (0usize, 0usize);
    while let Some((rel, depth)) = queue.pop_front() {
        let dir = if rel.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&rel)
        };
        let Ok(items) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<(String, std::fs::FileType)> = items
            .filter_map(Result::ok)
            .filter_map(|i| Some((i.file_name().into_string().ok()?, i.file_type().ok()?)))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, file_type) in names {
            seen += 1;
            if seen > WALK_ENTRY_MAX {
                found.truncated = true;
                return found;
            }
            if hidden_name(&name) || file_type.is_symlink() {
                continue;
            }
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if file_type.is_dir() {
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
            if !file_type.is_file() {
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
            if !grep_file(root, &path, needle, max, &mut found) {
                return found;
            }
        }
    }
    found
}

/// Adds the content hits of one file; false when the hit limit is reached.
fn grep_file(root: &Path, path: &str, needle: &str, max: usize, found: &mut Found) -> bool {
    let Ok((file, size)) = open_file(&root.join(path), path) else {
        return true;
    };
    if size > WALK_FILE_BYTES {
        return true;
    }
    let mut buf = Vec::new();
    if file.take(WALK_FILE_BYTES).read_to_end(&mut buf).is_err()
        || buf[..buf.len().min(BINARY_SNIFF_BYTES)].contains(&0)
    {
        return true;
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
