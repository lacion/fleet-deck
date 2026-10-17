//! Owner-edited config files: `station.toml` (PLAN §6.7, L10 provider
//! instances) and `deck.toml`.
//!
//! Rules:
//! - Unknown keys are errors (a typo must not silently drop a guardrail).
//! - A missing file means the defaults.
//! - A file that other users can write is refused: it names binaries that the
//!   station runs.
//! - Listen addresses must pass the D13 bind rule (`netscope::check_bind`).
//! - Instance `env` values are [`Secret`]s (they often hold API keys). A
//!   value is a plain string or a reference `{ secret = "<name>" }` to a file
//!   of the secret store (DL-172).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_proto::ProviderInstanceId;
use fleetdeck_proto::event::ProviderKind;
use serde::Deserialize;

use crate::netscope::{self, AddrError};
use crate::paths::{Env, PathError, expand_path};
use crate::secret::Secret;

#[path = "config_model_help.rs"]
mod model_help_cfg;
#[cfg(test)]
pub(crate) use model_help_cfg::RAW_MODEL_HELP_KEYS;
pub use model_help_cfg::{
    DEFAULT_ASSIST_CLAUDE_MODEL, DEFAULT_ASSIST_CODEX_MODEL, DEFAULT_ASSIST_DAILY_TOKEN_CAP,
    DEFAULT_ASSIST_MAX_OUTPUT_TOKENS, ModelHelpConfig,
};
#[path = "config_clone.rs"]
mod clone_cfg;
#[path = "config_deck.rs"]
mod deck_cfg;
#[cfg(test)]
pub(crate) use deck_cfg::RAW_DECK_KEYS;
pub use deck_cfg::{CloneTransport, DeckConfig};
#[path = "config_fleet.rs"]
mod fleet_cfg;
#[path = "config_fleet_git.rs"]
mod fleet_git_cfg;
#[path = "config_tailnet.rs"]
mod tailnet_cfg;
pub(crate) use tailnet_cfg::valid_dns_name;
#[path = "config_owner_file.rs"]
mod owner_file;
#[path = "config_raw.rs"]
mod raw_cfg;
pub(crate) use raw_cfg::RAW_INSTANCE_KEYS;
#[cfg(test)]
pub(crate) use raw_cfg::RAW_STATION_KEYS;
use raw_cfg::{RawEnvValue, RawStation, parse_instances};
#[path = "config_toml.rs"]
mod toml_text;
#[path = "config_trackers.rs"]
mod trackers_cfg;
use owner_file::{has_plain_env, read_owner_file};
use toml_text::toml_error;
use trackers_cfg::parse_trackers;
pub use trackers_cfg::{TokenError, TokenSource, TrackerConfig};

pub const DEFAULT_MAX_SESSIONS: u32 = 12;
pub const DEFAULT_MAX_SHELLS: u32 = 8;
/// The default shim output buffer in MiB (ADR-005 §4.1).
pub const DEFAULT_SHIM_BUFFER_MIB: u32 = 16;

/// The host that runs new provider processes (ADR-005 §1 "Selection"). A
/// session keeps the host it started with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostChoice {
    /// Children of the station process (M2); they end with the station.
    #[default]
    Direct,
    /// Durable shims: the provider survives a station restart.
    Shim,
}

impl HostChoice {
    /// The host of a `station.toml` without `host`: `shim` on Linux, where
    /// the shim host has parity with the direct host (shim parity step 5,
    /// DL-199); `direct` elsewhere until the macOS port (step 4).
    #[must_use]
    pub fn for_this_system() -> Self {
        if cfg!(target_os = "linux") {
            Self::Shim
        } else {
            Self::Direct
        }
    }

    fn parse(text: &str) -> Result<Self, &'static str> {
        match text {
            "direct" => Ok(Self::Direct),
            "shim" => Ok(Self::Shim),
            _ => Err("use \"direct\" or \"shim\""),
        }
    }
}
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
    #[error(
        "{path} holds a plain `env` value of a provider instance and has mode {mode:o}: other \
         users can read it. Run `chmod 600 {path}`, or use a secret reference \
         (`{{ secret = \"name\" }}`)."
    )]
    ReadableSecrets { path: PathBuf, mode: u32 },
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

