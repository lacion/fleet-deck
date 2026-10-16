//! `fleetdeck doctor`: checks the local setup and says what is wrong
//! (v1 parity row 136).
//!
//! One line per check: `ok   <check>: <detail>`, `warn <check>: <detail>` or
//! `fail <check>: <detail>`, then `doctor: N ok, N warn, N fail`. The exit
//! code is 1 when a check failed. No check changes anything.

use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_core::config::{ConfigError, DeckConfig, StationConfig};
use fleetdeck_core::paths::{Env, NodeDirs};
use fleetdeck_deck::local::{DeckRequest, DeckResponse};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::VersionStatus;
use tokio::net::TcpStream;
use tokio::process::Command;

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const DECK_TIMEOUT: Duration = Duration::from_secs(5);
const BOARD_TIMEOUT: Duration = Duration::from_secs(2);
const VERSION_LINE_MAX: usize = 200;

#[derive(Default)]
struct Report {
    ok: u32,
    warn: u32,
    fail: u32,
}

impl Report {
    fn ok(&mut self, check: &str, detail: impl std::fmt::Display) {
        self.ok += 1;
        println!("ok   {check}: {detail}");
    }

    fn warn(&mut self, check: &str, detail: impl std::fmt::Display) {
        self.warn += 1;
        println!("warn {check}: {detail}");
    }

    fn fail(&mut self, check: &str, detail: impl std::fmt::Display) {
        self.fail += 1;
        println!("fail {check}: {detail}");
    }

    fn skipped(&mut self, check: &str, file: &str) {
        self.warn(check, format_args!("skipped: {file} does not load"));
    }
}

/// What a config file check found: the file is absent, loaded, or broken.
enum Loaded<T> {
    Absent,
    Config(T),
    Broken,
}

/// `fleetdeck doctor`.
pub async fn run(dirs: &NodeDirs, env: &Env) -> ExitCode {
    let mut report = Report::default();
    check_data_dirs(&mut report, dirs);
    let station = load_station(&mut report, dirs, env);
    let deck = load_deck(&mut report, dirs);
    check_git(&mut report).await;
    check_landlock(&mut report).await;
    check_providers_and_shells(&mut report, &station).await;
    if !matches!(deck, Loaded::Absent) {
        check_deck_socket(&mut report, dirs, &deck).await;
        check_board(&mut report, &deck).await;
    }
    check_v1_plugin(&mut report, env);
    println!(
        "doctor: {} ok, {} warn, {} fail",
        report.ok, report.warn, report.fail
    );
    if report.fail > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn mode_of(path: &Path) -> std::io::Result<u32> {
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
}

/// (a): the data directory and its `deck/` and `station/` directories.
fn check_data_dirs(report: &mut Report, dirs: &NodeDirs) {
    let first = ("data-dir", dirs.data.clone());
    let subs = [
        ("data-dir deck", dirs.deck()),
        ("data-dir station", dirs.station()),
    ];
    for (name, path) in std::iter::once(first).chain(subs) {
        match fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && name != "data-dir" => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                report.fail(name, format_args!("{} does not exist", path.display()));
            }
            Err(e) => report.fail(name, format_args!("{}: {e}", path.display())),
            Ok(meta) if !meta.is_dir() => {
                report.fail(name, format_args!("{} is not a directory", path.display()));
            }
            Ok(meta) => {
                let mode = meta.permissions().mode() & 0o777;
                if mode & 0o077 == 0 {
                    report.ok(name, format_args!("{}, mode {mode:04o}", path.display()));
                } else {
                    report.warn(
                        name,
                        format_args!("{}: mode {mode:04o}, use 0700", path.display()),
                    );
                }
            }
        }
    }
}

/// (b) for one config file: absent, loaded, or the loader's message. A
/// file that loads but that others can read is a warning.
fn check_file<T>(
    report: &mut Report,
    check: &str,
    path: &Path,
    load: impl FnOnce() -> Result<T, ConfigError>,
) -> Loaded<T> {
    if matches!(fs::metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
        report.ok(check, "not configured");
        return Loaded::Absent;
    }
    match load() {
        Err(e) => {
            report.fail(check, e);
            Loaded::Broken
        }
        Ok(config) => {
            match mode_of(path) {
                Ok(mode) if mode & 0o077 != 0 => {
                    report.warn(check, format_args!("mode {mode:04o}, use 0600"));
                }
                Ok(mode) => report.ok(check, format_args!("loads, mode {mode:04o}")),
                Err(e) => report.fail(check, format_args!("{}: {e}", path.display())),
            }
            Loaded::Config(config)
        }
    }
}

/// An absent `station.toml` gives the defaults, which the station uses too.
fn load_station(report: &mut Report, dirs: &NodeDirs, env: &Env) -> Loaded<StationConfig> {
    let path = dirs.station_config();
    let store = dirs.station().join("secrets");
    let loaded = check_file(report, "station.toml", &path, || {
        StationConfig::load_with_store(&path, env, &store)
    });
    match loaded {
        Loaded::Absent => match StationConfig::load_with_store(&path, env, &store) {
            Ok(config) => Loaded::Config(config),
            Err(e) => {
                report.fail("station.toml", e);
                Loaded::Broken
            }
        },
        other => other,
    }
}

