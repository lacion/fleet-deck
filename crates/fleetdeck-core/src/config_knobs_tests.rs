//! The owner knobs of `deck.toml` and `station.toml` (knobs design sections 2
//! and 3): defaults, bounds, pairs and unknown keys. `DECK_KNOBS` and
//! `STATION_KNOBS` are the spec; the settings tests use them too.

use super::{ConfigError, DeckConfig, StationConfig};
use crate::paths::Env;

/// One knob of the design tables.
pub(crate) struct Knob {
    /// `table.key`, the key of the settings registry.
    pub key: &'static str,
    pub default: u32,
    pub min: u32,
    pub max: u32,
    /// `true`: the knob applies after a restart.
    pub restart: bool,
    /// `true`: the field has the danger badge.
    pub danger: bool,
}

const fn knob(
    key: &'static str,
    default: u32,
    min: u32,
    max: u32,
    restart: bool,
    danger: bool,
) -> Knob {
    Knob {
        key,
        default,
        min,
        max,
        restart,
        danger,
    }
}

pub(crate) const DECK_KNOBS: &[Knob] = &[
    knob("security.min_password_chars", 12, 8, 128, false, true),
    knob("security.login_idle_days", 7, 1, 90, false, true),
    knob("security.login_max_days", 30, 1, 365, false, true),
    knob("security.elevation_minutes", 10, 1, 60, false, true),
    knob("security.api_token_max_days", 90, 1, 365, false, true),
    knob("security.display_idle_days", 30, 1, 365, false, true),
    knob("security.display_max_days", 90, 1, 730, false, true),
    knob("security.invite_minutes", 15, 5, 1440, false, true),
    knob(
        "security.desktop_screenshots_per_minute",
        2,
        1,
        60,
        false,
        true,
    ),
    knob(
        "security.desktop_actions_per_minute",
        30,
        1,
        300,
        false,
        true,
    ),
    knob("retention.audit_days", 90, 7, 3650, false, true),
    knob(
        "retention.audit_max_rows",
        200_000,
        10_000,
        10_000_000,
        false,
        true,
    ),
    knob("limits.delivery_hours", 24, 1, 168, false, false),
    knob("limits.radar_minutes", 5, 1, 60, true, false),
];

pub(crate) const STATION_KNOBS: &[Knob] = &[
    knob("limits.shell_command_minutes", 10, 1, 240, false, false),
    knob("limits.setup_minutes", 10, 1, 240, false, false),
    knob("limits.land_check_minutes", 10, 1, 240, false, false),
    knob("limits.clone_minutes", 15, 1, 240, false, false),
    knob("limits.git_push_seconds", 60, 10, 600, false, false),
    knob(
        "limits.provider_stop_grace_seconds",
        30,
        5,
        300,
        true,
        false,
    ),
    knob("limits.reconnect_max_seconds", 300, 5, 600, true, false),
    knob("limits.scrollback_lines", 5000, 500, 100_000, false, false),
    knob("limits.scrollback_mib", 8, 1, 64, false, false),
    knob("retention.history_days", 90, 1, 3650, true, true),
    knob("retention.event_days", 7, 1, 90, true, false),
    knob("retention.raw_log_mib", 16, 0, 256, true, true),
    knob("capture.quota_gib", 4, 1, 1024, false, false),
    knob("capture.free_reserve_gib", 2, 1, 100, false, false),
    knob("capture.recording_max_seconds", 120, 10, 3600, false, false),
];

fn env() -> Env {
    Env {
        home: Some("/home/ana".into()),
        ..Env::default()
    }
}

/// The text of a deck file with `key = value` and, for a key of a pair, a
/// partner value that keeps the pair valid at every edge.
fn deck_text(key: &str, value: u32) -> String {
    let partner = match key {
        "security.login_idle_days" => "security.login_max_days = 365\n",
        "security.display_idle_days" => "security.display_max_days = 730\n",
        "security.login_max_days" => "security.login_idle_days = 1\n",
        "security.display_max_days" => "security.display_idle_days = 1\n",
        _ => "",
    };
    format!("{key} = {value}\n{partner}")
}

