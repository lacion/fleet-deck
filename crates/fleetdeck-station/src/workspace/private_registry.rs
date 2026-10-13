//! The inode registry of the station's credential files (DL-173 section
//! 2.1, PRE-PR r2 finding 1).
//!
//! A hard link to a credential file keeps the inode of the file. The
//! registry holds the (device, inode) of every credential file that the
//! station has seen or written, so a link stays refused after the original
//! is deleted (logout) or replaced (an atomic write), whatever its link
//! count.
//!
//! - It is persisted in one file: one `dev:ino` line each, mode 0600,
//!   appended to. It is loaded at start.
//! - It holds at most [`REGISTRY_CAP`] entries: when an entry makes it
//!   longer, the oldest half is dropped and the file is rewritten.
//! - Every file write is best effort: a failure logs one warning and keeps
//!   the in-memory entry.
//! - An inode that the file system reuses for an unrelated file makes that
//!   file unreadable through the file routes (fail closed).

use std::collections::{HashSet, VecDeque};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use rustix::fs::OFlags;

/// The most entries of the registry.
pub const REGISTRY_CAP: usize = 20_000;

/// The depth of the walk of `register_dir` (accounts and secrets).
const DEEP: usize = 8;

/// The depth below a credential directory of the user (`~/.ssh`,
/// `~/.config/gh`): `dir/a/b/c/d/file` is the deepest file.
pub const CREDENTIAL_DEPTH: usize = 4;

/// The most files that a scan registers for one credential directory.
pub const CREDENTIAL_FILES: usize = 10_000;

/// The (device, inode) of a file.
pub type FileId = (u64, u64);

#[derive(Debug, Default)]
struct State {
    /// Oldest first.
    order: VecDeque<FileId>,
    set: HashSet<FileId>,
}

/// The registry: in memory, and in `file` when it has one.
#[derive(Debug)]
pub struct Registry {
    file: Option<PathBuf>,
    cap: usize,
    state: RwLock<State>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            file: None,
            cap: REGISTRY_CAP,
            state: RwLock::default(),
        }
    }
}

fn flags(extra: OFlags) -> i32 {
    i32::try_from((OFlags::NOFOLLOW | extra).bits()).unwrap_or(0)
}

impl Registry {
    /// The registry that is persisted in `file`; the file is loaded now.
    #[must_use]
    pub fn load(file: PathBuf) -> Self {
        Self::load_with_cap(file, REGISTRY_CAP)
    }

