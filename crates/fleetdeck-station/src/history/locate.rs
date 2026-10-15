//! Finding and trusting a provider transcript (DL-189 §3.2).
//!
//! The input is the station's own session row: provider, provider home,
//! working directory and native id. The caller gives no path. Directories
//! are walked with the no-follow descriptor walk of `workspace/files.rs`, and
//! the file is opened with `O_NOFOLLOW`; the checks run on the open file.

use std::ffi::CStr;
use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::HistoryUnavailable;
use rustix::fs::{Dir, FileType, Mode};
use rustix::io::Errno;

use super::lines::{Line, Lines};
use super::parse;
use super::{Fail, Limits};
use crate::workspace::PrivatePaths;
use crate::workspace::files::{descend, file_flags, open_root};

/// How many records of the head of a file may pass before one names a
/// session.
const HEAD_RECORDS: usize = 1000;

/// An open, trusted transcript.
#[derive(Debug)]
pub struct Opened {
    pub file: File,
    pub size: u64,
    /// The canonical provider home.
    pub home: PathBuf,
    /// The path of the file below the home.
    pub path: PathBuf,
}

/// What the trust checks need.
pub struct Trust<'a> {
    pub private: &'a PrivatePaths,
    /// The user id that must own the file.
    pub uid: u32,
    pub limits: &'a Limits,
    pub deadline: Instant,
}