/// `30s`, `5m`, `2h`. Any other text is an error, never a panic (DL-136).
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let bad = || format!("`{text}` is not a duration like 30s, 5m or 2h");
    let (num, factor) = [("s", 1_u64), ("m", 60), ("h", 3600)]
        .into_iter()
        .find_map(|(unit, factor)| text.strip_suffix(unit).map(|num| (num, factor)))
        .ok_or_else(bad)?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = num.parse().map_err(|_| bad())?;
    n.checked_mul(factor)
        .map(Duration::from_secs)
        .ok_or_else(bad)
}

/// The largest `hibernate_after_min`: one week.
pub const MAX_HIBERNATE_AFTER_MIN: u32 = 7 * 24 * 60;

/// The validated `station.toml`.
#[derive(Debug)]
pub struct StationConfig {
    pub name: String,
    pub allowed_roots: Vec<PathBuf>,
    pub max_sessions: u32,
    pub allow_setup_cmd: bool,
    pub allow_shell: bool,
    /// `allow_bypass_permissions`: offer the mode that skips every permission check (Claude `bypassPermissions`, Codex `danger-full-access`).
    pub allow_bypass_permissions: bool,
    /// `allow_fleet_control`: coordinators with a fleet-control grant may act here (ADR-008).
    pub allow_fleet_control: bool,
    pub allow_desktop: bool,
    pub max_shells: u32,
    pub offline_approval_policy: OfflineApprovalPolicy,
    pub listen: Vec<SocketAddr>,
    pub provider_instances: Vec<ProviderInstance>,
    /// `host = "direct" | "shim"` (default `shim` on Linux, `direct`
    /// elsewhere; ADR-005 §1, DL-199).
    pub host: HostChoice,
    /// `shim_buffer_mib`: the shim output buffer, 1–256 MiB.
    pub shim_buffer_mib: u32,
    /// Explicit repo groups for the conflict radar (PLAN §6.6, M3 6-1,
    /// DL-201): repository path → group name, the owner's choice for repos
    /// with several or no remotes.
    pub repo_groups: BTreeMap<PathBuf, String>,
    /// `hibernate_after_min`: minutes of idleness after which the station
    /// stops a session gracefully so that the board can resume it (R6.6,
    /// DL-169). 0 (the default) turns hibernation off.
    pub hibernate_after_min: u32,
    /// The `[[trackers]]` entries: Jira and Linear (R3, DL-164). GitHub and
    /// GitLab need no entry, only their CLIs.
    pub trackers: Vec<TrackerConfig>,
    /// `member_deny`: more patterns that hide files from the members of a
    /// shared session (R11.9, DL-166). The station adds them to its default
    /// deny list; they never remove a default pattern.
    pub member_deny: Vec<String>,
    /// `[model_help]`: small generated text from the owner's own provider
    /// login (R10, DL-167).
    pub model_help: ModelHelpConfig,
    /// `tailscale_bin`: the absolute path of the program, or `None`: look it up.
    pub tailscale_bin: Option<PathBuf>,
    /// `clone_root`: where Fleet Git clones new project homes. `None`: the
    /// first allowed root + `/projects` (FG-5).
    pub clone_root: Option<PathBuf>,
    /// `traits`: the labels that a start can require (FG-6).
    pub traits: Vec<String>,
    /// `allow_check_cmd`: a project may run its check command here (FG-8).
    pub allow_check_cmd: bool,
    /// `allow_local_remotes`: a local bare repository can be a remote (FG-5).
    pub allow_local_remotes: bool,
    /// `[taildrive_mounts]`: `<machine>/<share>` to the mount folder (FG-12).
    pub taildrive_mounts: BTreeMap<String, PathBuf>,
}

impl StationConfig {
    /// Reads `path`; a missing file gives the defaults. A secret reference
    /// is an error here: use [`Self::load_with_store`].
    pub fn load(path: &Path, env: &Env) -> Result<Self, ConfigError> {
        Self::load_inner(path, env, None)
    }

    /// Like [`Self::load`], and resolves the secret references from `store`
    /// (`<station data>/secrets`, DL-172).
    pub fn load_with_store(path: &Path, env: &Env, store: &Path) -> Result<Self, ConfigError> {
        Self::load_inner(path, env, Some(store))
    }

    fn load_inner(path: &Path, env: &Env, store: Option<&Path>) -> Result<Self, ConfigError> {
        match read_owner_file(path)? {
            Some((text, mode)) => {
                // A plain `env` value in a readable file leaks to other users (NV-08).
                if mode & 0o044 != 0 && has_plain_env(&text) {
                    return Err(ConfigError::ReadableSecrets {
                        path: path.to_path_buf(),
                        mode,
                    });
                }
                Self::parse_with_store(&text, env, store).map_err(|e| with_path(e, path))
            }
            None => Self::parse_with_store("", env, store),
        }
    }

