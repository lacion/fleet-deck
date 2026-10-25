//! The traits of this station for placement (design §9.1, FG-6, O19):
//! the system, the architecture, whether the machine runs on a battery, and
//! the labels of `station.toml`.

use std::path::Path;
use std::time::Duration;

use fleetdeck_proto::fleet_git::StationTraits;

/// The folder of the Linux power supplies.
const POWER_SUPPLY: &str = "/sys/class/power_supply";
/// The file that names the kernel build; WSL has `microsoft` in it.
const OSRELEASE: &str = "/proc/sys/kernel/osrelease";
/// The time limit and the output cap of `pmset` (design §9.1).
const PMSET_TIMEOUT: Duration = Duration::from_secs(3);
const PMSET_CAP: u64 = 4 * 1024;

/// The traits of this machine with the station `labels`. It may run
/// `pmset` (macOS, up to 3 s): call it from a blocking context.
#[must_use]
pub fn read(labels: &[String]) -> StationTraits {
    let wsl = std::fs::read_to_string(OSRELEASE).is_ok_and(|text| is_wsl(&text));
    StationTraits {
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        on_battery: on_battery(std::env::consts::OS, wsl, Path::new(POWER_SUPPLY), pmset),
        labels: labels.to_vec(),
    }
}

/// WSL names `microsoft` in the kernel release, in any case.
#[must_use]
pub fn is_wsl(osrelease: &str) -> bool {
    osrelease.to_lowercase().contains("microsoft")
}

/// Whether the machine runs on a battery (design §9.1): WSL is always
/// `Some(false)` (O19); Linux reads `power_supply`; macOS reads `pmset`;
/// any other system and any unreadable source give `None`.
pub fn on_battery(
    os: &str,
    wsl: bool,
    power_supply: &Path,
    pmset: impl FnOnce() -> Option<String>,
) -> Option<bool> {
    match os {
        "linux" if wsl => Some(false),
        "linux" => linux_on_battery(power_supply),
        "macos" => pmset().and_then(|text| parse_pmset(&text)),
        _ => None,
    }
}

/// `Some(true)` when no `Mains` supply is online and a `Battery` supply is
/// `Discharging`; `Some(false)` when a `Mains` supply is online or no battery
/// discharges; `None` when the folder cannot be read.
fn linux_on_battery(root: &Path) -> Option<bool> {
    let entries = std::fs::read_dir(root).ok()?;
    let (mut mains_online, mut discharging) = (false, false);
    for entry in entries.flatten() {
        let dir = entry.path();
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
        match read("type").as_deref().map(str::trim) {
            Some("Mains") => mains_online |= read("online").is_some_and(|v| v.trim() == "1"),
            Some("Battery") => {
                discharging |= read("status").is_some_and(|v| v.trim() == "Discharging");
            }
            _ => {}
        }
    }
    Some(!mains_online && discharging)
}

/// The first line of `pmset -g batt`: `'Battery Power'` is `Some(true)`,
/// `'AC Power'` is `Some(false)`, anything else `None`.
#[must_use]
pub fn parse_pmset(text: &str) -> Option<bool> {
    let first = text.lines().next()?;
    if first.contains("'Battery Power'") {
        Some(true)
    } else if first.contains("'AC Power'") {
        Some(false)
    } else {
        None
    }
}

/// Runs `pmset -g batt`: 3 s, 4 KiB. `None` on any failure.
fn pmset() -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new("pmset")
        .args(["-g", "batt"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + PMSET_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut text = String::new();
    child
        .stdout
        .take()?
        .take(PMSET_CAP)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

#[cfg(test)]
#[path = "traits_tests.rs"]
mod tests;
