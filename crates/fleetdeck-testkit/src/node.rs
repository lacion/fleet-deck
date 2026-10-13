//! Temporary node directories: one fake home and one data directory per
//! station or deck, removed when the [`TempNode`] is dropped.
//!
//! Multi-station tests (PLAN §13) create one `TempNode` for each process, so
//! no two nodes share a key, a database, or a config file.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use fleetdeck_core::config::{ConfigError, StationConfig};
use fleetdeck_core::paths::{Env, NodeDirs, ensure_private_dir};

use crate::fake::FakeAgent;

/// A temporary node: `<tmp>/home` is the fake home directory and
/// `<tmp>/node` is the `--data-dir` of the node.
#[derive(Debug)]
pub struct TempNode {
    name: String,
    root: tempfile::TempDir,
    env: Env,
    dirs: NodeDirs,
}

impl TempNode {
    /// Creates the directories with mode 0700.
    pub fn new(name: &str) -> io::Result<Self> {
        let root = tempfile::Builder::new()
            .prefix(&format!("fleetdeck-{name}-"))
            .tempdir()?;
        let home = root.path().join("home");
        ensure_private_dir(&home)?;
        let env = Env {
            home: Some(home),
            xdg_config_home: None,
            xdg_data_home: None,
        };
        let dirs =
            NodeDirs::resolve(Some(&root.path().join("node")), &env).map_err(io::Error::other)?;
        ensure_private_dir(&dirs.config)?;
        ensure_private_dir(&dirs.data)?;
        Ok(Self {
            name: name.to_owned(),
            root,
            env,
            dirs,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// The fake home directory (`~`).
    #[must_use]
    pub fn home(&self) -> &Path {
        self.env.home.as_deref().unwrap_or(self.root.path())
    }

    /// The environment that config parsing sees (`~` = [`Self::home`]).
    #[must_use]
    pub fn env(&self) -> &Env {
        &self.env
    }

    #[must_use]
    pub fn dirs(&self) -> &NodeDirs {
        &self.dirs
    }

    /// The value for `fleetdeck --data-dir`.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.dirs.data
    }

    /// Creates `~/projects/<name>` (an allowed root of the default config).
    pub fn workspace(&self, name: &str) -> io::Result<PathBuf> {
        let dir = self.home().join("projects").join(name);
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Writes `station.toml` (mode 0600) and returns its path.
    pub fn write_station_toml(&self, text: &str) -> io::Result<PathBuf> {
        self.write_config(self.dirs.station_config(), text)
    }

    /// Writes `deck.toml` (mode 0600) and returns its path.
    pub fn write_deck_toml(&self, text: &str) -> io::Result<PathBuf> {
        self.write_config(self.dirs.deck_config(), text)
    }

    fn write_config(&self, path: PathBuf, text: &str) -> io::Result<PathBuf> {
        if let Some(parent) = path.parent() {
            ensure_private_dir(parent)?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        Ok(path)
    }

    /// A `station.toml` for tests: allowed root `~/projects`, no listener,
    /// and one provider instance per given fake agent, with the fake binary
    /// in `fake_bin_dir`.
    #[must_use]
    pub fn station_toml(&self, fake_bin_dir: &Path, agents: &[FakeAgent]) -> String {
        let mut text = format!(
            "name = \"{}\"\nallowed_roots = [\"~/projects\"]\nmax_sessions = 4\nlisten = []\n",
            self.name
        );
        for agent in agents {
            let binary = fake_bin_dir.join(agent.bin_name());
            text.push_str(&format!(
                "\n[[provider_instance]]\nid = \"{}\"\ndriver = \"{}\"\nbinary = \"{}\"\n",
                agent.instance_id(),
                agent.driver(),
                binary.display()
            ));
        }
        text
    }

    /// Loads `station.toml` the same way the station does.
    pub fn station_config(&self) -> Result<StationConfig, ConfigError> {
        StationConfig::load_with_store(
            &self.dirs.station_config(),
            &self.env,
            &self.dirs.station().join("secrets"),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use fleetdeck_core::config::Driver;
    use fleetdeck_proto::ProviderInstanceId;

    use super::*;

    #[test]
    fn nodes_get_private_separate_directories() {
        let a = TempNode::new("alpha").unwrap();
        let b = TempNode::new("beta").unwrap();
        assert_ne!(a.data_dir(), b.data_dir());
        assert!(a.data_dir().starts_with(a.root()));
        for dir in [a.home(), &a.dirs().config, &a.dirs().data] {
            let mode = fs::metadata(dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
        }
        assert_eq!(
            a.dirs().station_db(),
            a.data_dir().join("station/station.db")
        );
    }

    #[test]
    fn the_test_station_config_loads() {
        let node = TempNode::new("laptop").unwrap();
        let ws = node.workspace("app").unwrap();
        let bins = node.root().join("bin");
        let text = node.station_toml(&bins, &[FakeAgent::Claude, FakeAgent::Codex]);
        let path = node.write_station_toml(&text).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let cfg = node.station_config().unwrap();
        assert_eq!(cfg.name, "laptop");
        assert!(cfg.listen.is_empty());
        assert!(cfg.is_allowed(&ws));
        assert!(!cfg.is_allowed(node.root()));
        let claude = cfg
            .instance(&ProviderInstanceId::from("fake-claude".to_owned()))
            .unwrap();
        assert_eq!(claude.driver, Driver::Claude);
        assert_eq!(claude.binary, bins.join("fake-claude"));
        let codex = cfg
            .instance(&ProviderInstanceId::from("fake-codex".to_owned()))
            .unwrap();
        assert_eq!(codex.driver, Driver::Codex);
    }

    #[test]
    fn the_directories_are_removed_on_drop() {
        let node = TempNode::new("gone").unwrap();
        let root = node.root().to_path_buf();
        drop(node);
        assert!(!root.exists());
    }
}
