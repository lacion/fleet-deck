//! The deck setting types and the parser of `deck.toml`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config_grant_limits::{GrantLimits, RawGrantLimits};
use crate::config_worker_grant::{RawWorkerGrant, WorkerGrantConfig};

use super::clone_cfg::{valid_clone_host, valid_clone_org};
use super::knobs_deck_cfg::{
    DeckLimits, DeckRetention, DeckSecurity, RawDeckLimits, RawDeckRetention, RawSecurity,
};
use super::toml_text::toml_error;
use super::{
    ConfigError, DEFAULT_BOARD_LISTEN, DEFAULT_LINK_LISTEN, check_name, invalid, parse_listen,
    read_owner_file, with_path,
};

/// How a clone reaches the Git host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneTransport {
    /// `https://host/path.git`, with the Git credential helper.
    Https,
    /// `git@host:path.git`, with the SSH keys.
    Ssh,
}

impl CloneTransport {
    /// The text form, as in `deck.toml`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Https => "https",
            Self::Ssh => "ssh",
        }
    }
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
    /// Tell both sessions once when two sessions change the same file
    /// (DL-145). On by default.
    pub conflict_mail: bool,
    /// Fleet control (ADR-008 section 3): off by default; off stops it all.
    pub fleet_control: bool,
    /// Desktop control (R13 §6.4): off by default; off stops every desktop action
    /// and wake that has not started.
    pub desktop_control: bool,
    /// `tailscale_cert`: a trusted board certificate from Tailscale (DL-190).
    pub tailscale_cert: bool,
    /// The Git host of `owner/name` (lower case, `github.com` by default).
    pub clone_host: String,
    /// The owner or group of a repository given by name only (empty by default).
    pub clone_org: String,
    /// How the board clones a repository that it gets as a short name.
    pub clone_transport: CloneTransport,
    /// `tailscale_bin`: the absolute path of the program, or `None`: look it up.
    pub tailscale_bin: Option<PathBuf>,
    pub tailnet_auto: bool,
    pub tailnet_auto_listen: Vec<SocketAddr>,
    pub tailnet_auto_link_listen: Vec<SocketAddr>,
    /// `[security]`: sign-in lifetimes, password length and desktop rates (knobs K1 and K2).
    pub security: DeckSecurity,
    /// `[retention]`: how long the deck keeps its audit rows.
    pub retention: DeckRetention,
    /// `[limits]`: the delivery time of mail and tasks, the radar interval.
    pub limits: DeckLimits,
    /// `[worker_grant]`: the default grant of an eligible session start (MCP P4a).
    pub worker_grant: WorkerGrantConfig,
    /// `[grant_limits]`: the ceilings of every grant; "none" turns one off (B27).
    pub grant_limits: GrantLimits,
}

/// The keys of `deck.toml` (`RawDeck`).
#[cfg(test)]
pub(crate) const RAW_DECK_KEYS: &[&str] = &[
    "name",
    "listen",
    "link_listen",
    "tailnet_hostname",
    "conflict_mail",
    "fleet_control",
    "desktop_control",
    "tailscale_cert",
    "clone_host",
    "clone_org",
    "clone_transport",
    "tailscale_bin",
    "tailnet_auto",
    "tailnet_auto_listen",
    "tailnet_auto_link_listen",
    "security",
    "retention",
    "limits",
    "worker_grant",
    "grant_limits",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeck {
    name: Option<String>,
    listen: Option<Vec<String>>,
    link_listen: Option<Vec<String>>,
    tailnet_hostname: Option<String>,
    conflict_mail: Option<bool>,
    fleet_control: Option<bool>,
    desktop_control: Option<bool>,
    tailscale_cert: Option<bool>,
    clone_host: Option<String>,
    clone_org: Option<String>,
    clone_transport: Option<String>,
    tailscale_bin: Option<String>,
    tailnet_auto: Option<bool>,
    tailnet_auto_listen: Option<Vec<String>>,
    tailnet_auto_link_listen: Option<Vec<String>>,
    security: Option<RawSecurity>,
    retention: Option<RawDeckRetention>,
    limits: Option<RawDeckLimits>,
    worker_grant: Option<RawWorkerGrant>,
    grant_limits: Option<RawGrantLimits>,
}

impl DeckConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match read_owner_file(path)? {
            Some((text, _)) => Self::parse(&text).map_err(|e| with_path(e, path)),
            None => Self::parse(""),
        }
    }

    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let raw: RawDeck = toml::from_str(text).map_err(|e| toml_error(text, &e))?;
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
        let tailnet_hostname = super::tailnet_cfg::parse_hostname(raw.tailnet_hostname)?;
        let clone_host = raw.clone_host.map(|h| h.to_ascii_lowercase());
        if let Some(h) = clone_host.as_deref().filter(|h| !valid_clone_host(h)) {
            return Err(invalid(
                "clone_host",
                format!("`{h}` is not a Git host name"),
            ));
        }
        let clone_org = raw.clone_org.unwrap_or_default();
        if !clone_org.is_empty() && !valid_clone_org(&clone_org) {
            return Err(invalid("clone_org", format!("`{clone_org}` is not a name")));
        }
        let clone_transport = match raw.clone_transport.as_deref() {
            None | Some("https") => CloneTransport::Https,
            Some("ssh") => CloneTransport::Ssh,
            Some(_) => return Err(invalid("clone_transport", "use \"https\" or \"ssh\"")),
        };
        let tailscale_bin = super::tailnet_cfg::parse_deck(raw.tailscale_bin)?;
        let (tailnet_auto_listen, tailnet_auto_link_listen) = super::tailnet_cfg::parse_provenance(
            raw.tailnet_auto_listen,
            raw.tailnet_auto_link_listen,
        )?;
        let security = match raw.security {
            Some(raw) => raw.validate()?,
            None => DeckSecurity::default(),
        };
        let retention = match raw.retention {
            Some(raw) => raw.validate()?,
            None => DeckRetention::default(),
        };
        let limits = match raw.limits {
            Some(raw) => raw.validate()?,
            None => DeckLimits::default(),
        };
        let grant_limits = match raw.grant_limits {
            Some(raw) => raw.validate()?,
            None => GrantLimits::default(),
        };
        let worker_grant = match raw.worker_grant {
            Some(raw) => raw.validate(&grant_limits)?,
            None => WorkerGrantConfig::default(),
        };
        Ok(Self {
            name,
            listen,
            link_listen,
            tailnet_hostname,
            conflict_mail: raw.conflict_mail.unwrap_or(true),
            fleet_control: raw.fleet_control.unwrap_or(false),
            desktop_control: raw.desktop_control.unwrap_or(false),
            tailscale_cert: raw.tailscale_cert.unwrap_or(false),
            clone_host: clone_host.unwrap_or_else(|| "github.com".to_owned()),
            clone_org,
            clone_transport,
            tailscale_bin,
            tailnet_auto: raw.tailnet_auto.unwrap_or(true),
            tailnet_auto_listen,
            tailnet_auto_link_listen,
            security,
            retention,
            limits,
            worker_grant,
            grant_limits,
        })
    }
}
