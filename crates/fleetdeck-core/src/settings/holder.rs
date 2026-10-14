//! The two holders of a config file (the station and the deck): how to
//! parse their text and how to render a parsed config as settings values.
//!
//! The values are built from the parsed config and from the field table
//! only. A secret is shown as `{ "set": true }`; no value of an `env` entry
//! is ever read here.

use std::path::Path;

use fleetdeck_proto::settings::FieldMeta;
use serde_json::{Map, Value, json};

use super::fields::{deck_fields, station_fields};
use crate::config::{
    ConfigError, DeckConfig, Driver, HostChoice, OfflineApprovalPolicy, StationConfig, TokenSource,
    TrackerConfig,
};
use crate::paths::Env;

/// A config file that the settings code can read and write.
pub trait Holder: 'static {
    type Parsed: Send + 'static;

    /// The field table.
    fn fields() -> Vec<FieldMeta>;
    /// Parses the file text with the full validation. `store` resolves the
    /// secret references; the deck has none.
    fn parse(text: &str, env: &Env, store: Option<&Path>) -> Result<Self::Parsed, ConfigError>;
    /// The settings values of a parsed config, one for each field key.
    fn values(parsed: &Self::Parsed) -> Map<String, Value>;
    /// The values to compare two configs. It differs from [`Self::values`]
    /// only where a value is secret: a digest stands for the secret, so a
    /// change of a secret is a change. It never leaves the process.
    fn compare_values(parsed: &Self::Parsed) -> Map<String, Value> {
        Self::values(parsed)
    }
    /// The keys of the changes that need a recent sign-in (design 1.5).
    fn danger(old: &Self::Parsed, new: &Self::Parsed) -> Vec<String>;
    /// The ids of the provider instances that the config has (also the
    /// defaults that the file does not name).
    fn instance_ids(_parsed: &Self::Parsed) -> Vec<String> {
        Vec::new()
    }
}

/// `station.toml`.
#[derive(Debug, Clone, Copy)]
pub struct StationHolder;

/// `deck.toml`.
#[derive(Debug, Clone, Copy)]
pub struct DeckHolder;

impl Holder for StationHolder {
    type Parsed = StationConfig;

    fn fields() -> Vec<FieldMeta> {
        station_fields()
    }

    fn parse(text: &str, env: &Env, store: Option<&Path>) -> Result<StationConfig, ConfigError> {
        StationConfig::parse_with_store(text, env, store)
    }

    fn values(c: &StationConfig) -> Map<String, Value> {
        let mut m = Map::new();
        let strings = |v: Vec<String>| Value::Array(v.into_iter().map(Value::String).collect());
        m.insert("name".into(), json!(c.name));
        m.insert(
            "allowed_roots".into(),
            strings(
                c.allowed_roots
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
            ),
        );
        m.insert("max_sessions".into(), json!(c.max_sessions));
        m.insert("allow_setup_cmd".into(), json!(c.allow_setup_cmd));
        m.insert("allow_shell".into(), json!(c.allow_shell));
        m.insert(
            "allow_bypass_permissions".into(),
            json!(c.allow_bypass_permissions),
        );
        m.insert("max_shells".into(), json!(c.max_shells));
        m.insert(
            "offline_approval_policy".into(),
            json!(policy_text(c.offline_approval_policy)),
        );
        m.insert(
            "listen".into(),
            strings(c.listen.iter().map(ToString::to_string).collect()),
        );
        m.insert(
            "provider_instance".into(),
            Value::Array(
                c.provider_instances
                    .iter()
                    .map(|i| {
                        let env: Map<String, Value> = i
                            .env
                            .keys()
                            .map(|k| (k.clone(), json!({ "set": true })))
                            .collect();
                        json!({
                            "id": i.id.as_str(),
                            "driver": driver_text(i.driver),
                            "binary": i.binary.display().to_string(),
                            "config_home": i.config_home.as_ref().map(|p| p.display().to_string()),
                            "env": env,
                        })
                    })
                    .collect(),
            ),
        );
        m.insert(
            "host".into(),
            json!(match c.host {
                HostChoice::Direct => "direct",
                HostChoice::Shim => "shim",
            }),
        );
        m.insert("shim_buffer_mib".into(), json!(c.shim_buffer_mib));
        m.insert(
            "repo_groups".into(),
            Value::Object(
                c.repo_groups
                    .iter()
                    .map(|(p, g)| (p.display().to_string(), json!(g)))
                    .collect(),
            ),
        );
        m.insert("hibernate_after_min".into(), json!(c.hibernate_after_min));
        m.insert(
            "trackers".into(),
            Value::Array(c.trackers.iter().map(tracker_value).collect()),
        );
        m.insert("member_deny".into(), strings(c.member_deny.clone()));
        let h = &c.model_help;
        m.insert("model_help.enabled".into(), json!(h.enabled));
        m.insert("model_help.claude_model".into(), json!(h.claude_model));
        m.insert("model_help.codex_model".into(), json!(h.codex_model));
        m.insert(
            "model_help.provider".into(),
            h.provider.map_or(Value::Null, |d| json!(driver_text(d))),
        );
        m.insert(
            "model_help.per_call_max_output_tokens".into(),
            json!(h.per_call_max_output_tokens),
        );
        m.insert(
            "model_help.daily_token_cap".into(),
            json!(h.daily_token_cap),
        );
        m
    }

