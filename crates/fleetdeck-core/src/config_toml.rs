//! The value-free TOML error text of `config.rs` (DL-136, DL-137).

use std::path::PathBuf;

use super::ConfigError;

/// A TOML error with the position and a fixed description only. A config
/// file can hold provider secrets in `env`, and the parser's message and
/// excerpt can copy a value (a number, a string, a variant) to the terminal
/// or the service log, so no parser text is shown (DL-136, DL-137).
pub(super) fn toml_error(text: &str, e: &toml::de::Error) -> ConfigError {
    let at = e
        .span()
        .map(|span| {
            let before = text.get(..span.start).unwrap_or(text);
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            format!("line {line}, column {column}: ")
        })
        .unwrap_or_default();
    ConfigError::Toml {
        path: PathBuf::new(),
        message: format!("{at}{}", toml_category(e.message())),
    }
}

/// A value-free description of a parser message. Only names from this
/// crate's own schema (`missing field`) are kept; everything else becomes a
/// fixed text.
fn toml_category(message: &str) -> String {
    if message.starts_with("missing field `")
        && let Some(name) = message
            .strip_prefix("missing field `")
            .and_then(|rest| rest.strip_suffix('`'))
        && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
    {
        return format!("missing field `{name}`");
    }
    let fixed = [
        ("unknown field", "unknown field (check the key names)"),
        (
            "unknown variant",
            "unknown variant (not one of the allowed values)",
        ),
        ("invalid type", "invalid type (a value has the wrong type)"),
        ("invalid value", "invalid value"),
        ("invalid length", "invalid length"),
        ("duplicate key", "duplicate key"),
        ("duplicate field", "duplicate key"),
        (
            "data did not match",
            "invalid type (a value has the wrong type)",
        ),
    ];
    fixed
        .iter()
        .find(|(prefix, _)| message.starts_with(prefix))
        .map_or_else(
            || "invalid TOML syntax".to_owned(),
            |(_, text)| (*text).to_owned(),
        )
}
