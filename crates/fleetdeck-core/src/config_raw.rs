//! The raw TOML shapes of `station.toml` and the provider instance parser
//! (moved out of `config.rs` for size; no behavior change).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fleetdeck_proto::ProviderInstanceId;
use serde::Deserialize;

use super::model_help_cfg::RawModelHelp;
use super::trackers_cfg::RawTracker;
use super::{ConfigError, Driver, ProviderInstance, invalid, valid_env_key, valid_instance_id};
use crate::paths::expand_path;
use crate::secret::Secret;
use crate::settings::refs;

/// The keys of `station.toml` (`RawStation`). `model_help` is a table; its
/// keys are in [`RAW_MODEL_HELP_KEYS`].
#[cfg(test)]
pub(crate) const RAW_STATION_KEYS: &[&str] = &[
    "name",
    "allowed_roots",
    "max_sessions",
    "allow_setup_cmd",
    "allow_shell",
    "allow_bypass_permissions",
    "allow_fleet_control",
    "allow_desktop",
    "allow_desktop_capture",
    "allow_desktop_input",
    "capture_monitors",
    "fleet_summary",
    "max_shells",
    "offline_approval_policy",
    "listen",
    "provider_instance",
    "host",
    "shim_buffer_mib",
    "repo_groups",
    "hibernate_after_min",
    "trackers",
    "member_deny",
    "model_help",
    "tailscale_bin",
    "clone_root",
    "traits",
    "allow_check_cmd",
    "allow_local_remotes",
    "taildrive_mounts",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawStation {
    pub(super) name: Option<String>,
    pub(super) allowed_roots: Option<Vec<String>>,
    pub(super) max_sessions: Option<u32>,
    pub(super) allow_setup_cmd: Option<bool>,
    pub(super) allow_shell: Option<bool>,
    pub(super) allow_bypass_permissions: Option<bool>,
    pub(super) allow_fleet_control: Option<bool>,
    pub(super) allow_desktop: Option<bool>,
    pub(super) allow_desktop_capture: Option<bool>,
    pub(super) allow_desktop_input: Option<bool>,
    pub(super) capture_monitors: Option<Vec<String>>,
    pub(super) fleet_summary: Option<bool>,
    pub(super) max_shells: Option<u32>,
    pub(super) offline_approval_policy: Option<String>,
    pub(super) listen: Option<Vec<String>>,
    pub(super) provider_instance: Option<Vec<RawInstance>>,
    pub(super) host: Option<String>,
    pub(super) shim_buffer_mib: Option<u32>,
    pub(super) repo_groups: Option<BTreeMap<String, String>>,
    pub(super) hibernate_after_min: Option<u32>,
    pub(super) trackers: Option<Vec<RawTracker>>,
    pub(super) member_deny: Option<Vec<String>>,
    pub(super) model_help: Option<RawModelHelp>,
    pub(super) tailscale_bin: Option<String>,
    pub(super) clone_root: Option<String>,
    pub(super) traits: Option<Vec<String>>,
    pub(super) allow_check_cmd: Option<bool>,
    pub(super) allow_local_remotes: Option<bool>,
    pub(super) taildrive_mounts: Option<BTreeMap<String, String>>,
}

/// The keys of one `[[provider_instance]]` (`RawInstance`).
pub(crate) const RAW_INSTANCE_KEYS: &[&str] = &["id", "driver", "binary", "config_home", "env"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawInstance {
    id: String,
    driver: Driver,
    binary: Option<String>,
    config_home: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, RawEnvValue>,
}

/// An `env` value: the plain text, or a reference to the secret store.
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum RawEnvValue {
    Plain(String),
    Reference(RawSecretRef),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawSecretRef {
    pub(super) secret: String,
}

pub(super) fn parse_instances(
    list: Vec<RawInstance>,
    home: Option<&Path>,
    store: Option<&Path>,
) -> Result<Vec<ProviderInstance>, ConfigError> {
    let mut out: Vec<ProviderInstance> = Vec::with_capacity(list.len());
    for (i, raw) in list.into_iter().enumerate() {
        let field = |k: &str| format!("provider_instance[{i}].{k}");
        if !valid_instance_id(&raw.id) {
            return Err(invalid(
                field("id"),
                format!(
                    "`{}`: use 1-32 of a-z, 0-9, `-`, `_`, starting with a letter or digit",
                    raw.id
                ),
            ));
        }
        if out.iter().any(|o| o.id.as_str() == raw.id) {
            return Err(invalid(field("id"), format!("`{}` is used twice", raw.id)));
        }
        let binary = match raw.binary.filter(|b| !b.is_empty()) {
            None => PathBuf::from(raw.driver.default_binary()),
            Some(b) if !b.contains('/') => PathBuf::from(b),
            Some(b) => expand_path(&b, home).map_err(|source| ConfigError::Path {
                field: field("binary"),
                source,
            })?,
        };
        let config_home = match raw.config_home.filter(|c| !c.is_empty()) {
            None => None,
            Some(c) => Some(expand_path(&c, home).map_err(|source| ConfigError::Path {
                field: field("config_home"),
                source,
            })?),
        };
        let mut env = BTreeMap::new();
        for (key, value) in raw.env {
            let value = match value {
                RawEnvValue::Plain(text) => text,
                RawEnvValue::Reference(r) => refs::read_reference(store, &r.secret)
                    .map_err(|m| invalid(field("env"), format!("`{key}`: {m}")))?,
            };
            if !valid_env_key(&key) || value.contains('\0') {
                return Err(invalid(
                    field("env"),
                    format!("`{key}` is not a valid variable"),
                ));
            }
            env.insert(key, Secret::new(value));
        }
        out.push(ProviderInstance {
            id: ProviderInstanceId::new(raw.id),
            driver: raw.driver,
            binary,
            config_home,
            env,
        });
    }
    Ok(out)
}
