//! The Omarchy lines of `fleetdeck doctor` (R13 P4 task C, `omarchy-p4.md`
//! §3.4): `omarchy` and `omarchy-sudo`. Read-only.

use std::time::Duration;

use fleetdeck_core::paths::Env;
use fleetdeck_station::desktop::code;
use fleetdeck_station::desktop::detect::omarchy_root;

use crate::doctor::Report;
use crate::omarchy::fsx::Home;
use crate::omarchy::{self, record, status};

/// The time limit of the sudo check.
const SUDO_LIMIT: Duration = Duration::from_secs(5);

/// The `omarchy` line and the `omarchy-sudo` line.
pub async fn check(report: &mut Report, env: &Env) {
    let omarchy_env = omarchy::omarchy_env();
    check_install(report, env, &omarchy_env);
    check_sudo(report, &omarchy_env).await;
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
