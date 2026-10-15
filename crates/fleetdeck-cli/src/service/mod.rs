//! `fleetdeck service install|uninstall|start|stop|status` (DL-186, design
//! `docs/v2/design/service.md`): the deck, the station or both as a service
//! of the operating system. systemd user services on Linux and WSL, a
//! Windows logon task under WSL, launchd user agents on macOS.
//!
//! Every program that the commands run goes through [`Deps`]. The tests give
//! a recording runner; they never run `systemctl`, `loginctl`, `launchctl`
//! or PowerShell.

mod commands;
mod launchd;
mod path;
mod program;
pub mod runner;
mod status;
mod systemd;
mod wsl_task;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use fleetdeck_core::paths::{Env, NodeDirs};
use fleetdeck_core::{IdSource as _, SharedClock, SystemClock, UlidSource};

use crate::cli::ServiceCommand;
pub use crate::cli::ServiceKind as Kind;
use fleetdeck_core::config::{HostChoice, StationConfig};
use fleetdeck_core::tailscale_wsl;
use runner::{Output, RunError};

/// The time limit of a read-only query.
pub const QUERY: Duration = Duration::from_secs(5);
/// The time limit of `daemon-reload`, `enable` and `disable` (no `--now`).
pub const RELOAD: Duration = Duration::from_secs(15);
/// The time limit of `restart`, `start`, `stop`, `disable --now`,
/// `bootstrap`, `bootout` and `kickstart`.
pub const WRITE: Duration = Duration::from_secs(60);

/// The init system that runs the services.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Systemd,
    Launchd,
}

/// Every fact of this machine and of the command line that the commands
/// use. The tests build it by hand.
#[derive(Debug, Clone)]
pub struct Context {
    pub platform: Platform,
    pub dirs: NodeDirs,
    /// Pass `--data-dir <dirs.data>` to the service.
    pub data_dir_flag: bool,
    pub home: PathBuf,
    pub uid: u32,
    pub user: String,
    /// The `PATH` of the service: only the safe entries (see
    /// [`path::safe_path`]).
    pub path: Option<String>,
    /// The `PATH` entries that the service leaves out, each once: the entry
    /// and the reason.
    pub path_dropped: Vec<(String, String)>,
    /// The raw `$PATH` of the shell. Only the check of the programs uses it.
    pub shell_path: Option<String>,
    /// The raw `$XDG_CONFIG_HOME` (checked in the plan: it must be absolute).
    pub xdg_config_home: Option<String>,
    pub xdg_data_home: Option<String>,
    pub wsl: bool,
    pub wsl_kernel: bool,
    pub systemd_booted: bool,
    pub distro: Option<String>,
    pub powershell: Option<PathBuf>,
    pub exe: PathBuf,
    pub version: String,
    pub host: Option<HostChoice>,
    pub ready_limit: Duration,
    pub ready_step: Duration,
}

/// A text variable that is set and not empty. A value that is not UTF-8 is
/// an error.
fn text_var(name: &str) -> anyhow::Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("{name} is not valid text. Fix it, then run the command again.")
        }
    }
}

impl Context {
    /// The facts of this machine.
    pub fn detect(dirs: &NodeDirs, env: &Env, data_dir_flag: bool) -> anyhow::Result<Self> {
        let home = env.home.clone().ok_or_else(|| anyhow!("HOME is not set"))?;
        let uid = rustix::process::getuid().as_raw();
        let gid = rustix::process::getgid().as_raw();
        let user = text_var("USER")?.unwrap_or_else(|| uid.to_string());
        let exe = std::env::current_exe()
            .map_err(|e| anyhow!("Cannot find the path of this program ({e})."))?;
        let kernel = std::fs::read_to_string(tailscale_wsl::OSRELEASE).unwrap_or_default();
        let shell_path = text_var("PATH")?;
        let check = shell_path
            .as_deref()
            .map(|raw| path::safe_path(raw, uid, path::read_private_gid(&user, gid)));
        let (path, path_dropped) = check.map_or((None, Vec::new()), |c| (c.kept, c.dropped));
        let host = StationConfig::load(&dirs.station_config(), env)
            .ok()
            .map(|config| config.host);
        Ok(Self {
            platform: if cfg!(target_os = "macos") {
                Platform::Launchd
            } else {
                Platform::Systemd
            },
            dirs: dirs.clone(),
            data_dir_flag,
            home,
            uid,
            user,
            path,
            path_dropped,
            shell_path,
            xdg_config_home: text_var("XDG_CONFIG_HOME")?,
            xdg_data_home: text_var("XDG_DATA_HOME")?,
            wsl: tailscale_wsl::is_wsl(),
            wsl_kernel: kernel.to_ascii_lowercase().contains("microsoft"),
            systemd_booted: Path::new("/run/systemd/system").is_dir(),
            distro: text_var("WSL_DISTRO_NAME")?,
            powershell: tailscale_wsl::windows_powershell_ok(),
            exe,
            version: fleetdeck_core::build::version().to_owned(),
            host,
            ready_limit: Duration::from_secs(20),
            ready_step: Duration::from_millis(200),
        })
    }

