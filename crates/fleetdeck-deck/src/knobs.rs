//! The owner knobs that the deck reads (knobs design section 2): the values
//! of `[security]`, `[retention]` and `[limits]` of `deck.toml` in the units
//! that the code uses. [`DeckKnobs::default`] is the single source of the
//! defaults; [`DeckLive`](crate::live::DeckLive) carries a copy that a save
//! swaps.

use std::time::Duration;

use fleetdeck_core::config::{CloneTransport, DeckConfig};
use fleetdeck_core::config_grant_limits::GrantLimits;

use crate::fleet_git_radar::RadarOptions;
use crate::store::DeckStore;

const MINUTE_MS: i64 = 60 * 1000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

/// The deck knobs, in code units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeckKnobs {
    /// The shortest password.
    pub min_password_chars: usize,
    /// A board login ends after this idle time.
    pub login_idle_ms: i64,
    /// A board login ends this long after the login.
    pub login_max_ms: i64,
    /// An elevation lives this long.
    pub elevation_ms: i64,
    /// The longest API token lifetime.
    pub api_token_max_ms: i64,
    /// A display session ends after this idle time.
    pub display_idle_ms: i64,
    /// A display session ends this long after it starts.
    pub display_max_ms: i64,
    /// An invitation lives this long.
    pub invite: Duration,
    /// The agent desktop screenshots that one grant may take in a rate window.
    pub screenshots_per_window: u32,
    /// The agent desktop actions that one grant may take in a rate window.
    pub mutations_per_window: u32,
    /// The audit rows older than this are pruned.
    pub audit_retention_ms: i64,
    /// The audit rows above this count are pruned (oldest first).
    pub audit_max_rows: i64,
    /// A mail or a task that the board sends expires after this time.
    pub delivery_ms: i64,
    /// `clone_transport = "ssh"`: a remote derived from the forge uses `git@host:repo.git` (Fleet Sync §1.3).
    pub clone_ssh: bool,
    /// `[grant_limits]` (B27 §3.2), read at each grant creation and cap change.
    pub grant: GrantLimits,
    /// `worker_grant.allow_bypass` (B27 §3.4), read at each decision and dispatch check.
    pub worker_bypass: bool,
}

impl Default for DeckKnobs {
    fn default() -> Self {
        Self {
            min_password_chars: 12,
            login_idle_ms: 7 * DAY_MS,
            login_max_ms: 30 * DAY_MS,
            elevation_ms: 10 * MINUTE_MS,
            api_token_max_ms: 90 * DAY_MS,
            display_idle_ms: 30 * DAY_MS,
            display_max_ms: 90 * DAY_MS,
            invite: Duration::from_secs(15 * 60),
            screenshots_per_window: 2,
            mutations_per_window: 30,
            audit_retention_ms: 90 * DAY_MS,
            audit_max_rows: 200_000,
            delivery_ms: 24 * HOUR_MS,
            clone_ssh: false,
            grant: GrantLimits::default(),
            worker_bypass: false,
        }
    }
}

impl DeckKnobs {
    /// The knobs of a parsed `deck.toml` (a unit conversion of the K1 values).
    #[must_use]
    pub fn of(config: &DeckConfig) -> Self {
        let security = &config.security;
        Self {
            min_password_chars: security.min_password_chars as usize,
            login_idle_ms: i64::from(security.login_idle_days) * DAY_MS,
            login_max_ms: i64::from(security.login_max_days) * DAY_MS,
            elevation_ms: i64::from(security.elevation_minutes) * MINUTE_MS,
            api_token_max_ms: i64::from(security.api_token_max_days) * DAY_MS,
            display_idle_ms: i64::from(security.display_idle_days) * DAY_MS,
            display_max_ms: i64::from(security.display_max_days) * DAY_MS,
            invite: Duration::from_secs(u64::from(security.invite_minutes) * 60),
            screenshots_per_window: security.desktop_screenshots_per_minute,
            mutations_per_window: security.desktop_actions_per_minute,
            audit_retention_ms: i64::from(config.retention.audit_days) * DAY_MS,
            audit_max_rows: i64::from(config.retention.audit_max_rows),
            delivery_ms: i64::from(config.limits.delivery_hours) * HOUR_MS,
            clone_ssh: config.clone_transport == CloneTransport::Ssh,
            grant: config.grant_limits,
            worker_bypass: config.worker_grant.allow_bypass,
        }
    }
}

/// The options of the radar task: the scan interval is the deck's
/// `limits.radar_minutes` (read at the deck start); the other option stays.
#[must_use]
pub fn radar_options(config: &DeckConfig) -> RadarOptions {
    RadarOptions {
        every: Duration::from_secs(u64::from(config.limits.radar_minutes) * 60),
        ..RadarOptions::PRODUCTION
    }
}

