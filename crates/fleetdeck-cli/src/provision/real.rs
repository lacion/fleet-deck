//! The real effects of the provisioning bootstrap (P8 T3b, design
//! `omarchy-p8.md` 3 and 5): the station, the service, the tailnet join and
//! the P4 installer of this machine, behind [`Effects`].
//!
//! Nothing here parses the text of another command. The calls to the station
//! use its typed local responses; the child processes run with null streams
//! and only their exit code is read. Stdout stays empty.

use std::fs::{self, DirBuilder, File};
use std::io::{self, Read as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_core::config::StationConfig;
use fleetdeck_core::identity::Identity;
use fleetdeck_core::localsock::LineClient;
use fleetdeck_core::paths::{Env, NodeDirs};
use fleetdeck_core::private_file::{PrivateReadError, read_private, write_new_shared_parent};
use fleetdeck_link::{ReportReply, ValidReport};
use fleetdeck_station::link::runtime::DECK_SCOPES;
use fleetdeck_station::local::{LocalRequest, LocalResponse, socket_path};
use fleetdeck_station::local_fingerprint::FEATURE;
use ring::rand::{SecureRandom, SystemRandom};
use serde::Serialize;
use tokio::io::AsyncReadExt as _;

use super::effects::{
    BoxFuture, Build, CallError, ConfigState, Effects, JoinOutcome, ReportFailure, RootFile,
    ServiceState, StationView, Terminal,
};
use super::{binding, real_report};
use crate::cli::{ServiceCommand, ServiceKind};
use crate::service::inspect::{ServiceInspect, inspect, sha256_file, sha256_reader};
use crate::service::{self, Context};
use crate::tailnet_join::parse_fingerprint;

/// One call to `station.fingerprint` without and with the provider probe.
const CALL_WAIT: Duration = Duration::from_secs(2);
const PROBE_CALL_WAIT: Duration = Duration::from_secs(35);
/// The wait of the CLI for the tailnet join (the owner has 660 s).
const JOIN_WAIT: Duration = Duration::from_secs(690);
/// The limit of one setup report (the lookup and the send).
const REPORT_WAIT: Duration = Duration::from_secs(30);
/// The limit of `omarchy install` and `omarchy status`.
const OMARCHY_LIMIT: Duration = Duration::from_secs(60);
/// A file of the release prerequisite is at most this large.
const ROOT_FILE_MAX: u64 = 64 * 1024;
/// The stdout of `omarchy status --json` is read up to this size.
const STATUS_MAX: u64 = 64 * 1024;
/// The prefix of the temporary leaf of the `station.toml` write.
const CONFIG_TEMP_PREFIX: &str = ".station-toml.";
/// The fixed text of a failed service step (no command output).
const SERVICE_FAILED: &str = "the service step failed";
const NOTIFY_MISSING: &str =
    "fleetdeck: omarchy-notification-send is not installed; the fingerprint is only on stderr.";
const NOTIFY_FAILED: &str =
    "fleetdeck: omarchy-notification-send could not start; the fingerprint is only on stderr.";

/// The effects of this machine.
pub struct RealEffects {
    dirs: NodeDirs,
    env: Env,
    ctx: Context,
    notified: AtomicBool,
    /// The SHA-256 of the station binary of the checked manifest.
    trusted: Mutex<Option<String>>,
}

impl RealEffects {
    #[must_use]
    pub fn new(dirs: NodeDirs, env: Env, ctx: Context) -> Self {
        Self {
            dirs,
            env,
            ctx,
            notified: AtomicBool::new(false),
            trusted: Mutex::new(None),
        }
    }

    fn trusted_binary(&self) -> Option<String> {
        self.trusted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn service_command(&self, command: &ServiceCommand) -> io::Result<()> {
        // The summary of the command is not wanted: stdout stays empty.
        service::run(command, &self.ctx, &service::System, &mut io::sink())
            .map_err(|_| io::Error::other(SERVICE_FAILED))
    }

    /// Runs `fleetdeck omarchy <args>` and returns its exit code and, with
    /// `capture`, its stdout (up to [`STATUS_MAX`] bytes).
    async fn omarchy(&self, args: &[&str], capture: bool) -> io::Result<(i32, Vec<u8>)> {
        let mut command = tokio::process::Command::new(std::env::current_exe()?);
        if self.ctx.data_dir_flag {
            command.arg("--data-dir").arg(&self.dirs.data);
        }
        let mut child = command
            .arg("omarchy")
            .args(args)
            .stdin(Stdio::null())
            .stdout(if capture {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child.stdout.take();
        let work = async {
            let mut bytes = Vec::new();
            if let Some(out) = stdout {
                // The pipe closes at the end of this block.
                out.take(STATUS_MAX).read_to_end(&mut bytes).await?;
            }
            Ok::<_, io::Error>((child.wait().await?, bytes))
        };
        match tokio::time::timeout(OMARCHY_LIMIT, work).await {
            Ok(Ok((status, bytes))) => status
                .code()
                .map(|code| (code, bytes))
                .ok_or_else(|| io::Error::other("the omarchy command ended by a signal")),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // Kills the child and reaps it.
                child.kill().await?;
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the omarchy command passed its time limit",
                ))
            }
        }
    }
}

impl Effects for RealEffects {
    fn station_dir(&self) -> PathBuf {
        self.dirs.station()
    }

    fn trust_root(&self) -> PathBuf {
        PathBuf::from("/")
    }

    fn ensure_station_dir(&self) -> io::Result<()> {
        ensure_private_dir(&self.dirs.station())
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, d: Duration) -> BoxFuture<'_, ()> {
        Box::pin(tokio::time::sleep(d))
    }

    fn read_provision(&self, path: &Path) -> Result<Vec<u8>, Terminal> {
        let max = u64::try_from(fleetdeck_core::provision::MAX_BYTES).unwrap_or(u64::MAX);
        read_private(path, max).map_err(|_| Terminal::FILE_INVALID)
    }

    fn root_file(&self, path: &Path) -> io::Result<RootFile> {
        read_root_file(path)
    }

    fn own_binary_sha256(&self) -> io::Result<String> {
        sha256_file(&std::env::current_exe()?)
    }

    fn set_trusted_binary(&self, sha256: &str) {
        *self.trusted.lock().unwrap_or_else(PoisonError::into_inner) = Some(sha256.to_owned());
    }

    fn build(&self) -> Build {
        Build {
            version: fleetdeck_core::build::version().to_owned(),
            commit: fleetdeck_core::build::COMMIT.to_owned(),
            release: fleetdeck_core::build::RELEASE,
        }
    }

    fn station_config(&self) -> ConfigState {
        config_state(&self.dirs, &self.env)
    }

    fn create_station_config(&self, name: &str) -> io::Result<()> {
        create_config(&self.dirs, name)
    }

    fn service(&self) -> ServiceState {
        match inspect(&self.ctx, &service::System) {
            ServiceInspect::Absent => ServiceState::Absent,
            ServiceInspect::Matching { active } => ServiceState::Matching { active },
            ServiceInspect::Conflict => ServiceState::Conflict,
        }
    }

    fn install_service(&self) -> io::Result<()> {
        self.service_command(&ServiceCommand::Install {
            kind: ServiceKind::Station,
            dry_run: false,
            verbose: false,
        })
    }

    fn start_service(&self) -> io::Result<()> {
        self.service_command(&ServiceCommand::Start {
            kind: Some(ServiceKind::Station),
        })
    }

    fn fingerprint(&self, probe: bool) -> BoxFuture<'_, Result<StationView, CallError>> {
        let path = socket_path(&self.dirs.station());
        let trusted = self.trusted_binary();
        Box::pin(async move { ask_fingerprint(&path, probe, trusted.as_deref()).await })
    }

    fn join(
        &self,
        host: &str,
        port: u16,
        deck_fingerprint: &str,
        name: &str,
    ) -> BoxFuture<'_, JoinOutcome> {
        let path = socket_path(&self.dirs.station());
        let host = host.to_owned();
        let fingerprint = deck_fingerprint.to_owned();
        let name = name.to_owned();
        let trusted = self.trusted_binary();
        Box::pin(async move {
            ask_join(&path, host, port, &fingerprint, name, trusted.as_deref()).await
        })
    }

    fn omarchy_install(&self) -> BoxFuture<'_, io::Result<i32>> {
        Box::pin(async move {
            self.omarchy(&["install", "--yes"], false)
                .await
                .map(|(code, _)| code)
        })
    }

    fn omarchy_status_json(&self) -> BoxFuture<'_, io::Result<(i32, Vec<u8>)>> {
        Box::pin(async move { self.omarchy(&["status", "--json"], true).await })
    }

    fn notify(&self, grouped_fingerprint: &str) {
        if self.notified.swap(true, Ordering::SeqCst) {
            return;
        }
        let body = format!("Approve this station fingerprint on the board: {grouped_fingerprint}");
        let spawned = std::process::Command::new("omarchy-notification-send")
            .args([
                "--app-name",
                "Fleet Deck",
                "-u",
                "normal",
                "Fleet Deck pairing",
            ])
            .arg(body)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                // Detached: a thread reaps the child, nobody waits for it.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => eprintln!("{NOTIFY_MISSING}"),
            Err(_) => eprintln!("{NOTIFY_FAILED}"),
        }
    }

    fn progress(&self, line: &str) {
        eprintln!("{line}");
    }

    fn identity_loads(&self) -> bool {
        Identity::load(&self.dirs.station()).is_ok()
    }

    fn report(
        &self,
        host: &str,
        port: u16,
        deck_fingerprint: &str,
        report: &ValidReport,
    ) -> BoxFuture<'_, Result<ReportReply, ReportFailure>> {
        let dir = self.dirs.station();
        let host = host.to_owned();
        let fingerprint = deck_fingerprint.to_owned();
        let report = report.clone();
        Box::pin(async move {
            real_report::send(&dir, &host, port, &fingerprint, &report, REPORT_WAIT).await
        })
    }

    fn jitter(&self, max: Duration) -> Duration {
        let mut bytes = [0_u8; 8];
        if SystemRandom::new().fill(&mut bytes).is_err() {
            return Duration::ZERO;
        }
        let span = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
        Duration::from_millis(u64::from_le_bytes(bytes) % span.saturating_add(1))
    }

    fn read_authorization(&self, path: &Path) -> io::Result<Vec<u8>> {
        read_private(path, binding::MAX_BYTES).map_err(|e| match e {
            PrivateReadError::Io(e) => e,
            other => io::Error::other(other),
        })
    }

    fn delete_authorization(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }
}

