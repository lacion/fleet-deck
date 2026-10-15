//! The launchd plans: the commands in the order that they run (§10).

use std::os::unix::fs::PermissionsExt as _;

use super::{Rig, text};
use crate::cli::ServiceCommand;
use crate::service::Kind;
use crate::service::fake::{fail, is_read_only, ok};

const UP: &str = "gui/1000/dev.fleetdeck.up";

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn loaded(rig: &Rig, kind: Kind, state: &str) {
    let target = format!("gui/1000/{}", kind.label());
    rig.fake.always(
        "launchctl",
        &["print", &target],
        ok(&format!("{} = {{\n\tstate = {state}\n}}\n", kind.label())),
    );
}

#[test]
fn the_first_install_of_both_enables_and_bootstraps() {
    let rig = Rig::launchd();
    rig.sockets(Kind::Both, &[false, false, true]);
    let (result, out) = rig.install(Kind::Both);
    result.unwrap();
    let plist = rig.file(Kind::Both);
    assert_eq!(
        rig.fake.lines(),
        [
            format!("launchctl print {UP}"),
            "launchctl print gui/1000".to_owned(),
            format!("launchctl enable {UP}"),
            format!("launchctl bootstrap gui/1000 {}", plist.display()),
        ]
    );
    // Modes: file 0644, log directory 0700, log file 0600.
    let logs = rig.ctx.home.join("Library/Logs/fleetdeck");
    assert_eq!(mode(&plist), 0o644);
    assert_eq!(mode(&logs), 0o700);
    assert_eq!(mode(&logs.join("up.log")), 0o600);
    assert_eq!(std::fs::read(logs.join("up.log")).unwrap(), b"");
    let root = rig.ctx.dirs.station().join("bin");
    let copy = std::fs::read_link(root.join("current")).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), super::BUILD);
    let expect = format!(
        "Installed the both service: {}\n\
         Program: {} -> {}\n\
         Data directory: {}\n\
         Provider host: direct. The station ends its agents when the service stops. Set host = \
         \"shim\" in station.toml to keep them running.\n\
         Started.\n\
         Logs: {}\n\
         A user agent starts when you log in and ends when you log out.\n\
         Run `fleetdeck service install` again after you install a new Fleet Deck version or a \
         provider CLI in a new folder.\n",
        plist.display(),
        root.join("current").display(),
        copy.display(),
        rig.data(),
        logs.join("up.log").display(),
    );
    assert_eq!(out, expect);
}

#[test]
fn a_loaded_job_is_booted_out_before_the_file_is_written() {
    let rig = Rig::launchd();
    loaded(&rig, Kind::Deck, "running");
    rig.sockets(Kind::Deck, &[true]);
    let (result, _) = rig.install(Kind::Deck);
    result.unwrap();
    let plist = rig.file(Kind::Deck);
    assert_eq!(
        rig.fake.changes(),
        [
            "launchctl bootout gui/1000/dev.fleetdeck.deck".to_owned(),
            "launchctl enable gui/1000/dev.fleetdeck.deck".to_owned(),
            format!("launchctl bootstrap gui/1000 {}", plist.display()),
        ]
    );
}

#[test]
fn a_bootout_that_says_not_loaded_is_ignored_and_another_error_fails() {
    for answer in [
        fail(3, ""),
        fail(113, ""),
        fail(1, "Boot-out failed: No such process"),
        fail(1, "Could not find service \"x\" in domain"),
    ] {
        let rig = Rig::launchd();
        loaded(&rig, Kind::Both, "waiting");
        rig.fake.always("launchctl", &["bootout"], answer);
        rig.sockets(Kind::Both, &[false, false, true]);
        let (result, out) = rig.install(Kind::Both);
        result.unwrap();
        assert!(out.contains("Started.\n"));
    }
    let rig = Rig::launchd();
    loaded(&rig, Kind::Both, "waiting");
    rig.fake.always(
        "launchctl",
        &["bootout"],
        fail(5, "Boot-out failed: 5: Input/output error"),
    );
    let (result, out) = rig.install(Kind::Both);
    assert_eq!(
        text(result),
        format!("`launchctl bootout {UP}` failed (exit 5): Boot-out failed: 5: Input/output error")
    );
    assert_eq!(out, "");
    assert!(!rig.file(Kind::Both).exists());
    assert!(!rig.fake.lines().iter().any(|l| l.contains("bootstrap")));
}

