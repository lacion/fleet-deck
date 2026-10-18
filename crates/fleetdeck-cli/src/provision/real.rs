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
use std::time::{Duration, Instant};

use fleetdeck_core::config::StationConfig;
use fleetdeck_core::paths::{Env, NodeDirs};
use fleetdeck_core::private_file::{read_private, write_new_shared_parent};
use fleetdeck_station::link::runtime::DECK_SCOPES;
use fleetdeck_station::local::{LocalClient, LocalRequest, LocalResponse, socket_path};
use fleetdeck_station::local_fingerprint::FEATURE;
use serde::Serialize;

use super::effects::{
    BoxFuture, Build, CallError, ConfigState, Effects, JoinOutcome, RootFile, ServiceState,
    StationView, Terminal,
};
use crate::cli::{ServiceCommand, ServiceKind};
use crate::service::inspect::{ServiceInspect, inspect, sha256_file};
use crate::service::{self, Context};
use crate::tailnet_join::parse_fingerprint;

/// One call to `station.fingerprint` without and with the provider probe.
const CALL_WAIT: Duration = Duration::from_secs(2);
const PROBE_CALL_WAIT: Duration = Duration::from_secs(35);
/// The wait of the CLI for the tailnet join (the owner has 660 s).
const JOIN_WAIT: Duration = Duration::from_secs(690);
/// The limit of `omarchy install` and `omarchy status`.
const OMARCHY_LIMIT: Duration = Duration::from_secs(60);
/// A file of the release prerequisite is at most this large.
const ROOT_FILE_MAX: u64 = 64 * 1024;
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
}

impl RealEffects {
    #[must_use]
    pub fn new(dirs: NodeDirs, env: Env, ctx: Context) -> Self {
        Self {
            dirs,
            env,
            ctx,
            notified: AtomicBool::new(false),
        }
    }

    fn service_command(&self, command: &ServiceCommand) -> io::Result<()> {
        // The summary of the command is not wanted: stdout stays empty.
        service::run(command, &self.ctx, &service::System, &mut io::sink())
            .map_err(|_| io::Error::other(SERVICE_FAILED))
    }

    /// Runs `fleetdeck omarchy <args>` and returns its exit code.
    async fn omarchy(&self, args: &[&str]) -> io::Result<i32> {
        let mut command = tokio::process::Command::new(std::env::current_exe()?);
        if self.ctx.data_dir_flag {
            command.arg("--data-dir").arg(&self.dirs.data);
        }
        let mut child = command
            .arg("omarchy")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        match tokio::time::timeout(OMARCHY_LIMIT, child.wait()).await {
            Ok(Ok(status)) => status
                .code()
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
        Box::pin(async move { ask_fingerprint(&path, probe).await })
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
        Box::pin(async move { ask_join(&path, host, port, &fingerprint, name).await })
    }

    fn omarchy_install(&self) -> BoxFuture<'_, io::Result<i32>> {
        Box::pin(async move { self.omarchy(&["install", "--yes"]).await })
    }

    fn omarchy_status(&self) -> BoxFuture<'_, io::Result<i32>> {
        Box::pin(async move { self.omarchy(&["status", "--json"]).await })
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

/// `station.fingerprint` on the socket `path`.
async fn ask_fingerprint(path: &Path, probe: bool) -> Result<StationView, CallError> {
    let wait = if probe { PROBE_CALL_WAIT } else { CALL_WAIT };
    let call = async {
        let mut client = LocalClient::connect(path)
            .await
            .map_err(|_| CallError::Unavailable)?;
        client
            .call(&LocalRequest::StationFingerprint {
                probe_providers: probe,
            })
            .await
            .map_err(|_| CallError::Invalid)
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

/// The tailnet join on the socket `path`, pinned to `deck_fingerprint`.
async fn ask_join(
    path: &Path,
    host: String,
    port: u16,
    deck_fingerprint: &str,
    name: String,
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
        let mut client = LocalClient::connect(path)
            .await
            .map_err(|_| code("failed"))?;
        client.call(&request).await.map_err(|_| code("failed"))
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
