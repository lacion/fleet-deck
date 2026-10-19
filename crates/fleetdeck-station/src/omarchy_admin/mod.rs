//! Omarchy management on the station: the link methods `omarchy.status`,
//! `omarchy.act` and the repair methods (`repair.rs`), and the screensaver
//! tick (design `omarchy-admin.md`).
//!
//! The station runs fixed `fleetdeck` subcommands of its own executable as
//! children (`child.rs`). No value from the wire reaches the argv, the
//! environment or a path. One action runs at a time: the action slot is a
//! mutex that is never waited for.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_core::identity::Fingerprint;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::omarchy_admin::{
    ActOutcome, OmarchyActResult, OmarchyAction, OmarchyStatus, code,
};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::watch;

mod act;
pub mod child;
mod config;
mod provision;
mod repair;
mod status;
mod tick;

pub use config::{Config, Limits, SessionEnv};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_act;
#[cfg(test)]
mod tests_provision;
#[cfg(test)]
mod tests_repair;
#[cfg(test)]
mod tests_repair_act;
#[cfg(test)]
mod tests_retry;
#[cfg(test)]
mod tests_runner;

use child::{RunResult, Spec};

/// The text of `unavailable`: no service is installed.
const TEXT_UNAVAILABLE: &str = "Omarchy management is not available on this station.";
const TEXT_ELEVATION: &str = "This action needs the owner's elevation.";
const TEXT_BUSY: &str = "Another Omarchy action is running on this station.";
const TEXT_MISSING: &str = "Omarchy was not found on this station.";
const TEXT_NOT_PROVISIONED: &str = "Provisioning cannot be retried from the board in this state.";
const TEXT_INVALID: &str = "The request is not valid.";

/// The names of the session variables that a child gets.
const SESSION_VARS: [&str; 3] = [
    "XDG_RUNTIME_DIR",
    "WAYLAND_DISPLAY",
    "HYPRLAND_INSTANCE_SIGNATURE",
];

/// Who holds the action slot.
#[derive(Debug, Clone, Copy)]
enum Holder {
    Act(OmarchyAction),
    Tick,
}

#[derive(Debug, Default)]
struct Shared {
    holder: Mutex<Option<Holder>>,
    last_retry: Mutex<Option<&'static str>>,
}

/// The action slot, held. It is freed at drop.
struct Permit(Arc<Shared>);

