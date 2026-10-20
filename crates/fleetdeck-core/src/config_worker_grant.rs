//! The `[worker_grant]` table of `deck.toml` (MCP P4a section 2): the default
//! grant that an eligible session start gets.

use serde::Deserialize;

use crate::config::ConfigError;

/// The most station names of a list (`["all"]` stands for every station).
pub const MAX_WORKER_STATIONS: usize = 16;
/// The keyword of the list that stands for every paired station.
pub const ALL_STATIONS: &str = "all";

/// The Claude modes, most restrictive first (the same order as the deck's
/// `coordinator_ops`; a deck test keeps both copies equal).
const CLAUDE_MODES: &[&str] = &[
    "default",
    "plan",
    "acceptEdits",
    "dontAsk",
    "auto",
    "bypassPermissions",
];
/// The Codex approval policies and sandboxes, most restrictive first.
const CODEX_POLICIES: &[&str] = &["untrusted", "on-request", "never"];
const CODEX_SANDBOXES: &[&str] = &["read-only", "workspace-write", "danger-full-access"];
/// The mode of a Codex start without a mode.
const CODEX_DEFAULT: &str = "untrusted/workspace-write";

/// The Claude ceilings that the setting accepts (every mode but the bypass one).
pub const CLAUDE_CEILINGS: &[&str] = &["default", "plan", "acceptEdits", "dontAsk", "auto"];
/// The Codex ceilings that the setting accepts (the eight non-bypass values).
pub const CODEX_CEILINGS: &[&str] = &[
    "untrusted/read-only",
    "untrusted/workspace-write",
    "untrusted/danger-full-access",
    "on-request/read-only",
    "on-request/workspace-write",
    "on-request/danger-full-access",
    "never/read-only",
    "never/workspace-write",
];

/// The `[worker_grant]` table of `deck.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerGrantConfig {
    /// Whether an eligible start gets a worker grant.
    pub enabled: bool,
    /// Station names, or `["all"]` as the only entry.
    pub stations: Vec<String>,
    /// The most sessions that a worker can start.
    pub spawn_cap: u32,
    /// The Claude mode ceiling of the grant.
    pub claude_mode: String,
    /// The Codex mode ceiling of the grant.
    pub codex_mode: String,
    pub lifetime_hours: u32,
    pub idle_minutes: u32,
}

impl Default for WorkerGrantConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            stations: vec![ALL_STATIONS.to_owned()],
            spawn_cap: 2,
            claude_mode: "acceptEdits".to_owned(),
            codex_mode: "on-request/workspace-write".to_owned(),
            lifetime_hours: 12,
            idle_minutes: 30,
        }
    }
}

impl WorkerGrantConfig {
    /// True when the list is the single entry `all`.
    #[must_use]
    pub fn all_stations(&self) -> bool {
        self.stations.len() == 1 && self.stations[0] == ALL_STATIONS
    }
}

/// The keys of `[worker_grant]` (`RawWorkerGrant`). A test in `settings` checks
/// that the field table has one entry for each key.
#[cfg(test)]
pub(crate) const RAW_WORKER_GRANT_KEYS: &[&str] = &[
    "enabled",
    "stations",
    "spawn_cap",
    "claude_mode",
    "codex_mode",
    "lifetime_hours",
    "idle_minutes",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWorkerGrant {
    enabled: Option<bool>,
    stations: Option<Vec<String>>,
    spawn_cap: Option<u32>,
    claude_mode: Option<String>,
    codex_mode: Option<String>,
    lifetime_hours: Option<u32>,
    idle_minutes: Option<u32>,
}

fn invalid(key: &str, message: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        field: format!("worker_grant.{key}"),
        message: message.into(),
    }
}

fn bounded(
    key: &str,
    value: Option<u32>,
    default: u32,
    (min, max): (u32, u32),
) -> Result<u32, ConfigError> {
    let value = value.unwrap_or(default);
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(invalid(key, format!("use a value from {min} to {max}")))
    }
}

