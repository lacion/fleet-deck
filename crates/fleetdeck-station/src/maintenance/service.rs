//! The service of the maintenance channel (`board-gaps.md` §2.9): the
//! folder, the admission mutex, the test seams ([`Host`], [`Plans`]) and the
//! registry of the live relays.
//!
//! The service is a process singleton, as the Omarchy service is. When the
//! folder is not private, the socket path would be too long, or the own
//! executable is unknown, no service is installed and every method answers
//! `unavailable`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use fleetdeck_core::identity::Fingerprint;
use futures_util::future::BoxFuture;
use tokio::sync::mpsc;

use super::files::{ensure_dir, socket_path};
use super::relay::RelayCmd;
use crate::desktop_tools::StoredPlan;
use crate::omarchy_admin::child::{self, RunResult, Spec};

/// The longest path of a unix socket, in bytes (`sun_path` is 108 with the
/// final zero byte).
const SOCKET_PATH_MAX: usize = 107;
/// The longest run of `systemd-run`.
const LAUNCH_LIMIT: Duration = Duration::from_secs(15);
/// The longest run of `systemctl show`.
const UNIT_LIMIT: Duration = Duration::from_secs(5);
/// The longest wait for the state `running` after a launch.
const SETTLE: Duration = Duration::from_secs(10);
/// The commands that a live relay can have in its queue.
const RELAY_QUEUE: usize = 16;

const SYSTEMCTL: &str = "/usr/bin/systemctl";

/// What `systemctl --user show` says about the unit of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// The unit is active (or on its way in or out).
    Active,
    /// The unit does not exist or has ended.
    Gone,
    /// The query gave no answer that proves anything.
    Unknown,
}

/// The processes of the service: `systemd-run` and `systemctl`. Tests
/// replace it.
pub trait Host: Send + Sync {
    /// Runs `argv` (the program first) with exactly `env` and returns the
    /// exit code.
    fn launch(
        &self,
        argv: Vec<OsString>,
        env: Vec<(OsString, OsString)>,
    ) -> BoxFuture<'_, io::Result<i32>>;

    /// The state of the unit `fleetdeck-maintenance-<id>.service`.
    fn unit(&self, id: &str) -> BoxFuture<'_, Unit>;
}

/// The plans of `station.desktop_tools_plan` and the check before a launch.
/// Tests replace it.
pub trait Plans: Send + Sync {
    /// The live plan `plan_id` of the deck `peer`.
    fn plan(&self, plan_id: &str, peer: &Fingerprint, now_ms: i64) -> Option<StoredPlan>;
    /// The configuration check and the dry run again.
    fn recheck<'a>(&'a self, plan: &'a StoredPlan) -> BoxFuture<'a, Result<(), &'static str>>;
    /// Drops the plan.
    fn remove(&self, plan_id: &str);
}

/// The plans of this process.
struct RealPlans;

impl Plans for RealPlans {
    fn plan(&self, plan_id: &str, peer: &Fingerprint, now_ms: i64) -> Option<StoredPlan> {
        crate::desktop_tools::plan(plan_id, peer, now_ms)
    }

    fn recheck<'a>(&'a self, plan: &'a StoredPlan) -> BoxFuture<'a, Result<(), &'static str>> {
        Box::pin(crate::desktop_tools::recheck(plan))
    }

    fn remove(&self, plan_id: &str) {
        crate::desktop_tools::remove(plan_id);
    }
}

/// The environment of `systemd-run` and `systemctl`: a fixed `PATH` and
/// `LANG`, and four variables of the station process when they are set.
#[must_use]
pub fn host_env() -> Vec<(OsString, OsString)> {
    let mut env = vec![
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (OsString::from("LANG"), OsString::from("C")),
    ];
    for name in [
        "HOME",
        "USER",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
    ] {
        if let Some(value) = std::env::var_os(name) {
            env.push((OsString::from(name), value));
        }
    }
    env
}

/// The real processes.
struct RealHost;

impl Host for RealHost {
    fn launch(
        &self,
        argv: Vec<OsString>,
        env: Vec<(OsString, OsString)>,
    ) -> BoxFuture<'_, io::Result<i32>> {
        Box::pin(async move {
            let Some((program, args)) = argv.split_first() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the launch has no program",
                ));
            };
            let spec = Spec {
                program: Path::new(program),
                args,
                env: &env,
                extra: &[],
                stdin: None,
                limit: LAUNCH_LIMIT,
            };
            match child::run(&spec).await? {
                RunResult::Exit {
                    code: Some(code), ..
                } => Ok(code),
                RunResult::Exit { code: None, .. } => {
                    Err(io::Error::other("the launch ended on a signal"))
                }
                RunResult::TimedOut => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the launch took too long",
                )),
                RunResult::TooLarge => Err(io::Error::other("the launch printed too much")),
            }
        })
    }

    fn unit(&self, id: &str) -> BoxFuture<'_, Unit> {
        let mut args: Vec<OsString> = [
            "--user",
            "show",
            "-p",
            "LoadState",
            "-p",
            "ActiveState",
            "--value",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        args.push(OsString::from(format!(
            "fleetdeck-maintenance-{id}.service"
        )));
        Box::pin(async move {
            let env = host_env();
            let spec = Spec {
                program: Path::new(SYSTEMCTL),
                args: &args,
                env: &env,
                extra: &[],
                stdin: None,
                limit: UNIT_LIMIT,
            };
            match child::run(&spec).await {
                Ok(RunResult::Exit {
                    code: Some(0),
                    stdout,
                    ..
                }) => classify(&stdout),
                _ => Unit::Unknown,
            }
        })
    }
}

