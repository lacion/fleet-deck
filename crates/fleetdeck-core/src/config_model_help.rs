//! The `[model_help]` table of `station.toml` (R10, DL-167).

use serde::Deserialize;

use super::{ConfigError, Driver, invalid};

/// The default models of the station's model help (R10, DL-167).
pub const DEFAULT_ASSIST_CLAUDE_MODEL: &str = "claude-haiku-5-5";
pub const DEFAULT_ASSIST_CODEX_MODEL: &str = "gpt-5.6-luna";
pub const DEFAULT_ASSIST_MAX_OUTPUT_TOKENS: u32 = 400;
pub const DEFAULT_ASSIST_DAILY_TOKEN_CAP: u64 = 200_000;
/// The `[model_help]` table of `station.toml` (R10, DL-167). It is also the
/// off switch: `enabled = false` makes every call answer `disabled`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelHelpConfig {
    pub enabled: bool,
    pub claude_model: String,
    pub codex_model: String,
    /// The driver to use. `None`: the first provider instance that is
    /// signed in.
    pub provider: Option<Driver>,
    /// The output limit of one call, in tokens.
    pub per_call_max_output_tokens: u32,
    /// The tokens (input and output) that the station spends in one UTC day.
    pub daily_token_cap: u64,
}

impl Default for ModelHelpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            claude_model: DEFAULT_ASSIST_CLAUDE_MODEL.to_owned(),
            codex_model: DEFAULT_ASSIST_CODEX_MODEL.to_owned(),
            provider: None,
            per_call_max_output_tokens: DEFAULT_ASSIST_MAX_OUTPUT_TOKENS,
            daily_token_cap: DEFAULT_ASSIST_DAILY_TOKEN_CAP,
        }
    }
}

/// The keys of `[model_help]` (`RawModelHelp`). A test in `settings` checks
/// that the field table has one entry for each key.
#[cfg(test)]
pub(crate) const RAW_MODEL_HELP_KEYS: &[&str] = &[
    "enabled",
    "claude_model",
    "codex_model",
    "provider",
    "per_call_max_output_tokens",
    "daily_token_cap",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawModelHelp {
    enabled: Option<bool>,
    claude_model: Option<String>,
    codex_model: Option<String>,
    provider: Option<Driver>,
    per_call_max_output_tokens: Option<u32>,
    daily_token_cap: Option<u64>,
}

impl RawModelHelp {
    pub(super) fn validate(self) -> Result<ModelHelpConfig, ConfigError> {
        let model = |field: &str, value: Option<String>, default: &str| {
            let value = value.unwrap_or_else(|| default.to_owned());
            // A model name is a command line argument: it must not look
            // like an option.
            let ok = (1..=100).contains(&value.len())
                && !value.starts_with('-')
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
            if ok {
                Ok(value)
            } else {
                Err(invalid(
                    format!("model_help.{field}"),
                    "use 1-100 of A-Z, a-z, 0-9, `-`, `_`, `.`, `:`, not starting with `-`",
                ))
            }
        };
        let per_call = self
            .per_call_max_output_tokens
            .unwrap_or(DEFAULT_ASSIST_MAX_OUTPUT_TOKENS);
        if !(1..=4000).contains(&per_call) {
            return Err(invalid(
                "model_help.per_call_max_output_tokens",
                "use a value from 1 to 4000",
            ));
        }
        let cap = self
            .daily_token_cap
            .unwrap_or(DEFAULT_ASSIST_DAILY_TOKEN_CAP);
        if !(1..=1_000_000_000).contains(&cap) {
            return Err(invalid(
                "model_help.daily_token_cap",
                "use a value from 1 to 1000000000",
            ));
        }
        Ok(ModelHelpConfig {
            enabled: self.enabled.unwrap_or(true),
            claude_model: model(
                "claude_model",
                self.claude_model,
                DEFAULT_ASSIST_CLAUDE_MODEL,
            )?,
            codex_model: model("codex_model", self.codex_model, DEFAULT_ASSIST_CODEX_MODEL)?,
            provider: self.provider,
            per_call_max_output_tokens: per_call,
            daily_token_cap: cap,
        })
    }
}
