//! Include files of a new worktree (FG-4, design §7.2, O14).
//!
//! The station copies ignored files (`.env`, `.env.local`) from the main
//! checkout of the home into a new worktree. Every step opens its object
//! with `openat` and without following a link, so a link in the source or in
//! the destination is never followed. A file that is not copied is listed;
//! the start does not fail.

use std::collections::HashSet;
use std::io::Read;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;
use std::sync::Arc;

use fleetdeck_core::launch_dir::open_root;
use fleetdeck_proto::fleet_git::SkippedFile;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;

use super::identity::{READ_TIMEOUT, read_git};
use crate::workspace::GitOptions;

/// The most matches of all patterns together.
const MAX_MATCHES: usize = 200;
/// The largest file, in bytes (1 MiB).
const MAX_FILE: u64 = 1024 * 1024;
/// The largest total of the copied files, in bytes (10 MiB).
const MAX_TOTAL: u64 = 10 * 1024 * 1024;
/// The most entries of the result (`workspace.include_skipped`).
const MAX_SKIPPED: usize = 50;
/// The most bytes of the answer of `git check-ignore`.
const IGNORE_CAP: usize = 64 * 1024;

fn skip(path: &str, reason: &str) -> SkippedFile {
    SkippedFile {
        path: path.to_owned(),
        reason: reason.to_owned(),
    }
}

pub(super) fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// Does `name` match the segment `pattern`? `*` and `?` match inside one
/// segment; a hidden name matches only a pattern that starts with `.`.
fn glob(pattern: &str, name: &str) -> bool {
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) && p[pi] != '*' {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// A pattern segment that the walk can use: not empty, not `.` or `..`, no
/// control character or backslash.
fn segment_ok(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.contains(|c: char| c.is_control() || c == '\\')
}

/// The matches of the patterns, in the order of the patterns and, inside a
/// folder, of the names.
struct Walk {
    found: Vec<String>,
    seen: HashSet<String>,
    skipped: Vec<SkippedFile>,
    pattern: String,
}

impl Walk {
    /// Adds a match. False: the limit of [`MAX_MATCHES`] is over.
    fn add(&mut self, path: String) -> bool {
        if self.seen.contains(&path) {
            return true;
        }
        if self.found.len() >= MAX_MATCHES {
            self.skipped.push(skip(&self.pattern, "too_many_matches"));
            return false;
        }
        self.seen.insert(path.clone());
        self.found.push(path);
        true
    }
}

/// The names in `dir`, sorted, that match `segment`.
fn matching_names(dir: BorrowedFd<'_>, segment: &str) -> Vec<String> {
    let Ok(entries) = Dir::read_from(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str().ok().map(str::to_owned))
        .filter(|name| name != "." && name != ".." && glob(segment, name))
        .collect();
    names.sort();
    names
}

/// Matches `segments` below `dir`. False: stop, the match limit is over.
fn expand(dir: BorrowedFd<'_>, segments: &[&str], prefix: &str, walk: &mut Walk) -> bool {
    let Some((segment, rest)) = segments.split_first() else {
        return true;
    };
    let names = if segment.contains(['*', '?']) {
        matching_names(dir, segment)
    } else {
        vec![(*segment).to_owned()]
    };
    for name in names {
        // Nothing below `.git` is an ignored file that a start copies.
        if name == ".git" && !rest.is_empty() {
            continue;
        }
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let Ok(stat) = rustix::fs::statat(dir, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) else {
            continue;
        };
        if rest.is_empty() {
            if !walk.add(path) {
                return false;
            }
        } else if FileType::from_raw_mode(stat.st_mode) == FileType::Directory
            && let Ok(sub) = rustix::fs::openat(dir, name.as_str(), dir_flags(), Mode::empty())
            && !expand(sub.as_fd(), rest, &path, walk)
        {
            return false;
        }
    }
    true
}

/// The relative paths that the patterns match below `root` (design §7.2
/// step 2), and the entries to report.
fn find_matches(root: &OwnedFd, patterns: &[String]) -> (Vec<String>, Vec<SkippedFile>) {
    let mut walk = Walk {
        found: Vec::new(),
        seen: HashSet::new(),
        skipped: Vec::new(),
        pattern: String::new(),
    };
    for pattern in patterns {
        let segments: Vec<&str> = pattern.split('/').collect();
        if !segments.iter().all(|s| segment_ok(s)) {
            walk.skipped.push(skip(pattern, "unreadable"));
            continue;
        }
        pattern.clone_into(&mut walk.pattern);
        if !expand(root.as_fd(), &segments, "", &mut walk) {
            break;
        }
    }
    (walk.found, walk.skipped)
}

/// The paths of `candidates` that Git says are ignored (design §7.2 step 3):
/// `git check-ignore -z --stdin` prints only paths that are ignored and not
/// tracked. A failure of the command gives an empty set, so every match is
/// skipped (fail closed).
pub(super) async fn ignored_among(
    options: &GitOptions,
    home: &Path,
    candidates: &[String],
) -> HashSet<String> {
    let mut stdin = Vec::new();
    for path in candidates {
        stdin.extend_from_slice(path.as_bytes());
        stdin.push(0);
    }
    let git = read_git(options, home, READ_TIMEOUT);
    match git
        .status(&["check-ignore", "-z", "--stdin"], &[], Some(stdin))
        .await
    {
        Ok((0, out, _)) if out.len() <= IGNORE_CAP => out
            .split(|b| *b == 0)
            .filter(|field| !field.is_empty())
            .filter_map(|field| std::str::from_utf8(field).ok().map(str::to_owned))
            .collect(),
        _ => HashSet::new(),
    }
}

/// Opens `names` below `root`, one folder at a time, without following a
/// link. With `make`, a missing folder is made (mode 0700). The error:
/// `exists` (a link or a file is in the way) or `unreadable`.
pub(super) fn open_chain(
    root: BorrowedFd<'_>,
    names: &[&str],
    make: bool,
) -> Result<OwnedFd, &'static str> {
    let mut dir =
        rustix::fs::openat(root, ".", dir_flags(), Mode::empty()).map_err(|_| "unreadable")?;
    for name in names {
        if make {
            match rustix::fs::mkdirat(&dir, *name, Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(_) => return Err("unreadable"),
            }
        }
        dir = rustix::fs::openat(&dir, *name, dir_flags(), Mode::empty()).map_err(|e| {
            if make && matches!(e, Errno::LOOP | Errno::NOTDIR) {
                "exists"
            } else {
                "unreadable"
            }
        })?;
    }
    Ok(dir)
}