fn invalid_of(result: Result<impl std::fmt::Debug, ConfigError>) -> (String, String) {
    match result {
        Err(ConfigError::Invalid { field, message }) => (field, message),
        other => panic!("expected an invalid value, got {other:?}"),
    }
}

fn deck_value(c: &DeckConfig, key: &str) -> u32 {
    let (s, r, l) = (&c.security, &c.retention, &c.limits);
    match key {
        "security.min_password_chars" => s.min_password_chars,
        "security.login_idle_days" => s.login_idle_days,
        "security.login_max_days" => s.login_max_days,
        "security.elevation_minutes" => s.elevation_minutes,
        "security.api_token_max_days" => s.api_token_max_days,
        "security.display_idle_days" => s.display_idle_days,
        "security.display_max_days" => s.display_max_days,
        "security.invite_minutes" => s.invite_minutes,
        "security.desktop_screenshots_per_minute" => s.desktop_screenshots_per_minute,
        "security.desktop_actions_per_minute" => s.desktop_actions_per_minute,
        "retention.audit_days" => r.audit_days,
        "retention.audit_max_rows" => r.audit_max_rows,
        "limits.delivery_hours" => l.delivery_hours,
        "limits.radar_minutes" => l.radar_minutes,
        other => panic!("no deck knob `{other}`"),
    }
}

fn station_value(c: &StationConfig, key: &str) -> u32 {
    let (l, r, p) = (&c.limits, &c.retention, &c.capture);
    match key {
        "limits.shell_command_minutes" => l.shell_command_minutes,
        "limits.setup_minutes" => l.setup_minutes,
        "limits.land_check_minutes" => l.land_check_minutes,
        "limits.clone_minutes" => l.clone_minutes,
        "limits.git_push_seconds" => l.git_push_seconds,
        "limits.provider_stop_grace_seconds" => l.provider_stop_grace_seconds,
        "limits.reconnect_max_seconds" => l.reconnect_max_seconds,
        "limits.scrollback_lines" => l.scrollback_lines,
        "limits.scrollback_mib" => l.scrollback_mib,
        "retention.history_days" => r.history_days,
        "retention.event_days" => r.event_days,
        "retention.raw_log_mib" => r.raw_log_mib,
        "capture.quota_gib" => p.quota_gib,
        "capture.free_reserve_gib" => p.free_reserve_gib,
        "capture.recording_max_seconds" => p.recording_max_seconds,
        other => panic!("no station knob `{other}`"),
    }
}

#[test]
fn the_deck_defaults_are_the_design_values() {
    let c = DeckConfig::parse("").unwrap();
    assert_eq!(c.security, super::DeckSecurity::default());
    assert_eq!(c.retention, super::DeckRetention::default());
    assert_eq!(c.limits, super::DeckLimits::default());
    for k in DECK_KNOBS {
        assert_eq!(deck_value(&c, k.key), k.default, "{}", k.key);
    }
    // An empty table is the same as no table.
    let c = DeckConfig::parse("[security]\n[retention]\n[limits]\n").unwrap();
    assert_eq!(c.security, super::DeckSecurity::default());
}

#[test]
fn the_station_defaults_are_the_design_values() {
    let c = StationConfig::parse("", &env()).unwrap();
    assert_eq!(c.limits, super::StationLimits::default());
    assert_eq!(c.retention, super::StationRetention::default());
    assert_eq!(c.capture, super::CaptureLimits::default());
    for k in STATION_KNOBS {
        assert_eq!(station_value(&c, k.key), k.default, "{}", k.key);
    }
    let c = StationConfig::parse("[limits]\n[retention]\n[capture]\n", &env()).unwrap();
    assert_eq!(c.limits, super::StationLimits::default());
}

