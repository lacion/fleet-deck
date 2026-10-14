//! The station's own private paths (DL-173 section 2.1): its credential
//! and config locations. The file service, the search, the turn diff, the
//! session diff and the artifact publish refuse them for every principal,
//! the owner too, also when they lie inside an allowed root or are the
//! workspace root. The check does not depend on `member_deny`.
//!
//! The set is built once at start ([`PrivatePaths::new`]); it never changes
//! while the process runs, so it is not part of the live policy snapshot
//! (the inode registry below grows, see [`PrivatePaths::register`]). A
//! manager that never received a set refuses every path
//! ([`PrivatePaths::deny_all`]).
//!
//! Every comparison is on canonical paths. The file service walks without
//! following a symlink, so the root joined with the validated relative path
//! is already canonical there ([`PrivatePaths::is_private_canonical`]). A
//! credential file also counts by its (device, inode), so a hard link to it
//! is private too, also after the original is deleted or replaced: the
//! inode registry ([`super::private_registry`]) keeps every identity that
//! the station has seen or written (PRE-PR r2 finding 1).

use std::borrow::Cow;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use fleetdeck_core::paths::NodeDirs;

use super::private_registry::Registry;

/// The name of the inode registry file in the station data directory.
pub const REGISTRY_FILE: &str = "private-inodes";

/// The process environment values that name more credential directories.
#[derive(Debug, Clone, Default)]
pub struct PrivateEnv {
    /// `$XDG_CONFIG_HOME` (absolute): `gh` and `glab-cli` live below it.
    pub xdg_config_home: Option<PathBuf>,
    /// `$GH_CONFIG_DIR` (absolute): the `gh` config directory.
    pub gh_config_dir: Option<PathBuf>,
}

impl PrivateEnv {
    /// The values of this process: an unset, empty or relative one is
    /// ignored.
    #[must_use]
    pub fn from_process() -> Self {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        Self {
            xdg_config_home: var("XDG_CONFIG_HOME"),
            gh_config_dir: var("GH_CONFIG_DIR"),
        }
    }
}

/// The private paths of one station process.
#[derive(Debug, Clone, Default)]
pub struct PrivatePaths {
    /// Private files (canonical where they exist).
    pub exact: Vec<PathBuf>,
    /// Private directories: they and everything below them (canonical where
    /// they exist).
    pub dirs: Vec<PathBuf>,
    /// True when every path is private (a manager that has no set yet).
    pub deny_all: bool,
    /// A name pattern `<prefix>*` below a directory: the temp files of an
    /// atomic config write (`<config file name>.tmp-*`).
    temp: Vec<(PathBuf, String)>,
    /// The credential directories of the user (`~/.ssh`, the `gh` and
    /// `glab-cli` config directories): every file below them is registered
    /// by [`Self::scan`] (PRE-PR r3 finding 1).
    credential_dirs: Vec<PathBuf>,
    /// True when HOME is not known: then every path that ends in a
    /// credential location of the home directory is private, and the
    /// pathspecs below match it at any depth (NV-31).
    home_unknown: bool,
    /// The (device, inode) of every credential file that the station has
    /// seen or written. It is shared by the clones of the set.
    registry: Arc<Registry>,
}