/// The copy of one file. Returns the bytes copied, or the reason.
fn copy_one(
    src: BorrowedFd<'_>,
    dst: BorrowedFd<'_>,
    path: &str,
    total: &mut u64,
    over_total: &mut bool,
) -> Result<(), &'static str> {
    let parts: Vec<&str> = path.split('/').collect();
    let Some((name, parents)) = parts.split_last() else {
        return Err("unreadable");
    };
    let src_parent = open_chain(src, parents, false)?;
    let stat = rustix::fs::statat(&src_parent, *name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| "unreadable")?;
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::Symlink => return Err("symlink"),
        FileType::RegularFile => {}
        _ => return Err("not_regular"),
    }
    let size = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
    if size > MAX_FILE {
        return Err("too_large");
    }
    if *over_total || *total + size > MAX_TOTAL {
        *over_total = true;
        return Err("total_too_large");
    }
    // NONBLOCK: a FIFO that replaced the file cannot block the open.
    let fd = rustix::fs::openat(
        &src_parent,
        *name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|e| {
        if e == Errno::LOOP {
            "symlink"
        } else {
            "unreadable"
        }
    })?;
    let opened = rustix::fs::fstat(&fd).map_err(|_| "unreadable")?;
    if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile {
        return Err("not_regular");
    }
    #[allow(clippy::useless_conversion, reason = "st_mode differs by platform")]
    let mode = rustix::fs::RawMode::from(opened.st_mode) & 0o777;
    let dst_parent = open_chain(dst, parents, true)?;
    let out = rustix::fs::openat(
        &dst_parent,
        *name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(mode),
    )
    .map_err(|e| {
        if e == Errno::EXIST {
            "exists"
        } else {
            "unreadable"
        }
    })?;
    let undo = |reason: &'static str| {
        let _ = rustix::fs::unlinkat(&dst_parent, *name, AtFlags::empty());
        reason
    };
    let mut reader = std::fs::File::from(fd).take(MAX_FILE + 1);
    let mut writer = std::fs::File::from(out);
    let copied = std::io::copy(&mut reader, &mut writer).map_err(|_| undo("unreadable"))?;
    if copied > MAX_FILE {
        return Err(undo("too_large"));
    }
    if *total + copied > MAX_TOTAL {
        *over_total = true;
        return Err(undo("total_too_large"));
    }
    // The umask of the station must not change the mode of the copy.
    rustix::fs::fchmod(&writer, Mode::from_raw_mode(mode)).map_err(|_| undo("unreadable"))?;
    *total += copied;
    Ok(())
}

