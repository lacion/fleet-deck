//! Owner-edited config files: `station.toml` (PLAN §6.7, L10 provider
//! instances) and `deck.toml`.
//!
//! Rules:
//! - Unknown keys are errors (a typo must not silently drop a guardrail).
//! - A missing file means the defaults.
//! - A file that other users can write is refused: it names binaries that the
//!   station runs.
//! - Listen addresses must pass the D13 bind rule (`netscope::check_bind`).
//! - Instance `env` values are [`Secret`]s (they often hold API keys).

use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_proto::ProviderInstanceId;
use fleetdeck_proto::event::ProviderKind;
use serde::Deserialize;

use crate::netscope::{self, AddrError};
use crate::paths::{Env, PathError, expand_path};
use crate::secret::Secret;

pub const DEFAULT_MAX_SESSIONS: u32 = 12;
pub const DEFAULT_MAX_SHELLS: u32 = 8;
pub const DEFAULT_BOARD_LISTEN: &str = "127.0.0.1:7420";
pub const DEFAULT_LINK_LISTEN: &str = "127.0.0.1:7421";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is writable by other users (mode {mode:o}); use mode 600 or 644")]
    Mode { path: PathBuf, mode: u32 },
    #[error("{path}: {message}")]
    Toml { path: PathBuf, message: String },
    #[error("`{field}`: {message}")]
    Invalid { field: String, message: String },
    #[error("`{field}`: {source}")]
    Path { field: String, source: PathError },
    #[error("`{field}`: {source}")]
    Addr { field: String, source: AddrError },
}

fn invalid(field: impl Into<String>, message: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        field: field.into(),
        message: message.into(),
    }
}

/// The provider drivers of v2 (D14: Pi later).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Driver {
    Claude,
    Codex,
}

impl Driver {
    pub fn kind(self) -> ProviderKind {
        match self {
            Self::Claude => ProviderKind::Claude,
            Self::Codex => ProviderKind::Codex,
        }
    }

    /// The binary name on `PATH` when the instance names none.
    pub fn default_binary(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// The variable that points the CLI at the instance's config home.
    pub fn config_home_var(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE_CONFIG_DIR",
            Self::Codex => "CODEX_HOME",
        }
    }
}

/// One provider instance (L10). Work is routed by `id`, never by driver.
#[derive(Debug)]
pub struct ProviderInstance {
    pub id: ProviderInstanceId,
    pub driver: Driver,
    /// A bare name is looked up on `PATH` at launch; a path is absolute.
    pub binary: PathBuf,
    pub config_home: Option<PathBuf>,
    pub env: BTreeMap<String, Secret<String>>,
}

impl ProviderInstance {
    pub fn default_for(driver: Driver) -> Self {
        Self {
            id: ProviderInstanceId::new(driver.default_binary()),
            driver,
            binary: PathBuf::from(driver.default_binary()),
            config_home: None,
            env: BTreeMap::new(),
        }
    }
}

/// What the station does with an approval while no deck is connected (§6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfflineApprovalPolicy {
    Wait,
    DenyAfter(Duration),
}

impl OfflineApprovalPolicy {
    /// `wait` or `deny_after(<n>s|m|h)`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text == "wait" {
            return Ok(Self::Wait);
        }
        let inner = text
            .strip_prefix("deny_after(")
            .and_then(|t| t.strip_suffix(')'))
            .ok_or_else(|| format!("`{text}` is not `wait` or `deny_after(<duration>)`"))?;
        let duration = parse_duration(inner.trim())?;
        if duration.is_zero() {
            return Err("deny_after needs a duration above zero".into());
        }
        Ok(Self::DenyAfter(duration))
    }
}

/// `30s`, `5m`, `2h`.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let bad = || format!("`{text}` is not a duration like 30s, 5m or 2h");
    let split = text.len().checked_sub(1).ok_or_else(bad)?;
    let (num, unit) = text.split_at(split);
    let n: u64 = num.parse().map_err(|_| bad())?;
    let secs = match unit {
        "s" => Some(n),
        "m" => n.checked_mul(60),
        "h" => n.checked_mul(3600),
        _ => None,
    };
    secs.map(Duration::from_secs).ok_or_else(bad)
}