impl Drop for Permit {
    fn drop(&mut self) {
        *self.0.holder.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// The Omarchy service of one station.
#[derive(Debug)]
pub struct OmarchyAdmin {
    cfg: Config,
    shared: Arc<Shared>,
    plans: repair::PlanStore,
    clock: repair::Clock,
}

impl OmarchyAdmin {
    #[must_use]
    pub fn new(cfg: Config) -> Self {
        Self::with_clock(cfg, repair::Clock::system())
    }

    /// A service with the clock of the plan store (tests set it).
    fn with_clock(cfg: Config, clock: repair::Clock) -> Self {
        Self {
            cfg,
            shared: Arc::default(),
            plans: repair::PlanStore::default(),
            clock,
        }
    }

    /// Takes the action slot without waiting.
    fn try_hold(&self, holder: Holder) -> Option<Permit> {
        let mut slot = self
            .shared
            .holder
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if slot.is_some() {
            return None;
        }
        *slot = Some(holder);
        Some(Permit(Arc::clone(&self.shared)))
    }

    /// The action that holds the slot; `None` while only a tick holds it.
    fn running(&self) -> Option<OmarchyAction> {
        match *self
            .shared
            .holder
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            Some(Holder::Act(action)) => Some(action),
            Some(Holder::Tick) | None => None,
        }
    }

    fn last_retry(&self) -> Option<String> {
        (*self
            .shared
            .last_retry
            .lock()
            .unwrap_or_else(PoisonError::into_inner))
        .map(str::to_owned)
    }

    fn set_last_retry(&self, value: &'static str) {
        *self
            .shared
            .last_retry
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(value);
    }

    /// The three session variables of the found desktop; none when no
    /// desktop is found or its environment does not build.
    async fn session_vars(&self) -> Vec<(&'static str, OsString)> {
        match &self.cfg.session {
            SessionEnv::Fixed(vars) => vars.clone(),
            SessionEnv::Detect => {
                let base = &self.cfg.omarchy_env;
                let Some(found) = crate::desktop::detect::detect(base).await else {
                    return Vec::new();
                };
                let Ok(env) = crate::desktop::env::desktop_env(base, &found) else {
                    tracing::debug!("the desktop environment of the Omarchy child did not build");
                    return Vec::new();
                };
                SESSION_VARS
                    .iter()
                    .filter_map(|name| env.get(name).map(|v| (*name, v.to_owned())))
                    .collect()
            }
        }
    }

    /// Runs the own executable with the fixed words `tail`.
    async fn run_tail(&self, tail: &[&str], limit: Duration) -> std::io::Result<RunResult> {
        let tail: Vec<OsString> = tail.iter().map(OsString::from).collect();
        self.run_argv(&tail, limit).await
    }

    /// Runs the own executable with `tail`, after `--data-dir` when the
    /// station has one.
    async fn run_argv(&self, tail: &[OsString], limit: Duration) -> std::io::Result<RunResult> {
        self.run_argv_stdin(tail, limit, None).await
    }

    /// [`Self::run_argv`] with `stdin` bytes for the child (`None`: `/dev/null`).
    async fn run_argv_stdin(
        &self,
        tail: &[OsString],
        limit: Duration,
        stdin: Option<Vec<u8>>,
    ) -> std::io::Result<RunResult> {
        let mut args: Vec<OsString> = Vec::new();
        if let Some(dir) = &self.cfg.data_dir {
            args.push("--data-dir".into());
            args.push(dir.clone().into_os_string());
        }
        args.extend(tail.iter().cloned());
        let extra = self.session_vars().await;
        child::run(&Spec {
            program: &self.cfg.program,
            args: &args,
            env: &self.cfg.env,
            extra: &extra,
            stdin,
            limit,
        })
        .await
    }

    /// `omarchy.status`: params `null` or `{}` only.
    pub async fn status_call(&self, params: Value) -> Outcome {
        if !is_empty_params(&params) {
            return wire(code::INVALID_REQUEST, TEXT_INVALID);
        }
        answer(self.build_status(self.running()).await)
    }
}

/// True for params that are `null` or `{}`.
fn is_empty_params(params: &Value) -> bool {
    match params {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// A wire error with a fixed text.
fn wire(code: &str, text: &str) -> Outcome {
    Outcome::Err(WireError::new(code, text))
}

fn answer(value: impl Serialize) -> Outcome {
    match serde_json::to_value(value) {
        Ok(v) => Outcome::Ok(v),
        Err(_) => wire("internal", "The answer could not be built."),
    }
}

fn result(
    outcome: ActOutcome,
    failed_code: Option<&str>,
    message: Option<String>,
    status: OmarchyStatus,
) -> Outcome {
    answer(OmarchyActResult {
        outcome,
        code: failed_code.map(str::to_owned),
        message,
        status,
        repair: None,
    })
}

static SERVICE: Mutex<Option<Arc<OmarchyAdmin>>> = Mutex::new(None);

/// The service of this process, when one is installed.
#[must_use]
pub fn service() -> Option<Arc<OmarchyAdmin>> {
    SERVICE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Installs the service of this process.
pub fn install(admin: Arc<OmarchyAdmin>) {
    *SERVICE.lock().unwrap_or_else(PoisonError::into_inner) = Some(admin);
}

/// The link handler of the Omarchy methods. `peer` is the fingerprint of
/// the deck link of the request: a plan is bound to it.
pub async fn call(method: &str, params: Value, peer: Fingerprint) -> Outcome {
    let Some(admin) = service() else {
        return wire("unavailable", TEXT_UNAVAILABLE);
    };
    match method {
        "omarchy.status" => admin.status_call(params).await,
        "omarchy.act" => admin.act_call(params, peer).await,
        "omarchy.install_plan" => admin.install_plan_call(params, peer).await,
        "omarchy.repair_status" => admin.repair_status_call(params).await,
        "omarchy.restore_plan" => admin.restore_plan_call(params, peer).await,
        other => Outcome::Err(WireError::new(
            crate::link::NOT_IMPLEMENTED,
            format!("`{other}` is not served by this station build"),
        )),
    }
}

/// The station's own executable: `<station dir>/bin/current` when that link
/// exists, else the running executable.
fn own_program(station_dir: &Path) -> std::io::Result<PathBuf> {
    let link = station_dir.join("bin").join("current");
    if std::fs::symlink_metadata(&link).is_ok() {
        Ok(link)
    } else {
        std::env::current_exe()
    }
}

/// Builds the service, installs it and starts the screensaver tick. When the
/// own executable cannot be found, it logs one warning and starts nothing:
/// the methods answer `unavailable`, and the returned task has ended.
pub fn start(
    dirs: &NodeDirs,
    station_dir: &Path,
    stop: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let program = match own_program(station_dir) {
        Ok(program) => program,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "the station cannot find its own executable: Omarchy management is off"
            );
            return tokio::spawn(async {});
        }
    };
    let admin = Arc::new(OmarchyAdmin::new(Config::from_process(dirs, program)));
    install(Arc::clone(&admin));
    tokio::spawn(tick::run(admin, tick::EVERY, stop))
}