fn load_deck(report: &mut Report, dirs: &NodeDirs) -> Loaded<DeckConfig> {
    let path = dirs.deck_config();
    check_file(report, "deck.toml", &path, || DeckConfig::load(&path))
}

/// The first non-empty line of `bytes`, trimmed and cut to 200 characters.
fn first_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(VERSION_LINE_MAX).collect())
}

/// `<binary> --version` within 5 s. `Ok(line)` on exit 0, else the text of
/// the failure. `env` is the child environment; `None` inherits.
async fn version(binary: &Path, env: Option<&ChildEnv>) -> Result<String, String> {
    let mut cmd = Command::new(binary);
    cmd.arg("--version").stdin(Stdio::null()).kill_on_drop(true);
    if let Some(env) = env {
        cmd.env_clear().envs(env.iter());
    }
    let out = match tokio::time::timeout(VERSION_TIMEOUT, cmd.output()).await {
        Err(_) => return Err("timed out after 5 s".to_owned()),
        Ok(Err(e)) => return Err(format!("{}: {e}", binary.display())),
        Ok(Ok(out)) => out,
    };
    let line = first_line(&out.stdout).or_else(|| first_line(&out.stderr));
    if out.status.success() {
        return Ok(line.unwrap_or_else(|| "no version output".to_owned()));
    }
    let status = out
        .status
        .code()
        .map_or_else(|| out.status.to_string(), |c| c.to_string());
    Err(match line {
        Some(line) => format!("exit {status}: {line}"),
        None => format!("exit {status}"),
    })
}

/// (c).
async fn check_git(report: &mut Report) {
    match version(Path::new("git"), None).await {
        Ok(line) => report.ok("git", line),
        Err(e) => report.fail("git", e),
    }
}

/// (c2) Landlock (B9, DL-200): a test ruleset is enforced in a child
/// process, `fleetdeck confine-exec --write-root / -- /bin/true`. Linux only.
#[cfg(target_os = "linux")]
async fn check_landlock(report: &mut Report) {
    const WARN: &str = "Landlock is not available: worktree and clone provisioning is checked after Git, not confined.";
    let enforced = match std::env::current_exe() {
        Ok(exe) => {
            let out = Command::new(exe)
                .args(["confine-exec", "--write-root", "/", "--", "/bin/true"])
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output();
            match tokio::time::timeout(VERSION_TIMEOUT, out).await {
                Ok(Ok(out)) if out.status.success() => {
                    let text = String::from_utf8_lossy(&out.stderr);
                    text.lines().any(|l| {
                        matches!(
                            l,
                            "fleetdeck-confine: fully_enforced"
                                | "fleetdeck-confine: partially_enforced"
                        )
                    })
                }
                _ => false,
            }
        }
        Err(_) => false,
    };
    if enforced {
        report.ok(
            "landlock",
            "Landlock confines worktree and clone provisioning",
        );
    } else {
        report.warn("landlock", WARN);
    }
}

/// Other systems: no Landlock line.
#[cfg(not(target_os = "linux"))]
async fn check_landlock(_report: &mut Report) {}

/// (d) and (e).
async fn check_providers_and_shells(report: &mut Report, station: &Loaded<StationConfig>) {
    let Loaded::Config(config) = station else {
        report.skipped("provider", "station.toml");
        report.skipped("shells", "station.toml");
        return;
    };
    for instance in &config.provider_instances {
        let check = format!("provider {}", instance.id.as_str());
        let env = match ChildEnv::for_instance(ChildEnv::from_process(), instance) {
            Ok(env) => env,
            Err(e) => {
                report.fail(&check, e);
                continue;
            }
        };
        match version(&instance.binary, Some(&env)).await {
            Ok(line) => report_provider_version(report, &check, instance.driver.kind(), &line),
            Err(e) => report.fail(&check, e),
        }
    }
    if config.allow_shell {
        report.ok("shells", "shells on");
    } else {
        report.warn("shells", "shells off: the board offers no terminal");
    }
}

/// (d) for one provider: the version line is `ok`, unless the version is
/// older than the adapter's tested minimum (a warning, R8.2) or on its deny
/// list (a failure: the station refuses starts).
fn report_provider_version(report: &mut Report, check: &str, kind: ProviderKind, line: &str) {
    let parsed = match kind {
        ProviderKind::Claude => fleetdeck_providers::claude::parse_version(line),
        ProviderKind::Codex => fleetdeck_providers::codex::parse_version(line),
        ProviderKind::Unknown => None,
    };
    let status = fleetdeck_providers::version::version_status(kind, parsed.as_deref());
    let min = fleetdeck_providers::version::tested_min_version(kind).unwrap_or("?");
    let shown = parsed.as_deref().unwrap_or(line);
    match status {
        VersionStatus::OlderThanTested => report.warn(
            check,
            format_args!("{shown} is older than the tested {min}"),
        ),
        VersionStatus::Broken => report.fail(
            check,
            format_args!("{shown} is known to break the protocol"),
        ),
        VersionStatus::Ok | VersionStatus::Unknown => report.ok(check, line),
    }
}

