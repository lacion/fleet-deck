//! `fleetdeck station drain` and `fleetdeck station upgrade
//! prepare|trial|activate|rollback` (M3 7-3, DL-128). The rules live in
//! `fleetdeck_station::upgrade`; this module runs the steps.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_core::{IdSource as _, SharedClock, SystemClock, UlidSource};
use fleetdeck_providers::host::shim::wire;
use fleetdeck_station::local::{LocalClient, LocalRequest, LocalResponse, socket_path};
use fleetdeck_station::upgrade::{
    self, Launcher, RollbackPlan, UpdateRecord, UpdateState, live_shims, rollback_plan,
};

use crate::cli::UpgradeCommand;
use crate::station::call;

/// The version of this build.
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn upgrades_dir(dirs: &NodeDirs) -> PathBuf {
    dirs.station().join("upgrades")
}

fn launcher(dirs: &NodeDirs) -> Launcher {
    Launcher::new(dirs.station().join("bin"))
}

/// `fleetdeck station drain`.
pub async fn drain(dirs: &NodeDirs, timeout_secs: u64) -> anyhow::Result<()> {
    let response = call(
        dirs,
        &LocalRequest::Drain {
            timeout_ms: timeout_secs.saturating_mul(1000),
        },
    )
    .await?;
    let LocalResponse::Drained { report } = response else {
        bail!("unexpected answer from the station: {response:?}");
    };
    if !report.idle {
        bail!(
            "the station refuses new work, but these sessions are still busy: {}",
            report.busy.join(", ")
        );
    }
    println!("drained: no session has open work; stop the station now");
    Ok(())
}

/// Fails when a station answers on the local socket.
async fn require_stopped(dirs: &NodeDirs) -> anyhow::Result<()> {
    if LocalClient::connect(&socket_path(&dirs.station()))
        .await
        .is_ok()
    {
        bail!("the station is running: run `fleetdeck station drain`, then stop it first");
    }
    Ok(())
}

/// `fleetdeck station upgrade ...`.
pub async fn run(dirs: &NodeDirs, command: &UpgradeCommand) -> anyhow::Result<()> {
    match command {
        UpgradeCommand::Prepare { binary, to_version } => prepare(dirs, binary, to_version).await,
        UpgradeCommand::Trial { snapshot } => trial(snapshot),
        UpgradeCommand::Activate { update } => activate(dirs, update),
        UpgradeCommand::Rollback { update } => rollback(dirs, update).await,
    }
}

async fn prepare(dirs: &NodeDirs, binary: &Path, to_version: &str) -> anyhow::Result<()> {
    require_stopped(dirs).await?;
    let clock: SharedClock = Arc::new(SystemClock);
    let update_id = format!(
        "upd-{}",
        UlidSource::new(clock)
            .next_ulid()
            .to_string()
            .to_lowercase()
    );
    let dir = upgrades_dir(dirs);
    let snapshot = dir.join(format!("{update_id}-snapshot"));
    upgrade::snapshot(&dirs.station_db(), &snapshot)?;
    let mut record = UpdateRecord {
        update_id: update_id.clone(),
        from_version: VERSION.to_owned(),
        to_version: to_version.to_owned(),
        state: UpdateState::Prepared,
        live_advanced: false,
        snapshot: snapshot.clone(),
    };
    record.save(&dir)?;
    let status = tokio::process::Command::new(binary)
        .args(["station", "upgrade", "trial", "--snapshot"])
        .arg(&snapshot)
        .status()
        .await
        .with_context(|| format!("cannot run the trial of {}", binary.display()))?;
    if !status.success() {
        record.state = UpdateState::Failed;
        record.save(&dir)?;
        bail!("the trial of the new build failed ({status}); the old build stays active");
    }
    let launcher = launcher(dirs);
    let running = std::env::current_exe().context("cannot find the running binary")?;
    launcher.adopt(&running, VERSION)?;
    launcher.install(binary, to_version, &update_id)?;
    record.state = UpdateState::Trialed;
    record.save(&dir)?;
    println!("{update_id}");
    Ok(())
}

fn trial(snapshot: &Path) -> anyhow::Result<()> {
    // A disposable copy next to the snapshot; removed after the trial.
    let work = snapshot.join("trial-copy");
    let result = upgrade::trial(&snapshot.join("station.db"), &work, wire::PROTOCOLS);
    let _ = std::fs::remove_dir_all(&work);
    let report = result?;
    println!(
        "trial passed: schema version {}, {} unresolved shims",
        report.schema_version,
        report.shims.len()
    );
    Ok(())
}

fn activate(dirs: &NodeDirs, update: &str) -> anyhow::Result<()> {
    let dir = upgrades_dir(dirs);
    let mut record = UpdateRecord::load(&dir, update)?;
    let launcher = launcher(dirs);
    let release = launcher.release_path(&record.to_version, update);
    launcher.activate(&record, &release)?;
    record.state = UpdateState::Activated;
    record.save(&dir)?;
    println!(
        "activated {}: restart the station service",
        release.display()
    );
    Ok(())
}

async fn rollback(dirs: &NodeDirs, update: &str) -> anyhow::Result<()> {
    require_stopped(dirs).await?;
    let dir = upgrades_dir(dirs);
    let mut record = UpdateRecord::load(&dir, update)?;
    let launcher = launcher(dirs);
    if record.state == UpdateState::Activated {
        launcher.roll_back()?;
    }
    match rollback_plan(&record, live_shims(&dirs.station_db())?) {
        RollbackPlan::RestoreSnapshot => {
            let clock: SharedClock = Arc::new(SystemClock);
            let incarnation = UlidSource::new(clock).next_ulid().to_string();
            upgrade::restore(&record.snapshot, &dirs.station_db(), &incarnation)?;
            println!("rolled back: snapshot restored with a new incarnation");
        }
        RollbackPlan::BinaryOnly { reason } => {
            println!("rolled back the binary only ({reason}); the live database is kept");
        }
    }
    record.state = UpdateState::RolledBack;
    record.save(&dir)?;
    Ok(())
}