/// The canonical form of `path`; for a path that does not exist, the
/// canonical form of its nearest existing ancestor joined with the rest.
#[must_use]
pub fn canonical_of(path: &Path) -> PathBuf {
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path;
    loop {
        if let Ok(mut canonical) = current.canonicalize() {
            for name in rest.iter().rev() {
                canonical.push(name);
            }
            return canonical;
        }
        match (current.parent(), current.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                current = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The path as compared: on macOS the file systems fold case, so the
/// comparison is ASCII case-insensitive there.
fn fold(path: &Path) -> Cow<'_, Path> {
    if cfg!(target_os = "macos") {
        Cow::Owned(PathBuf::from(path.to_string_lossy().to_ascii_lowercase()))
    } else {
        Cow::Borrowed(path)
    }
}

fn fold_text(text: &str) -> Cow<'_, str> {
    if cfg!(target_os = "macos") {
        Cow::Owned(text.to_ascii_lowercase())
    } else {
        Cow::Borrowed(text)
    }
}

impl PrivatePaths {
    /// The set of a station process.
    ///
    /// - `dirs`: the station data directory and the station config file;
    /// - `home`: the home directory, for `~/.config/gh`, `~/.config/glab-cli`,
    ///   `~/.ssh`, `~/.claude/.credentials.json` and `~/.codex/auth.json`
    ///   (without it, every path that ends in such a location is private and
    ///   one warning is logged, NV-31);
    /// - `deck`: the deck config file and the deck data directory, when the
    ///   deck runs in the same process;
    /// - `env`: the environment values that name more credential directories.
    ///
    /// The inode registry is loaded from `<station data>/private-inodes`.
    /// Then every regular file in `<station data>/accounts/` and
    /// `<station data>/secrets/`, and every exact entry, is registered.
    #[must_use]
    pub fn new(
        dirs: &NodeDirs,
        home: Option<&Path>,
        deck: Option<(&Path, &Path)>,
        env: &PrivateEnv,
    ) -> Self {
        let mut out = Self {
            registry: Arc::new(Registry::load(dirs.station().join(REGISTRY_FILE))),
            ..Self::default()
        };
        out.add_dir(&dirs.station());
        out.add_config_file(&dirs.station_config());
        if let Some((config, data)) = deck {
            out.add_dir(data);
            out.add_config_file(config);
        }
        match home {
            Some(home) => {
                for dir in [".config/gh", ".config/glab-cli", ".ssh"] {
                    out.add_credential_dir(&home.join(dir));
                }
                for file in [".claude/.credentials.json", ".codex/auth.json"] {
                    out.add_file(&home.join(file));
                }
            }
            None => {
                // NV-31: fail closed. The allowed roots may hold the real home.
                out.home_unknown = true;
                tracing::warn!(
                    "HOME is not set: every path that ends in a gh, glab, ssh, claude or codex \
                     credential location is a private path"
                );
            }
        }
        if let Some(xdg) = &env.xdg_config_home {
            out.add_credential_dir(&xdg.join("gh"));
            out.add_credential_dir(&xdg.join("glab-cli"));
        }
        if let Some(gh) = &env.gh_config_dir {
            out.add_credential_dir(gh);
        }
        out.scan();
        out
    }

    /// Registers the credential files that exist now: the account and the
    /// secret directories of the station, the credential directories of the
    /// user (`~/.ssh`, `gh`, `glab-cli`; depth 4, 10,000 files each) and the
    /// exact entries. The station runs it at start and every minute
    /// ([`Self::rescan`]), so a credential that a tool created or rotated
    /// after the start is registered within one interval. A link to a file
    /// that is created and rotated within one interval stays unregistered
    /// (accepted residual risk, DL-173).
    fn scan(&self) {
        // The first `dirs` entry is the station data directory.
        if let Some(station) = self.dirs.first() {
            self.register_dir(&station.join("accounts"));
            self.register_dir(&station.join("secrets"));
        }
        for dir in &self.credential_dirs {
            self.registry.register_credential_dir(dir);
        }
        for file in &self.exact {
            self.register(file);
        }
    }

    /// Scans again (blocking file system work): the periodic task of the
    /// station and the tests call it.
    pub fn rescan(&self) {
        self.scan();
    }

    /// Registers the identity of the file at `path` (opened without
    /// following a link) in the inode registry. The registry file write is
    /// best effort: a failure logs one warning and keeps the entry in memory.
    /// Call it after every credential write.
    pub fn register(&self, path: &Path) {
        self.registry.register(path);
    }

    /// [`Self::register`] for every regular file below `dir`.
    pub fn register_dir(&self, dir: &Path) {
        self.registry.register_dir(dir);
    }

    /// A set that refuses every path: the state of a manager that has no
    /// set (fail closed).
    #[must_use]
    pub fn deny_all() -> Self {
        Self {
            deny_all: true,
            ..Self::default()
        }
    }

    fn add_dir(&mut self, dir: &Path) {
        self.dirs.push(canonical_of(dir));
    }

    /// A private directory whose files the scan registers too.
    fn add_credential_dir(&mut self, dir: &Path) {
        self.add_dir(dir);
        self.credential_dirs.push(canonical_of(dir));
    }

    fn add_file(&mut self, file: &Path) {
        self.exact.push(canonical_of(file));
    }

    /// A config file, and the temp files that an atomic write leaves next to
    /// it.
    fn add_config_file(&mut self, file: &Path) {
        self.add_file(file);
        if let (Some(dir), Some(name)) = (file.parent(), file.file_name()) {
            self.temp.push((
                canonical_of(dir),
                format!("{}.tmp-", name.to_string_lossy()),
            ));
        }
    }

    /// True when the canonical `path` equals an exact entry, lies under a
    /// directory entry, is a temp file of a config file, or is a hard link
    /// to a credential file of the inode registry.
    #[must_use]
    pub fn is_private_canonical(&self, path: &Path) -> bool {
        if self.deny_all {
            return true;
        }
        // Lexical forms that no canonical path has: fail closed.
        if path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return true;
        }
        let folded = fold(path);
        if self.home_unknown && has_home_credential_name(&folded) {
            return true;
        }
        if self.exact.iter().any(|e| *fold(e) == *folded) {
            return true;
        }
        if self.dirs.iter().any(|d| folded.starts_with(fold(d))) {
            return true;
        }
        let temp = self.temp.iter().any(|(dir, prefix)| {
            folded
                .strip_prefix(fold(dir))
                .ok()
                .and_then(|rest| rest.components().next())
                .is_some_and(|first| {
                    fold_text(&first.as_os_str().to_string_lossy())
                        .starts_with(fold_text(prefix).as_ref())
                })
        });
        // A hard link to a credential file has another path and the same
        // inode. The link itself is not followed.
        temp || (!self.registry.is_empty()
            && std::fs::symlink_metadata(path).is_ok_and(|m| self.is_private_file(&m)))
    }

    /// True when `meta` (of a file that is open or not followed) is a
    /// private file by its (device, inode): a file of the inode registry,
    /// or a hard link to one, whatever its link count.
    #[must_use]
    pub fn is_private_file(&self, meta: &std::fs::Metadata) -> bool {
        self.deny_all || (meta.is_file() && self.registry.contains((meta.dev(), meta.ino())))
    }

    /// True when the opened file must not be read, searched or staged:
    /// [`Self::is_private_file`], or a regular file with more than one hard
    /// link (a link to a credential cannot be told apart cheaply) unless one
    /// component of `rel` (its path below the root) is `.git`, where git
    /// itself makes hard links. `meta` is the metadata of the opened file,
    /// not of its name.
    #[must_use]
    pub fn is_private_opened(&self, meta: &std::fs::Metadata, rel: &str) -> bool {
        self.is_private_file(meta)
            || (meta.is_file() && meta.nlink() > 1 && !rel.split('/').any(|c| c == ".git"))
    }

    /// [`Self::is_private_canonical`] after the path is made canonical (for
    /// a path that does not exist: its nearest existing ancestor).
    #[must_use]
    pub fn is_private(&self, path: &Path) -> bool {
        self.deny_all || self.is_private_canonical(&canonical_of(path))
    }

    /// The git pathspecs that leave out the private entries below the
    /// worktree top `top` (canonical): `:(exclude,literal)<rel>` for each
    /// exact or directory entry under it, and a glob for each temp pattern.
    /// The pathspecs are relative to the directory where git runs, which is
    /// `top` for the checkpoint and session diffs.
    #[must_use]
    pub fn excludes_for(&self, top: &Path) -> Vec<String> {
        let magic = if cfg!(target_os = "macos") {
            "exclude,literal,icase"
        } else {
            "exclude,literal"
        };
        let mut out = Vec::new();
        for entry in self.exact.iter().chain(self.dirs.iter()) {
            let Ok(rel) = entry.strip_prefix(top) else {
                continue;
            };
            if let Some(rel) = rel.to_str().filter(|r| !r.is_empty()) {
                out.push(format!(":({magic}){rel}"));
            }
        }
        let glob = if cfg!(target_os = "macos") {
            "exclude,glob,icase"
        } else {
            "exclude,glob"
        };
        for (_, prefix) in &self.temp {
            out.push(format!(":({glob})**/{prefix}*"));
        }
        if self.home_unknown {
            // NV-31: the home is unknown, so match the names at any depth.
            for pattern in HOME_CREDENTIAL_GLOBS {
                out.push(format!(":({glob}){pattern}"));
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// The pathspecs for the credential locations of an unknown home (NV-31).
const HOME_CREDENTIAL_GLOBS: [&str; 5] = [
    "**/.claude/.credentials.json",
    "**/.codex/auth.json",
    "**/.config/gh/**",
    "**/.config/glab-cli/**",
    "**/.ssh/**",
];

/// True when the folded `path` ends in `.claude/.credentials.json` or
/// `.codex/auth.json`, has `.config/gh` or `.config/glab-cli` anywhere, or
/// has a `.ssh` component (NV-31).
fn has_home_credential_name(path: &Path) -> bool {
    let names: Vec<Cow<'_, str>> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy()),
            _ => None,
        })
        .collect();
    if names.iter().any(|n| n == ".ssh") {
        return true;
    }
    if let [.., dir, file] = names.as_slice()
        && ((dir == ".claude" && file == ".credentials.json")
            || (dir == ".codex" && file == "auth.json"))
    {
        return true;
    }
    names
        .windows(2)
        .any(|w| w[0] == ".config" && (w[1] == "gh" || w[1] == "glab-cli"))
}

#[cfg(test)]
#[path = "private_paths_tests.rs"]
pub(crate) mod private_paths_tests;
