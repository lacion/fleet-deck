//! Tests of the settings core. `CANARY` strings stand for secrets: none may
//! appear in a view, an answer or an error.

mod fault;
mod fields;
mod p7b;
mod save;
mod secrets;
mod tailnet_auto;
mod testfs;

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;

use fleetdeck_proto::settings::SettingsChange;
use serde_json::Value;

use super::{Fs, SettingsWriter, StationHolder};
use crate::paths::Env;

pub const CANARY: &str = "canary-inline-secret-7731";
pub const CANARY_REF: &str = "canary-stored-secret-4402";

/// A temp directory with the layout of one machine.
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub config: PathBuf,
    pub store: PathBuf,
    pub env: Env,
}

impl Fixture {
    /// `text` is the content of `station.toml`.
    pub fn new(text: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("cfg");
        std::fs::create_dir_all(&cfg).unwrap();
        let config = cfg.join("station.toml");
        if let Some(text) = text {
            std::fs::write(&config, text).unwrap();
            // The settings writer writes mode 0600. NV-08 refuses a readable
            // file with plain `env` values.
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let store = dir.path().join("data").join("station").join("secrets");
        let env = Env {
            home: Some(dir.path().to_path_buf()),
            ..Env::default()
        };
        Self {
            dir,
            config,
            store,
            env,
        }
    }

    pub fn writer(&self) -> SettingsWriter<StationHolder> {
        SettingsWriter::new(&self.config, Some(&self.store), &self.env)
    }

    pub fn writer_with(&self, fs: Arc<dyn Fs>) -> SettingsWriter<StationHolder> {
        SettingsWriter::with_fs(&self.config, Some(&self.store), &self.env, fs)
    }

    pub fn text(&self) -> String {
        std::fs::read_to_string(&self.config).unwrap_or_default()
    }

    pub fn revision(&self) -> String {
        super::revision_of(self.text().as_bytes())
    }

    pub fn change(&self, changes: Value) -> SettingsChange {
        change_with(&self.revision(), changes)
    }

    pub fn load(&self) -> Result<crate::config::StationConfig, crate::config::ConfigError> {
        crate::config::StationConfig::load_with_store(&self.config, &self.env, &self.store)
    }

    /// The names in the store directory.
    pub fn secrets(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.store)
            .map(|d| {
                d.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Files in the config directory other than `station.toml`.
    pub fn leftovers(&self) -> Vec<String> {
        std::fs::read_dir(self.config.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "station.toml")
            .collect()
    }
}

pub fn change_with(revision: &str, changes: Value) -> SettingsChange {
    let Value::Object(changes) = changes else {
        panic!("changes must be an object");
    };
    SettingsChange {
        changes,
        revision: revision.to_owned(),
        elevation: None,
    }
}

pub fn ok_prepare<T>(_: &T) -> Result<(), (String, String)> {
    Ok(())
}

/// A station file with one instance that holds a plain inline value.
pub fn inline_station() -> String {
    format!(
        "# my station\nname = \"box\"\nallowed_roots = [\"/tmp\"]\n\n[[provider_instance]]\nid = \"work\"\ndriver = \"claude\"\nenv = {{ ANTHROPIC_API_KEY = \"{CANARY}\" }}\n"
    )
}
