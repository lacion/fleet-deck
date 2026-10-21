//! `project.secret_browse` (Fleet Sync, design `docs/v2/design/fleet-sync.md`
//! §3 and §7): the entries of one folder of a home, hidden ones included, and
//! for each file whether it can be a secret file.
//!
//! The station never reads the content of a file here. It follows no link: the
//! folder is opened below the pinned home with the parent walk of the other
//! secret methods, and each entry is looked at with `AT_SYMLINK_NOFOLLOW`.

use std::collections::HashSet;
use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::Arc;

use fleetdeck_proto::fleet_git_secrets::{
    MAX_SECRET_BYTES, SecretBrowseAnswer, SecretBrowseEntry, SecretBrowseParams, SecretEntryKind,
    SecretPathError, check_secret_path,
};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use serde_json::Value;

use super::{
    Env, MISSING_FOLDER, SecretError, UNREADABLE, answer, begin, ignored_among, joined,
    open_parent, path_error,
};
use crate::workspace::PrivatePaths;

/// The most entries of one answer.
const MAX_ENTRIES: usize = 500;

/// The shape of `dir`: `""` or a relative path with the shape rules of a
/// secret file path. The member-hidden rule does not apply to a folder.
fn check_dir(dir: &str) -> Result<(), SecretError> {
    if dir.is_empty() {
        return Ok(());
    }
    match check_secret_path(dir) {
        Ok(()) | Err(SecretPathError::NotMemberHidden) => Ok(()),
        Err(e @ SecretPathError::Invalid) => Err(path_error(e)),
    }
}

fn entry(name: &str, kind: SecretEntryKind) -> SecretBrowseEntry {
    SecretBrowseEntry {
        name: name.to_owned(),
        kind,
        eligible: false,
        reason: None,
        size: None,
    }
}

fn refused(name: &str, kind: SecretEntryKind, reason: &str) -> SecretBrowseEntry {
    SecretBrowseEntry {
        reason: Some(reason.to_owned()),
        ..entry(name, kind)
    }
}

/// One entry and, when only Git can still decide, its path below the home.
struct Draft {
    entry: SecretBrowseEntry,
    candidate: Option<String>,
}

fn settled(entry: SecretBrowseEntry) -> Draft {
    Draft {
        entry,
        candidate: None,
    }
}

/// The path of `name` below the home, without a leading `/`.
fn rel_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// The checks of a regular file in the order of the design: path, size, then
/// the open and the private file check. A file that passes waits for Git.
fn file_draft(
    parent: BorrowedFd<'_>,
    rel: &str,
    name: &str,
    size: u64,
    private: &PrivatePaths,
) -> Draft {
    let with_size = |mut e: SecretBrowseEntry| {
        e.size = Some(size);
        e
    };
    let file = |reason: &str| settled(with_size(refused(name, SecretEntryKind::File, reason)));
    match check_secret_path(rel) {
        Ok(()) => {}
        Err(SecretPathError::Invalid) => return file("invalid_path"),
        Err(SecretPathError::NotMemberHidden) => return file("not_member_hidden"),
    }
    if size > MAX_SECRET_BYTES {
        return file("too_large");
    }
    // NONBLOCK: a FIFO that replaced the file cannot block the open. The file
    // is never read.
    let opened = rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    );
    let Ok(fd) = opened else {
        return file("unreadable");
    };
    let Ok(meta) = File::from(fd).metadata() else {
        return file("unreadable");
    };
    if private.is_private_opened(&meta, rel) {
        return file("private_file");
    }
    Draft {
        entry: with_size(entry(name, SecretEntryKind::File)),
        candidate: Some(rel.to_owned()),
    }
}

/// The entries of the folder `dir` of the home `root`, before Git answered.
struct Listing {
    drafts: Vec<Draft>,
    truncated: bool,
}

fn list(root: BorrowedFd<'_>, dir: &str, private: &PrivatePaths) -> Result<Listing, SecretError> {
    let parts: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    let Some(folder) = open_parent(root, &parts)? else {
        return Err(MISSING_FOLDER);
    };
    let mut reader = Dir::read_from(&folder).map_err(|_| UNREADABLE)?;
    let mut names: Vec<String> = Vec::new();
    while let Some(item) = reader.read() {
        let item = item.map_err(|_| UNREADABLE)?;
        let bytes = item.file_name().to_bytes();
        if matches!(bytes, b"." | b".." | b".git") {
            continue;
        }
        // A name that is not UTF-8 cannot be a path of the wire: skipped.
        if let Ok(name) = std::str::from_utf8(bytes) {
            names.push(name.to_owned());
        }
    }
    names.sort();
    let truncated = names.len() > MAX_ENTRIES;
    names.truncate(MAX_ENTRIES);
    let mut drafts = Vec::with_capacity(names.len());
    for name in names {
        let Ok(stat) = rustix::fs::statat(&folder, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) else {
            continue;
        };
        let draft = match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory => settled(entry(&name, SecretEntryKind::Dir)),
            FileType::Symlink => settled(refused(&name, SecretEntryKind::Other, "symlink")),
            FileType::RegularFile => {
                let size = u64::try_from(stat.st_size).unwrap_or(0);
                let rel = rel_path(dir, &name);
                file_draft(folder.as_fd(), &rel, &name, size, private)
            }
            _ => settled(refused(&name, SecretEntryKind::Other, "not_regular")),
        };
        drafts.push(draft);
    }
    // The folders first; the sort is stable, so each group stays by name.
    drafts.sort_by_key(|d| d.entry.kind != SecretEntryKind::Dir);
    Ok(Listing { drafts, truncated })
}

/// The last step: a file that Git ignores is eligible, any other is not.
fn finish(listing: Listing, ignored: &HashSet<String>) -> SecretBrowseAnswer {
    let entries = listing
        .drafts
        .into_iter()
        .map(|d| match d.candidate {
            Some(rel) if ignored.contains(&rel) => SecretBrowseEntry {
                eligible: true,
                ..d.entry
            },
            Some(_) => SecretBrowseEntry {
                reason: Some("not_ignored".to_owned()),
                ..d.entry
            },
            None => d.entry,
        })
        .collect();
    SecretBrowseAnswer {
        dir: String::new(),
        entries,
        truncated: listing.truncated,
    }
}

/// `project.secret_browse`.
pub(super) async fn browse(env: &Env<'_>, p: SecretBrowseParams) -> Result<Value, SecretError> {
    check_dir(&p.dir)?;
    // No file lock: the call reads names and sizes only.
    let (home, root, _guards) = begin(
        env,
        (&p.project_id, p.generation),
        (&p.dest, &p.remote_name),
        Vec::new(),
    )
    .await?;
    let private = Arc::clone(&env.private);
    let dir = p.dir.clone();
    let listing =
        joined(tokio::task::spawn_blocking(move || list(root.as_fd(), &dir, &private)).await)??;
    let candidates: Vec<String> = listing
        .drafts
        .iter()
        .filter_map(|d| d.candidate.clone())
        .collect();
    let ignored = if candidates.is_empty() {
        HashSet::new()
    } else {
        ignored_among(env.options, &home, &candidates).await
    };
    let mut result = finish(listing, &ignored);
    result.dir = p.dir;
    answer(result)
}