/// The validated `station.toml`.
#[derive(Debug)]
pub struct StationConfig {
    pub name: String,
    pub allowed_roots: Vec<PathBuf>,
    pub max_sessions: u32,
    pub allow_setup_cmd: bool,
    pub allow_shell: bool,
    pub max_shells: u32,
    pub offline_approval_policy: OfflineApprovalPolicy,
    pub listen: Vec<SocketAddr>,
    pub provider_instances: Vec<ProviderInstance>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStation {
    name: Option<String>,
    allowed_roots: Option<Vec<String>>,
    max_sessions: Option<u32>,
    allow_setup_cmd: Option<bool>,
    allow_shell: Option<bool>,
    max_shells: Option<u32>,
    offline_approval_policy: Option<String>,
    listen: Option<Vec<String>>,
    provider_instance: Option<Vec<RawInstance>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInstance {
    id: String,
    driver: Driver,
    binary: Option<String>,
    config_home: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

impl StationConfig {
    /// Reads `path`; a missing file gives the defaults.
    pub fn load(path: &Path, env: &Env) -> Result<Self, ConfigError> {
        match read_owner_file(path)? {
            Some(text) => Self::parse(&text, env).map_err(|e| with_path(e, path)),
            None => Self::parse("", env),
        }
    }

    pub fn parse(text: &str, env: &Env) -> Result<Self, ConfigError> {
        let raw: RawStation = toml::from_str(text).map_err(|e| ConfigError::Toml {
            path: PathBuf::new(),
            message: e.to_string(),
        })?;
        let home = env.home.as_deref();
        let name = match raw.name {
            Some(n) => check_name("name", n)?,
            None => default_node_name("station"),
        };
        let roots = raw.allowed_roots.unwrap_or_else(|| vec!["~".to_owned()]);
        if roots.is_empty() {
            return Err(invalid("allowed_roots", "give at least one directory"));
        }
        let allowed_roots = roots
            .iter()
            .enumerate()
            .map(|(i, r)| {
                expand_path(r, home).map_err(|source| ConfigError::Path {
                    field: format!("allowed_roots[{i}]"),
                    source,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let max_sessions = raw.max_sessions.unwrap_or(DEFAULT_MAX_SESSIONS);
        if !(1..=256).contains(&max_sessions) {
            return Err(invalid("max_sessions", "use a value from 1 to 256"));
        }
        let max_shells = raw.max_shells.unwrap_or(DEFAULT_MAX_SHELLS);
        if max_shells > 64 {
            return Err(invalid("max_shells", "use a value from 0 to 64"));
        }
        let offline_approval_policy = match raw.offline_approval_policy {
            Some(p) => OfflineApprovalPolicy::parse(&p)
                .map_err(|m| invalid("offline_approval_policy", m))?,
            None => OfflineApprovalPolicy::Wait,
        };
        let listen = parse_listen("listen", raw.listen.unwrap_or_default())?;
        let provider_instances = match raw.provider_instance {
            Some(list) => parse_instances(list, home)?,
            None => vec![
                ProviderInstance::default_for(Driver::Claude),
                ProviderInstance::default_for(Driver::Codex),
            ],
        };
        Ok(Self {
            name,
            allowed_roots,
            max_sessions,
            allow_setup_cmd: raw.allow_setup_cmd.unwrap_or(false),
            allow_shell: raw.allow_shell.unwrap_or(true),
            max_shells,
            offline_approval_policy,
            listen,
            provider_instances,
        })
    }

    pub fn instance(&self, id: &ProviderInstanceId) -> Option<&ProviderInstance> {
        self.provider_instances.iter().find(|i| &i.id == id)
    }

    /// True when `path` (absolute, without `..`) is inside an allowed root.
    /// Symlinks are resolved by the caller (t3code trap 8).
    pub fn is_allowed(&self, path: &Path) -> bool {
        self.allowed_roots.iter().any(|root| path.starts_with(root))
    }
}

fn parse_instances(
    list: Vec<RawInstance>,
    home: Option<&Path>,
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

/// The validated `deck.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeckConfig {
    pub name: String,
    /// Board HTTP/WS listen addresses (login is always required, A13).
    pub listen: Vec<SocketAddr>,
    /// Link listener for stations that dial the deck (§8.5). Empty: the deck
    /// only dials.
    pub link_listen: Vec<SocketAddr>,
    /// The deck's tailnet host name, for the WS `Origin` check (§9).
    pub tailnet_hostname: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeck {
    name: Option<String>,
    listen: Option<Vec<String>>,
    link_listen: Option<Vec<String>>,
    tailnet_hostname: Option<String>,
}

impl DeckConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match read_owner_file(path)? {
            Some(text) => Self::parse(&text).map_err(|e| with_path(e, path)),
            None => Self::parse(""),
        }
    }

    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let raw: RawDeck = toml::from_str(text).map_err(|e| ConfigError::Toml {
            path: PathBuf::new(),
            message: e.to_string(),
        })?;
        let name = match raw.name {
            Some(n) => check_name("name", n)?,
            None => "deck".to_owned(),
        };
        let listen = parse_listen(
            "listen",
            raw.listen
                .unwrap_or_else(|| vec![DEFAULT_BOARD_LISTEN.to_owned()]),
        )?;
        if listen.is_empty() {
            return Err(invalid("listen", "the board needs one listen address"));
        }
        let link_listen = parse_listen(
            "link_listen",
            raw.link_listen
                .unwrap_or_else(|| vec![DEFAULT_LINK_LISTEN.to_owned()]),
        )?;
        let tailnet_hostname = match raw.tailnet_hostname.filter(|h| !h.is_empty()) {
            None => None,
            Some(h) if valid_dns_name(&h) => Some(h.to_ascii_lowercase()),
            Some(h) => {
                return Err(invalid(
                    "tailnet_hostname",
                    format!("`{h}` is not a DNS name"),
                ));
            }
        };
        Ok(Self {
            name,
            listen,
            link_listen,
            tailnet_hostname,
        })
    }
}

fn read_owner_file(path: &Path) -> Result<Option<String>, ConfigError> {
    let io = |source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    };
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o022 != 0 {
        return Err(ConfigError::Mode {
            path: path.to_path_buf(),
            mode,
        });
    }
    fs::read_to_string(path).map(Some).map_err(io)
}

fn with_path(err: ConfigError, path: &Path) -> ConfigError {
    match err {
        ConfigError::Toml { message, .. } => ConfigError::Toml {
            path: path.to_path_buf(),
            message,
        },
        other => other,
    }
}

fn parse_listen(field: &str, list: Vec<String>) -> Result<Vec<SocketAddr>, ConfigError> {
    let mut out = Vec::with_capacity(list.len());
    for (i, text) in list.iter().enumerate() {
        let field = format!("{field}[{i}]");
        let addr: SocketAddr = text.parse().map_err(|_| {
            invalid(
                field.clone(),
                format!("`{text}` is not an IP address with a port (for example 192.168.1.5:7421)"),
            )
        })?;
        netscope::check_bind(addr).map_err(|source| ConfigError::Addr { field, source })?;
        out.push(addr);
    }
    Ok(out)
}

fn check_name(field: &str, name: String) -> Result<String, ConfigError> {
    let ok = (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if ok {
        Ok(name)
    } else {
        Err(invalid(
            field,
            format!("`{name}`: use 1-63 of A-Z, a-z, 0-9, `-`, `_`, `.`"),
        ))
    }
}

/// The short host name, cleaned to the name rule, or `fallback`.
pub fn default_node_name(fallback: &str) -> String {
    let uname = rustix::system::uname();
    let host = uname.nodename().to_string_lossy();
    let short = host.split('.').next().unwrap_or_default();
    let clean: String = short
        .chars()
        .take(63)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if clean.is_empty() {
        fallback.to_owned()
    } else {
        clean
    }
}

fn valid_instance_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=32).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
}

fn valid_env_key(key: &str) -> bool {
    let b = key.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

fn valid_dns_name(name: &str) -> bool {
    name.len() <= 253
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