fn station_name_ok(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

impl RawWorkerGrant {
    pub(crate) fn validate(self) -> Result<WorkerGrantConfig, ConfigError> {
        let d = WorkerGrantConfig::default();
        let stations = match self.stations {
            None => d.stations.clone(),
            Some(list) => {
                if list.is_empty() || list.len() > MAX_WORKER_STATIONS {
                    return Err(invalid(
                        "stations",
                        format!("use [\"all\"] or 1 to {MAX_WORKER_STATIONS} station names"),
                    ));
                }
                if list.iter().any(|s| s == ALL_STATIONS) && list.len() != 1 {
                    return Err(invalid(
                        "stations",
                        "\"all\" is valid only as the sole entry",
                    ));
                }
                if let Some(bad) = list.iter().find(|s| !station_name_ok(s)) {
                    return Err(invalid(
                        "stations",
                        format!("`{bad}`: use 1-63 of A-Z, a-z, 0-9, `-`, `_`, `.`"),
                    ));
                }
                list
            }
        };
        let claude_mode = self.claude_mode.unwrap_or(d.claude_mode);
        if !CLAUDE_CEILINGS.contains(&claude_mode.as_str()) {
            return Err(invalid(
                "claude_mode",
                format!("use one of {}", CLAUDE_CEILINGS.join(", ")),
            ));
        }
        let codex_mode = self.codex_mode.unwrap_or(d.codex_mode);
        if !CODEX_CEILINGS.contains(&codex_mode.as_str()) {
            return Err(invalid(
                "codex_mode",
                format!("use one of {}", CODEX_CEILINGS.join(", ")),
            ));
        }
        Ok(WorkerGrantConfig {
            enabled: self.enabled.unwrap_or(d.enabled),
            stations,
            spawn_cap: bounded("spawn_cap", self.spawn_cap, d.spawn_cap, (0, 10))?,
            claude_mode,
            codex_mode,
            lifetime_hours: bounded(
                "lifetime_hours",
                self.lifetime_hours,
                d.lifetime_hours,
                (1, 168),
            )?,
            idle_minutes: bounded("idle_minutes", self.idle_minutes, d.idle_minutes, (5, 1440))?,
        })
    }
}

/// Is `mode` (or the provider default) within `ceiling` for `family`? This is
/// a copy of the deck's `coordinator_ops::mode_within` (core does not depend
/// on the deck); a deck test keeps both equal.
#[must_use]
pub fn mode_within(family: &str, mode: Option<&str>, ceiling: &str) -> bool {
    let at = |list: &[&str], m: &str| list.iter().position(|x| *x == m);
    match family {
        "claude" => {
            let m = mode.unwrap_or("default");
            matches!((at(CLAUDE_MODES, m), at(CLAUDE_MODES, ceiling)), (Some(a), Some(b)) if a <= b)
        }
        "codex" => {
            let parse = |s: &str| {
                let (p, b) = s.split_once('/')?;
                Some((at(CODEX_POLICIES, p)?, at(CODEX_SANDBOXES, b)?))
            };
            match (parse(mode.unwrap_or(CODEX_DEFAULT)), parse(ceiling)) {
                (Some((p, s)), Some((cp, cs))) => p <= cp && s <= cs,
                _ => false,
            }
        }
        _ => false,
    }
}

/// The keys that `new` widens against `old` (MCP P4a section 2): a change that
/// needs the owner's elevation.
#[must_use]
pub fn widens(old: &WorkerGrantConfig, new: &WorkerGrantConfig) -> Vec<&'static str> {
    let mut keys = Vec::new();
    if !old.enabled && new.enabled {
        keys.push("worker_grant.enabled");
    }
    if new.spawn_cap > old.spawn_cap {
        keys.push("worker_grant.spawn_cap");
    }
    let stations_wider = if new.all_stations() {
        !old.all_stations()
    } else if old.all_stations() {
        false
    } else {
        new.stations.iter().any(|n| !old.stations.contains(n))
    };
    if stations_wider {
        keys.push("worker_grant.stations");
    }
    if new.lifetime_hours > old.lifetime_hours {
        keys.push("worker_grant.lifetime_hours");
    }
    if new.idle_minutes > old.idle_minutes {
        keys.push("worker_grant.idle_minutes");
    }
    if new.claude_mode != old.claude_mode
        && !mode_within("claude", Some(&new.claude_mode), &old.claude_mode)
    {
        keys.push("worker_grant.claude_mode");
    }
    if new.codex_mode != old.codex_mode
        && !mode_within("codex", Some(&new.codex_mode), &old.codex_mode)
    {
        keys.push("worker_grant.codex_mode");
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(f: impl FnOnce(&mut WorkerGrantConfig)) -> WorkerGrantConfig {
        let mut c = WorkerGrantConfig::default();
        f(&mut c);
        c
    }

    #[test]
    fn the_defaults_are_the_design_values() {
        let c = RawWorkerGrant {
            enabled: None,
            stations: None,
            spawn_cap: None,
            claude_mode: None,
            codex_mode: None,
            lifetime_hours: None,
            idle_minutes: None,
        }
        .validate()
        .unwrap();
        assert_eq!(c, WorkerGrantConfig::default());
        assert!(c.enabled && c.all_stations());
    }

    #[test]
    fn widening_follows_the_design() {
        let base = WorkerGrantConfig::default();
        assert!(widens(&base, &base).is_empty());
        let off = with(|c| c.enabled = false);
        assert_eq!(widens(&off, &base), ["worker_grant.enabled"]);
        assert!(widens(&base, &off).is_empty());
        assert_eq!(
            widens(&base, &with(|c| c.spawn_cap = 3)),
            ["worker_grant.spawn_cap"]
        );
        assert!(widens(&base, &with(|c| c.spawn_cap = 1)).is_empty());
        let list = with(|c| c.stations = vec!["a".into()]);
        assert_eq!(widens(&list, &base), ["worker_grant.stations"]);
        assert!(widens(&base, &list).is_empty());
        let two = with(|c| c.stations = vec!["a".into(), "b".into()]);
        assert_eq!(widens(&list, &two), ["worker_grant.stations"]);
        assert!(widens(&two, &list).is_empty());
        assert_eq!(
            widens(&base, &with(|c| c.lifetime_hours = 13)),
            ["worker_grant.lifetime_hours"]
        );
        assert_eq!(
            widens(&base, &with(|c| c.idle_minutes = 31)),
            ["worker_grant.idle_minutes"]
        );
        assert_eq!(
            widens(&base, &with(|c| c.claude_mode = "auto".into())),
            ["worker_grant.claude_mode"]
        );
        assert!(widens(&base, &with(|c| c.claude_mode = "plan".into())).is_empty());
        // An incomparable Codex change is widening, in both directions.
        let a = with(|c| c.codex_mode = "on-request/workspace-write".into());
        let b = with(|c| c.codex_mode = "never/read-only".into());
        assert_eq!(widens(&a, &b), ["worker_grant.codex_mode"]);
        assert_eq!(widens(&b, &a), ["worker_grant.codex_mode"]);
        assert!(widens(&a, &with(|c| c.codex_mode = "untrusted/read-only".into())).is_empty());
    }

    #[test]
    fn the_table_parser_checks_the_bounds() {
        let ok = |text: &str| crate::config::DeckConfig::parse(text);
        assert!(ok("[worker_grant]\nstations = [\"a\", \"b\"]\n").is_ok());
        assert!(ok("[worker_grant]\nstations = [\"all\", \"b\"]\n").is_err());
        assert!(ok("[worker_grant]\nstations = []\n").is_err());
        assert!(ok("[worker_grant]\nspawn_cap = 11\n").is_err());
        assert!(ok("[worker_grant]\nlifetime_hours = 0\n").is_err());
        assert!(ok("[worker_grant]\nidle_minutes = 4\n").is_err());
        assert!(ok("[worker_grant]\nclaude_mode = \"bypassPermissions\"\n").is_err());
        assert!(ok("[worker_grant]\ncodex_mode = \"never/danger-full-access\"\n").is_err());
        assert!(ok("[worker_grant]\nnope = 1\n").is_err());
        let names: Vec<String> = (0..17).map(|i| format!("\"s{i}\"")).collect();
        assert!(
            ok(&format!(
                "[worker_grant]\nstations = [{}]\n",
                names.join(",")
            ))
            .is_err()
        );
    }
}