/// (f): the deck answers on its local socket.
async fn check_deck_socket(report: &mut Report, dirs: &NodeDirs, deck: &Loaded<DeckConfig>) {
    if let Loaded::Broken = deck {
        report.skipped("deck-socket", "deck.toml");
        return;
    }
    let call = crate::deck::call(dirs, &DeckRequest::Stations);
    match tokio::time::timeout(DECK_TIMEOUT, call).await {
        Err(_) => report.fail("deck-socket", "timed out after 5 s"),
        Ok(Err(e)) => report.fail("deck-socket", format_args!("{e:#}")),
        Ok(Ok(DeckResponse::Stations { stations })) => {
            report.ok("deck-socket", format_args!("{} stations", stations.len()));
        }
        Ok(Ok(_)) => report.fail("deck-socket", "unexpected answer from the deck"),
    }
}

/// A wildcard address connects to loopback, with the same port.
fn connect_target(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), addr.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv6Addr::LOCALHOST.into(), addr.port())
        }
        _ => addr,
    }
}

/// (g): each board listen address accepts a TCP connection within 2 s.
async fn check_board(report: &mut Report, deck: &Loaded<DeckConfig>) {
    let Loaded::Config(config) = deck else {
        report.skipped("board", "deck.toml");
        return;
    };
    for addr in &config.listen {
        let check = format!("board {addr}");
        match tokio::time::timeout(BOARD_TIMEOUT, TcpStream::connect(connect_target(*addr))).await {
            Err(_) => report.warn(&check, "not listening: timed out after 2 s"),
            Ok(Err(e)) => report.warn(&check, format_args!("not listening: {e}")),
            Ok(Ok(_)) => report.ok(&check, "accepts a TCP connection"),
        }
    }
}

const V1_PLUGIN_ADVICE: &str = "the v1 plugin runs its hooks in v2 sessions: they slow every \
                                stop; remove it after the cutover";

fn is_v1_key(key: &str, bare_allowed: bool) -> bool {
    (bare_allowed && key == "fleetdeck") || key.starts_with("fleetdeck@")
}

/// Reads one JSON file of `~/.claude`. `Ok(None)` when it is absent.
fn read_json(path: &Path) -> Result<Option<serde_json::Value>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// (h): the v1 Fleet Deck Claude plugin.
fn check_v1_plugin(report: &mut Report, env: &Env) {
    const CHECK: &str = "v1-plugin";
    let Some(home) = env.home.as_deref() else {
        report.warn(CHECK, "HOME is not set, so ~/.claude was not checked");
        return;
    };
    let claude = home.join(".claude");
    let mut found: Vec<String> = Vec::new();
    let mut unreadable = false;

    let settings = claude.join("settings.json");
    match read_json(&settings) {
        Ok(None) => {}
        Ok(Some(json)) => {
            let enabled = json.get("enabledPlugins").and_then(|v| v.as_object());
            for (key, value) in enabled.into_iter().flatten() {
                if is_v1_key(key, true) && value == &serde_json::Value::Bool(true) {
                    found.push(format!("{} enabledPlugins `{key}`", settings.display()));
                }
            }
        }
        Err(e) => {
            unreadable = true;
            report.warn(CHECK, format_args!("{}: {e}", settings.display()));
        }
    }

    let cache = claude.join("plugins").join("cache");
    match fs::read_dir(&cache) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let dir: PathBuf = entry.path().join("fleetdeck");
                if dir.is_dir() {
                    found.push(format!("directory {}", dir.display()));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            unreadable = true;
            report.warn(CHECK, format_args!("{}: {e}", cache.display()));
        }
    }

    let installed = claude.join("plugins").join("installed_plugins.json");
    match read_json(&installed) {
        Ok(None) => {}
        Ok(Some(json)) => {
            let tables = [
                json.as_object(),
                json.get("plugins").and_then(|v| v.as_object()),
            ];
            for table in tables.into_iter().flatten() {
                for key in table.keys().filter(|k| is_v1_key(k, false)) {
                    found.push(format!("{} `{key}`", installed.display()));
                }
            }
        }
        Err(e) => {
            unreadable = true;
            report.warn(CHECK, format_args!("{}: {e}", installed.display()));
        }
    }

    if !found.is_empty() {
        report.warn(
            CHECK,
            format_args!("found in {}: {V1_PLUGIN_ADVICE}", found.join("; ")),
        );
    } else if !unreadable {
        report.ok(CHECK, "not installed");
    }
}
