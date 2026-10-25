use std::fs;
use std::path::Path;

use fleetdeck_proto::fleet_git::StationTraits;

use super::*;

/// A fake `power_supply` folder: each entry is `(name, type, online, status)`.
fn supplies(entries: &[(&str, &str, Option<&str>, Option<&str>)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, kind, online, status) in entries {
        let supply = dir.path().join(name);
        fs::create_dir(&supply).unwrap();
        fs::write(supply.join("type"), format!("{kind}\n")).unwrap();
        if let Some(online) = online {
            fs::write(supply.join("online"), format!("{online}\n")).unwrap();
        }
        if let Some(status) = status {
            fs::write(supply.join("status"), format!("{status}\n")).unwrap();
        }
    }
    dir
}

fn linux(root: &Path) -> Option<bool> {
    on_battery("linux", false, root, || panic!("no pmset on Linux"))
}

#[test]
fn a_discharging_battery_without_mains_is_on_battery() {
    let dir = supplies(&[
        ("AC", "Mains", Some("0"), None),
        ("BAT0", "Battery", None, Some("Discharging")),
    ]);
    assert_eq!(linux(dir.path()), Some(true));
}

#[test]
fn an_online_mains_supply_is_not_on_battery() {
    let dir = supplies(&[
        ("AC", "Mains", Some("1"), None),
        ("BAT0", "Battery", None, Some("Discharging")),
    ]);
    assert_eq!(linux(dir.path()), Some(false));
}

#[test]
fn no_battery_or_a_charging_one_is_not_on_battery() {
    let none = supplies(&[("AC", "Mains", Some("0"), None)]);
    assert_eq!(linux(none.path()), Some(false));
    let empty = supplies(&[]);
    assert_eq!(linux(empty.path()), Some(false));
    let full = supplies(&[("BAT0", "Battery", None, Some("Full"))]);
    assert_eq!(linux(full.path()), Some(false));
}

#[test]
fn an_unreadable_folder_gives_none() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(linux(&dir.path().join("missing")), None);
}

#[test]
fn wsl_is_never_on_battery_even_with_a_discharging_battery() {
    let dir = supplies(&[("BAT0", "Battery", None, Some("Discharging"))]);
    let got = on_battery("linux", true, dir.path(), || panic!("no pmset on WSL"));
    assert_eq!(got, Some(false));
    assert!(is_wsl("5.15.153.1-microsoft-standard-WSL2"));
    assert!(is_wsl("6.6.87.2-MICROSOFT-standard"));
    assert!(!is_wsl("6.8.0-generic"));
}

#[test]
fn the_pmset_first_line_decides() {
    assert_eq!(
        parse_pmset(
            "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t80%; discharging"
        ),
        Some(true)
    );
    assert_eq!(
        parse_pmset("Now drawing from 'AC Power'\n -InternalBattery-0\t100%; charged"),
        Some(false)
    );
    assert_eq!(parse_pmset("Now drawing from 'UPS Power'\n"), None);
    assert_eq!(parse_pmset(""), None);
    // Only the first line counts.
    assert_eq!(parse_pmset("nothing\n'Battery Power'"), None);
}

#[test]
fn macos_uses_pmset_and_another_system_gives_none() {
    let dir = tempfile::tempdir().unwrap();
    let on = on_battery("macos", false, dir.path(), || {
        Some("Now drawing from 'Battery Power'".to_owned())
    });
    assert_eq!(on, Some(true));
    assert_eq!(on_battery("macos", false, dir.path(), || None), None);
    assert_eq!(on_battery("freebsd", false, dir.path(), || None), None);
}

#[test]
fn read_reports_the_system_and_the_labels() {
    let traits = read(&["gpu".to_owned()]);
    assert_eq!(traits.os, std::env::consts::OS);
    assert_eq!(traits.arch, std::env::consts::ARCH);
    assert_eq!(traits.labels, ["gpu"]);
}

#[test]
fn the_runtime_reads_the_traits_at_most_every_60_seconds() {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    use crate::fleet_git_runtime::{FleetGitRuntime, TRAITS_EVERY};
    use crate::manager::Policy;
    use crate::store::OfflineApprovalPolicy;

    let runtime = FleetGitRuntime::new(std::sync::Arc::new(crate::workspace::GitOptions::isolated));
    let policy = Policy::new(
        vec![std::path::PathBuf::from("/")],
        1,
        OfflineApprovalPolicy::Wait,
        std::collections::BTreeMap::new(),
    )
    .with_traits(vec!["gpu".to_owned()]);
    let reads = Cell::new(0);
    let reader = |labels: &[String]| {
        reads.set(reads.get() + 1);
        StationTraits {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            on_battery: Some(reads.get() % 2 == 0),
            labels: labels.to_vec(),
        }
    };
    let start = Instant::now();
    let first = runtime.traits_at(&policy, start, reader);
    assert_eq!(first.labels, ["gpu"]);
    // Inside the window the stored traits come back, with no new read.
    let soon = start + TRAITS_EVERY - Duration::from_secs(1);
    let again = runtime.traits_at(&policy, soon, |_| panic!("no new read"));
    assert_eq!(again, first);
    // After the window the reader runs again.
    let later = start + TRAITS_EVERY;
    let fresh = runtime.traits_at(&policy, later, reader);
    assert_eq!(reads.get(), 2);
    assert_ne!(fresh.on_battery, first.on_battery);
}
