//! The `[provider_updates]` table of `station.toml` (B28, design
//! `cli-update.md` section 3.10): the nightly window in which the station
//! updates the Claude and Codex CLIs, and the pins.

use fleetdeck_proto::cli_update::is_release;
use serde::Deserialize;

use super::knobs_deck_cfg::bounded;
use super::{ConfigError, invalid};

/// The largest offset of a fixed time zone: 14:00, in minutes.
const MAX_OFFSET_MIN: i32 = 14 * 60;

/// The `[provider_updates]` table of `station.toml` (B28).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUpdatesConfig {
    /// The nightly window. A restart value: it also sets
    /// `DISABLE_AUTOUPDATER=1` for the Claude processes of the station.
    pub enabled: bool,
    /// Minutes after 00:00 (from `HH:MM`).
    pub window_start_min: u16,
    /// 1 to 12.
    pub window_hours: u8,
    pub timezone: UpdateZone,
    pub claude_channel: ClaudeChannel,
    pub claude_pin: Option<String>,
    pub codex_pin: Option<String>,
    /// 0 to 336.
    pub min_age_hours: u16,
}

/// The time zone of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateZone {
    /// The clock of this machine.
    Local,
    /// A fixed offset, in minutes east of UTC.
    Fixed(i32),
}

/// The release channel of Claude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeChannel {
    Stable,
    Latest,
}

impl Default for ProviderUpdatesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            window_start_min: 3 * 60,
            window_hours: 2,
            timezone: UpdateZone::Local,
            claude_channel: ClaudeChannel::Stable,
            claude_pin: None,
            codex_pin: None,
            min_age_hours: 48,
        }
    }
}

impl ProviderUpdatesConfig {
    /// The window start as `HH:MM`.
    pub fn window_start_text(&self) -> String {
        format!(
            "{:02}:{:02}",
            self.window_start_min / 60,
            self.window_start_min % 60
        )
    }
}

impl UpdateZone {
    /// `local`, `UTC` or `+HH:MM` / `-HH:MM`.
    pub fn text(self) -> String {
        match self {
            Self::Local => "local".to_owned(),
            Self::Fixed(0) => "UTC".to_owned(),
            Self::Fixed(min) => {
                let sign = if min < 0 { '-' } else { '+' };
                let abs = min.abs();
                format!("{sign}{:02}:{:02}", abs / 60, abs % 60)
            }
        }
    }
}

impl ClaudeChannel {
    /// `stable` or `latest`.
    pub fn text(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Latest => "latest",
        }
    }
}

/// The keys of `[provider_updates]` (`RawProviderUpdates`). A test in
/// `settings` checks that the field table has one entry for each key.
#[cfg(test)]
pub(crate) const RAW_PROVIDER_UPDATES_KEYS: &[&str] = &[
    "enabled",
    "window_start",
    "window_hours",
    "timezone",
    "claude_channel",
    "claude_pin",
    "codex_pin",
    "min_age_hours",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawProviderUpdates {
    enabled: Option<bool>,
    window_start: Option<String>,
    window_hours: Option<u32>,
    timezone: Option<String>,
    claude_channel: Option<String>,
    claude_pin: Option<String>,
    codex_pin: Option<String>,
    min_age_hours: Option<u32>,
}

impl RawProviderUpdates {
    pub(super) fn validate(self) -> Result<ProviderUpdatesConfig, ConfigError> {
        let defaults = ProviderUpdatesConfig::default();
        let window_start_min = match self.window_start {
            None => defaults.window_start_min,
            Some(text) => parse_window_start(&text).ok_or_else(|| {
                invalid(
                    "provider_updates.window_start",
                    "use HH:MM from 00:00 to 23:59",
                )
            })?,
        };
        let window_hours = bounded(
            "provider_updates",
            "window_hours",
            self.window_hours,
            u32::from(defaults.window_hours),
            (1, 12),
        )?;
        let timezone = match self.timezone {
            None => defaults.timezone,
            Some(text) => parse_zone(&text).ok_or_else(|| {
                invalid(
                    "provider_updates.timezone",
                    "use local, UTC or an offset like +02:00 or -05:30",
                )
            })?,
        };
        let claude_channel = match self.claude_channel.as_deref() {
            None | Some("stable") => ClaudeChannel::Stable,
            Some("latest") => ClaudeChannel::Latest,
            Some(_) => {
                return Err(invalid(
                    "provider_updates.claude_channel",
                    "use stable or latest",
                ));
            }
        };
        let min_age_hours = bounded(
            "provider_updates",
            "min_age_hours",
            self.min_age_hours,
            u32::from(defaults.min_age_hours),
            (0, 336),
        )?;
        Ok(ProviderUpdatesConfig {
            enabled: self.enabled.unwrap_or(false),
            window_start_min,
            // Both values are inside their ranges.
            window_hours: u8::try_from(window_hours).unwrap_or(2),
            timezone,
            claude_channel,
            claude_pin: pin("claude_pin", self.claude_pin)?,
            codex_pin: pin("codex_pin", self.codex_pin)?,
            min_age_hours: u16::try_from(min_age_hours).unwrap_or(48),
        })
    }
}

/// A pin: empty is no pin; else an exact release version.
fn pin(key: &str, value: Option<String>) -> Result<Option<String>, ConfigError> {
    match value {
        None => Ok(None),
        Some(v) if v.is_empty() => Ok(None),
        Some(v) if is_release(&v) => Ok(Some(v)),
        Some(_) => Err(invalid(
            format!("provider_updates.{key}"),
            "use an exact version like 2.1.305, or leave it empty",
        )),
    }
}

/// `HH:MM`, exactly five characters, 00:00 to 23:59.
fn parse_window_start(text: &str) -> Option<u16> {
    let b = text.as_bytes();
    if b.len() != 5 || b[2] != b':' {
        return None;
    }
    let digits = |s: &str| {
        (s.len() == 2 && s.bytes().all(|c| c.is_ascii_digit()))
            .then(|| s.parse::<u16>().ok())
            .flatten()
    };
    let hours = digits(text.get(0..2)?)?;
    let minutes = digits(text.get(3..5)?)?;
    (hours <= 23 && minutes <= 59).then_some(hours * 60 + minutes)
}

/// `local`, `UTC` or `+HH:MM` / `-HH:MM` (at most 14:00).
fn parse_zone(text: &str) -> Option<UpdateZone> {
    match text {
        "local" => return Some(UpdateZone::Local),
        "UTC" => return Some(UpdateZone::Fixed(0)),
        _ => {}
    }
    let b = text.as_bytes();
    if b.len() != 6 || b[3] != b':' || !matches!(b[0], b'+' | b'-') {
        return None;
    }
    let digits = |s: &str| {
        (s.len() == 2 && s.bytes().all(|c| c.is_ascii_digit()))
            .then(|| s.parse::<i32>().ok())
            .flatten()
    };
    let hours = digits(text.get(1..3)?)?;
    let minutes = digits(text.get(4..6)?)?;
    let total = hours * 60 + minutes;
    if minutes > 59 || total > MAX_OFFSET_MIN {
        return None;
    }
    Some(UpdateZone::Fixed(if b[0] == b'-' { -total } else { total }))
}

#[cfg(test)]
#[path = "config_provider_updates_tests.rs"]
mod tests;
