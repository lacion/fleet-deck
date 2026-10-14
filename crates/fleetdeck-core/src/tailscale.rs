//! The `tailscale` CLI as a display source (tailnet design sections 2 to 4).
//!
//! Rules of this module:
//! - Tailscale is never authentication. Its answers only fill the board:
//!   the tailnet addresses of this machine and the devices of the tailnet.
//! - One child per call: a minimal environment ([`child_env`]), no stdin,
//!   3 s, at most 2 MiB of stdout and 64 KiB of stderr; a child that goes
//!   over a limit is killed and reaped.
//! - The answers are read with two typed schemas ([`Status`], [`WhoIs`]).
//!   Unknown fields are ignored. A duplicate key, a missing required field
//!   or an address outside the Tailscale range is an error, never a guess.
//! - [`plan_use`] computes the listen addresses of "Use tailnet".

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde::de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use crate::childenv::ChildEnv;
use crate::config::valid_dns_name;
use crate::netscope::{self, AddrClass};

/// The longest run of one call.
pub const TIMEOUT: Duration = Duration::from_secs(3);
/// The most stdout bytes of one call.
pub const MAX_STDOUT: usize = 2 * 1024 * 1024;
/// The most stderr bytes of one call.
pub const MAX_STDERR: usize = 64 * 1024;
/// The most stderr bytes that a log line shows.
const LOG_STDERR: usize = 512;
/// The longest reason text of a parse error.
const MAX_REASON: usize = 160;
/// The command-line program of the macOS app.
pub const MACOS_BUNDLED: &str = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
/// The port of the board when the `listen` list has no entry.
pub const DEFAULT_BOARD_PORT: u16 = 7420;
/// The port of the station link when the `link_listen` list has no entry.
pub const DEFAULT_LINK_PORT: u16 = 7421;

/// Why Tailscale gave no answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TailscaleError {
    #[error("not installed")]
    NotInstalled,
    #[error("not running")]
    NotRunning,
    #[error("timeout")]
    Timeout,
    #[error("output too large")]
    TooLarge,
    #[error("parse error: {0}")]
    Parse(String),
    #[error("exit status {0}")]
    Exit(i32),
    #[error("ended by a signal")]
    Signal,
    #[error("cannot run: {0}")]
    Spawn(String),
}

impl TailscaleError {
    /// A short text for the board and the CLI.
    #[must_use]
    pub fn reason(&self) -> String {
        self.to_string()
    }
}

fn parse_error(message: impl ToString) -> TailscaleError {
    let mut text: String = message.to_string().chars().take(MAX_REASON).collect();
    text.retain(|c| !c.is_control());
    TailscaleError::Parse(text)
}

fn io_error(e: &std::io::Error) -> TailscaleError {
    if e.kind() == std::io::ErrorKind::NotFound {
        TailscaleError::NotInstalled
    } else {
        TailscaleError::Spawn(e.kind().to_string())
    }
}

/// One node of the tailnet: this machine or a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub host_name: String,
    /// The MagicDNS name without the trailing dot (may be empty).
    pub dns_name: String,
    /// The tailnet addresses (all inside the Tailscale range).
    pub ips: Vec<IpAddr>,
    pub os: Option<String>,
    /// The login name of the owner of the node (not known for a tagged one).
    pub login: Option<String>,
    pub tags: Vec<String>,
    pub online: bool,
}

/// The answer of `tailscale status --json` when Tailscale runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// This machine (always online).
    pub this: Node,
    pub peers: Vec<Node>,
    pub magic_dns_suffix: Option<String>,
}

/// The answer of `tailscale whois --json <ip>`. It is for display only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhoIs {
    pub name: String,
    pub addresses: Vec<IpAddr>,
    pub tags: Vec<String>,
    pub login: Option<String>,
    /// `Some(true)` when the node is shared in from another tailnet. A
    /// document without the field is `None` (unknown), never `Some(false)`.
    pub shared: Option<bool>,
}

/// The `tailscale` program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailscaleCli {
    program: PathBuf,
}

impl TailscaleCli {
    /// Runs exactly `program`.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// The program of this machine: the setting `tailscale_bin`, else
    /// `tailscale` on `PATH`, else (macOS) the program of the app.
    #[must_use]
    pub fn locate(configured: Option<&Path>) -> Self {
        let bundled = cfg!(target_os = "macos").then(|| Path::new(MACOS_BUNDLED));
        Self::new(select_program(
            configured,
            std::env::var_os("PATH").as_deref(),
            bundled,
        ))
    }

    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// `tailscale status --json`.
    ///
    /// # Errors
    /// [`TailscaleError`]: no program, no answer in time, a bad exit, a
    /// document that is too large or does not fit the schema, or a backend
    /// that is not running.
    pub async fn status(&self) -> Result<Status, TailscaleError> {
        let out = self.run(&["status", "--json"], ambient_env()).await?;
        parse_status(&out)
    }

