//! The owner rules for the config files of `config.rs` (NV-08).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde::Deserialize;

use super::{ConfigError, RawEnvValue};

/// Reads the file and gives its text and its mode (`mode & 0o777`). A
/// missing file gives `None`.
pub(super) fn read_owner_file(path: &Path) -> Result<Option<(String, u32)>, ConfigError> {
    let io = |source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    };
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o022 != 0 {
        return Err(ConfigError::Mode {
            path: path.to_path_buf(),
            mode,
        });
    }
    fs::read_to_string(path)
        .map(|text| Some((text, mode)))
        .map_err(io)
}

/// The part of a station file that holds the provider `env` tables.
#[derive(Deserialize)]
struct EnvScan {
    #[serde(default)]
    provider_instance: Vec<EnvOnly>,
}

#[derive(Deserialize)]
struct EnvOnly {
    #[serde(default)]
    env: BTreeMap<String, RawEnvValue>,
}

/// True when a provider instance has at least one plain `env` value. A text
/// that does not parse gives false: the main parse reports the error.
pub(super) fn has_plain_env(text: &str) -> bool {
    toml::from_str::<EnvScan>(text).is_ok_and(|scan| {
        scan.provider_instance
            .iter()
            .flat_map(|i| i.env.values())
            .any(|v| matches!(v, RawEnvValue::Plain(_)))
    })
}