#[test]
fn without_a_graphical_login_the_file_is_written_and_nothing_is_loaded() {
    let rig = Rig::launchd();
    rig.fake.always(
        "launchctl",
        &["print", "gui/1000"],
        fail(113, "Could not find domain"),
    );
    let (result, out) = rig.install(Kind::Station);
    result.unwrap();
    assert!(rig.file(Kind::Station).exists());
    assert!(rig.fake.changes().is_empty());
    assert!(out.contains("Installed. It starts at your next graphical login.\n"));
    assert!(!out.contains("Started."));
    assert!(out.contains("A user agent starts when you log in"));
    assert_eq!(
        mode(&rig.ctx.home.join("Library/Logs/fleetdeck/station.log")),
        0o600
    );
}

#[test]
fn another_process_that_answers_gets_the_file_and_enable_but_no_bootstrap() {
    let rig = Rig::launchd();
    rig.sockets(Kind::Both, &[true]);
    let (result, out) = rig.install(Kind::Both);
    result.unwrap();
    assert!(rig.file(Kind::Both).exists());
    assert_eq!(rig.fake.changes(), [format!("launchctl enable {UP}")]);
    assert!(out.contains(
        "Installed for automatic start. Fleet Deck runs now in another process. Stop it \
         (Ctrl-C in its terminal), then run `fleetdeck service start`.\n"
    ));
    assert!(!out.contains("Started."));
}

#[test]
fn a_service_that_never_answers_is_an_error_with_the_log_path() {
    let rig = Rig::launchd();
    let (result, _) = rig.install(Kind::Deck);
    let log = rig.ctx.home.join("Library/Logs/fleetdeck/deck.log");
    assert_eq!(
        text(result),
        format!(
            "The service did not start. Look at its log: {}",
            log.display()
        )
    );
}

#[test]
fn conflicts_and_another_data_directory_change_nothing() {
    let rig = Rig::launchd();
    rig.put_installed(Kind::Station);
    assert_eq!(
        text(rig.install(Kind::Both).0),
        "The station service is installed. Remove it first: `fleetdeck service uninstall station`."
    );
    let rig = Rig::launchd();
    rig.put_file(Kind::Deck, Some("/elsewhere"));
    assert_eq!(
        text(rig.install(Kind::Deck).0),
        "The deck service runs Fleet Deck with the data directory /elsewhere. Remove it first: \
         `fleetdeck service uninstall deck`."
    );
    // A marker that does not decode is `unknown`.
    let rig = Rig::launchd();
    let path = rig.file(Kind::Deck);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "<!-- fleetdeck-data-hex: zz -->\n").unwrap();
    assert!(text(rig.install(Kind::Deck).0).contains("with the data directory unknown."));
    assert!(rig.fake.changes().is_empty());
}

#[test]
fn start_enables_then_bootstraps_kickstarts_or_does_nothing() {
    // Not loaded: bootstrap.
    let rig = Rig::launchd();
    rig.put_installed(Kind::Deck);
    rig.sockets(Kind::Deck, &[false, true]);
    let (result, out) = rig.run(&ServiceCommand::Start { kind: None });
    result.unwrap();
    assert_eq!(out, "Started the deck service.\n");
    assert_eq!(
        rig.fake.changes(),
        [
            "launchctl enable gui/1000/dev.fleetdeck.deck".to_owned(),
            format!(
                "launchctl bootstrap gui/1000 {}",
                rig.file(Kind::Deck).display()
            ),
        ]
    );
    // Loaded but not running: kickstart.
    let rig = Rig::launchd();
    rig.put_installed(Kind::Deck);
    loaded(&rig, Kind::Deck, "waiting");
    rig.sockets(Kind::Deck, &[false, true]);
    rig.run(&ServiceCommand::Start { kind: None }).0.unwrap();
    assert_eq!(
        rig.fake.changes(),
        [
            "launchctl enable gui/1000/dev.fleetdeck.deck",
            "launchctl kickstart gui/1000/dev.fleetdeck.deck"
        ]
    );
    // Loaded and running: only enable.
    let rig = Rig::launchd();
    rig.put_installed(Kind::Deck);
    loaded(&rig, Kind::Deck, "running");
    rig.sockets(Kind::Deck, &[true]);
    rig.run(&ServiceCommand::Start { kind: None }).0.unwrap();
    assert_eq!(
        rig.fake.changes(),
        ["launchctl enable gui/1000/dev.fleetdeck.deck"]
    );
    // Another process answers: an error and no change.
    let rig = Rig::launchd();
    rig.put_installed(Kind::Deck);
    rig.sockets(Kind::Deck, &[true]);
    assert!(
        text(rig.run(&ServiceCommand::Start { kind: None }).0)
            .starts_with("Fleet Deck runs now in another process.")
    );
    assert!(rig.fake.changes().is_empty());
}