    /// `tailscale whois --json <ip>`.
    ///
    /// # Errors
    /// As [`Self::status`].
    pub async fn whois(&self, ip: IpAddr) -> Result<WhoIs, TailscaleError> {
        let ip = ip.to_string();
        let out = self.run(&["whois", "--json", &ip], ambient_env()).await?;
        parse_whois(&out)
    }

    /// Runs the program with `args` and `env` and returns its stdout.
    async fn run(&self, args: &[&str], env: ChildEnv) -> Result<Vec<u8>, TailscaleError> {
        let mut cmd = Command::new(&self.program);
        cmd.args(args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (name, value) in env.iter() {
            cmd.env(name, value);
        }
        let mut child = cmd.spawn().map_err(|e| io_error(&e))?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            reap(&mut child).await;
            return Err(TailscaleError::Spawn("no pipes".to_owned()));
        };
        let io = async {
            let (out, err) = tokio::try_join!(
                read_capped(stdout, MAX_STDOUT),
                read_capped(stderr, MAX_STDERR)
            )?;
            let status = child.wait().await.map_err(|e| io_error(&e))?;
            Ok::<_, TailscaleError>((out, err, status))
        };
        let done = tokio::time::timeout(TIMEOUT, io).await;
        let (out, err, status) = match done {
            Ok(Ok(all)) => all,
            Ok(Err(e)) => {
                reap(&mut child).await;
                return Err(e);
            }
            Err(_) => {
                reap(&mut child).await;
                return Err(TailscaleError::Timeout);
            }
        };
        if status.success() {
            return Ok(out);
        }
        let shown = &err[..err.len().min(LOG_STDERR)];
        tracing::debug!(
            program = %self.program.display(),
            stderr = %String::from_utf8_lossy(shown),
            "tailscale exited with an error"
        );
        Err(status
            .code()
            .map_or(TailscaleError::Signal, TailscaleError::Exit))
    }
}

/// Kills the child and waits for it, so that no zombie stays.
async fn reap(child: &mut Child) {
    if let Err(e) = child.start_kill() {
        tracing::debug!(error = %e, "tailscale child kill");
    }
    if tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .is_err()
    {
        // `kill_on_drop` and the runtime reap it later.
        tracing::warn!("a tailscale child did not end after the kill");
    }
}

/// Reads all of `reader`; more than `limit` bytes is an error.
async fn read_capped(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<Vec<u8>, TailscaleError> {
    let mut data = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let n = reader.read(&mut chunk).await.map_err(|e| io_error(&e))?;
        if n == 0 {
            return Ok(data);
        }
        if data.len() + n > limit {
            return Err(TailscaleError::TooLarge);
        }
        data.extend_from_slice(&chunk[..n]);
    }
}

/// The program to run (design section 2): `configured`, else `tailscale` on
/// `path_var`, else `bundled` when that file exists, else the bare name (the
/// run then reports "not installed").
#[must_use]
pub fn select_program(
    configured: Option<&Path>,
    path_var: Option<&OsStr>,
    bundled: Option<&Path>,
) -> PathBuf {
    if let Some(path) = configured {
        return path.to_path_buf();
    }
    let found = path_var.and_then(|paths| {
        std::env::split_paths(paths)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join("tailscale"))
            .find(|p| {
                p.metadata()
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
    });
    if let Some(path) = found {
        return path;
    }
    match bundled {
        Some(path) if path.is_file() => path.to_path_buf(),
        _ => PathBuf::from("tailscale"),
    }
}

/// The environment of a child: `PATH`, `HOME` and `LANG` of `parent`, and
/// `TAILSCALE_BE_CLI=1` on macOS. Nothing else, so no credential passes.
#[must_use]
pub fn child_env(parent: impl IntoIterator<Item = (OsString, OsString)>, macos: bool) -> ChildEnv {
    let mut env = ChildEnv::empty();
    let mut set = |name: OsString, value: OsString| {
        if let Err(e) = env.set(&name, value) {
            tracing::debug!(error = %e, "tailscale child environment");
        }
    };
    for (name, value) in parent {
        if matches!(name.to_str(), Some("PATH" | "HOME" | "LANG")) {
            set(name, value);
        }
    }
    if macos {
        set("TAILSCALE_BE_CLI".into(), "1".into());
    }
    env
}

fn ambient_env() -> ChildEnv {
    child_env(std::env::vars_os(), cfg!(target_os = "macos"))
}

/// True when `ip` is assigned to a local interface: a socket can bind it.
/// An address that Tailscale gives in userspace networking is not.
#[must_use]
pub fn is_assigned_locally(ip: IpAddr) -> bool {
    UdpSocket::bind((ip, 0)).is_ok()
}

/// A JSON value that refuses a duplicate key at any depth (`serde_json`'s own
/// `Value` keeps the last one).
struct Strict(Value);

impl<'de> de::Deserialize<'de> for Strict {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(StrictVisitor).map(Self)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("a number that is not finite"))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate key `{key}`")));
            }
            let Strict(value) = map.next_value()?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}

