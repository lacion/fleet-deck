//! The effective security posture (B27 §3.6): which controls are weaker
//! than their secure default, in the file, in the running process or from a
//! command-line flag.

use std::net::SocketAddr;

use fleetdeck_proto::settings::{Apply, Relaxed, RelaxedState};
use serde_json::{Map, Value};

use super::diff::danger_keys;
use super::fields::station_fields;
use super::holder::{DeckHolder, Holder, StationHolder, policy_rank};
use crate::config::{
    CLAUDE_PROVIDER_DEFAULT, CODEX_PROVIDER_DEFAULT, DeckConfig, OfflineApprovalPolicy,
    StationConfig,
};
use crate::config_worker_grant::mode_within;

/// The deck danger keys that are configuration, or a value whose default is
/// already the widest (`worker_grant.enabled` and `keep_grants_on_update` are
/// on and `worker_grant.stations` is `all` by default), not a relaxation.
pub const DECK_NOT_POSTURE: &[&str] = &[
    "keep_grants_on_update",
    "tailscale_bin",
    "tailnet_auto",
    "worker_grant.enabled",
    "worker_grant.stations",
];

/// The station danger keys that are configuration, or a value whose default
/// is already the widest.
pub const STATION_NOT_POSTURE: &[&str] = &[
    "allowed_roots",
    "capture_monitors",
    "provider_instance",
    "host",
    "tailscale_bin",
    "retention.history_days",
];

/// The station keys that the posture can list (the table of §3.6).
pub const STATION_POSTURE_KEYS: &[&str] = &[
    "allow_setup_cmd",
    "allow_shell",
    "allow_bypass_permissions",
    "allow_fleet_control",
    "allow_desktop",
    "allow_desktop_capture",
    "allow_desktop_input",
    "allow_desktop_click",
    "allow_desktop_input_any",
    "fleet_summary",
    "allow_check_cmd",
    "allow_local_remotes",
    "offline_approval_policy",
    "listen",
    "retention.raw_log_mib",
    "default_mode.claude",
    "default_mode.codex",
];

/// The text of a value in an item of the posture.
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) if s == crate::config_grant_limits::NO_LIMIT_WORD => "no limit".to_owned(),
        Value::String(s) => s.clone(),
        Value::Bool(true) => "on".to_owned(),
        Value::Bool(false) => "off".to_owned(),
        Value::Array(items) => items.iter().map(value_text).collect::<Vec<_>>().join(", "),
        Value::Null => "unset".to_owned(),
        other => other.to_string(),
    }
}

/// The text of one item: a bool key `{label}`; a listen key `{label}: every
/// network interface`; any other key `{label}: {value} (default {default})`.
fn item_text(key: &str, label: &str, value: &Value, default: &Value) -> String {
    if value.is_boolean() {
        label.to_owned()
    } else if key == "listen" || key == "link_listen" {
        format!("{label}: every network interface")
    } else {
        format!(
            "{label}: {} (default {})",
            value_text(value),
            value_text(default)
        )
    }
}

fn unspecified(addrs: &[SocketAddr]) -> bool {
    addrs.iter().any(|a| a.ip().is_unspecified())
}

/// The relaxed controls of one deck config: the keys of the danger check of
/// the default config against `c`, without the keys that are not a
/// relaxation (§3.6).
pub(super) fn deck_relaxed(c: &DeckConfig) -> Vec<(String, String)> {
    let Ok(default) = DeckConfig::parse("") else {
        return Vec::new();
    };
    let (now, base) = (DeckHolder::values(c), DeckHolder::values(&default));
    let labels = labels::<DeckHolder>();
    danger_keys::<DeckHolder>(&default, c)
        .into_iter()
        .filter(|k| !DECK_NOT_POSTURE.contains(&k.as_str()))
        .filter(|k| match k.as_str() {
            "listen" => unspecified(&c.listen),
            "link_listen" => unspecified(&c.link_listen),
            _ => true,
        })
        .map(|k| {
            let (value, default) = (value_of(&now, &k), value_of(&base, &k));
            let label = labels.get(&k).map_or(k.as_str(), String::as_str);
            let text = item_text(&k, label, &value, &default);
            (k, text)
        })
        .collect()
}

