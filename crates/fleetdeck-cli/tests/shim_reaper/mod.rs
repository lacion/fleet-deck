//! Test hygiene for real shims: a shim outlives every station handle by
//! design (ADR-005 §2) and keeps an ended provider's exit until the
//! station acknowledges it. A test rig therefore kills the shims of its
//! station directory when it drops, so no shim outlives the test.

use std::path::Path;

/// The pid of the `fleetdeck shim --id <shim_id>` process.
pub fn shim_pid(shim_id: &str) -> Option<u32> {
    let needle = format!("shim\0--id\0{shim_id}\0");
    std::fs::read_dir("/proc").ok()?.flatten().find_map(|e| {
        let pid: u32 = e.file_name().to_str()?.parse().ok()?;
        let cmdline = std::fs::read(e.path().join("cmdline")).ok()?;
        String::from_utf8_lossy(&cmdline)
            .contains(&needle)
            .then_some(pid)
    })
}

/// Sends SIGKILL to `pid`.
pub fn kill9(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
}

/// Kills every shim whose journal is in `shims_dir`.
pub fn reap(shims_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(shims_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".journal")) else {
            continue;
        };
        if let Some(pid) = shim_pid(id) {
            kill9(pid);
        }
    }
}
