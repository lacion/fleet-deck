//! The `tailscale_bin` key of `station.toml` and `deck.toml` (tailnet design
//! section 2): the absolute path of the `tailscale` program.

use std::path::{Path, PathBuf};

use super::{ConfigError, invalid};
use crate::paths::{PathError, expand_path};

const KEY: &str = "tailscale_bin";
const ABSOLUTE: &str = "tailscale_bin must be an absolute path";

/// The deck: the path as written, which must be absolute (`~` is not
/// expanded). An empty text is unset.
pub(super) fn parse_deck(raw: Option<String>) -> Result<Option<PathBuf>, ConfigError> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    if raw.contains('\0') || !Path::new(&raw).is_absolute() {
        return Err(invalid(KEY, ABSOLUTE));
    }
    Ok(Some(PathBuf::from(raw)))
}

/// The station: like the deck, and a leading `~` expands to the home of the
/// owner (the rule of every other station path).
pub(super) fn parse_station(
    raw: Option<String>,
    home: Option<&Path>,
) -> Result<Option<PathBuf>, ConfigError> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    if raw.contains('\0') {
        return Err(invalid(KEY, ABSOLUTE));
    }
    match expand_path(&raw, home) {
        Ok(path) => Ok(Some(path)),
        Err(PathError::Relative(_)) => Err(invalid(KEY, ABSOLUTE)),
        Err(source) => Err(ConfigError::Path {
            field: KEY.to_owned(),
            source,
        }),
    }
}

/// `tailnet_hostname` of the deck: empty is unset, else a DNS name in lower
/// case.
pub(super) fn parse_hostname(raw: Option<String>) -> Result<Option<String>, ConfigError> {
    match raw.filter(|h| !h.is_empty()) {
        None => Ok(None),
        Some(h) if valid_dns_name(&h) => Ok(Some(h.to_ascii_lowercase())),
        Some(h) => Err(invalid(
            "tailnet_hostname",
            format!("`{h}` is not a DNS name"),
        )),
    }
}

/// A DNS name of up to 253 bytes: labels of 1 to 63 letters, digits and `-`.
pub(crate) fn valid_dns_name(name: &str) -> bool {
    name.len() <= 253
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}
