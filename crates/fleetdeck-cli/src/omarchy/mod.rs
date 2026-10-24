//! `fleetdeck omarchy install | uninstall | status` (R13 P4 task C,
//! `omarchy-p4.md` §3.4, decisions P4-7 and P4-13).
//!
//! The installer writes a fixed list of files below the config home (never
//! through a link), records their hashes, and adds one managed line to
//! `hyprland.lua`. The commands that it runs are fixed argv lists, resolved
//! on the `PATH` of the Omarchy environment, with a time limit; their output
//! is never echoed.

pub mod assets;
pub mod digits;
pub mod fsx;
pub mod hyprland;
pub mod install;
pub mod journal;
pub mod lock;
pub mod plan;
pub mod record;
pub mod repair;
pub mod repair_cmd;
pub mod screensaver;
pub mod status;
pub mod theme_sync;
pub mod uninstall;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use fleetdeck_core::ChildEnv;
use fleetdeck_core::paths::{Env, NodeDirs};
use fleetdeck_proto::settings::SettingsChange;
use fleetdeck_station::desktop::run::{Out, Runner};
use fleetdeck_station::local::{LocalRequest, LocalResponse};
use serde_json::{Map, Value};

use crate::cli_omarchy::{OmarchyCommand, ScreensaverAction};
use fsx::Home;

/// The time limit of one external command.
pub const TOOL_LIMIT: Duration = Duration::from_secs(10);
/// The time limit of one call to the station.
pub const STATION_LIMIT: Duration = Duration::from_secs(5);
/// The time limit of the `summary` line of `status`.
pub const STATUS_STATION_LIMIT: Duration = Duration::from_secs(2);

/// The code of a save that read an old revision.
const CHANGED: &str = "settings_changed";

/// The config home: `$XDG_CONFIG_HOME` when it is set and absolute, else
/// `$HOME/.config`.
///
/// # Errors
/// Neither variable gives an absolute path.
pub fn config_home(env: &Env) -> anyhow::Result<PathBuf> {
    match (&env.xdg_config_home, &env.home) {
        (Some(xdg), _) => Ok(xdg.clone()),
        (None, Some(home)) => Ok(home.join(".config")),
        (None, None) => Err(anyhow!("HOME is not set, so the config home is not known")),
    }
}

/// Opens the config home of this process.
///
/// # Errors
/// The config home is not known or cannot be opened.
pub fn open_home() -> anyhow::Result<Home> {
    let path = config_home(&Env::from_process())?;
    Home::open(&path).with_context(|| format!("cannot open {}", path.display()))
}

/// The Omarchy environment: the scrubbed environment of this process.
#[must_use]
pub fn omarchy_env() -> ChildEnv {
    ChildEnv::from_process()
}

/// Runs `program args` on the `PATH` of `env`, with stdin null and a time
/// limit. The output is returned, never printed.
///
/// # Errors
/// The program is not installed, cannot start, or passes the limit.
pub async fn tool(
    env: &ChildEnv,
    program: &str,
    args: &[&str],
    limit: Duration,
) -> Result<Out, fleetdeck_station::desktop::Fail> {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    Runner {
        env,
        gate: None,
        start: None,
    }
    .run(program, &args, limit)
    .await
}

/// Reads `fleet_summary` from the station: `None` when it does not answer
/// within `limit`.
pub async fn summary_value(dirs: &NodeDirs, limit: Duration) -> Option<bool> {
    let call = crate::station::call(dirs, &LocalRequest::SettingsGet);
    match tokio::time::timeout(limit, call).await {
        Ok(Ok(LocalResponse::Settings { view })) => Some(
            view.values
                .get("fleet_summary")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        _ => None,
    }
}

/// Sets `fleet_summary` through the station local socket. `Ok(true)` when
/// this call changed the value.
///
/// # Errors
/// The station does not answer, or it refuses; the text is for the user.
pub async fn set_summary(dirs: &NodeDirs, on: bool) -> Result<bool, String> {
    let work = async {
        for attempt in 0..2 {
            let view = match crate::station::call(dirs, &LocalRequest::SettingsGet).await {
                Ok(LocalResponse::Settings { view }) => view,
                Ok(_) => return Err("unexpected answer from the station".to_owned()),
                Err(e) => return Err(format!("{e:#}")),
            };
            let current = view
                .values
                .get("fleet_summary")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if current == on {
                return Ok(false);
            }
            let mut changes = Map::new();
            changes.insert("fleet_summary".to_owned(), Value::Bool(on));
            let change = SettingsChange {
                changes,
                revision: view.revision,
                elevation: None,
            };
            match crate::station::call(dirs, &LocalRequest::SettingsSet { change }).await {
                Ok(LocalResponse::SettingsSaved { .. }) => return Ok(true),
                Ok(LocalResponse::SettingsError { code, .. })
                    if code == CHANGED && attempt == 0 => {}
                Ok(LocalResponse::SettingsError { code, message }) => {
                    return Err(format!("{code}: {message}"));
                }
                Ok(_) => return Err("unexpected answer from the station".to_owned()),
                Err(e) => return Err(format!("{e:#}")),
            }
        }
        Err("the settings changed during the call".to_owned())
    };
    tokio::time::timeout(STATION_LIMIT, work)
        .await
        .unwrap_or_else(|_| Err("the station did not answer in 5 s".to_owned()))
}

/// `fleetdeck omarchy ...`.
///
/// # Errors
/// The failure of the command; the caller prints `fleetdeck: <text>` and
/// exits 1.
pub async fn run(dirs: &NodeDirs, command: &OmarchyCommand) -> anyhow::Result<ExitCode> {
    match command {
        OmarchyCommand::Install {
            yes,
            screensaver,
            replace_conflicts,
            plan_stdin: _,
            resume,
        } => {
            let mode = match (resume, replace_conflicts) {
                (Some(id), _) => install::Mode::Resume(id.clone()),
                (None, true) => install::Mode::Replace(repair_cmd::read_install_input()?),
                (None, false) => install::Mode::Plain { yes: *yes },
            };
            install::run(dirs, mode, *screensaver).await
        }
        OmarchyCommand::InstallPlan { json: _ } => repair_cmd::install_plan(),
        OmarchyCommand::RepairStatus { json: _ } => repair_cmd::status(),
        OmarchyCommand::RestorePlan { repair, json: _ } => repair_cmd::restore_plan(repair),
        OmarchyCommand::Restore { plan_stdin: _ } => {
            repair_cmd::restore(&repair_cmd::read_restore_input()?)
        }
        OmarchyCommand::Uninstall => uninstall::run(dirs).await,
        OmarchyCommand::Status { json } => status::run(dirs, *json).await,
        OmarchyCommand::ThemeSync => theme_sync::run(dirs).await,
        OmarchyCommand::Screensaver { action } => {
            let home = open_home()?;
            match action {
                ScreensaverAction::On => screensaver::on(&home, lock::WAIT),
                ScreensaverAction::Off => screensaver::off(&home, lock::WAIT),
                ScreensaverAction::Rebaseline => screensaver::rebaseline(&home, lock::WAIT),
            }
        }
        OmarchyCommand::ScreensaverTick => screensaver::tick(dirs).await,
    }
}

#[cfg(test)]
#[path = "assets_qml_tests.rs"]
mod assets_qml_tests;
#[cfg(test)]
#[path = "fsx_tests.rs"]
mod fsx_tests;
#[cfg(test)]
#[path = "hyprland_tests.rs"]
mod hyprland_tests;