/// The provider home of a launch environment: `CLAUDE_CONFIG_DIR` or
/// `CODEX_HOME`, else `HOME` with `.claude` or `.codex`.
#[must_use]
pub fn resolve_home(env: &ChildEnv, kind: ProviderKind) -> Option<PathBuf> {
    let (var, dir) = match kind {
        ProviderKind::Claude => ("CLAUDE_CONFIG_DIR", ".claude"),
        ProviderKind::Codex => ("CODEX_HOME", ".codex"),
        ProviderKind::Unknown => return None,
    };
    let set = |name: &str| env.get(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    set(var).or_else(|| set("HOME").map(|home| home.join(dir)))
}

/// Claude's directory name of a working directory: every character that is
/// not ASCII alphanumeric becomes `-`.
#[must_use]
pub fn encode_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn unavailable(reason: HistoryUnavailable) -> Fail {
    Fail::Unavailable(reason)
}

/// The failure of an `openat` step: a missing name is `not_found`, anything
/// else (a link, a wrong kind, no access) is `not_trusted`.
fn open_fail(e: Errno) -> Fail {
    if e == Errno::NOENT {
        unavailable(HistoryUnavailable::NotFound)
    } else {
        unavailable(HistoryUnavailable::NotTrusted)
    }
}

/// Finds the transcript of `native` below `home` and checks it.
///
/// # Errors
/// [`Fail::Unavailable`] with the reason, or the deadline.
pub fn locate(
    home: &Path,
    kind: ProviderKind,
    cwd: &str,
    native: &str,
    trust: &Trust<'_>,
) -> Result<Opened, Fail> {
    let home =
        std::fs::canonicalize(home).map_err(|_| unavailable(HistoryUnavailable::NotFound))?;
    let root = open_root(&home).map_err(open_fail)?;
    let segments: Vec<String> = match kind {
        ProviderKind::Claude => vec![
            "projects".to_owned(),
            encode_cwd(cwd),
            format!("{native}.jsonl"),
        ],
        ProviderKind::Codex => codex_path(&root, native, trust.limits)?,
        ProviderKind::Unknown => return Err(unavailable(HistoryUnavailable::UnsupportedFormat)),
    };
    let (name, dirs) = segments.split_last().ok_or(Fail::Io)?;
    let dir_refs: Vec<&str> = dirs.iter().map(String::as_str).collect();
    let dir = descend(&root, &dir_refs).map_err(open_fail)?;
    let fd =
        rustix::fs::openat(&dir, name.as_str(), file_flags(), Mode::empty()).map_err(open_fail)?;
    let file = File::from(fd);
    let size = check_file(&file, kind, native, trust)?;
    Ok(Opened {
        file,
        size,
        home,
        path: segments.iter().collect(),
    })
}

/// The trust checks on the open file; returns its size.
fn check_file(
    file: &File,
    kind: ProviderKind,
    native: &str,
    trust: &Trust<'_>,
) -> Result<u64, Fail> {
    let not_trusted = || unavailable(HistoryUnavailable::NotTrusted);
    let meta = file.metadata().map_err(|_| Fail::Io)?;
    if !meta.is_file() || meta.uid() != trust.uid || meta.nlink() != 1 {
        return Err(not_trusted());
    }
    if trust.private.is_private_file(&meta) {
        return Err(not_trusted());
    }
    if meta.len() > trust.limits.file_max {
        return Err(unavailable(HistoryUnavailable::TooLarge));
    }
    // The first record that names a session must name this one.
    let mut lines =
        Lines::new(file, 0, meta.len(), trust.limits.line_max).with_deadline(trust.deadline);
    for _ in 0..HEAD_RECORDS {
        let next = lines.next_line().map_err(|e| match e {
            super::lines::ReadError::Deadline => Fail::Busy,
            super::lines::ReadError::Io(_) => Fail::Io,
        })?;
        let Some((_, line)) = next else { break };
        let Line::Record(bytes) = line else { continue };
        if let Some(named) = parse::embedded_session(kind, bytes) {
            return if named == native {
                Ok(meta.len())
            } else {
                Err(not_trusted())
            };
        }
    }
    Err(not_trusted())
}

/// The path segments of the single Codex rollout of `native`.
fn codex_path(root: &OwnedFd, native: &str, limits: &Limits) -> Result<Vec<String>, Fail> {
    let suffix = format!("-{native}.jsonl");
    let mut visited = 0_usize;
    let mut found: Vec<Vec<String>> = Vec::new();
    // `sessions/YYYY/MM/DD/rollout-…`
    if let Ok(sessions) = descend(root, &["sessions"]) {
        for year in entries(&sessions, &mut visited, limits)? {
            if year.1 != FileType::Directory {
                continue;
            }
            let Ok(year_dir) = descend(&sessions, &[year.0.as_str()]) else {
                continue;
            };
            for month in entries(&year_dir, &mut visited, limits)? {
                if month.1 != FileType::Directory {
                    continue;
                }
                let Ok(month_dir) = descend(&year_dir, &[month.0.as_str()]) else {
                    continue;
                };
                for day in entries(&month_dir, &mut visited, limits)? {
                    if day.1 != FileType::Directory {
                        continue;
                    }
                    let Ok(day_dir) = descend(&month_dir, &[day.0.as_str()]) else {
                        continue;
                    };
                    for file in entries(&day_dir, &mut visited, limits)? {
                        if is_rollout(&file, &suffix) {
                            found.push(vec![
                                "sessions".to_owned(),
                                year.0.clone(),
                                month.0.clone(),
                                day.0.clone(),
                                file.0,
                            ]);
                        }
                    }
                }
            }
        }
    }
    if let Ok(archived) = descend(root, &["archived_sessions"]) {
        for file in entries(&archived, &mut visited, limits)? {
            if is_rollout(&file, &suffix) {
                found.push(vec!["archived_sessions".to_owned(), file.0]);
            }
        }
    }
    match found.len() {
        0 => Err(unavailable(HistoryUnavailable::NotFound)),
        1 => Ok(found.remove(0)),
        _ => Err(unavailable(HistoryUnavailable::Ambiguous)),
    }
}

/// A rollout file name: a regular file or a link (the open then refuses it).
fn is_rollout(entry: &(String, FileType), suffix: &str) -> bool {
    let (name, kind) = entry;
    matches!(kind, FileType::RegularFile | FileType::Symlink)
        && name.starts_with("rollout-")
        && name.len() > "rollout-".len() + suffix.len()
        && name.ends_with(suffix)
}

/// The entries of a directory with their kinds; the count of all entries
/// visited is limited.
fn entries(
    dir: &OwnedFd,
    visited: &mut usize,
    limits: &Limits,
) -> Result<Vec<(String, FileType)>, Fail> {
    let reader = Dir::read_from(dir.as_fd()).map_err(open_fail)?;
    let mut out = Vec::new();
    for entry in reader {
        let entry = entry.map_err(open_fail)?;
        let name: &CStr = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        *visited += 1;
        if *visited > limits.dir_entries {
            return Err(unavailable(HistoryUnavailable::Busy));
        }
        if let Ok(name) = name.to_str() {
            out.push((name.to_owned(), entry.file_type()));
        }
    }
    Ok(out)
}
