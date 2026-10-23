//! The timeouts of the station operations that come from the `[limits]`
//! table of `station.toml` (knobs K3, design `docs/v2/design/knobs.md`
//! section 3). They are live values of the [`Policy`]: an operation reads
//! them once when it starts (rule 9).

use std::time::Duration;

use fleetdeck_core::config::StationLimits;
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_providers::host::StopPolicy;

use super::policy::Policy;

/// The limits of the operations that the station runs for the deck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StationTimeouts {
    /// An agent `!` command (`limits.shell_command_minutes`).
    pub shell_command: Duration,
    /// A setup command (`limits.setup_minutes`).
    pub setup: Duration,
    /// A land check command (`limits.land_check_minutes`).
    pub land_check: Duration,
    /// A clone (`limits.clone_minutes`).
    pub clone: Duration,
    /// A Fleet Git push (`limits.git_push_seconds`).
    pub git_push: Duration,
}

impl Default for StationTimeouts {
    /// The values that the station used before the knobs: 600 s, 600 s,
    /// 600 s, 900 s and 60 s.
    fn default() -> Self {
        Self {
            shell_command: Duration::from_secs(600),
            setup: Duration::from_secs(600),
            land_check: Duration::from_secs(600),
            clone: Duration::from_secs(900),
            git_push: Duration::from_secs(60),
        }
    }
}

impl StationTimeouts {
    /// The timeouts of the `[limits]` table.
    #[must_use]
    pub fn from_limits(limits: &StationLimits) -> Self {
        let minutes = |m: u32| Duration::from_secs(u64::from(m) * 60);
        Self {
            shell_command: minutes(limits.shell_command_minutes),
            setup: minutes(limits.setup_minutes),
            land_check: minutes(limits.land_check_minutes),
            clone: minutes(limits.clone_minutes),
            git_push: Duration::from_secs(u64::from(limits.git_push_seconds)),
        }
    }

    /// The run limit of an agent `!` command: the test override
    /// `override_ms` when it is not 0, else the policy value.
    #[must_use]
    pub fn shell_command_limit(&self, override_ms: u64) -> Duration {
        if override_ms == 0 {
            self.shell_command
        } else {
            Duration::from_millis(override_ms)
        }
    }
}

/// The stop bounds of the provider processes: the grace of
/// `limits.provider_stop_grace_seconds`; the `term_grace` stays 5 s. It
/// applies at a station start.
#[must_use]
pub fn stop_policy(limits: &StationLimits) -> StopPolicy {
    StopPolicy {
        grace: Duration::from_secs(u64::from(limits.provider_stop_grace_seconds)),
        ..StopPolicy::default()
    }
}

/// The backoff of the station's link dialer: the cap of
/// `limits.reconnect_max_seconds`. It applies at a station start.
#[must_use]
pub fn reconnect_backoff(limits: &StationLimits) -> BackoffPolicy {
    BackoffPolicy {
        cap: Duration::from_secs(u64::from(limits.reconnect_max_seconds)),
        ..BackoffPolicy::default()
    }
}

/// The ring of a new terminal: the lines of `limits.scrollback_lines` and
/// the bytes of `limits.scrollback_mib`.
#[must_use]
pub(super) fn scrollback_of(limits: &StationLimits) -> (usize, usize) {
    let mib = usize::try_from(limits.scrollback_mib).unwrap_or(usize::MAX);
    (
        usize::try_from(limits.scrollback_lines).unwrap_or(usize::MAX),
        mib.saturating_mul(1024 * 1024),
    )
}

impl Policy {
    /// The lines and the bytes that the scrollback of a new terminal keeps.
    #[must_use]
    pub fn scrollback_limits(&self) -> (usize, usize) {
        self.scrollback
    }

    /// The stop bounds of the provider processes (a restart value).
    #[must_use]
    pub fn stop_policy(&self) -> StopPolicy {
        self.stop
    }

    /// The timeouts of the station operations (knobs K3).
    #[must_use]
    pub fn timeouts(&self) -> StationTimeouts {
        self.timeouts
    }

    /// The same policy with other timeouts (tests).
    #[must_use]
    pub fn with_timeouts(mut self, timeouts: StationTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }
}