/// Copies the files of `paths` and returns the entries that were not copied.
fn copy_all(
    src: &OwnedFd,
    dst: &OwnedFd,
    paths: &[String],
    ignored: &HashSet<String>,
) -> Vec<SkippedFile> {
    let mut skipped = Vec::new();
    let (mut total, mut over_total) = (0u64, false);
    for path in paths {
        let git_part = path.split('/').any(|part| part == ".git");
        if git_part || !ignored.contains(path) {
            skipped.push(skip(path, "not_ignored"));
            continue;
        }
        if let Err(reason) = copy_one(src.as_fd(), dst.as_fd(), path, &mut total, &mut over_total) {
            skipped.push(skip(path, reason));
        }
    }
    skipped
}

/// Copies the ignored files that `patterns` match from the main checkout
/// `home` into the new worktree `worktree` (design §7.2). Both paths are
/// canonical and absolute. The patterns are the `worktree_include` of the
/// settings: relative, `*` and `?` inside one segment, hidden names only for a
/// pattern segment that starts with `.`, at most 200 matches.
///
/// Only a regular file of at most 1 MiB is copied, 10 MiB in total, with its
/// mode (`& 0o777`) and never over an existing file. A match that Git does not
/// print for `git check-ignore -z --stdin` is `not_ignored`. The result lists
/// every file that was not copied, at most 50 entries, with the reasons
/// `not_ignored`, `symlink`, `not_regular`, `too_large`, `total_too_large`,
/// `exists`, `unreadable` and `too_many_matches` (one entry with the pattern
/// that went over 200 matches; an invalid pattern is `unreadable`).
pub async fn copy_includes(
    home: &Path,
    worktree: &Path,
    patterns: &[String],
    options: &GitOptions,
) -> Vec<SkippedFile> {
    let (Ok(src), Ok(dst)) = (open_root(home), open_root(worktree)) else {
        return vec![skip(".", "unreadable")];
    };
    let (src, dst) = (Arc::new(src), Arc::new(dst));
    let owned = patterns.to_vec();
    let walked = {
        let src = Arc::clone(&src);
        tokio::task::spawn_blocking(move || find_matches(&src, &owned)).await
    };
    let Ok((found, mut skipped)) = walked else {
        return vec![skip(".", "unreadable")];
    };
    if !found.is_empty() {
        let candidates: Vec<String> = found
            .iter()
            .filter(|path| !path.split('/').any(|part| part == ".git"))
            .cloned()
            .collect();
        let ignored = ignored_among(options, home, &candidates).await;
        let copied =
            tokio::task::spawn_blocking(move || copy_all(&src, &dst, &found, &ignored)).await;
        match copied {
            Ok(more) => skipped.extend(more),
            Err(_) => skipped.push(skip(".", "unreadable")),
        }
    }
    skipped.truncate(MAX_SKIPPED);
    skipped
}

#[cfg(test)]
#[path = "include_tests.rs"]
mod tests;