/// Parses `bytes` as `T`, and refuses a duplicate key anywhere.
fn parse_strict<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, TailscaleError> {
    let Strict(value) = serde_json::from_slice(bytes).map_err(parse_error)?;
    serde_json::from_value(value).map_err(parse_error)
}

#[derive(Deserialize)]
struct RawStatus {
    #[serde(rename = "BackendState")]
    backend_state: String,
    #[serde(rename = "Self")]
    this: Option<RawSelf>,
    #[serde(rename = "Peer")]
    peer: Option<BTreeMap<String, RawPeer>>,
    #[serde(rename = "User")]
    user: Option<BTreeMap<String, RawUser>>,
    #[serde(rename = "CurrentTailnet")]
    current_tailnet: Option<RawTailnet>,
    #[serde(rename = "MagicDNSSuffix")]
    magic_dns_suffix: Option<String>,
}

#[derive(Deserialize)]
struct RawSelf {
    #[serde(rename = "HostName")]
    host_name: Option<String>,
    #[serde(rename = "DNSName")]
    dns_name: String,
    #[serde(rename = "TailscaleIPs")]
    ips: Vec<String>,
    #[serde(rename = "UserID")]
    user_id: Option<Number>,
    #[serde(rename = "Tags")]
    tags: Option<Vec<String>>,
    #[serde(rename = "OS")]
    os: Option<String>,
}

#[derive(Deserialize)]
struct RawPeer {
    #[serde(rename = "HostName")]
    host_name: String,
    #[serde(rename = "DNSName")]
    dns_name: Option<String>,
    #[serde(rename = "TailscaleIPs")]
    ips: Vec<String>,
    #[serde(rename = "UserID")]
    user_id: Option<Number>,
    #[serde(rename = "Tags")]
    tags: Option<Vec<String>>,
    #[serde(rename = "OS")]
    os: Option<String>,
    #[serde(rename = "Online")]
    online: Option<bool>,
}

#[derive(Deserialize)]
struct RawUser {
    #[serde(rename = "LoginName")]
    login_name: Option<String>,
}

#[derive(Deserialize)]
struct RawTailnet {
    #[serde(rename = "MagicDNSSuffix")]
    magic_dns_suffix: Option<String>,
}

/// The tailnet addresses of a node: valid, and inside the Tailscale range.
fn tailnet_ips(texts: &[String]) -> Result<Vec<IpAddr>, TailscaleError> {
    texts
        .iter()
        .map(|text| {
            let ip: IpAddr = text
                .parse()
                .map_err(|_| parse_error("TailscaleIPs has an address that is not valid"))?;
            if netscope::classify(ip) == AddrClass::Tailnet {
                Ok(ip)
            } else {
                Err(parse_error(
                    "TailscaleIPs has an address outside the tailnet",
                ))
            }
        })
        .collect()
}

fn login_of(users: &BTreeMap<String, RawUser>, user_id: Option<&Number>) -> Option<String> {
    user_id
        .and_then(|id| users.get(&id.to_string()))
        .and_then(|u| u.login_name.clone())
        .filter(|l| !l.is_empty())
}

fn dns_name(raw: Option<String>) -> String {
    raw.unwrap_or_default().trim_end_matches('.').to_owned()
}

/// Parses the document of `tailscale status --json`.
///
/// # Errors
/// [`TailscaleError::NotRunning`] unless `BackendState` is `Running`;
/// [`TailscaleError::Parse`] for a duplicate key, a missing required field or
/// an address outside the Tailscale range.
pub fn parse_status(bytes: &[u8]) -> Result<Status, TailscaleError> {
    let raw: RawStatus = parse_strict(bytes)?;
    if raw.backend_state != "Running" {
        return Err(TailscaleError::NotRunning);
    }
    let this = raw
        .this
        .ok_or_else(|| parse_error("missing field `Self`"))?;
    let users = raw.user.unwrap_or_default();
    let ips = tailnet_ips(&this.ips)?;
    if ips.is_empty() {
        return Err(parse_error("Self has no TailscaleIPs"));
    }
    let me = Node {
        host_name: this.host_name.unwrap_or_default(),
        dns_name: dns_name(Some(this.dns_name)),
        ips,
        os: this.os,
        login: login_of(&users, this.user_id.as_ref()),
        tags: this.tags.unwrap_or_default(),
        online: true,
    };
    let mut peers = Vec::new();
    for peer in raw.peer.unwrap_or_default().into_values() {
        peers.push(Node {
            host_name: peer.host_name,
            dns_name: dns_name(peer.dns_name),
            ips: tailnet_ips(&peer.ips)?,
            os: peer.os,
            login: login_of(&users, peer.user_id.as_ref()),
            tags: peer.tags.unwrap_or_default(),
            online: peer.online.unwrap_or(false),
        });
    }
    let magic_dns_suffix = raw
        .current_tailnet
        .and_then(|t| t.magic_dns_suffix)
        .or(raw.magic_dns_suffix)
        .filter(|s| !s.is_empty());
    Ok(Status {
        this: me,
        peers,
        magic_dns_suffix,
    })
}