/// A directory that the effective user owns and that group and others
/// cannot write; a missing one is created (and its missing parents) with
/// mode 0700. An existing directory is only checked, never changed.
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        Err(e) => return Err(e),
    }
    let meta = fs::symlink_metadata(dir)?;
    let owner = rustix::process::geteuid().as_raw();
    if !meta.is_dir() || meta.uid() != owner || meta.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the directory is not owned by this user or others can write it",
        ));
    }
    Ok(())
}

/// A file of the release prerequisite with its owner and mode. The bytes,
/// the owner and the mode come from one open file.
fn read_root_file(path: &Path) -> io::Result<RootFile> {
    let symlink = fs::symlink_metadata(path)?.file_type().is_symlink();
    let file = File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the path is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(ROOT_FILE_MAX + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).is_ok_and(|n| n > ROOT_FILE_MAX) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the file is larger than 64 KiB",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path has no parent"))?;
    let parent_meta = fs::metadata(parent)?;
    Ok(RootFile {
        bytes,
        uid: meta.uid(),
        mode: meta.mode() & 0o7777,
        symlink,
        parent_uid: parent_meta.uid(),
        parent_mode: parent_meta.mode() & 0o7777,
    })
}

/// The state of `station.toml` (D12).
fn config_state(dirs: &NodeDirs, env: &Env) -> ConfigState {
    let path = dirs.station_config();
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return ConfigState::Absent,
        Err(_) => return ConfigState::Invalid,
    }
    let store = dirs.station().join("secrets");
    if StationConfig::load_with_store(&path, env, &store).is_err() {
        return ConfigState::Invalid;
    }
    // The loaded config always has a name (a default one): look for the key.
    let Ok(text) = fs::read_to_string(&path) else {
        return ConfigState::Invalid;
    };
    let Ok(table) = toml::from_str::<toml::Table>(&text) else {
        return ConfigState::Invalid;
    };
    match table.get("name").and_then(toml::Value::as_str) {
        Some(name) => ConfigState::Named(name.to_owned()),
        None => ConfigState::Unnamed,
    }
}

