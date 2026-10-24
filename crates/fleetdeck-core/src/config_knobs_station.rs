//! The owner knobs of `station.toml`: the tables `[limits]`, `[retention]`
//! and `[capture]` (knobs design section 3). The station reads them through
//! its policy (live values) or at its start (restart values).

use serde::Deserialize;

use super::ConfigError;
use super::knobs_deck_cfg::bounded;

/// The `[limits]` table of `station.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StationLimits {
    pub shell_command_minutes: u32,
    pub setup_minutes: u32,
    pub land_check_minutes: u32,
    pub clone_minutes: u32,
    pub git_push_seconds: u32,
    pub provider_stop_grace_seconds: u32,
    pub reconnect_max_seconds: u32,
    pub scrollback_lines: u32,
    pub scrollback_mib: u32,
}

impl Default for StationLimits {
    fn default() -> Self {
        Self {
            shell_command_minutes: 10,
            setup_minutes: 10,
            land_check_minutes: 10,
            clone_minutes: 15,
            git_push_seconds: 60,
            provider_stop_grace_seconds: 30,
            reconnect_max_seconds: 300,
            scrollback_lines: 5000,
            scrollback_mib: 8,
        }
    }
}

/// The `[retention]` table of `station.toml`. `raw_log_mib = 0` turns the raw
/// provider logs off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StationRetention {
    pub history_days: u32,
    pub event_days: u32,
    pub raw_log_mib: u32,
}

impl Default for StationRetention {
    fn default() -> Self {
        Self {
            history_days: 90,
            event_days: 7,
            raw_log_mib: 16,
        }
    }
}

/// The `[capture]` table of `station.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureLimits {
    pub quota_gib: u32,
    pub free_reserve_gib: u32,
    pub recording_max_seconds: u32,
}

impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            quota_gib: 4,
            free_reserve_gib: 2,
            recording_max_seconds: 120,
        }
    }
}

/// The keys of the station `[limits]` (`RawStationLimits`). A test in
/// `settings` checks that the field table has one entry for each key.
#[cfg(test)]
pub(crate) const RAW_STATION_LIMITS_KEYS: &[&str] = &[
    "shell_command_minutes",
    "setup_minutes",
    "land_check_minutes",
    "clone_minutes",
    "git_push_seconds",
    "provider_stop_grace_seconds",
    "reconnect_max_seconds",
    "scrollback_lines",
    "scrollback_mib",
];

/// The keys of the station `[retention]` (`RawStationRetention`).
#[cfg(test)]
pub(crate) const RAW_STATION_RETENTION_KEYS: &[&str] =
    &["history_days", "event_days", "raw_log_mib"];

/// The keys of `[capture]` (`RawCapture`).
#[cfg(test)]
pub(crate) const RAW_CAPTURE_KEYS: &[&str] =
    &["quota_gib", "free_reserve_gib", "recording_max_seconds"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawStationLimits {
    shell_command_minutes: Option<u32>,
    setup_minutes: Option<u32>,
    land_check_minutes: Option<u32>,
    clone_minutes: Option<u32>,
    git_push_seconds: Option<u32>,
    provider_stop_grace_seconds: Option<u32>,
    reconnect_max_seconds: Option<u32>,
    scrollback_lines: Option<u32>,
    scrollback_mib: Option<u32>,
}

impl RawStationLimits {
    pub(super) fn validate(self) -> Result<StationLimits, ConfigError> {
        let d = StationLimits::default();
        let get = |key: &str, value: Option<u32>, default: u32, range: (u32, u32)| {
            bounded("limits", key, value, default, range)
        };
        Ok(StationLimits {
            shell_command_minutes: get(
                "shell_command_minutes",
                self.shell_command_minutes,
                d.shell_command_minutes,
                (1, 240),
            )?,
            setup_minutes: get(
                "setup_minutes",
                self.setup_minutes,
                d.setup_minutes,
                (1, 240),
            )?,
            land_check_minutes: get(
                "land_check_minutes",
                self.land_check_minutes,
                d.land_check_minutes,
                (1, 240),
            )?,
            clone_minutes: get(
                "clone_minutes",
                self.clone_minutes,
                d.clone_minutes,
                (1, 240),
            )?,
            git_push_seconds: get(
                "git_push_seconds",
                self.git_push_seconds,
                d.git_push_seconds,
                (10, 600),
            )?,
            provider_stop_grace_seconds: get(
                "provider_stop_grace_seconds",
                self.provider_stop_grace_seconds,
                d.provider_stop_grace_seconds,
                (5, 300),
            )?,
            reconnect_max_seconds: get(
                "reconnect_max_seconds",
                self.reconnect_max_seconds,
                d.reconnect_max_seconds,
                (5, 600),
            )?,
            scrollback_lines: get(
                "scrollback_lines",
                self.scrollback_lines,
                d.scrollback_lines,
                (500, 100_000),
            )?,
            scrollback_mib: get(
                "scrollback_mib",
                self.scrollback_mib,
                d.scrollback_mib,
                (1, 64),
            )?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawStationRetention {
    history_days: Option<u32>,
    event_days: Option<u32>,
    raw_log_mib: Option<u32>,
}

impl RawStationRetention {
    pub(super) fn validate(self) -> Result<StationRetention, ConfigError> {
        let d = StationRetention::default();
        Ok(StationRetention {
            history_days: bounded(
                "retention",
                "history_days",
                self.history_days,
                d.history_days,
                (1, 3650),
            )?,
            event_days: bounded(
                "retention",
                "event_days",
                self.event_days,
                d.event_days,
                (1, 90),
            )?,
            raw_log_mib: bounded(
                "retention",
                "raw_log_mib",
                self.raw_log_mib,
                d.raw_log_mib,
                (0, 256),
            )?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawCapture {
    quota_gib: Option<u32>,
    free_reserve_gib: Option<u32>,
    recording_max_seconds: Option<u32>,
}

impl RawCapture {
    pub(super) fn validate(self) -> Result<CaptureLimits, ConfigError> {
        let d = CaptureLimits::default();
        Ok(CaptureLimits {
            quota_gib: bounded(
                "capture",
                "quota_gib",
                self.quota_gib,
                d.quota_gib,
                (1, 1024),
            )?,
            free_reserve_gib: bounded(
                "capture",
                "free_reserve_gib",
                self.free_reserve_gib,
                d.free_reserve_gib,
                (1, 100),
            )?,
            recording_max_seconds: bounded(
                "capture",
                "recording_max_seconds",
                self.recording_max_seconds,
                d.recording_max_seconds,
                (10, 3600),
            )?,
        })
    }
}