    /// The arguments of the service: `[--data-dir <dir>] up|deck serve|...`.
    pub fn exec_args(&self, kind: Kind) -> Vec<String> {
        let mut args = Vec::new();
        if self.data_dir_flag {
            args.push("--data-dir".to_owned());
            args.push(self.dirs.data.to_string_lossy().into_owned());
        }
        args.extend(kind.args().iter().map(|a| (*a).to_owned()));
        args
    }
}

/// The three kinds, in the order of the output.
pub const KINDS: [Kind; 3] = [Kind::Both, Kind::Deck, Kind::Station];

impl Kind {
    /// The word of the output and of the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Deck => "deck",
            Self::Station => "station",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Self::Both => "fleetdeck-up.service",
            Self::Deck => "fleetdeck-deck.service",
            Self::Station => "fleetdeck-station.service",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Both => "dev.fleetdeck.up",
            Self::Deck => "dev.fleetdeck.deck",
            Self::Station => "dev.fleetdeck.station",
        }
    }

    pub fn log_name(self) -> &'static str {
        match self {
            Self::Both => "up",
            Self::Deck => "deck",
            Self::Station => "station",
        }
    }

    pub fn args(self) -> &'static [&'static str] {
        match self {
            Self::Both => &["up"],
            Self::Deck => &["deck", "serve"],
            Self::Station => &["station", "serve"],
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Both => "Fleet Deck deck and local station",
            Self::Deck => "Fleet Deck deck",
            Self::Station => "Fleet Deck station",
        }
    }

    /// The kind runs a station: only the main process gets the stop signal
    /// (`KillMode=process`, `AbandonProcessGroup`).
    pub fn keeps_agents(self) -> bool {
        self != Self::Deck
    }

    /// The launcher root of the program copy.
    pub fn root(self, dirs: &NodeDirs) -> PathBuf {
        match self {
            Self::Deck => dirs.deck().join("bin"),
            Self::Both | Self::Station => dirs.station().join("bin"),
        }
    }

    /// The local sockets that answer when the kind runs.
    pub fn sockets(self, dirs: &NodeDirs) -> Vec<PathBuf> {
        let deck = fleetdeck_deck::local::socket_path(&dirs.deck());
        let station = fleetdeck_station::local::socket_path(&dirs.station());
        match self {
            Self::Both => vec![deck, station],
            Self::Deck => vec![deck],
            Self::Station => vec![station],
        }
    }

    /// The command or the path of the log.
    pub fn logs(self, ctx: &Context) -> String {
        match ctx.platform {
            Platform::Systemd => format!("journalctl --user -u {}", self.unit()),
            Platform::Launchd => launchd::log_path(ctx, self).display().to_string(),
        }
    }

    /// The service file of the kind.
    pub fn file(self, ctx: &Context) -> anyhow::Result<PathBuf> {
        match ctx.platform {
            Platform::Systemd => systemd::file_path(ctx, self),
            Platform::Launchd => Ok(launchd::file_path(ctx, self)),
        }
    }
}

/// What the commands need from the machine.
pub trait Deps {
    /// Runs one program; `stdin` is written then closed; killed at `limit`.
    fn run(
        &self,
        program: &str,
        args: &[String],
        stdin: Option<&str>,
        limit: Duration,
    ) -> Result<Output, RunError>;
    /// True when a process accepts a connection on the Unix socket.
    fn answers(&self, socket: &Path) -> bool;
    fn sleep(&self, d: Duration);
}

/// The real machine.
pub struct System;

impl Deps for System {
    fn run(
        &self,
        program: &str,
        args: &[String],
        stdin: Option<&str>,
        limit: Duration,
    ) -> Result<Output, RunError> {
        runner::run_system(program, args, stdin, limit)
    }