    /// [`Self::load`] with another cap (tests).
    #[must_use]
    pub fn load_with_cap(file: PathBuf, cap: usize) -> Self {
        let mut state = State::default();
        match read_file(&file) {
            Ok(ids) => {
                for id in ids {
                    if state.set.insert(id) {
                        state.order.push_back(id);
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "the private inode registry cannot be read"),
        }
        let registry = Self {
            file: Some(file),
            cap,
            state: RwLock::new(state),
        };
        {
            let mut state = registry.write();
            if registry.trim(&mut state) {
                registry.rewrite(&state);
            }
        }
        registry
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, State> {
        self.state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn contains(&self, id: FileId) -> bool {
        self.read().set.contains(&id)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.read().set.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.read().set.len()
    }

    /// Adds `id`: in memory first, then to the file.
    pub fn add(&self, id: FileId) {
        let mut state = self.write();
        if !state.set.insert(id) {
            return;
        }
        state.order.push_back(id);
        if self.trim(&mut state) {
            self.rewrite(&state);
        } else {
            self.append(id);
        }
    }

    /// Drops the oldest half when the registry is over its cap. Returns
    /// whether it dropped.
    fn trim(&self, state: &mut State) -> bool {
        if state.order.len() <= self.cap {
            return false;
        }
        for _ in 0..state.order.len() / 2 {
            if let Some(old) = state.order.pop_front() {
                state.set.remove(&old);
            }
        }
        true
    }

    fn append(&self, id: FileId) {
        let Some(path) = &self.file else { return };
        let done = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(flags(OFlags::empty()))
            .open(path)
            .and_then(|mut f| {
                writeln!(f, "{}:{}", id.0, id.1)?;
                f.sync_data()
            });
        if let Err(e) = done {
            tracing::warn!(error = %e, "the private inode registry file cannot be written");
        }
    }

    /// Replaces the file by the entries in memory (a temp file, renamed).
    fn rewrite(&self, state: &State) {
        let Some(path) = &self.file else { return };
        let mut tmp = path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let done = (|| -> std::io::Result<()> {
            let mut text = String::new();
            for (dev, ino) in &state.order {
                text.push_str(&format!("{dev}:{ino}\n"));
            }
            let _ = std::fs::remove_file(&tmp);
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if let Err(e) = done {
            tracing::warn!(error = %e, "the private inode registry file cannot be written");
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Registers the file at `path`: opens it without following a link and
    /// takes the identity of the opened file. Anything but a regular file
    /// is left out. A missing file is not an error.
    pub fn register(&self, path: &Path) {
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(flags(OFlags::NONBLOCK))
            .open(path)
            .and_then(|f| f.metadata());
        match opened {
            Ok(meta) if meta.is_file() => self.add((meta.dev(), meta.ino())),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                path = %path.display(),
                error = %e,
                "a credential file cannot be registered"
            ),
        }
    }

    /// Registers every regular file below `dir`. A link is not followed.
    pub fn register_dir(&self, dir: &Path) {
        self.walk(dir, 0, DEEP, &mut Budget::unlimited());
    }

    /// [`Self::register_dir`] for a credential directory of the user: at
    /// most [`CREDENTIAL_DEPTH`] levels below `dir` and
    /// [`CREDENTIAL_FILES`] files. A directory over the budget is logged
    /// once; the files after the budget stay unregistered.
    pub fn register_credential_dir(&self, dir: &Path) {
        self.register_credential_dir_with(dir, CREDENTIAL_DEPTH, CREDENTIAL_FILES);
    }

    /// [`Self::register_credential_dir`] with other limits (tests).
    pub fn register_credential_dir_with(&self, dir: &Path, depth: usize, files: usize) {
        let mut budget = Budget {
            files,
            exceeded: false,
        };
        self.walk(dir, 0, depth, &mut budget);
        if budget.exceeded {
            tracing::warn!(
                path = %dir.display(),
                files,
                "a credential directory has more files than the scan registers"
            );
        }
    }

    fn walk(&self, dir: &Path, depth: usize, max_depth: usize, budget: &mut Budget) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!(
                    path = %dir.display(),
                    error = %e,
                    "a credential directory cannot be scanned"
                );
                return;
            }
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_file() {
                if budget.files == 0 {
                    budget.exceeded = true;
                    return;
                }
                budget.files -= 1;
                self.register(&entry.path());
            } else if kind.is_dir() && depth < max_depth {
                self.walk(&entry.path(), depth + 1, max_depth, budget);
                if budget.exceeded {
                    return;
                }
            }
        }
    }
}

/// How many regular files a walk may still register.
struct Budget {
    files: usize,
    /// A file was left out because `files` ran out.
    exceeded: bool,
}

impl Budget {
    fn unlimited() -> Self {
        Self {
            files: usize::MAX,
            exceeded: false,
        }
    }
}

/// The identities of the file: `Ok(empty)` when it does not exist. A line
/// that does not parse is left out and one warning counts them.
fn read_file(path: &Path) -> std::io::Result<Vec<FileId>> {
    let text = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags(OFlags::empty()))
        .open(path)
    {
        Ok(mut f) => {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut f, &mut text)?;
            text
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut ids = Vec::new();
    let mut bad = 0usize;
    for line in text.lines().filter(|l| !l.is_empty()) {
        match line
            .split_once(':')
            .and_then(|(d, i)| Some((d.parse().ok()?, i.parse().ok()?)))
        {
            Some(id) => ids.push(id),
            None => bad += 1,
        }
    }
    if bad > 0 {
        tracing::warn!(
            lines = bad,
            "the private inode registry has lines that do not parse"
        );
    }
    Ok(ids)
}

#[cfg(test)]
#[path = "private_registry_tests.rs"]
mod private_registry_tests;