    fn compare_values(c: &StationConfig) -> Map<String, Value> {
        let mut m = Self::values(c);
        if let Some(Value::Array(list)) = m.get_mut("provider_instance") {
            for (entry, instance) in list.iter_mut().zip(&c.provider_instances) {
                let env: Map<String, Value> = instance
                    .env
                    .iter()
                    .map(|(k, v)| {
                        let digest =
                            ring::digest::digest(&ring::digest::SHA256, v.expose().as_bytes());
                        (
                            k.clone(),
                            json!({ "set": true, "digest": hex(digest.as_ref()) }),
                        )
                    })
                    .collect();
                entry["env"] = Value::Object(env);
            }
        }
        m
    }

    fn danger(old: &StationConfig, new: &StationConfig) -> Vec<String> {
        let mut keys = Vec::new();
        let mut add = |cond: bool, key: &str| {
            if cond {
                keys.push(key.to_owned());
            }
        };
        add(
            !old.allow_setup_cmd && new.allow_setup_cmd,
            "allow_setup_cmd",
        );
        add(!old.allow_shell && new.allow_shell, "allow_shell");
        add(
            !old.allow_bypass_permissions && new.allow_bypass_permissions,
            "allow_bypass_permissions",
        );
        add(
            new.allowed_roots
                .iter()
                .any(|n| !old.allowed_roots.contains(n)),
            "allowed_roots",
        );
        add(
            policy_rank(new.offline_approval_policy) > policy_rank(old.offline_approval_policy),
            "offline_approval_policy",
        );
        add(new_non_loopback(&old.listen, &new.listen), "listen");
        add(
            Self::compare_values(old).get("provider_instance")
                != Self::compare_values(new).get("provider_instance"),
            "provider_instance",
        );
        add(old.host != new.host, "host");
        keys
    }

    fn instance_ids(c: &StationConfig) -> Vec<String> {
        c.provider_instances
            .iter()
            .map(|i| i.id.as_str().to_owned())
            .collect()
    }
}

impl Holder for DeckHolder {
    type Parsed = DeckConfig;

    fn fields() -> Vec<FieldMeta> {
        deck_fields()
    }

    fn parse(text: &str, _env: &Env, _store: Option<&Path>) -> Result<DeckConfig, ConfigError> {
        DeckConfig::parse(text)
    }

    fn values(c: &DeckConfig) -> Map<String, Value> {
        let addrs = |v: &[std::net::SocketAddr]| {
            Value::Array(v.iter().map(|a| json!(a.to_string())).collect())
        };
        let mut m = Map::new();
        m.insert("name".into(), json!(c.name));
        m.insert("listen".into(), addrs(&c.listen));
        m.insert("link_listen".into(), addrs(&c.link_listen));
        m.insert("tailnet_hostname".into(), json!(c.tailnet_hostname));
        m.insert("conflict_mail".into(), json!(c.conflict_mail));
        m.insert("fleet_control".into(), json!(c.fleet_control));
        m.insert("clone_host".into(), json!(c.clone_host));
        m.insert("clone_org".into(), json!(c.clone_org));
        m.insert("clone_transport".into(), json!(c.clone_transport.as_str()));
        m
    }

    fn danger(old: &DeckConfig, new: &DeckConfig) -> Vec<String> {
        let mut keys = Vec::new();
        if new_non_loopback(&old.listen, &new.listen) {
            keys.push("listen".to_owned());
        }
        if new_non_loopback(&old.link_listen, &new.link_listen) {
            keys.push("link_listen".to_owned());
        }
        // Turning fleet control on needs the owner's elevation; off does not
        // (ADR-008 section 3).
        if !old.fleet_control && new.fleet_control {
            keys.push("fleet_control".to_owned());
        }
        keys
    }
}

/// The order of the offline policies, from the least to the most
/// permissive: a shorter `deny_after` denies sooner than a longer one, and
/// `wait` never denies.
fn policy_rank(p: OfflineApprovalPolicy) -> u64 {
    match p {
        OfflineApprovalPolicy::Wait => u64::MAX,
        OfflineApprovalPolicy::DenyAfter(d) => d.as_secs(),
    }
}

/// True when `new` has an address that is not loopback and `old` did not.
fn new_non_loopback(old: &[std::net::SocketAddr], new: &[std::net::SocketAddr]) -> bool {
    new.iter()
        .any(|a| !a.ip().is_loopback() && !old.contains(a))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn driver_text(d: Driver) -> &'static str {
    match d {
        Driver::Claude => "claude",
        Driver::Codex => "codex",
    }
}

/// The file text of a policy: `wait`, `deny_after(90s)`, `deny_after(5m)`.
fn policy_text(p: OfflineApprovalPolicy) -> String {
    match p {
        OfflineApprovalPolicy::Wait => "wait".to_owned(),
        OfflineApprovalPolicy::DenyAfter(d) => {
            let s = d.as_secs();
            if s % 3600 == 0 {
                format!("deny_after({}h)", s / 3600)
            } else if s % 60 == 0 {
                format!("deny_after({}m)", s / 60)
            } else {
                format!("deny_after({s}s)")
            }
        }
    }
}

/// A tracker entry for the read-only view: where the token comes from,
/// never the token.
fn tracker_value(t: &TrackerConfig) -> Value {
    let token = match &t.token {
        TokenSource::Env(name) => json!({ "env": name }),
        TokenSource::File(path) => json!({ "file": path.display().to_string() }),
    };
    json!({
        "id": t.id,
        "kind": t.kind,
        "site": t.site,
        "email": t.email,
        "token": token,
    })
}
