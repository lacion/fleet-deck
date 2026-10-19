//! The Omarchy lines of `fleetdeck doctor` (R13 P4 task C, `omarchy-p4.md`
//! §3.4): `omarchy` and `omarchy-sudo`; from R13 P7 (§8.1), `desktop-input`:
//! whether `wtype` is found; and from R13 P7b (§15), `desktop-click`:
//! whether `wlrctl` is found. Read-only: the doctor never runs `wlrctl` and
//! never clicks.

use std::time::Duration;

use fleetdeck_core::paths::Env;
use fleetdeck_station::desktop::code;
use fleetdeck_station::desktop::detect::omarchy_root;
use fleetdeck_station::desktop::run::on_path;

use crate::doctor::Report;
use crate::omarchy::fsx::Home;
use crate::omarchy::{self, record, status};

/// The time limit of the sudo check.
const SUDO_LIMIT: Duration = Duration::from_secs(5);

/// The `omarchy` line, the `desktop-input` line, the `desktop-click` line and
/// the `omarchy-sudo` line.
pub async fn check(report: &mut Report, env: &Env) {
    let omarchy_env = omarchy::omarchy_env();
    check_install(report, env, &omarchy_env);
    check_input(report, &omarchy_env);
    check_click(report, &omarchy_env);
    check_sudo(report, &omarchy_env).await;
}

/// What a `desktop-input` or `desktop-click` line found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputTool {
    /// No Omarchy desktop on this machine: no line.
    NoDesktop,
    Found,
    Missing,
}

/// Whether the executable `name` is on the `PATH` of an Omarchy machine.
fn tool(omarchy_env: &fleetdeck_core::ChildEnv, name: &str) -> InputTool {
    if omarchy_root(omarchy_env).is_none() {
        InputTool::NoDesktop
    } else if on_path(omarchy_env, name) {
        InputTool::Found
    } else {
        InputTool::Missing
    }
}

/// Whether the keyboard tool of agent input (`wtype`) is on the `PATH` of an
/// Omarchy machine.
fn input_tool(omarchy_env: &fleetdeck_core::ChildEnv) -> InputTool {
    tool(omarchy_env, "wtype")
}

/// Whether the pointer click tool of agent input (`wlrctl`) is on the `PATH`
/// of an Omarchy machine.
fn click_tool(omarchy_env: &fleetdeck_core::ChildEnv) -> InputTool {
    tool(omarchy_env, "wlrctl")
}

/// The `desktop-input` line (R13 P7 §8.1): a missing `wtype` is a warning,
/// never a failure. Agent keyboard input and text need it; the pointer does
/// not.
fn check_input(report: &mut Report, omarchy_env: &fleetdeck_core::ChildEnv) {
    const CHECK: &str = "desktop-input";
    match input_tool(omarchy_env) {
        InputTool::NoDesktop => {}
        InputTool::Found => report.ok(CHECK, "wtype found"),
        InputTool::Missing => report.warn(
            CHECK,
            "wtype not found: agent keyboard input and text fail until it is installed",
        ),
    }
}

/// The text of the `desktop-click` line: `Ok` for an `ok` line, `Err` for a
/// `warn` line; `None` without an Omarchy desktop.
fn click_line(
    omarchy_env: &fleetdeck_core::ChildEnv,
) -> Option<Result<&'static str, &'static str>> {
    match click_tool(omarchy_env) {
        InputTool::NoDesktop => None,
        InputTool::Found => Some(Ok(
            "wlrctl found; compatibility is not verified (live smoke pending)",
        )),
        InputTool::Missing => Some(Err(
            "wlrctl not found: agent pointer clicks fail until it is installed",
        )),
    }
}

/// The `desktop-click` line (R13 P7b §15): a missing `wlrctl` is a warning,
/// never a failure. A found `wlrctl` is not a verified one: the live smoke is
/// pending. The doctor never runs `wlrctl`.
fn check_click(report: &mut Report, omarchy_env: &fleetdeck_core::ChildEnv) {
    const CHECK: &str = "desktop-click";
    match click_line(omarchy_env) {
        None => {}
        Some(Ok(detail)) => report.ok(CHECK, detail),
        Some(Err(detail)) => report.warn(CHECK, detail),
    }
}

fn check_install(report: &mut Report, env: &Env, omarchy_env: &fleetdeck_core::ChildEnv) {
    const CHECK: &str = "omarchy";
    let Some((_, version)) = omarchy_root(omarchy_env) else {
        report.ok(CHECK, "no Omarchy on this machine");
        return;
    };
    let version = version.as_deref().unwrap_or("(version unknown)");
    let home = omarchy::config_home(env)
        .ok()
        .and_then(|path| Home::open(&path).ok());
    let loaded = match &home {
        Some(home) => record::load(home),
        None => Ok(None),
    };
    match (loaded, &home) {
        (Ok(Some(rec)), Some(home)) => match status::drift(home, &rec) {
            0 => report.ok(CHECK, "installed, no drift"),
            n => report.warn(
                CHECK,
                format_args!("{n} changed or missing files (run fleetdeck omarchy status)"),
            ),
        },
        (Err(_), _) => report.warn(
            CHECK,
            "the install record is not valid (run fleetdeck omarchy status)",
        ),
        _ => report.ok(
            CHECK,
            format_args!(
                "Omarchy {version}; the integration is not installed (run fleetdeck omarchy install)"
            ),
        ),
    }
}

async fn check_sudo(report: &mut Report, omarchy_env: &fleetdeck_core::ChildEnv) {
    const CHECK: &str = "omarchy-sudo";
    match omarchy::tool(
        omarchy_env,
        "omarchy-sudo-passwordless",
        &["--active"],
        SUDO_LIMIT,
    )
    .await
    {
        Ok(out) if out.code == 0 => report.warn(CHECK, "passwordless sudo is on for this user"),
        Ok(_) => report.ok(CHECK, "off"),
        // The script is not on the Omarchy PATH: no line.
        Err(fail) if fail.code == code::UNSUPPORTED => {}
        Err(_) => report.warn(CHECK, "could not check"),
    }
}

#[cfg(test)]
#[path = "doctor_omarchy_input_tests.rs"]
mod input_tests;

#[cfg(test)]
#[path = "doctor_omarchy_click_tests.rs"]
mod click_tests;