#[derive(Deserialize)]
struct RawWhoIs {
    #[serde(rename = "Node")]
    node: RawWhoNode,
    #[serde(rename = "UserProfile")]
    profile: Option<RawProfile>,
}

#[derive(Deserialize)]
struct RawWhoNode {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Addresses")]
    addresses: Option<Vec<String>>,
    #[serde(rename = "Tags")]
    tags: Option<Vec<String>>,
    #[serde(rename = "Sharer")]
    sharer: Option<Value>,
}

#[derive(Deserialize)]
struct RawProfile {
    #[serde(rename = "LoginName")]
    login_name: Option<String>,
}

/// Parses the document of `tailscale whois --json <ip>`. An address may have
/// a prefix length (`100.64.0.1/32`).
///
/// # Errors
/// [`TailscaleError::Parse`] for a duplicate key, a missing `Node.Name` or an
/// address that is not valid.
pub fn parse_whois(bytes: &[u8]) -> Result<WhoIs, TailscaleError> {
    let raw: RawWhoIs = parse_strict(bytes)?;
    let addresses = raw
        .node
        .addresses
        .unwrap_or_default()
        .iter()
        .map(|a| {
            a.split('/')
                .next()
                .unwrap_or_default()
                .parse()
                .map_err(|_| parse_error("Addresses has an address that is not valid"))
        })
        .collect::<Result<Vec<IpAddr>, _>>()?;
    Ok(WhoIs {
        name: raw.node.name.trim_end_matches('.').to_owned(),
        addresses,
        tags: raw.node.tags.unwrap_or_default(),
        login: raw
            .profile
            .and_then(|p| p.login_name)
            .filter(|l| !l.is_empty()),
        shared: raw.node.sharer.filter(|s| !s.is_null()).map(|_| true),
    })
}

/// What "Use tailnet" and "Update" save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub listen: Vec<SocketAddr>,
    pub link_listen: Vec<SocketAddr>,
    /// The new `tailnet_hostname`; `None` leaves the setting as it is.
    pub tailnet_hostname: Option<String>,
    /// The tailnet addresses of this machine that a socket can bind.
    pub usable_ips: Vec<IpAddr>,
    /// No tailnet address of this machine is assigned to a local interface:
    /// Tailscale runs in userspace networking.
    pub userspace: bool,
}

/// The listen lists after "Use tailnet" (design section 3). In each list
/// every entry in the Tailscale range goes, and `<ip>:<port>` of each usable
/// address of `status` is added at the end (an IPv6 address is bracketed when
/// the entry is shown). The port is the port of the first entry of the list,
/// else the default of the list. Every other entry stays. A name that is not
/// a valid DNS name or is empty leaves `tailnet_hostname` as it is.
#[must_use]
pub fn plan_use(
    status: &Status,
    current_listen: &[SocketAddr],
    current_link_listen: &[SocketAddr],
    is_usable: impl Fn(IpAddr) -> bool,
) -> Plan {
    let usable_ips: Vec<IpAddr> = status
        .this
        .ips
        .iter()
        .copied()
        .filter(|ip| is_usable(*ip))
        .collect();
    let name = &status.this.dns_name;
    Plan {
        listen: replace_tailnet(current_listen, &usable_ips, DEFAULT_BOARD_PORT),
        link_listen: replace_tailnet(current_link_listen, &usable_ips, DEFAULT_LINK_PORT),
        tailnet_hostname: (!name.is_empty() && valid_dns_name(name))
            .then(|| name.to_ascii_lowercase()),
        userspace: usable_ips.is_empty(),
        usable_ips,
    }
}

fn replace_tailnet(current: &[SocketAddr], ips: &[IpAddr], default_port: u16) -> Vec<SocketAddr> {
    let port = current.first().map_or(default_port, SocketAddr::port);
    let mut out: Vec<SocketAddr> = Vec::new();
    let kept = current
        .iter()
        .copied()
        .filter(|a| netscope::classify(a.ip()) != AddrClass::Tailnet);
    for addr in kept.chain(ips.iter().map(|ip| SocketAddr::new(*ip, port))) {
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    out
}

#[cfg(test)]
#[path = "tailscale_tests.rs"]
mod tests;