#[derive(Serialize)]
struct NameOnly<'a> {
    name: &'a str,
}

/// Creates `station.toml` with `name = "<name>"`. It never replaces a file.
/// A missing config directory is created with mode 0700; an existing one is
/// only checked.
fn create_config(dirs: &NodeDirs, name: &str) -> io::Result<()> {
    let path = dirs.station_config();
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path has no parent"))?;
    ensure_private_dir(parent)?;
    let mut text = toml::to_string(&NameOnly { name }).map_err(io::Error::other)?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    write_new_shared_parent(&path, text.as_bytes(), CONFIG_TEMP_PREFIX).map_err(io::Error::other)
}

/// 64 lowercase hex digits.
fn is_hex64(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The view of a good `Fingerprint` response.
fn view_of(response: LocalResponse) -> Result<StationView, CallError> {
    let LocalResponse::Fingerprint {
        features,
        fingerprint,
        paired_deck,
        link_up,
        build,
        binary_sha256,
        providers,
    } = response
    else {
        return Err(CallError::Invalid);
    };
    if !features.iter().any(|f| f == FEATURE)
        || !is_hex64(&fingerprint)
        || paired_deck.as_deref().is_some_and(|d| !is_hex64(d))
        || !is_hex64(&binary_sha256)
    {
        return Err(CallError::Invalid);
    }
    Ok(StationView {
        fingerprint,
        paired_deck,
        link_up,
        build: Build {
            version: build.version,
            commit: build.commit,
            release: build.release,
        },
        binary_sha256,
        providers,
    })
}

/// What the check of the process behind a socket reads of `/proc`.
trait ProcReader: Send + Sync {
    /// The SHA-256 of the executable of `pid`, hashed through one open
    /// descriptor of `/proc/<pid>/exe` (64 KiB reads).
    fn exe_sha256(&self, pid: u32) -> io::Result<String>;
    /// The start time of `pid` (field 22 of `/proc/<pid>/stat`, in ticks).
    fn start_time(&self, pid: u32) -> io::Result<u64>;
}

/// The `/proc` of this machine.
struct RealProc;

impl ProcReader for RealProc {
    fn exe_sha256(&self, pid: u32) -> io::Result<String> {
        sha256_reader(File::open(format!("/proc/{pid}/exe"))?)
    }

    fn start_time(&self, pid: u32) -> io::Result<u64> {
        let bytes = fs::read(format!("/proc/{pid}/stat"))?;
        parse_start_time(&String::from_utf8_lossy(&bytes))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no start time"))
    }
}

/// The start time of a `/proc/<pid>/stat` line. The name of the process
/// (field 2) can hold spaces and brackets, so the fields count from the last
/// `)`: the state is field 3 and the start time is field 22.
fn parse_start_time(stat: &str) -> Option<u64> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// The process behind a socket, as the check saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Peer {
    pid: u32,
    start: u64,
}