#[test]
fn stop_boots_out_and_uninstall_boots_out_and_removes_the_file() {
    let rig = Rig::launchd();
    rig.put_installed(Kind::Station);
    let (result, out) = rig.run(&ServiceCommand::Stop { kind: None });
    result.unwrap();
    assert_eq!(
        out,
        "Stopped the station service. It starts again at the next boot or login.\n"
    );
    assert_eq!(
        rig.fake.changes(),
        ["launchctl bootout gui/1000/dev.fleetdeck.station"]
    );
    assert!(rig.file(Kind::Station).exists());
    // `bootout` that says not loaded is no error.
    rig.fake.always("launchctl", &["bootout"], fail(113, ""));
    let (result, out) = rig.run(&ServiceCommand::Uninstall {
        kind: Some(Kind::Station),
    });
    result.unwrap();
    assert!(!rig.file(Kind::Station).exists());
    assert_eq!(
        out,
        format!(
            "Removed the station service.\nYour data and the program copies in {} stay.\n",
            rig.data()
        )
    );
    // Another bootout error fails and the file stays.
    rig.put_installed(Kind::Station);
    rig.fake.always("launchctl", &["bootout"], fail(5, "boom"));
    assert_eq!(
        text(rig.run(&ServiceCommand::Uninstall { kind: None }).0),
        "`launchctl bootout gui/1000/dev.fleetdeck.station` failed (exit 5): boom"
    );
    assert!(rig.file(Kind::Station).exists());
}

#[test]
fn status_has_no_enabled_and_no_linger() {
    let rig = Rig::launchd();
    rig.put_installed(Kind::Both);
    loaded(&rig, Kind::Both, "running");
    let (result, out) = rig.run(&ServiceCommand::Status { json: false });
    result.unwrap();
    assert!(out.contains("  running: yes\n"));
    assert!(!out.contains("enabled:"));
    assert!(!out.contains("linger"));
    assert!(out.contains(&format!(
        "  logs: {}\n",
        rig.ctx.home.join("Library/Logs/fleetdeck/up.log").display()
    )));
    let (result, out) = rig.run(&ServiceCommand::Status { json: true });
    result.unwrap();
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["platform"], "launchd");
    assert_eq!(value["linger"], serde_json::Value::Null);
    assert_eq!(value["wsl_task"], serde_json::Value::Null);
    assert_eq!(value["services"][0]["enabled"], serde_json::Value::Null);
    assert_eq!(value["services"][0]["active"], true);
    // Only the job of the installed kind is asked.
    assert!(
        rig.fake
            .lines()
            .iter()
            .all(|l| l == &format!("launchctl print {UP}"))
    );
}

#[test]
fn the_dry_run_changes_no_file_and_runs_only_read_only_commands() {
    let rig = Rig::launchd();
    loaded(&rig, Kind::Both, "running");
    let (result, out) = rig.dry_run(Kind::Both);
    result.unwrap();
    for line in rig.fake.lines() {
        assert!(is_read_only(&line), "{line}");
    }
    assert!(!rig.ctx.home.exists());
    assert!(!rig.ctx.dirs.data.exists());
    let plist = rig.file(Kind::Both);
    assert!(out.starts_with(&format!("{}\n<?xml", plist.display())));
    assert!(out.contains(&format!(
        "$ launchctl bootout {UP}\n$ launchctl enable {UP}\n"
    )));
    assert!(out.ends_with(&format!(
        "$ launchctl bootstrap gui/1000 {}\n",
        plist.display()
    )));
    // No graphical login: no command line.
    let rig = Rig::launchd();
    rig.fake
        .always("launchctl", &["print", "gui/1000"], fail(113, ""));
    let (result, out) = rig.dry_run(Kind::Deck);
    result.unwrap();
    assert!(!out.contains("$ launchctl"));
}
