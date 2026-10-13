//! The station's own private paths (DL-173 section 2.1): its credential
//! and config locations. The file service, the search, the turn diff, the
//! session diff and the artifact publish refuse them for every principal,
//! the owner too, also when they lie inside an allowed root or are the
//! workspace root. The check does not depend on `member_deny`.
//!
//! The set is built once at start ([`PrivatePaths::new`]); it never changes
//! while the process runs, so it is not part of the live policy snapshot. A
//! manager that never received a set refuses every path
//! ([`PrivatePaths::deny_all`]).
//!
//! Every comparison is on canonical paths. The file service walks without
//! following a symlink, so the root joined with the validated relative path
//! is already canonical there ([`PrivatePaths::is_private_canonical`]). A
//! file that exists at build time also counts by its (device, inode), so a
//! hard link to it is private too.

use std::borrow::Cow;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use fleetdeck_core::paths::NodeDirs;

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
    /// The (device, inode) of the exact entries that existed at build time.
    ids: Vec<(u64, u64)>,
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
    ///   (without it those entries are left out and one warning is logged);
    /// - `deck`: the deck config file and the deck data directory, when the
    ///   deck runs in the same process;
    /// - `env`: the environment values that name more credential directories.
    #[must_use]
    pub fn new(
        dirs: &NodeDirs,
        home: Option<&Path>,
        deck: Option<(&Path, &Path)>,
        env: &PrivateEnv,
    ) -> Self {
        let mut out = Self::default();
        out.add_dir(&dirs.station());
        out.add_config_file(&dirs.station_config());
        if let Some((config, data)) = deck {
            out.add_dir(data);
            out.add_config_file(config);
        }
        match home {
            Some(home) => {
                for dir in [".config/gh", ".config/glab-cli", ".ssh"] {
                    out.add_dir(&home.join(dir));
                }
                for file in [".claude/.credentials.json", ".codex/auth.json"] {
                    out.add_file(&home.join(file));
                }
            }
            None => tracing::warn!(
                "HOME is not set: the credential paths of gh, glab, ssh, claude and codex are \
                 not private paths"
            ),
        }
        if let Some(xdg) = &env.xdg_config_home {
            out.add_dir(&xdg.join("gh"));
            out.add_dir(&xdg.join("glab-cli"));
        }
        if let Some(gh) = &env.gh_config_dir {
            out.add_dir(gh);
        }
        out
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

    fn add_file(&mut self, file: &Path) {
        let canonical = canonical_of(file);
        if let Some(meta) = std::fs::metadata(&canonical)
            .ok()
            .filter(std::fs::Metadata::is_file)
        {
            self.ids.push((meta.dev(), meta.ino()));
        }
        self.exact.push(canonical);
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
    /// to an exact entry that existed at build time.
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
        // A hard link to an exact entry has another path and the same
        // inode. The link itself is not followed.
        temp || (!self.ids.is_empty()
            && std::fs::symlink_metadata(path).is_ok_and(|m| self.is_private_file(&m)))
    }

    /// True when `meta` (of a file that is open or not followed) is a
    /// private file by its (device, inode): an exact entry that existed at
    /// build time, or a hard link to one.
    #[must_use]
    pub fn is_private_file(&self, meta: &std::fs::Metadata) -> bool {
        self.deny_all || (meta.is_file() && self.ids.contains(&(meta.dev(), meta.ino())))
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
        out.sort();
        out.dedup();
        out
    }
}

#[cfg(test)]
#[path = "private_paths_tests.rs"]
pub(crate) mod private_paths_tests;
