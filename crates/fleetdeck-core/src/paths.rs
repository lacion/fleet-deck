//! Data and config directories (A11: Linux and macOS).
//!
//! Default layout:
//! - config: `$XDG_CONFIG_HOME/fleetdeck` or `~/.config/fleetdeck`
//!   (`station.toml`, `deck.toml`; PLAN §6.7)
//! - data: `$XDG_DATA_HOME/fleetdeck` or `~/.local/share/fleetdeck`
//!
//! `--data-dir` (or `FLEETDECK_DATA_DIR`) puts everything into one directory,
//! config files included. Multi-process tests give each node its own
//! directory this way. Inside the data directory, the deck and the station
//! of one machine are separate nodes with separate identities and databases:
//! `deck/` and `station/`.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

/// The environment values that decide the default directories. Tests build
/// it by hand; the binary uses [`Env::from_process`].
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
}

impl Env {
    pub fn from_process() -> Self {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        Self {
            home: var("HOME"),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            xdg_data_home: var("XDG_DATA_HOME"),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathError {
    #[error("HOME is not set, so `{0}` cannot be expanded; give an absolute path")]
    NoHome(String),
    #[error("`{0}`: only `~` and `~/...` are supported, not `~user`")]
    OtherUser(String),
    #[error("`{0}` is not an absolute path")]
    Relative(String),
    #[error("`{0}` contains `..`; give a direct path")]
    ParentComponent(String),
}

/// Expands a leading `~` and checks that the result is absolute and has no
/// `..` component. `.` components are removed.
pub fn expand_path(raw: &str, home: Option<&Path>) -> Result<PathBuf, PathError> {
    let expanded = if raw == "~" || raw.starts_with("~/") {
        let home = home.ok_or_else(|| PathError::NoHome(raw.to_owned()))?;
        home.join(raw.trim_start_matches('~').trim_start_matches('/'))
    } else if raw.starts_with('~') {
        return Err(PathError::OtherUser(raw.to_owned()));
    } else {
        PathBuf::from(raw)
    };
    if !expanded.is_absolute() {
        return Err(PathError::Relative(raw.to_owned()));
    }
    let mut clean = PathBuf::new();
    for c in expanded.components() {
        match c {
            Component::ParentDir => return Err(PathError::ParentComponent(raw.to_owned())),
            Component::CurDir => {}
            other => clean.push(other),
        }
    }
    Ok(clean)
}

/// Where one machine keeps its Fleet Deck files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeDirs {
    pub config: PathBuf,
    pub data: PathBuf,
}

impl NodeDirs {
    /// `explicit` is `--data-dir`; without it, the XDG defaults apply.
    pub fn resolve(explicit: Option<&Path>, env: &Env) -> Result<Self, PathError> {
        if let Some(dir) = explicit {
            let raw = dir.to_string_lossy();
            let dir = if dir.is_absolute() {
                expand_path(&raw, env.home.as_deref())?
            } else {
                let cwd = std::env::current_dir()
                    .map_err(|_| PathError::Relative(raw.clone().into_owned()))?;
                expand_path(&cwd.join(dir).to_string_lossy(), env.home.as_deref())?
            };
            return Ok(Self {
                config: dir.clone(),
                data: dir,
            });
        }
        let home = env.home.as_deref();
        let base = |xdg: &Option<PathBuf>, fallback: &str| match xdg {
            Some(p) => Ok(p.join("fleetdeck")),
            None => expand_path(fallback, home),
        };
        Ok(Self {
            config: base(&env.xdg_config_home, "~/.config/fleetdeck")?,
            data: base(&env.xdg_data_home, "~/.local/share/fleetdeck")?,
        })
    }

    pub fn station_config(&self) -> PathBuf {
        self.config.join("station.toml")
    }

    pub fn deck_config(&self) -> PathBuf {
        self.config.join("deck.toml")
    }

    /// The station node: identity key and `station.db`.
    pub fn station(&self) -> PathBuf {
        self.data.join("station")
    }

    /// The deck node: identity key and `deck.db`.
    pub fn deck(&self) -> PathBuf {
        self.data.join("deck")
    }

    pub fn station_db(&self) -> PathBuf {
        self.station().join("station.db")
    }

    pub fn deck_db(&self) -> PathBuf {
        self.deck().join("deck.db")
    }
}

/// Creates `dir` (and its parents) and makes `dir` itself owner-only (0700).
/// Keys, databases and login files live in such directories.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = fs::metadata(dir)?;
    if meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Env {
        Env {
            home: Some("/home/ana".into()),
            ..Env::default()
        }
    }

    #[test]
    fn tilde_expansion_and_path_rules() {
        let home = Some(Path::new("/home/ana"));
        assert_eq!(expand_path("~", home).unwrap(), Path::new("/home/ana"));
        assert_eq!(
            expand_path("~/projects/./x", home).unwrap(),
            Path::new("/home/ana/projects/x")
        );
        assert_eq!(
            expand_path("/workspace/", home).unwrap(),
            Path::new("/workspace")
        );
        assert_eq!(
            expand_path("~/x", None),
            Err(PathError::NoHome("~/x".into()))
        );
        assert_eq!(
            expand_path("~bob/x", home),
            Err(PathError::OtherUser("~bob/x".into()))
        );
        assert_eq!(
            expand_path("projects", home),
            Err(PathError::Relative("projects".into()))
        );
        assert_eq!(
            expand_path("/a/../etc", home),
            Err(PathError::ParentComponent("/a/../etc".into()))
        );
    }

    #[test]
    fn default_directories_follow_xdg() {
        let dirs = NodeDirs::resolve(None, &env()).unwrap();
        assert_eq!(dirs.config, Path::new("/home/ana/.config/fleetdeck"));
        assert_eq!(dirs.data, Path::new("/home/ana/.local/share/fleetdeck"));
        assert_eq!(
            dirs.station_config(),
            Path::new("/home/ana/.config/fleetdeck/station.toml")
        );
        assert_eq!(
            dirs.station_db(),
            Path::new("/home/ana/.local/share/fleetdeck/station/station.db")
        );
        let xdg = Env {
            xdg_config_home: Some("/cfg".into()),
            xdg_data_home: Some("/data".into()),
            ..env()
        };
        let dirs = NodeDirs::resolve(None, &xdg).unwrap();
        assert_eq!(dirs.deck_config(), Path::new("/cfg/fleetdeck/deck.toml"));
        assert_eq!(dirs.deck_db(), Path::new("/data/fleetdeck/deck/deck.db"));
        assert!(NodeDirs::resolve(None, &Env::default()).is_err());
    }

    #[test]
    fn an_explicit_data_dir_holds_config_and_data() {
        let dirs = NodeDirs::resolve(Some(Path::new("/tmp/node-a")), &env()).unwrap();
        assert_eq!(dirs.config, Path::new("/tmp/node-a"));
        assert_eq!(dirs.station(), Path::new("/tmp/node-a/station"));
        assert_eq!(dirs.station_config(), Path::new("/tmp/node-a/station.toml"));
        let rel = NodeDirs::resolve(Some(Path::new("node-b")), &env()).unwrap();
        assert!(rel.data.is_absolute() && rel.data.ends_with("node-b"));
    }

    #[test]
    fn private_dirs_are_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a/b");
        ensure_private_dir(&dir).unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