#[test]
fn each_deck_bound_is_inclusive_and_refused_one_past() {
    for k in DECK_KNOBS {
        for edge in [k.min, k.max] {
            let c = DeckConfig::parse(&deck_text(k.key, edge))
                .unwrap_or_else(|e| panic!("{} = {edge}: {e}", k.key));
            assert_eq!(deck_value(&c, k.key), edge, "{}", k.key);
        }
        let mut past = vec![k.max + 1];
        if k.min > 0 {
            past.push(k.min - 1);
        }
        for value in past {
            let (field, message) = invalid_of(DeckConfig::parse(&deck_text(k.key, value)));
            assert_eq!(field, k.key);
            assert_eq!(message, format!("use a value from {} to {}", k.min, k.max));
        }
    }
}

#[test]
fn each_station_bound_is_inclusive_and_refused_one_past() {
    for k in STATION_KNOBS {
        for edge in [k.min, k.max] {
            let text = format!("{} = {edge}\n", k.key);
            let c = StationConfig::parse(&text, &env()).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(station_value(&c, k.key), edge, "{}", k.key);
        }
        let mut past = vec![k.max + 1];
        if k.min > 0 {
            past.push(k.min - 1);
        }
        for value in past {
            let text = format!("{} = {value}\n", k.key);
            let (field, message) = invalid_of(StationConfig::parse(&text, &env()));
            assert_eq!(field, k.key);
            assert_eq!(message, format!("use a value from {} to {}", k.min, k.max));
        }
    }
}

#[test]
fn a_negative_or_text_value_is_refused() {
    assert!(DeckConfig::parse("[security]\nmin_password_chars = -1\n").is_err());
    assert!(DeckConfig::parse("[security]\nmin_password_chars = \"12\"\n").is_err());
    assert!(StationConfig::parse("[capture]\nquota_gib = -4\n", &env()).is_err());
    assert!(StationConfig::parse("[capture]\nquota_gib = 4.5\n", &env()).is_err());
}

#[test]
fn a_pair_with_an_idle_time_above_the_longest_time_is_refused() {
    const TEXT: &str = "The idle time cannot be longer than the longest time.";
    // At the edge the pair holds.
    let ok = DeckConfig::parse("[security]\nlogin_idle_days = 30\nlogin_max_days = 30\n").unwrap();
    assert_eq!(ok.security.login_idle_days, ok.security.login_max_days);
    let ok =
        DeckConfig::parse("[security]\ndisplay_idle_days = 60\ndisplay_max_days = 60\n").unwrap();
    assert_eq!(ok.security.display_idle_days, ok.security.display_max_days);
    for (text, key) in [
        (
            "[security]\nlogin_idle_days = 31\nlogin_max_days = 30\n",
            "security.login_idle_days",
        ),
        (
            "[security]\ndisplay_idle_days = 91\ndisplay_max_days = 90\n",
            "security.display_idle_days",
        ),
        // A key alone breaks the pair with the default of the other key.
        (
            "[security]\nlogin_max_days = 6\n",
            "security.login_idle_days",
        ),
        (
            "[security]\ndisplay_max_days = 29\n",
            "security.display_idle_days",
        ),
        (
            "[security]\nlogin_idle_days = 31\n",
            "security.login_idle_days",
        ),
    ] {
        let (field, message) = invalid_of(DeckConfig::parse(text));
        assert_eq!(field, key, "{text}");
        assert_eq!(message, TEXT, "{text}");
    }
}

#[test]
fn an_unknown_key_in_a_table_is_refused() {
    for table in ["security", "retention", "limits"] {
        let text = format!("[{table}]\nnope = 1\n");
        let e = DeckConfig::parse(&text).unwrap_err().to_string();
        assert!(e.contains("unknown field"), "{table}: {e}");
    }
    for table in ["limits", "retention", "capture"] {
        let text = format!("[{table}]\nnope = 1\n");
        let e = StationConfig::parse(&text, &env()).unwrap_err().to_string();
        assert!(e.contains("unknown field"), "{table}: {e}");
    }
    // A deck key does not belong in the station table of the same name.
    assert!(StationConfig::parse("[retention]\naudit_days = 90\n", &env()).is_err());
    assert!(DeckConfig::parse("[retention]\nhistory_days = 90\n").is_err());
}