/// The process behind a connected socket is ours: the same user, a process id,
/// and an executable that hashes to the binary of the manifest. `None` when
/// any of it fails, and always without a trusted hash.
fn verify_peer(
    proc: &dyn ProcReader,
    euid: u32,
    trusted: Option<&str>,
    cred: io::Result<(u32, Option<u32>)>,
) -> Option<Peer> {
    let trusted = trusted?;
    let (uid, pid) = cred.ok()?;
    let pid = pid?;
    if uid != euid {
        return None;
    }
    // The start time first: a process that is replaced while it is hashed
    // shows another start time at the end.
    let start = proc.start_time(pid).ok()?;
    (proc.exe_sha256(pid).ok()? == trusted).then_some(Peer { pid, start })
}

/// The process of `peer` still runs (the same start time).
fn still_peer(proc: &dyn ProcReader, peer: Peer) -> bool {
    proc.start_time(peer.pid)
        .is_ok_and(|start| start == peer.start)
}

/// `station.fingerprint` on the socket `path`, only from a verified process.
async fn ask_fingerprint(
    path: &Path,
    probe: bool,
    trusted: Option<&str>,
) -> Result<StationView, CallError> {
    let euid = rustix::process::geteuid().as_raw();
    ask_fingerprint_with(path, probe, trusted, euid, &RealProc).await
}

