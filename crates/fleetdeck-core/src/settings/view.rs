//! The settings view: the values of the parsed file, the field table and
//! the revision (design 1.1).

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::SystemTime;

use fleetdeck_proto::settings::SettingsView;

use super::holder::{DeckHolder, Holder, StationHolder};
use crate::config::ConfigError;
use crate::paths::Env;

/// The hex SHA-256 of `bytes`: the `revision` of a file.
#[must_use]
pub fn revision_of(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().fold(String::new(), |mut out, b| {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The bytes and the modification time of a config file. A missing file is
/// empty text without a time. A file that other users can write is refused,
/// as the loader does.
pub(super) fn read_config(path: &Path) -> io::Result<(Vec<u8>, Option<SystemTime>)> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), None)),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if meta.permissions().mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the config file is writable by other users",
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok((bytes, meta.modified().ok()))
}

fn config_io(path: &Path, source: io::Error) -> ConfigError {
    ConfigError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn view<H: Holder>(
    path: &Path,
    env: &Env,
    store: Option<&Path>,
    overrides: &[(&str, &str)],
) -> Result<SettingsView, ConfigError> {
    let (bytes, _) = read_config(path).map_err(|e| config_io(path, e))?;
    let text = String::from_utf8(bytes.clone()).map_err(|_| ConfigError::Toml {
        path: path.to_path_buf(),
        message: "the file is not UTF-8 text".to_owned(),
    })?;
    let parsed = H::parse(&text, env, store)?;
    let mut fields = H::fields();
    for f in &mut fields {
        if let Some((_, flag)) = overrides.iter().find(|(k, _)| *k == f.key) {
            f.cli_flag = Some((*flag).to_owned());
        }
    }
    // The field table is the allowlist: only its keys reach the answer.
    let mut all = H::values(&parsed);
    let values = fields
        .iter()
        .filter_map(|f| all.remove(&f.key).map(|v| (f.key.clone(), v)))
        .collect();
    Ok(SettingsView {
        values,
        fields,
        revision: revision_of(&bytes),
        overrides: overrides
            .iter()
            .filter(|(k, _)| H::fields().iter().any(|f| f.key == *k))
            .map(|(k, _)| (*k).to_owned())
            .collect(),
    })
}

/// The settings of a station. `overrides` are (key, flag) pairs of the
/// values that the running process took from the command line.
///
/// # Errors
/// [`ConfigError`] when the file cannot be read or does not parse.
pub fn view_station(
    path: &Path,
    env: &Env,
    store: &Path,
    overrides: &[(&str, &str)],
) -> Result<SettingsView, ConfigError> {
    view::<StationHolder>(path, env, Some(store), overrides)
}

/// The settings of the deck.
///
/// # Errors
/// [`ConfigError`] when the file cannot be read or does not parse.
pub fn view_deck(path: &Path, overrides: &[(&str, &str)]) -> Result<SettingsView, ConfigError> {
    view::<DeckHolder>(path, &Env::default(), None, overrides)
}
