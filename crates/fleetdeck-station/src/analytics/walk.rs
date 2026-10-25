//! The walk of one provider home (B29, design section 3.5 step 2).
//!
//! It opens directories one component at a time with no link followed,
//! lists them, and reports the regular files that match the transcript
//! patterns. POSIX calls only, so it runs on Linux and macOS.
//!
//! - Claude: `projects/*/*.jsonl`, `projects/*/*/subagents/*.jsonl` and
//!   `projects/*/*/subagents/workflows/*/*.jsonl` without `journal.jsonl`.
//! - Codex: `sessions/*/*/*/rollout-*.jsonl` and
//!   `archived_sessions/rollout-*.jsonl`.

use std::os::fd::{AsFd, OwnedFd};

use fleetdeck_proto::event::ProviderKind;
use rustix::fs::{AtFlags, Dir, FileType};

use crate::workspace::files::descend;

/// The most directories that one pass opens in one home.
pub(super) const MAX_DIRS: usize = 20_000;
/// The most files that one pass lists in one home.
pub(super) const MAX_FILES: usize = 100_000;

/// One transcript file that the walk found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Found {
    /// The path below the home, with `/` between the components.
    pub rel: String,
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i64,
}

/// The result of the walk of one home.
#[derive(Debug, Default)]
pub(super) struct Walked {
    pub files: Vec<Found>,
    /// A budget was hit: the list is not complete.
    pub truncated: bool,
}

struct Walker {
    dirs: usize,
    max_dirs: usize,
    max_files: usize,
    out: Walked,
}

fn is_jsonl(name: &str) -> bool {
    name.ends_with(".jsonl")
}

fn is_claude_file(name: &str) -> bool {
    is_jsonl(name)
}

fn is_workflow_file(name: &str) -> bool {
    is_jsonl(name) && name != "journal.jsonl"
}

fn is_rollout(name: &str) -> bool {
    name.starts_with("rollout-") && is_jsonl(name)
}

#[allow(clippy::unnecessary_cast)]
fn found(dir: &OwnedFd, name: &str, rel: String) -> Option<Found> {
    let st = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
        return None;
    }
    let mtime_ns = (st.st_mtime as i64)
        .saturating_mul(1_000_000_000)
        .saturating_add(st.st_mtime_nsec as i64);
    Some(Found {
        rel,
        dev: st.st_dev as u64,
        ino: st.st_ino as u64,
        size: u64::try_from(st.st_size).unwrap_or(0),
        mtime_ns,
    })
}

impl Walker {
    fn new(max_dirs: usize, max_files: usize) -> Self {
        Self {
            dirs: 0,
            max_dirs,
            max_files,
            out: Walked::default(),
        }
    }

    /// Opens the directory `name` below `dir` (no link followed), counted
    /// against the budget. `None`: it is missing, not a directory, a link,
    /// unreadable, or the budget is spent.
    fn open(&mut self, dir: &OwnedFd, name: &str) -> Option<OwnedFd> {
        if self.dirs >= self.max_dirs {
            self.out.truncated = true;
            return None;
        }
        let child = descend(dir, &[name]).ok()?;
        self.dirs += 1;
        Some(child)
    }

    /// The names of a directory with their kinds.
    fn list(&self, dir: &OwnedFd) -> Vec<(String, FileType)> {
        let Ok(reader) = Dir::read_from(dir.as_fd()) else {
            return Vec::new();
        };
        let mut items = Vec::new();
        for entry in reader.flatten() {
            let name = entry.file_name();
            if name == c"." || name == c".." {
                continue;
            }
            if let Ok(name) = name.to_str() {
                items.push((name.to_owned(), entry.file_type()));
            }
        }
        items.sort_by(|a, b| a.0.cmp(&b.0));
        items
    }

    fn is_dir(dir: &OwnedFd, name: &str, kind: FileType) -> bool {
        let kind = if kind == FileType::Unknown {
            rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)
                .map_or(FileType::Unknown, |st| FileType::from_raw_mode(st.st_mode))
        } else {
            kind
        };
        kind == FileType::Directory
    }

    /// Adds the matching regular files of `dir`.
    fn files(&mut self, dir: &OwnedFd, trail: &str, accept: fn(&str) -> bool) {
        for (name, kind) in self.list(dir) {
            if kind == FileType::Directory || !accept(&name) {
                continue;
            }
            if self.out.files.len() >= self.max_files {
                self.out.truncated = true;
                return;
            }
            if let Some(file) = found(dir, &name, format!("{trail}/{name}")) {
                self.out.files.push(file);
            }
        }
    }

    /// The subdirectories of `dir`, each opened and passed to `each`.
    fn children(
        &mut self,
        dir: &OwnedFd,
        trail: &str,
        mut each: impl FnMut(&mut Self, &OwnedFd, &str),
    ) {
        for (name, kind) in self.list(dir) {
            if !Self::is_dir(dir, &name, kind) {
                continue;
            }
            let Some(child) = self.open(dir, &name) else {
                if self.out.truncated {
                    return;
                }
                continue;
            };
            each(self, &child, &format!("{trail}/{name}"));
            if self.out.truncated {
                return;
            }
        }
    }

    fn claude(&mut self, root: &OwnedFd) {
        let Some(projects) = self.open(root, "projects") else {
            return;
        };
        self.children(&projects, "projects", |w, project, trail| {
            w.files(project, trail, is_claude_file);
            w.children(project, trail, |w, session, trail| {
                let Some(subagents) = w.open(session, "subagents") else {
                    return;
                };
                let trail = format!("{trail}/subagents");
                w.files(&subagents, &trail, is_claude_file);
                let Some(workflows) = w.open(&subagents, "workflows") else {
                    return;
                };
                let trail = format!("{trail}/workflows");
                w.children(&workflows, &trail, |w, run, trail| {
                    w.files(run, trail, is_workflow_file);
                });
            });
        });
    }

    fn codex(&mut self, root: &OwnedFd) {
        if let Some(sessions) = self.open(root, "sessions") {
            self.children(&sessions, "sessions", |w, year, trail| {
                w.children(year, trail, |w, month, trail| {
                    w.children(month, trail, |w, day, trail| {
                        w.files(day, trail, is_rollout)
                    });
                });
            });
        }
        if let Some(archived) = self.open(root, "archived_sessions") {
            self.files(&archived, "archived_sessions", is_rollout);
        }
    }
}

/// Walks one home with the default budget.
pub(super) fn walk_home(root: &OwnedFd, kind: ProviderKind) -> Walked {
    walk_with(root, kind, MAX_DIRS, MAX_FILES)
}

/// Walks one home with a budget of `max_dirs` directories and `max_files`
/// files.
pub(super) fn walk_with(
    root: &OwnedFd,
    kind: ProviderKind,
    max_dirs: usize,
    max_files: usize,
) -> Walked {
    let mut walker = Walker::new(max_dirs, max_files);
    match kind {
        ProviderKind::Claude => walker.claude(root),
        ProviderKind::Codex => walker.codex(root),
        ProviderKind::Unknown => {}
    }
    walker.out
}