/// Reads the two values that `systemctl show --value` printed for
/// `LoadState` and `ActiveState`. Anything but exactly two known lines is
/// [`Unit::Unknown`]. The order of the lines does not matter, because the
/// two sets of values do not overlap.
#[must_use]
pub fn classify(stdout: &[u8]) -> Unit {
    let Ok(text) = std::str::from_utf8(stdout) else {
        return Unit::Unknown;
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() != 2 {
        return Unit::Unknown;
    }
    if lines.contains(&"not-found") || lines.iter().any(|l| matches!(*l, "inactive" | "failed")) {
        return Unit::Gone;
    }
    if lines
        .iter()
        .any(|l| matches!(*l, "active" | "activating" | "deactivating" | "reloading"))
    {
        return Unit::Active;
    }
    Unit::Unknown
}

/// A live relay: its ticket and the way to send it a command.
type RelayEntry = (u64, mpsc::Sender<RelayCmd>);

/// The state of the maintenance channel.
pub struct MaintenanceService {
    pub(super) dir: PathBuf,
    pub(super) program: PathBuf,
    /// One admission at a time.
    pub(super) admission: tokio::sync::Mutex<()>,
    pub(super) host: Arc<dyn Host>,
    pub(super) plans: Arc<dyn Plans>,
    /// The longest wait for the state `running` after a launch.
    pub(super) settle: Duration,
    relays: Mutex<HashMap<String, RelayEntry>>,
    next_ticket: AtomicU64,
}

impl MaintenanceService {
    pub(super) fn new(
        dir: PathBuf,
        program: PathBuf,
        host: Arc<dyn Host>,
        plans: Arc<dyn Plans>,
    ) -> Self {
        Self {
            dir,
            program,
            admission: tokio::sync::Mutex::new(()),
            host,
            plans,
            settle: SETTLE,
            relays: Mutex::new(HashMap::new()),
            next_ticket: AtomicU64::new(1),
        }
    }

    fn relays(&self) -> MutexGuard<'_, HashMap<String, RelayEntry>> {
        self.relays.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers a relay for `id`. A relay that was there loses its sender,
    /// which ends it.
    pub(super) fn register_relay(&self, id: &str) -> (u64, mpsc::Receiver<RelayCmd>) {
        let ticket = self.next_ticket.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel(RELAY_QUEUE);
        self.relays().insert(id.to_owned(), (ticket, tx));
        (ticket, rx)
    }

    /// Removes the relay `ticket` of `id`, unless a newer relay replaced it.
    pub(super) fn unregister_relay(&self, id: &str, ticket: u64) {
        let mut relays = self.relays();
        if relays.get(id).is_some_and(|(t, _)| *t == ticket) {
            relays.remove(id);
        }
    }

    /// The way to send a command to the live relay of `id`.
    pub(super) fn relay_sender(&self, id: &str) -> Option<mpsc::Sender<RelayCmd>> {
        self.relays().get(id).map(|(_, tx)| tx.clone())
    }
}

/// True when `<dir>/<32 hex>.sock` fits in a unix socket address.
#[must_use]
pub fn socket_fits(dir: &Path) -> bool {
    socket_path(dir, &"0".repeat(32))
        .as_os_str()
        .as_bytes()
        .len()
        <= SOCKET_PATH_MAX
}

/// Builds the service for the station folder `station_dir`, or logs one
/// warning and returns `None`.
pub(super) fn build(
    station_dir: &Path,
    host: Arc<dyn Host>,
    plans: Arc<dyn Plans>,
) -> Option<MaintenanceService> {
    let dir = station_dir.join("maintenance");
    if let Err(e) = ensure_dir(&dir) {
        tracing::warn!(
            error = %e,
            "the maintenance folder is not usable: desktop tool installation is off"
        );
        return None;
    }
    if !socket_fits(&dir) {
        tracing::warn!("the maintenance socket path is too long: desktop tool installation is off");
        return None;
    }
    let program = match crate::omarchy_admin::own_program(station_dir) {
        Ok(program) => program,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "the station cannot find its own executable: desktop tool installation is off"
            );
            return None;
        }
    };
    Some(MaintenanceService::new(dir, program, host, plans))
}

static SERVICE: Mutex<Option<Arc<MaintenanceService>>> = Mutex::new(None);

/// The service of this process, when one is installed.
#[must_use]
pub fn service() -> Option<Arc<MaintenanceService>> {
    SERVICE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Builds and installs the service of this process.
pub fn start(station_dir: &Path) {
    if let Some(service) = build(station_dir, Arc::new(RealHost), Arc::new(RealPlans)) {
        *SERVICE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(service));
    }
}
