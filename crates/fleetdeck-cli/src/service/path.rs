//! The safe `PATH` of the service (NV-22) and the check of the programs that
//! Fleet Deck uses.

use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

/// The programs that Fleet Deck runs. The install warns when one of them is
/// only in a folder that the service leaves out.
const PROGRAMS: [&str; 6] = ["claude", "codex", "git", "gh", "glab", "curl"];

/// The result of [`safe_path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathCheck {
    /// The `PATH` of the service. `None` when no entry stays.
    pub kept: Option<String>,
    /// Each dropped entry once, in order: the entry text and the reason.
    pub dropped: Vec<(String, String)>,
}

/// Keeps the `PATH` directories that only this user or root can change
/// (NV-22). An entry stays when it is an absolute path to a directory that no
/// group or other user can write, and that `uid` or root owns. The private
/// group of the user (`private_gid`) does not count as a group. Every parent
/// directory up to `/` must be safe too. The service stores the canonical
/// directory. It does not store a link, because another user can change the
/// target of a link later. A later entry with the same canonical directory
/// gives no note. Each dropped entry text gives one note. An empty entry gives
/// no note. `kept` is `None` when no entry stays.
pub fn safe_path(path: &str, uid: u32, private_gid: Option<u32>) -> PathCheck {
    let mut kept: Vec<String> = Vec::new();
    let mut dropped: Vec<(String, String)> = Vec::new();
    let mut leave_out = |entry: &str, reason: String| {
        if !dropped.iter().any(|(e, _)| e == entry) {
            dropped.push((entry.to_owned(), reason));
        }
    };
    for entry in path.split(':').filter(|e| !e.is_empty()) {
        if !Path::new(entry).is_absolute() {
            leave_out(entry, "not an absolute path".to_owned());
            continue;
        }
        let canonical = match std::fs::canonicalize(entry) {
            Ok(canonical) if canonical.is_dir() => canonical,
            _ => {
                leave_out(entry, "not a directory".to_owned());
                continue;
            }
        };
        let Some(text) = canonical.to_str() else {
            leave_out(entry, "not a directory".to_owned());
            continue;
        };
        // A `:` in the target would split it into more PATH entries that
        // nobody checked (NV-22).
        if text.contains(':') {
            leave_out(entry, "its directory name holds a `:`".to_owned());
            continue;
        }
        match unsafe_reason(&canonical, uid, private_gid) {
            None if kept.iter().any(|k| k == text) => {}
            None => kept.push(text.to_owned()),
            Some(reason) => leave_out(entry, reason),
        }
    }
    PathCheck {
        kept: (!kept.is_empty()).then(|| kept.join(":")),
        dropped,
    }
}

/// True when the mode lets another user write the directory. The private
/// group of the user is not another user.
fn writable_by_others(meta: &std::fs::Metadata, private_gid: Option<u32>) -> bool {
    let mode = meta.mode();
    mode & 0o002 != 0 || (mode & 0o020 != 0 && private_gid != Some(meta.gid()))
}

/// Why a canonical directory is not safe for the `PATH` of the service, or
/// `None` when it is safe (NV-22). The checks are the directory itself, then
/// each parent up to `/`.
fn unsafe_reason(dir: &Path, uid: u32, private_gid: Option<u32>) -> Option<String> {
    let owned = |meta: &std::fs::Metadata| meta.uid() == uid || meta.uid() == 0;
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return Some("not a directory".to_owned());
    };
    if writable_by_others(&meta, private_gid) {
        return Some("the group or others can write it".to_owned());
    }
    if !owned(&meta) {
        return Some("another user owns it".to_owned());
    }
    let mut child = dir;
    for parent in dir.ancestors().skip(1) {
        let safe = match std::fs::symlink_metadata(parent) {
            Err(_) => false,
            Ok(meta) if !owned(&meta) => false,
            Ok(meta) if !writable_by_others(&meta, private_gid) => true,
            // A sticky parent is safe when the next component below it is
            // owned by `uid` or root: others cannot rename or remove it.
            Ok(meta) => {
                meta.mode() & 0o1000 != 0
                    && std::fs::symlink_metadata(child).is_ok_and(|below| owned(&below))
            }
        };
        if !safe {
            return Some(format!(
                "a parent directory is not safe: {}",
                parent.display()
            ));
        }
        child = parent;
    }
    None
}

/// `Some(gid)` when `gid` is a private group of `user`: the `group` line of
/// the gid has no member or only `user`, and no other user of `passwd` has
/// it as the primary group. `None` otherwise.
pub fn private_gid(group_text: &str, passwd_text: &str, user: &str, gid: u32) -> Option<u32> {
    let field = |line: &str, index: usize| line.split(':').nth(index).map(str::to_owned);
    let mut found = false;
    for line in group_text.lines() {
        if field(line, 2).and_then(|g| g.parse::<u32>().ok()) != Some(gid) {
            continue;
        }
        found = true;
        let members = field(line, 3).unwrap_or_default();
        if members.split(',').any(|m| !m.is_empty() && m != user) {
            return None;
        }
    }
    if !found {
        return None;
    }
    let shared = passwd_text.lines().any(|line| {
        field(line, 3).and_then(|g| g.parse::<u32>().ok()) == Some(gid)
            && field(line, 0).is_some_and(|name| name != user)
    });
    (!shared).then_some(gid)
}

/// Reads `/etc/group` and `/etc/passwd` and gives [`private_gid`]. `None`
/// when a file cannot be read: the group is then not private.
pub fn read_private_gid(user: &str, gid: u32) -> Option<u32> {
    let group = std::fs::read_to_string("/etc/group").ok()?;
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    private_gid(&group, &passwd, user, gid)
}

/// The first executable file `<dir>/<name>` in a `PATH` text.
fn find_program(path: &str, name: &str) -> Option<String> {
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .find(|dir| {
            std::fs::metadata(Path::new(dir).join(name))
                .is_ok_and(|meta| meta.is_file() && meta.mode() & 0o111 != 0)
        })
        .map(str::to_owned)
}

/// The programs that Fleet Deck uses, and that the shell finds but the
/// service does not: the name and the folder of the shell `PATH` where the
/// shell finds it.
pub fn lost_programs(shell_path: &str, kept: Option<&str>) -> Vec<(String, String)> {
    PROGRAMS
        .iter()
        .filter_map(|name| {
            let dir = find_program(shell_path, name)?;
            let in_service = kept.is_some_and(|k| find_program(k, name).is_some());
            (!in_service).then(|| ((*name).to_owned(), dir))
        })
        .collect()
}
