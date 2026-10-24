//! `[default_mode]` of `station.toml` (B27 §3.5): the mode of a start that
//! gives none.

use serde::Deserialize;

use super::{ConfigError, invalid};
use crate::config_worker_grant::{CLAUDE_MODES, CODEX_POLICIES, CODEX_SANDBOXES, mode_within};

/// The Claude mode of a start without a mode.
pub const CLAUDE_PROVIDER_DEFAULT: &str = "default";
/// The Codex mode of a start without a mode.
pub const CODEX_PROVIDER_DEFAULT: &str = "untrusted/workspace-write";

/// `[default_mode]` of `station.toml` (B27 §3.5): the mode of a start that
/// gives none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefaultModes {
    /// `claude`; `None`: the provider default `default`.
    pub claude: Option<String>,
    /// `codex`; `None`: the provider default `untrusted/workspace-write`.
    pub codex: Option<String>,
}

impl DefaultModes {
    /// The mode for `family` ("claude" or "codex"): the key, or the provider
    /// default. `None` for another family.
    #[must_use]
    pub fn for_family(&self, family: &str) -> Option<&str> {
        match family {
            "claude" => Some(self.claude.as_deref().unwrap_or(CLAUDE_PROVIDER_DEFAULT)),
            "codex" => Some(self.codex.as_deref().unwrap_or(CODEX_PROVIDER_DEFAULT)),
            _ => None,
        }
    }
}

/// The keys of `[default_mode]` (a test compares them with the field table).
#[cfg(test)]
pub(crate) const RAW_DEFAULT_MODE_KEYS: &[&str] = &["claude", "codex"];

/// The nine Codex modes, `<policy>/<sandbox>`, policy first.
#[must_use]
pub fn codex_modes() -> Vec<String> {
    CODEX_POLICIES
        .iter()
        .flat_map(|p| CODEX_SANDBOXES.iter().map(move |s| format!("{p}/{s}")))
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawDefaultModes {
    claude: Option<String>,
    codex: Option<String>,
}

impl RawDefaultModes {
    /// # Errors
    /// An unknown mode; a bypass or full-access mode while `allow_bypass` is off.
    pub(super) fn validate(self, allow_bypass: bool) -> Result<DefaultModes, ConfigError> {
        let claude = match self.claude {
            None => None,
            Some(mode) => {
                if !CLAUDE_MODES.contains(&mode.as_str()) {
                    return Err(invalid(
                        "default_mode.claude",
                        format!("use one of {}", CLAUDE_MODES.join(", ")),
                    ));
                }
                if mode == "bypassPermissions" && !allow_bypass {
                    return Err(invalid(
                        "default_mode.claude",
                        "bypassPermissions needs allow_bypass_permissions = true",
                    ));
                }
                Some(mode)
            }
        };
        let codex = match self.codex {
            None => None,
            Some(mode) => {
                if !codex_modes().contains(&mode) {
                    return Err(invalid(
                        "default_mode.codex",
                        format!("use one of {}", codex_modes().join(", ")),
                    ));
                }
                if mode.ends_with("/danger-full-access") && !allow_bypass {
                    return Err(invalid(
                        "default_mode.codex",
                        "danger-full-access needs allow_bypass_permissions = true",
                    ));
                }
                Some(mode)
            }
        };
        Ok(DefaultModes { claude, codex })
    }
}

/// The keys (`default_mode.<family>`) whose new mode is not within the old one.
#[must_use]
pub fn widens(old: &DefaultModes, new: &DefaultModes) -> Vec<&'static str> {
    let mut keys = Vec::new();
    for (family, key) in [
        ("claude", "default_mode.claude"),
        ("codex", "default_mode.codex"),
    ] {
        if let (Some(o), Some(n)) = (old.for_family(family), new.for_family(family))
            && n != o
            && !mode_within(family, Some(n), o)
        {
            keys.push(key);
        }
    }
    keys
}