async fn ask_fingerprint_with(
    path: &Path,
    probe: bool,
    trusted: Option<&str>,
    euid: u32,
    proc: &dyn ProcReader,
) -> Result<StationView, CallError> {
    let wait = if probe { PROBE_CALL_WAIT } else { CALL_WAIT };
    let call = async {
        let mut client = LineClient::connect(path, "station")
            .await
            .map_err(|_| CallError::Unavailable)?;
        let peer =
            verify_peer(proc, euid, trusted, client.peer_cred()).ok_or(CallError::Invalid)?;
        let response: LocalResponse = client
            .call(&LocalRequest::StationFingerprint {
                probe_providers: probe,
            })
            .await
            .map_err(|_| CallError::Invalid)?;
        if still_peer(proc, peer) {
            Ok(response)
        } else {
            Err(CallError::Invalid)
        }
    };
    match tokio::time::timeout(wait, call).await {
        Ok(response) => view_of(response?),
        Err(_) => Err(CallError::Timeout),
    }
}

/// The outcome of the answer of the station to a tailnet join.
fn join_outcome(response: LocalResponse) -> JoinOutcome {
    match response {
        LocalResponse::TailnetPaired { .. } => JoinOutcome::Paired,
        LocalResponse::TailnetRefused { code, .. } => JoinOutcome::Code(code),
        // Cannot happen with a fingerprint.
        LocalResponse::TailnetObserved { .. } => JoinOutcome::Code("invalid_request".to_owned()),
        LocalResponse::Error { .. } => JoinOutcome::Code("failed".to_owned()),
        _ => JoinOutcome::Code("unsupported".to_owned()),
    }
}

/// The tailnet join on the socket `path`, pinned to `deck_fingerprint`, only
/// with a verified process.
async fn ask_join(
    path: &Path,
    host: String,
    port: u16,
    deck_fingerprint: &str,
    name: String,
    trusted: Option<&str>,
) -> JoinOutcome {
    let euid = rustix::process::geteuid().as_raw();
    ask_join_with(
        path,
        host,
        port,
        (deck_fingerprint, name),
        (trusted, euid, &RealProc),
    )
    .await
}

async fn ask_join_with(
    path: &Path,
    host: String,
    port: u16,
    (deck_fingerprint, name): (&str, String),
    (trusted, euid, proc): (Option<&str>, u32, &dyn ProcReader),
) -> JoinOutcome {
    let code = |text: &str| JoinOutcome::Code(text.to_owned());
    let Ok(pin) = parse_fingerprint(deck_fingerprint) else {
        return code("invalid_request");
    };
    let request = LocalRequest::JoinTailnet {
        host,
        port,
        deck_fingerprint: Some(pin),
        scopes: DECK_SCOPES.to_vec(),
        name,
    };
    let call = async {
        let mut client = LineClient::connect(path, "station")
            .await
            .map_err(|_| code("failed"))?;
        let peer = verify_peer(proc, euid, trusted, client.peer_cred())
            .ok_or_else(|| code("build_mismatch"))?;
        let response: LocalResponse = client.call(&request).await.map_err(|_| code("failed"))?;
        if still_peer(proc, peer) {
            Ok(response)
        } else {
            Err(code("build_mismatch"))
        }
    };
    match tokio::time::timeout(JOIN_WAIT, call).await {
        Ok(Ok(response)) => join_outcome(response),
        Ok(Err(outcome)) => outcome,
        Err(_) => code("timeout"),
    }
}

#[cfg(test)]
#[path = "real_tests.rs"]
mod tests;