    fn answers(&self, socket: &Path) -> bool {
        std::os::unix::net::UnixStream::connect(socket).is_ok()
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

fn new_ulid() -> String {
    UlidSource::new(Arc::new(SystemClock) as SharedClock)
        .next_ulid()
        .to_string()
        .to_lowercase()
}

/// Runs one `fleetdeck service` command.
pub fn run(
    command: &ServiceCommand,
    ctx: &Context,
    deps: &dyn Deps,
    out: &mut dyn std::io::Write,
) -> anyhow::Result<()> {
    match command {
        ServiceCommand::Install {
            kind,
            dry_run,
            verbose,
        } => commands::install(ctx, deps, *kind, *dry_run, *verbose, &new_ulid(), out),
        ServiceCommand::Uninstall { kind } => commands::uninstall(ctx, deps, *kind, out),
        ServiceCommand::Start { kind } => commands::start(ctx, deps, *kind, &new_ulid(), out),
        ServiceCommand::Stop { kind } => commands::stop(ctx, deps, *kind, out),
        ServiceCommand::Status { json } => status::run(ctx, deps, *json, out),
    }
}

// ---- Commands and their failures (decision 17) ----

/// The failure of a command: ``"`<program> <args>` failed (<detail>)"``,
/// then `: <first line of stderr>` when there is one.
pub fn failure(
    program: &str,
    args: &[String],
    limit: Duration,
    result: &Result<Output, RunError>,
) -> anyhow::Error {
    let (detail, line) = match result {
        Ok(out) => (out.status_text(), out.stderr_line()),
        Err(e) => (e.text(program, limit), None),
    };
    let mut text = format!("`{program} {}` failed ({detail})", args.join(" "));
    if let Some(line) = line {
        text.push_str(": ");
        text.push_str(line);
    }
    anyhow!(text)
}

/// Runs a write command; a failure is an error.
pub fn act(
    deps: &dyn Deps,
    program: &str,
    args: &[String],
    limit: Duration,
) -> anyhow::Result<Output> {
    match deps.run(program, args, None, limit) {
        Ok(out) if out.success() => Ok(out),
        other => Err(failure(program, args, limit, &other)),
    }
}

/// Runs a read-only query (5 s). `None` when the program did not run to its
/// end; the caller reads the status of the output.
pub fn query(deps: &dyn Deps, program: &str, args: &[String]) -> Option<Output> {
    deps.run(program, args, None, QUERY).ok()
}

/// The `--dry-run` line of a command.
pub fn command_line(program: &str, args: &[String]) -> String {
    format!("$ {program} {}", args.join(" "))
}

// ---- Validation (decision 6b, 18) ----

/// `Some(dir)` for a set absolute XDG value, `None` for an unset one.
pub fn absolute_xdg(name: &str, value: Option<&str>) -> anyhow::Result<Option<PathBuf>> {
    match value {
        None | Some("") => Ok(None),
        Some(v) if Path::new(v).is_absolute() => Ok(Some(PathBuf::from(v))),
        Some(v) => bail!(
            "{name} must be an absolute path (it is \"{v}\"). Fix it or unset it, then run \
             the command again."
        ),
    }
}

fn no_control(what: &str, text: &str) -> anyhow::Result<()> {
    if text.chars().any(|c| c.is_control()) {
        bail!("{what} contains a control character. Fix it, then run the command again.");
    }
    Ok(())
}

/// Checks every text of the service file and returns the environment of
/// the service: `PATH`, the absolute XDG values, `FLEETDECK_LOG_FORMAT`.
pub fn validate(ctx: &Context, program: &Path) -> anyhow::Result<Vec<(String, String)>> {
    absolute_xdg("XDG_CONFIG_HOME", ctx.xdg_config_home.as_deref())?;
    absolute_xdg("XDG_DATA_HOME", ctx.xdg_data_home.as_deref())?;
    let set = |value: &Option<String>| value.clone().filter(|v| !v.is_empty());
    let (path, config, data) = (
        set(&ctx.path),
        set(&ctx.xdg_config_home),
        set(&ctx.xdg_data_home),
    );
    for (what, text) in [
        ("PATH", path.as_deref()),
        ("XDG_CONFIG_HOME", config.as_deref()),
        ("XDG_DATA_HOME", data.as_deref()),
    ] {
        if let Some(text) = text {
            no_control(what, text)?;
        }
    }
    no_control("The program path", &program.to_string_lossy())?;
    no_control("The data directory", &ctx.dirs.data.to_string_lossy())?;
    if ctx.platform == Platform::Launchd {
        no_control("The home directory", &ctx.home.to_string_lossy())?;
    }
    let mut env = Vec::new();
    for (name, value) in [
        ("PATH", path),
        ("XDG_CONFIG_HOME", config),
        ("XDG_DATA_HOME", data),
    ] {
        if let Some(value) = value {
            env.push((name.to_owned(), value));
        }
    }
    env.push(("FLEETDECK_LOG_FORMAT".to_owned(), "json".to_owned()));
    Ok(env)
}

// ---- Installed services ----

/// A service file that exists.
#[derive(Debug, Clone)]
pub struct Installed {
    pub path: PathBuf,
    /// The data directory of the marker; `None` without a (valid) marker.
    pub marker: Option<String>,
}

/// The service file of `kind`, when it exists.
pub fn installed(ctx: &Context, kind: Kind) -> anyhow::Result<Option<Installed>> {
    let path = kind.file(ctx)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let text = String::from_utf8_lossy(&bytes);
    let marker = match ctx.platform {
        Platform::Systemd => systemd::marker(&text),
        Platform::Launchd => launchd::marker(&text),
    };
    Ok(Some(Installed { path, marker }))
}

/// The installed kinds, in the order of [`KINDS`].
pub fn all_installed(ctx: &Context) -> anyhow::Result<Vec<(Kind, Installed)>> {
    let mut found = Vec::new();
    for kind in KINDS {
        if let Some(info) = installed(ctx, kind)? {
            found.push((kind, info));
        }
    }
    Ok(found)
}

/// The kinds that a command works on: the named kind (it must be installed)
/// or every installed kind (there must be one).
pub fn select(ctx: &Context, named: Option<Kind>) -> anyhow::Result<Vec<(Kind, Installed)>> {
    let mut found = all_installed(ctx)?;
    match named {
        Some(kind) => {
            found.retain(|(k, _)| *k == kind);
            if found.is_empty() {
                bail!("The {} service is not installed.", kind.name());
            }
        }
        None if found.is_empty() => {
            bail!("No Fleet Deck service is installed. Run `fleetdeck service install`.")
        }
        None => {}
    }
    Ok(found)
}

// ---- The probe and readiness (§8) ----

/// What a probe saw.
#[derive(Debug, Clone, Copy)]
pub struct Probe {
    /// The service runs (systemd `active`; launchd `state = running`).
    pub active: bool,
    /// launchd: the job is loaded.
    pub loaded: bool,
    /// The service does not run, but a socket of the kind answers: another
    /// Fleet Deck process runs.
    pub other: bool,
}

pub fn probe(ctx: &Context, deps: &dyn Deps, kind: Kind) -> Probe {
    let (loaded, active) = match ctx.platform {
        Platform::Systemd => {
            let active = systemd::is_active(deps, kind);
            (active, active)
        }
        Platform::Launchd => match launchd::running(ctx, deps, kind) {
            None => (false, false),
            Some(running) => (true, running),
        },
    };
    let other = !active && kind.sockets(&ctx.dirs).iter().any(|s| deps.answers(s));
    Probe {
        active,
        loaded,
        other,
    }
}

/// The second probe, just before the start step.
pub fn other_running(ctx: &Context, deps: &dyn Deps, kind: Kind) -> bool {
    probe(ctx, deps, kind).other
}

/// The second probe of launchd, after a `bootout`: the job is not loaded
/// any more, so `was_active` (from the first probe) says that the sockets
/// belonged to it.
pub fn other_running_after(ctx: &Context, deps: &dyn Deps, kind: Kind, was_active: bool) -> bool {
    !was_active && kind.sockets(&ctx.dirs).iter().any(|s| deps.answers(s))
}

/// Waits until every socket of the kind answers, at most `ready_limit`.
pub fn wait_ready(ctx: &Context, deps: &dyn Deps, kind: Kind) -> bool {
    let sockets = kind.sockets(&ctx.dirs);
    let step = ctx.ready_step.as_millis().max(1);
    let tries = (ctx.ready_limit.as_millis() / step).max(1);
    for _ in 0..tries {
        if sockets.iter().all(|s| deps.answers(s)) {
            return true;
        }
        deps.sleep(ctx.ready_step);
    }
    false
}

// ---- Plan ----

/// The parts of an install after the checks.
pub struct Plan<'a> {
    pub ctx: &'a Context,
    pub deps: &'a dyn Deps,
    pub kind: Kind,
    /// The launcher root; the program is `<root>/current`.
    pub root: PathBuf,
    /// The service file.
    pub path: PathBuf,
    /// The text of the service file.
    pub text: String,
    pub ulid: &'a str,
}

/// How the start step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Started,
    /// Another process runs: nothing was started.
    Other,
    /// Started, but the sockets did not answer in time.
    NotReady,
    /// launchd without a graphical login.
    NoDomain,
}

/// What an install did, for the summary.
pub struct Done {
    pub linger: Option<String>,
    pub wsl: Option<String>,
    pub outcome: Outcome,
}

// ---- Files ----

/// Writes `text` to `path` (mode 0644): a temporary file, then a rename.
pub fn write_file(path: &Path, text: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no directory", path.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp"));
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("cannot remove {}", tmp.display())),
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&tmp)
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    drop(file);
    // The mode does not depend on the umask.
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))
        .with_context(|| format!("cannot set the mode of {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))
}

/// Creates `dir` with the mode 0700.
pub fn private_dir(dir: &Path) -> anyhow::Result<()> {
    fleetdeck_core::paths::ensure_private_dir(dir)
        .with_context(|| format!("cannot create {}", dir.display()))
}

/// Creates an empty file with the mode 0600 when it is absent.
pub fn private_file(path: &Path) -> anyhow::Result<()> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e).with_context(|| format!("cannot create {}", path.display())),
    }
}