/// The refusal text of a start or an import whose folder `cwd` is outside
/// the allowed roots (G1). `station` is the station name; an empty name
/// (a test policy) reads "this station".
#[must_use]
pub fn outside_roots_text(cwd: &str, station: &str) -> String {
    if station.is_empty() {
        format!("The folder {cwd} is outside the allowed roots of this station.")
    } else {
        format!("The folder {cwd} is outside the allowed roots of station {station}.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::policy_hub::test_config;

    #[test]
    fn the_default_config_gives_the_default_timeouts() {
        let root = tempfile::tempdir().unwrap();
        let policy = Policy::from_config(&test_config(root.path(), ""));
        assert_eq!(policy.timeouts(), StationTimeouts::default());
        let d = StationTimeouts::default();
        assert_eq!(d.shell_command, Duration::from_secs(600));
        assert_eq!(d.setup, Duration::from_secs(600));
        assert_eq!(d.land_check, Duration::from_secs(600));
        assert_eq!(d.clone, Duration::from_secs(900));
        assert_eq!(d.git_push, Duration::from_secs(60));
    }

    #[test]
    fn a_shell_command_uses_the_policy_limit_unless_a_test_overrides_it() {
        let one = StationTimeouts {
            shell_command: Duration::from_secs(60),
            ..StationTimeouts::default()
        };
        assert_eq!(one.shell_command_limit(0), Duration::from_secs(60));
        assert_eq!(one.shell_command_limit(300), Duration::from_millis(300));
        assert_eq!(
            StationTimeouts::default().shell_command_limit(0),
            Duration::from_secs(600)
        );
    }

    #[tokio::test]
    async fn a_setup_command_ends_at_the_setup_limit_of_the_policy() {
        use crate::manager::tests::setup::{start_with_setup, station};
        use fleetdeck_proto::command::StopMode;
        use fleetdeck_proto::event::Event;
        let mut st = station(true).await;
        let timeouts = StationTimeouts {
            setup: Duration::from_secs(1),
            ..StationTimeouts::default()
        };
        let hub = st.manager.policy_hub();
        hub.swap(hub.current().as_ref().clone().with_timeouts(timeouts));
        let ws = st.workspace();
        st.accepted(start_with_setup(&ws, "sleep 60")).await;
        let message = st
            .wait_for("start failure", |e| match e {
                Event::Error { code, message, .. } if code == "start_failed" => {
                    Some(message.clone())
                }
                _ => None,
            })
            .await;
        assert!(
            message.starts_with("the setup command timed out after 1 s"),
            "{message}"
        );
        st.manager.shutdown(StopMode::Kill).await;
    }

    #[test]
    fn the_stop_grace_and_the_reconnect_cap_come_from_the_limits() {
        let root = tempfile::tempdir().unwrap();
        let extra = "[limits]\nprovider_stop_grace_seconds = 45\nreconnect_max_seconds = 90\n";
        let config = test_config(root.path(), extra);
        let stop = stop_policy(&config.limits);
        assert_eq!(stop.grace, Duration::from_secs(45));
        assert_eq!(stop.term_grace, StopPolicy::default().term_grace);
        let backoff = reconnect_backoff(&config.limits);
        let base = BackoffPolicy::default();
        assert_eq!(backoff.cap, Duration::from_secs(90));
        assert_eq!((backoff.base, backoff.stable), (base.base, base.stable));
        // The defaults are the values from before the knobs.
        let plain = test_config(root.path(), "");
        assert_eq!(stop_policy(&plain.limits), StopPolicy::default());
        assert_eq!(reconnect_backoff(&plain.limits).cap, base.cap);
        // The policy keeps the stop bounds at a reload (a restart value).
        let policy = Policy::from_config(&config);
        assert_eq!(policy.stop_policy().grace, Duration::from_secs(45));
        let reloaded = policy.reconfigured(&plain);
        assert_eq!(reloaded.stop_policy().grace, Duration::from_secs(45));
    }

    #[test]
    fn the_scrollback_comes_from_the_limits_and_a_reload_takes_the_new_one() {
        let root = tempfile::tempdir().unwrap();
        let default = Policy::from_config(&test_config(root.path(), ""));
        assert_eq!(default.scrollback_limits(), (5000, 8 * 1024 * 1024));
        let extra = "[limits]\nscrollback_lines = 500\nscrollback_mib = 2\n";
        let set = Policy::from_config(&test_config(root.path(), extra));
        assert_eq!(set.scrollback_limits(), (500, 2 * 1024 * 1024));
        let back = set.reconfigured(&test_config(root.path(), ""));
        assert_eq!(back.scrollback_limits(), (5000, 8 * 1024 * 1024));
    }

    #[test]
    fn the_limits_table_sets_the_timeouts_and_a_reload_takes_the_new_ones() {
        let root = tempfile::tempdir().unwrap();
        let extra = "[limits]\nshell_command_minutes = 1\nsetup_minutes = 2\n\
                     land_check_minutes = 3\nclone_minutes = 4\ngit_push_seconds = 15\n";
        let config = test_config(root.path(), extra);
        let set = Policy::from_config(&config).timeouts();
        assert_eq!(set.shell_command, Duration::from_secs(60));
        assert_eq!(set.setup, Duration::from_secs(120));
        assert_eq!(set.land_check, Duration::from_secs(180));
        assert_eq!(set.clone, Duration::from_secs(240));
        assert_eq!(set.git_push, Duration::from_secs(15));
        let plain = Policy::from_config(&test_config(root.path(), ""));
        assert_eq!(plain.reconfigured(&config).timeouts(), set);
        let back = Policy::from_config(&config).reconfigured(&test_config(root.path(), ""));
        assert_eq!(back.timeouts(), StationTimeouts::default());
    }
}