/// True when a login or a display idle limit is shorter in `new` than in
/// `old`.
#[must_use]
pub fn idle_shrank(old: &DeckKnobs, new: &DeckKnobs) -> bool {
    new.login_idle_ms < old.login_idle_ms || new.display_idle_ms < old.display_idle_ms
}

/// Clamps the idle deadlines of the stored sessions to the idle limits of
/// `knobs` after a publish (a session issued between the first clamp and the
/// publish read the old knobs) and after a hand edit. A failure is logged:
/// the clamp before the file write and the clamp at the next deck start
/// cover the sessions.
pub async fn clamp_after_publish(store: &DeckStore, knobs: &DeckKnobs) {
    if let Err(e) = store
        .clamp_idle_deadlines(knobs.login_idle_ms, knobs.display_idle_ms)
        .await
    {
        tracing::warn!(error = %e, "the idle deadlines of the sessions were not clamped after a publish");
    }
}

/// Rule 11 of the design, as a best-effort cleanup: a save that lowered a
/// login or a display limit deletes the sessions over the new limit and ends
/// their sockets. Call it after the store holds the new knobs. A failure is
/// logged and does not fail the save, because the sign-in checks enforce the
/// current limits themselves (the age check against the current longest time
/// and the idle check against the current idle limit and the idle deadline),
/// also after a restart. A socket whose session fails the check ends at its
/// next recheck (30 s for the board, 2 s for a display).
pub async fn end_sessions_over_lowered(store: &DeckStore, old: &DeckKnobs, new: &DeckKnobs) {
    let logins = new.login_idle_ms < old.login_idle_ms || new.login_max_ms < old.login_max_ms;
    let displays =
        new.display_idle_ms < old.display_idle_ms || new.display_max_ms < old.display_max_ms;
    if !logins && !displays {
        return;
    }
    match store
        .end_sessions_over_limits(logins, displays, store.now_ms())
        .await
    {
        Ok(ended) => {
            let hashes: Vec<String> = ended.into_iter().map(|(_, hash)| hash).collect();
            if !hashes.is_empty() {
                // The waiters are not needed: the sockets end by themselves.
                let _ = store.registry().revoke(&hashes, &[]);
            }
        }
        Err(e) => tracing::warn!(error = %e, "the sessions over a lowered limit were not ended"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn default_config() -> DeckConfig {
        DeckConfig::parse("").unwrap()
    }

    #[test]
    fn the_default_config_gives_the_default_knobs() {
        assert_eq!(DeckKnobs::of(&default_config()), DeckKnobs::default());
    }

    #[test]
    fn the_radar_interval_comes_from_the_config() {
        let default = radar_options(&default_config());
        assert_eq!(default.every, RadarOptions::PRODUCTION.every);
        assert_eq!(default.after_push, RadarOptions::PRODUCTION.after_push);
        let mut config = default_config();
        config.limits.radar_minutes = 12;
        let options = radar_options(&config);
        assert_eq!(options.every, Duration::from_secs(12 * 60));
        assert_eq!(options.after_push, RadarOptions::PRODUCTION.after_push);
    }

    #[test]
    fn a_non_default_config_converts_each_unit() {
        let mut config = default_config();
        config.security.min_password_chars = 20;
        config.security.login_idle_days = 2;
        config.security.login_max_days = 3;
        config.security.elevation_minutes = 4;
        config.security.api_token_max_days = 5;
        config.security.display_idle_days = 6;
        config.security.display_max_days = 7;
        config.security.invite_minutes = 8;
        config.security.desktop_screenshots_per_minute = 9;
        config.security.desktop_actions_per_minute = 10;
        config.retention.audit_days = 11;
        config.retention.audit_max_rows = 12_000;
        config.limits.delivery_hours = 13;
        config.clone_transport = CloneTransport::Ssh;
        let day = 24 * 60 * 60 * 1000;
        assert_eq!(
            DeckKnobs::of(&config),
            DeckKnobs {
                min_password_chars: 20,
                login_idle_ms: 2 * day,
                login_max_ms: 3 * day,
                elevation_ms: 4 * 60 * 1000,
                api_token_max_ms: 5 * day,
                display_idle_ms: 6 * day,
                display_max_ms: 7 * day,
                invite: Duration::from_secs(8 * 60),
                screenshots_per_window: 9,
                mutations_per_window: 10,
                audit_retention_ms: 11 * day,
                audit_max_rows: 12_000,
                delivery_ms: 13 * 60 * 60 * 1000,
                clone_ssh: true,
                grant: GrantLimits::default(),
                worker_bypass: false,
            }
        );
    }
}
