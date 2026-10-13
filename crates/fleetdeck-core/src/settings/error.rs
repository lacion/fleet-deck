//! The errors of a save.

use crate::config::ConfigError;

/// Why a save or a secret write failed. The messages hold a key name and a
/// fixed text, never a value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SaveError {
    #[error("the settings changed since you read them; reload and try again")]
    SettingsChanged,
    #[error("the config file changed on disk while Fleet Deck runs; reload")]
    ChangedOnDisk,
    #[error("`{key}` has an invalid value")]
    Invalid { key: String },
    #[error("`{key}` cannot be applied: {message}")]
    CannotApply { key: String, message: String },
    #[error("the settings file could not be written; the old file is unchanged")]
    SettingsWriteFailed,
    #[error("the secret store is not available")]
    SecretStoreUnavailable,
    #[error("`{key}`: a secret is set only through the secret route")]
    SecretViaRelay { key: String },
    #[error("a recent sign-in is needed to change: {}", keys.join(", "))]
    ElevationRequired { keys: Vec<String> },
}

impl SaveError {
    /// The fixed error code of the wire.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SettingsChanged => "settings_changed",
            Self::ChangedOnDisk => "settings_changed_on_disk",
            Self::Invalid { .. } => "invalid",
            Self::CannotApply { .. } => "cannot_apply",
            Self::SettingsWriteFailed => "settings_write_failed",
            Self::SecretStoreUnavailable => "secret_store_unavailable",
            Self::SecretViaRelay { .. } => "secret_via_relay",
            Self::ElevationRequired { .. } => "elevation_required",
        }
    }

    /// An invalid key or value. The key comes from the client: a text that
    /// is not shaped like a key (a pasted value) is shown as `?`.
    pub(super) fn invalid(key: &str) -> Self {
        let shaped = key.len() <= 128
            && key.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'[' | b']')
            });
        Self::Invalid {
            key: if shaped { key } else { "?" }.to_owned(),
        }
    }
}

/// The top-level key that a parser error names: `allowed_roots[0]` is
/// `allowed_roots`, `provider_instance[1].binary` is `provider_instance`.
/// `fallback` stands for an error without a field (a TOML syntax error).
pub(super) fn key_of(e: &ConfigError, fallback: &str) -> String {
    let field = match e {
        ConfigError::Invalid { field, .. }
        | ConfigError::Path { field, .. }
        | ConfigError::Addr { field, .. } => field,
        ConfigError::Io { .. } | ConfigError::Mode { .. } | ConfigError::Toml { .. } => {
            return fallback.to_owned();
        }
    };
    let cut = field.split('[').next().unwrap_or(field);
    for table in ["repo_groups", "provider_instance", "trackers"] {
        if cut == table || cut.starts_with(&format!("{table}.")) {
            return table.to_owned();
        }
    }
    cut.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_that_is_not_shaped_like_a_key_is_hidden() {
        assert_eq!(
            SaveError::invalid("model_help.enabled").to_string(),
            "`model_help.enabled` has an invalid value"
        );
        let hidden = SaveError::invalid("sk-ant secret value").to_string();
        assert!(!hidden.contains("secret"), "{hidden}");
        assert!(
            SaveError::invalid(&"k".repeat(200))
                .to_string()
                .contains('?')
        );
    }
}