/// The relaxed controls of one station config: the table of §3.6.
pub(super) fn station_relaxed(c: &StationConfig) -> Vec<(String, String)> {
    let values = StationHolder::values(c);
    let mut out = Vec::new();
    for f in station_fields() {
        let key = f.key.as_str();
        let on = |b: bool| b.then(|| Value::Bool(false));
        let default = match key {
            "allow_setup_cmd" => on(c.allow_setup_cmd),
            "allow_shell" => on(c.allow_shell),
            "allow_bypass_permissions" => on(c.allow_bypass_permissions),
            "allow_fleet_control" => on(c.allow_fleet_control),
            "allow_desktop" => on(c.allow_desktop),
            "allow_desktop_capture" => on(c.allow_desktop_capture),
            "allow_desktop_input" => on(c.allow_desktop_input),
            "allow_desktop_click" => on(c.allow_desktop_click),
            "allow_desktop_input_any" => on(c.allow_desktop_input_any),
            "fleet_summary" => on(c.fleet_summary),
            "allow_check_cmd" => on(c.allow_check_cmd),
            "allow_local_remotes" => on(c.allow_local_remotes),
            "offline_approval_policy" => (policy_rank(c.offline_approval_policy)
                > policy_rank(OfflineApprovalPolicy::Wait))
            .then(|| Value::String("wait".to_owned())),
            "listen" => unspecified(&c.listen).then(|| Value::Null),
            "retention.raw_log_mib" => (c.retention.raw_log_mib > 16).then(|| Value::from(16_u32)),
            "default_mode.claude" => mode_default("claude", CLAUDE_PROVIDER_DEFAULT, c),
            "default_mode.codex" => mode_default("codex", CODEX_PROVIDER_DEFAULT, c),
            _ => None,
        };
        if let Some(default) = default {
            let value = value_of(&values, key);
            out.push((f.key.clone(), item_text(key, &f.label, &value, &default)));
        }
    }
    out
}

/// The provider default as the default value, when the station default of
/// `family` is not within it.
fn mode_default(family: &str, provider_default: &str, c: &StationConfig) -> Option<Value> {
    let mode = c.default_mode.for_family(family)?;
    (!mode_within(family, Some(mode), provider_default))
        .then(|| Value::String(provider_default.to_owned()))
}

fn value_of(values: &Map<String, Value>, key: &str) -> Value {
    values.get(key).cloned().unwrap_or(Value::Null)
}

fn labels<H: Holder>() -> std::collections::HashMap<String, String> {
    H::fields().into_iter().map(|f| (f.key, f.label)).collect()
}

/// True when the file value of a `default_mode.*` key needs the bypass that
/// the running process does not offer yet (§3.5, revision 2).
fn waits_for_bypass<H: Holder>(key: &str, file: &H::Parsed, running: &H::Parsed) -> bool {
    if !key.starts_with("default_mode.") {
        return false;
    }
    let offered = H::values(running)
        .get("allow_bypass_permissions")
        .and_then(Value::as_bool);
    let mode = H::values(file)
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned);
    let needs =
        mode.is_some_and(|m| m == "bypassPermissions" || m.ends_with("/danger-full-access"));
    needs && offered == Some(false)
}

/// The posture of the file `file` for a process that runs `running`, with
/// the command-line `overrides` (keys). `running = None` (no process known,
/// for example a test): every item of `file` is `Active`.
#[must_use]
pub fn posture<H: Holder>(
    file: &H::Parsed,
    running: Option<&H::Parsed>,
    overrides: &[&str],
) -> Vec<Relaxed> {
    let in_file = H::relaxed(file);
    let Some(running) = running else {
        return order::<H>(
            in_file
                .into_iter()
                .map(|(key, text)| item(key, text, RelaxedState::Active))
                .collect(),
        );
    };
    let in_running = H::relaxed(running);
    let find = |list: &[(String, String)], key: &str| {
        list.iter().find(|(k, _)| k == key).map(|(_, t)| t.clone())
    };
    let mut out = Vec::new();
    for f in H::fields() {
        let key = f.key.as_str();
        let (file_text, run_text) = (find(&in_file, key), find(&in_running, key));
        let found = if overrides.contains(&key) {
            run_text.map(|t| (t, RelaxedState::CommandLine))
        } else if f.apply == Apply::Live {
            file_text.map(|t| {
                let state = if waits_for_bypass::<H>(key, file, running) {
                    RelaxedState::AfterRestart
                } else {
                    RelaxedState::Active
                };
                (t, state)
            })
        } else {
            match (file_text, run_text) {
                (Some(t), Some(_)) => Some((t, RelaxedState::Active)),
                (Some(t), None) => Some((t, RelaxedState::AfterRestart)),
                (None, Some(t)) => Some((t, RelaxedState::UntilRestart)),
                (None, None) => None,
            }
        };
        if let Some((text, state)) = found {
            out.push(item(f.key.clone(), text, state));
        }
    }
    out
}

fn item(key: String, text: String, state: RelaxedState) -> Relaxed {
    Relaxed { key, text, state }
}

/// The items in the order of the field table.
fn order<H: Holder>(mut items: Vec<Relaxed>) -> Vec<Relaxed> {
    let fields = H::fields();
    items.sort_by_key(|i| fields.iter().position(|f| f.key == i.key));
    items
}
