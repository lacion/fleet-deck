//! The owner knobs of `deck.toml`: the tables `[security]`, `[retention]`
//! and `[limits]` (knobs design section 2). The deck code does not read them
//! yet (K2).

use serde::Deserialize;

use super::{ConfigError, invalid};

/// The text of a pair that is broken: an idle time above the longest time.
const PAIR_TEXT: &str = "The idle time cannot be longer than the longest time.";

/// Checks that `value` (or the default) is inside `min..=max`, both
/// inclusive. The error names `<table>.<key>`.
pub(super) fn bounded(
    table: &str,
    key: &str,
    value: Option<u32>,
    default: u32,
    (min, max): (u32, u32),
) -> Result<u32, ConfigError> {
    let value = value.unwrap_or(default);
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(invalid(
            format!("{table}.{key}"),
            format!("use a value from {min} to {max}"),
        ))
    }
}

/// The `[security]` table of `deck.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeckSecurity {
    pub min_password_chars: u32,
    pub login_idle_days: u32,
    pub login_max_days: u32,
    pub elevation_minutes: u32,
    pub api_token_max_days: u32,
    pub display_idle_days: u32,
    pub display_max_days: u32,
    pub invite_minutes: u32,
    pub desktop_screenshots_per_minute: u32,
    pub desktop_actions_per_minute: u32,
}

impl Default for DeckSecurity {
    fn default() -> Self {
        Self {
            min_password_chars: 12,
            login_idle_days: 7,
            login_max_days: 30,
            elevation_minutes: 10,
            api_token_max_days: 90,
            display_idle_days: 30,
            display_max_days: 90,
            invite_minutes: 15,
            desktop_screenshots_per_minute: 2,
            desktop_actions_per_minute: 30,
        }
    }
}

/// The `[retention]` table of `deck.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeckRetention {
    pub audit_days: u32,
    pub audit_max_rows: u32,
}

impl Default for DeckRetention {
    fn default() -> Self {
        Self {
            audit_days: 90,
            audit_max_rows: 200_000,
        }
    }
}

/// The `[limits]` table of `deck.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeckLimits {
    pub delivery_hours: u32,
    pub radar_minutes: u32,
}

impl Default for DeckLimits {
    fn default() -> Self {
        Self {
            delivery_hours: 24,
            radar_minutes: 5,
        }
    }
}

/// The keys of `[security]` (`RawSecurity`). A test in `settings` checks that
/// the field table has one entry for each key.
#[cfg(test)]
pub(crate) const RAW_SECURITY_KEYS: &[&str] = &[
    "min_password_chars",
    "login_idle_days",
    "login_max_days",
    "elevation_minutes",
    "api_token_max_days",
    "display_idle_days",
    "display_max_days",
    "invite_minutes",
    "desktop_screenshots_per_minute",
    "desktop_actions_per_minute",
];

/// The keys of the deck `[retention]` (`RawDeckRetention`).
#[cfg(test)]
pub(crate) const RAW_DECK_RETENTION_KEYS: &[&str] = &["audit_days", "audit_max_rows"];

/// The keys of the deck `[limits]` (`RawDeckLimits`).
#[cfg(test)]
pub(crate) const RAW_DECK_LIMITS_KEYS: &[&str] = &["delivery_hours", "radar_minutes"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawSecurity {
    min_password_chars: Option<u32>,
    login_idle_days: Option<u32>,
    login_max_days: Option<u32>,
    elevation_minutes: Option<u32>,
    api_token_max_days: Option<u32>,
    display_idle_days: Option<u32>,
    display_max_days: Option<u32>,
    invite_minutes: Option<u32>,
    desktop_screenshots_per_minute: Option<u32>,
    desktop_actions_per_minute: Option<u32>,
}

impl RawSecurity {
    pub(super) fn validate(self) -> Result<DeckSecurity, ConfigError> {
        let d = DeckSecurity::default();
        let get = |key: &str, value: Option<u32>, default: u32, range: (u32, u32)| {
            bounded("security", key, value, default, range)
        };
        let out = DeckSecurity {
            min_password_chars: get(
                "min_password_chars",
                self.min_password_chars,
                d.min_password_chars,
                (8, 128),
            )?,
            login_idle_days: get(
                "login_idle_days",
                self.login_idle_days,
                d.login_idle_days,
                (1, 90),
            )?,
            login_max_days: get(
                "login_max_days",
                self.login_max_days,
                d.login_max_days,
                (1, 365),
            )?,
            elevation_minutes: get(
                "elevation_minutes",
                self.elevation_minutes,
                d.elevation_minutes,
                (1, 60),
            )?,
            api_token_max_days: get(
                "api_token_max_days",
                self.api_token_max_days,
                d.api_token_max_days,
                (1, 365),
            )?,
            display_idle_days: get(
                "display_idle_days",
                self.display_idle_days,
                d.display_idle_days,
                (1, 365),
            )?,
            display_max_days: get(
                "display_max_days",
                self.display_max_days,
                d.display_max_days,
                (1, 730),
            )?,
            invite_minutes: get(
                "invite_minutes",
                self.invite_minutes,
                d.invite_minutes,
                (5, 1440),
            )?,
            desktop_screenshots_per_minute: get(
                "desktop_screenshots_per_minute",
                self.desktop_screenshots_per_minute,
                d.desktop_screenshots_per_minute,
                (1, 60),
            )?,
            desktop_actions_per_minute: get(
                "desktop_actions_per_minute",
                self.desktop_actions_per_minute,
                d.desktop_actions_per_minute,
                (1, 300),
            )?,
        };
        if out.login_idle_days > out.login_max_days {
            return Err(invalid("security.login_idle_days", PAIR_TEXT));
        }
        if out.display_idle_days > out.display_max_days {
            return Err(invalid("security.display_idle_days", PAIR_TEXT));
        }
        Ok(out)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawDeckRetention {
    audit_days: Option<u32>,
    audit_max_rows: Option<u32>,
}

impl RawDeckRetention {
    pub(super) fn validate(self) -> Result<DeckRetention, ConfigError> {
        let d = DeckRetention::default();
        Ok(DeckRetention {
            audit_days: bounded(
                "retention",
                "audit_days",
                self.audit_days,
                d.audit_days,
                (7, 3650),
            )?,
            audit_max_rows: bounded(
                "retention",
                "audit_max_rows",
                self.audit_max_rows,
                d.audit_max_rows,
                (10_000, 10_000_000),
            )?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawDeckLimits {
    delivery_hours: Option<u32>,
    radar_minutes: Option<u32>,
}

impl RawDeckLimits {
    pub(super) fn validate(self) -> Result<DeckLimits, ConfigError> {
        let d = DeckLimits::default();
        Ok(DeckLimits {
            delivery_hours: bounded(
                "limits",
                "delivery_hours",
                self.delivery_hours,
                d.delivery_hours,
                (1, 168),
            )?,
            radar_minutes: bounded(
                "limits",
                "radar_minutes",
                self.radar_minutes,
                d.radar_minutes,
                (1, 60),
            )?,
        })
    }
}
