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

use std::ffi::{OsStr, OsString};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use crate::childenv::ChildEnv;
use crate::config::valid_dns_name;
use crate::local_addrs;
use crate::netscope::{self, AddrClass};

#[path = "tailscale_cert.rs"]
mod tailscale_cert;
pub use tailscale_cert::CertError;
#[path = "tailscale_parse.rs"]
mod tailscale_parse;
pub use tailscale_parse::{parse_status, parse_whois};

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

/// How the program of a [`TailscaleCli`] was chosen (design section 15.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramKind {
    /// The setting `tailscale_bin`, or a program given to [`TailscaleCli::new`].
    Configured,
    /// `tailscale` on `PATH` (also the bare name when nothing is found).
    Path,
    /// The program of the macOS app.
    MacosApp,
    /// `tailscale.exe` of Windows, under WSL, after the checks of
    /// [`crate::tailscale_wsl::windows_program_ok`].
    Windows,
}

impl ProgramKind {
    /// The name on the wire (`GET /api/tailnet`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Path => "path",
            Self::MacosApp => "macos_app",
            Self::Windows => "windows",
        }
    }
}

/// The `tailscale` program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailscaleCli {
    program: PathBuf,
    kind: ProgramKind,
}

impl TailscaleCli {
    /// Runs exactly `program`.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            kind: ProgramKind::Configured,
        }
    }

    /// The program of this machine (design section 15.1): the setting
    /// `tailscale_bin`, else `tailscale` on `PATH`, else (macOS) the program
    /// of the app, else (WSL only) the Windows program when its checks pass.
    /// The checks read files: call it off the async threads.
    #[must_use]
    pub fn locate(configured: Option<&Path>) -> Self {
        let bundled = cfg!(target_os = "macos").then(|| Path::new(MACOS_BUNDLED));
        Self::locate_with(
            configured,
            std::env::var_os("PATH").as_deref(),
            bundled,
            crate::tailscale_wsl::windows_program_ok,
        )
    }

    /// [`Self::locate`] with its inputs: `windows` gives the Windows
    /// program when WSL and every check of it pass (a test gives its own).
    /// When nothing is found the bare name `tailscale` is the program, so
    /// that a run reports "not installed".
    #[must_use]
    pub fn locate_with(
        configured: Option<&Path>,
        path_var: Option<&OsStr>,
        bundled: Option<&Path>,
        windows: impl FnOnce() -> Option<PathBuf>,
    ) -> Self {
        let (program, kind) = if let Some(path) = configured {
            (path.to_path_buf(), ProgramKind::Configured)
        } else if let Some(found) = on_path(path_var) {
            (found, ProgramKind::Path)
        } else if let Some(app) = bundled.filter(|p| p.is_file()) {
            (app.to_path_buf(), ProgramKind::MacosApp)
        } else if let Some(exe) = windows() {
            (exe, ProgramKind::Windows)
        } else {
            (PathBuf::from("tailscale"), ProgramKind::Path)
        };
        Self { program, kind }
    }

    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// How the program was chosen.
    #[must_use]
    pub fn kind(&self) -> ProgramKind {
        self.kind
    }

    /// `tailscale status --json`.
    ///
    /// # Errors
    /// [`TailscaleError`]: no program, no answer in time, a bad exit, a
    /// document that is too large or does not fit the schema, or a backend
    /// that is not running.
    pub async fn status(&self) -> Result<Status, TailscaleError> {
        self.status_until(Instant::now() + TIMEOUT).await
    }

    /// [`Self::status`] that ends at `deadline` (at most [`TIMEOUT`] from the
    /// call): the child is killed and reaped then, and the answer is
    /// [`TailscaleError::Timeout`].
    ///
    /// # Errors
    /// As [`Self::status`].
    pub async fn status_until(&self, deadline: Instant) -> Result<Status, TailscaleError> {
        let deadline = deadline.min(Instant::now() + TIMEOUT);
        let out = self
            .run(&["status", "--json"], ambient_env(), deadline)
            .await?;
        parse_status(&out)
    }

    /// `tailscale whois --json <ip>`.
    ///
    /// # Errors
    /// As [`Self::status`].
    pub async fn whois(&self, ip: IpAddr) -> Result<WhoIs, TailscaleError> {
        let ip = ip.to_string();
        let out = self
            .run(
                &["whois", "--json", &ip],
                ambient_env(),
                Instant::now() + TIMEOUT,
            )
            .await?;
        parse_whois(&out)
    }

    /// Runs the program with `args` and `env` and returns its stdout. The
    /// run ends at `deadline`.
    pub(crate) async fn run(
        &self,
        args: &[&str],
        env: ChildEnv,
        deadline: Instant,
    ) -> Result<Vec<u8>, TailscaleError> {
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
        let left = deadline.saturating_duration_since(Instant::now());
        let done = tokio::time::timeout(left, io).await;
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
    find_program(path_var, bundled).unwrap_or_else(|| PathBuf::from("tailscale"))
}

/// The program that the lookup finds: `tailscale` on `path_var`, else
/// `bundled` when that file exists.
#[must_use]
pub fn find_program(path_var: Option<&OsStr>, bundled: Option<&Path>) -> Option<PathBuf> {
    on_path(path_var).or_else(|| bundled.filter(|p| p.is_file()).map(Path::to_path_buf))
}

/// The first executable `tailscale` in an absolute directory of `path_var`.
fn on_path(path_var: Option<&OsStr>) -> Option<PathBuf> {
    path_var.and_then(|paths| {
        std::env::split_paths(paths)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join("tailscale"))
            .find(|p| {
                p.metadata()
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
    })
}

/// True when the lookup of this machine finds a program without a setting:
/// `tailscale` on `PATH`, or the macOS app. (The Windows program of WSL is
/// not counted: see [`TailscaleCli::locate`].)
#[must_use]
pub fn is_installed() -> bool {
    let bundled = cfg!(target_os = "macos").then(|| Path::new(MACOS_BUNDLED));
    find_program(std::env::var_os("PATH").as_deref(), bundled).is_some()
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

/// True when `ip` is assigned to an interface of this machine (design
/// section 14.3): it is in the kernel's local table of its address family.
/// On Linux a table that cannot be read means "not usable" (fail closed). On
/// another platform the answer is the bind check: a socket can bind it. An
/// address that Tailscale gives in userspace networking is not local.
#[must_use]
pub fn is_usable(ip: IpAddr) -> bool {
    if cfg!(target_os = "linux") {
        local_addrs::local_addresses().contains(ip)
    } else {
        UdpSocket::bind((ip, 0)).is_ok()
    }
}

/// The same as [`is_usable`] (the name that the CLI uses).
#[must_use]
pub fn is_assigned_locally(ip: IpAddr) -> bool {
    is_usable(ip)
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