    pub fn parse(text: &str, env: &Env) -> Result<Self, ConfigError> {
        Self::parse_with_store(text, env, None)
    }

    /// Like [`Self::parse`], and resolves the secret references from `store`.
    pub fn parse_with_store(
        text: &str,
        env: &Env,
        store: Option<&Path>,
    ) -> Result<Self, ConfigError> {
        let raw: RawStation = toml::from_str(text).map_err(|e| toml_error(text, &e))?;
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
        let host = match raw.host {
            Some(h) => HostChoice::parse(&h).map_err(|m| invalid("host", m))?,
            None => HostChoice::for_this_system(),
        };
        let shim_buffer_mib = raw.shim_buffer_mib.unwrap_or(DEFAULT_SHIM_BUFFER_MIB);
        if !(1..=256).contains(&shim_buffer_mib) {
            return Err(invalid("shim_buffer_mib", "use a value from 1 to 256"));
        }
        let hibernate_after_min = raw.hibernate_after_min.unwrap_or(0);
        if hibernate_after_min > MAX_HIBERNATE_AFTER_MIN {
            return Err(invalid(
                "hibernate_after_min",
                "use 0 (off) or a number of minutes from 1 to 10080",
            ));
        }
        let provider_instances = match raw.provider_instance {
            Some(list) => parse_instances(list, home, store)?,
            None => vec![
                ProviderInstance::default_for(Driver::Claude),
                ProviderInstance::default_for(Driver::Codex),
            ],
        };
        let mut repo_groups = BTreeMap::new();
        for (path, group) in raw.repo_groups.unwrap_or_default() {
            let field = format!("repo_groups.{path}");
            let group = group.trim().to_owned();
            if group.is_empty()
                || group.chars().count() > 200
                || group.chars().any(char::is_control)
            {
                return Err(invalid(
                    &field,
                    "give a group name of 1 to 200 printable characters",
                ));
            }
            let path =
                expand_path(&path, home).map_err(|source| ConfigError::Path { field, source })?;
            repo_groups.insert(path, group);
        }
        let trackers = parse_trackers(raw.trackers.unwrap_or_default(), home)?;
        let member_deny = raw.member_deny.unwrap_or_default();
        if member_deny.len() > MAX_MEMBER_DENY {
            return Err(invalid(
                "member_deny",
                format!("give at most {MAX_MEMBER_DENY} patterns"),
            ));
        }
        for (i, pattern) in member_deny.iter().enumerate() {
            fleetdeck_proto::member_deny::check_pattern(pattern)
                .map_err(|m| invalid(format!("member_deny[{i}]"), m))?;
        }
        let model_help = match raw.model_help {
            Some(raw) => raw.validate()?,
            None => ModelHelpConfig::default(),
        };
        let tailscale_bin = tailnet_cfg::parse_station(raw.tailscale_bin, home)?;
        let fleet_git = fleet_git_cfg::parse(
            fleet_git_cfg::RawFleetGit {
                clone_root: raw.clone_root,
                traits: raw.traits,
                allow_check_cmd: raw.allow_check_cmd,
                allow_local_remotes: raw.allow_local_remotes,
                taildrive_mounts: raw.taildrive_mounts,
            },
            home,
            &allowed_roots,
        )?;
        Ok(Self {
            name,
            allowed_roots,
            max_sessions,
            allow_setup_cmd: raw.allow_setup_cmd.unwrap_or(false),
            allow_shell: raw.allow_shell.unwrap_or(false),
            allow_bypass_permissions: raw.allow_bypass_permissions.unwrap_or(false),
            allow_fleet_control: fleet_cfg::parse(raw.allow_fleet_control),
            allow_desktop: raw.allow_desktop.unwrap_or(false),
            max_shells,
            offline_approval_policy,
            listen,
            provider_instances,
            host,
            shim_buffer_mib,
            repo_groups,
            hibernate_after_min,
            trackers,
            member_deny,
            model_help,
            tailscale_bin,
            clone_root: fleet_git.clone_root,
            traits: fleet_git.traits,
            allow_check_cmd: fleet_git.allow_check_cmd,
            allow_local_remotes: fleet_git.allow_local_remotes,
            taildrive_mounts: fleet_git.taildrive_mounts,
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

/// The most `member_deny` patterns.
const MAX_MEMBER_DENY: usize = 200;

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

pub(crate) fn valid_env_key(key: &str) -> bool {
    let b = key.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
