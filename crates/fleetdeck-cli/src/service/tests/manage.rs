//! The other systemd commands: uninstall, start, stop, status and `--dry-run`.

use std::path::PathBuf;

use fleetdeck_core::tailscale_wsl::POWERSHELL;

use super::{BUILD, Rig, set_mtime, text, wsl_rig};
use crate::cli::ServiceCommand;
use crate::service::Kind;
use crate::service::fake::{fail, is_read_only, ok};

#[test]
fn uninstall_of_the_last_service_removes_the_task_and_of_one_of_two_does_not() {
    let rig = wsl_rig();
    rig.put_installed(Kind::Deck);
    rig.put_installed(Kind::Station);
    let (result, out) = rig.run(&ServiceCommand::Uninstall {
        kind: Some(Kind::Deck),
    });
    result.unwrap();
    assert_eq!(
        rig.fake.lines(),
        [
            "systemctl --user disable --now fleetdeck-deck.service",
            "systemctl --user daemon-reload"
        ]
    );
    assert!(!rig.file(Kind::Deck).exists());
    assert!(rig.file(Kind::Station).exists());
    assert_eq!(
        out,
        format!(
            "Removed the deck service.\nYour data and the program copies in {} stay.\n",
            rig.data()
        )
    );
    // The last one: the task goes.
    let (result, out) = rig.run(&ServiceCommand::Uninstall { kind: None });
    result.unwrap();
    let call = rig
        .fake
        .calls()
        .into_iter()
        .find(|c| c.program == POWERSHELL)
        .unwrap();
    assert_eq!(
        call.stdin.unwrap(),
        crate::service::wsl_task::remove_script("Ubuntu")
    );
    assert!(out.starts_with("Removed the station service.\n"));
    assert!(!rig.file(Kind::Station).exists());
}

#[test]
fn a_task_that_cannot_be_removed_is_a_warning() {
    let rig = wsl_rig();
    rig.fake.always(POWERSHELL, &[], fail(1, "no rights"));
    rig.put_installed(Kind::Both);
    let (result, out) = rig.run(&ServiceCommand::Uninstall { kind: None });
    result.unwrap();
    assert_eq!(
        out,
        format!(
            "Removed the both service.\n\
             Fleet Deck could not remove the Windows logon task (no rights). Remove the task \
             folder \"Fleet Deck\" in the Windows Task Scheduler.\n\
             Your data and the program copies in {} stay.\n",
            rig.data()
        )
    );
}

#[test]
fn uninstall_texts_and_failures() {
    let rig = Rig::systemd();
    let none = "No Fleet Deck service is installed. Run `fleetdeck service install`.";
    for command in [
        ServiceCommand::Uninstall { kind: None },
        ServiceCommand::Start { kind: None },
        ServiceCommand::Stop { kind: None },
    ] {
        assert_eq!(text(rig.run(&command).0), none);
    }
    assert_eq!(
        text(
            rig.run(&ServiceCommand::Uninstall {
                kind: Some(Kind::Deck)
            })
            .0
        ),
        "The deck service is not installed."
    );
    assert!(rig.fake.calls().is_empty());
    // `disable --now` fails: an error, the file stays.
    rig.put_file(Kind::Deck, Some("/other"));
    rig.fake
        .once("systemctl", &["--user", "disable"], fail(1, "busy"));
    assert_eq!(
        text(rig.run(&ServiceCommand::Uninstall { kind: None }).0),
        "`systemctl --user disable --now fleetdeck-deck.service` failed (exit 1): busy"
    );
    assert!(rig.file(Kind::Deck).exists());
    // The data directory of the marker is named when known.
    let (result, out) = rig.run(&ServiceCommand::Uninstall { kind: None });
    result.unwrap();
    assert!(out.ends_with("Your data and the program copies in /other stay.\n"));
    // A failed reload: the files are gone.
    rig.put_installed(Kind::Deck);
    rig.fake
        .once("systemctl", &["--user", "daemon-reload"], fail(1, "no bus"));
    let (result, _) = rig.run(&ServiceCommand::Uninstall { kind: None });
    assert_eq!(
        text(result),
        "The service files are removed, but `systemctl --user daemon-reload` failed (exit 1): no bus"
    );
    assert!(!rig.file(Kind::Deck).exists());
}

#[test]
fn start_and_stop() {
    let rig = Rig::systemd();
    rig.put_installed(Kind::Deck);
    rig.put_installed(Kind::Station);
    rig.sockets(Kind::Deck, &[false, true]);
    rig.sockets(Kind::Station, &[false, true]);
    let (result, out) = rig.run(&ServiceCommand::Start { kind: None });
    result.unwrap();
    assert_eq!(
        out,
        "Started the deck service.\nStarted the station service.\n"
    );
    assert_eq!(
        rig.fake.changes(),
        [
            "systemctl --user start fleetdeck-deck.service",
            "systemctl --user start fleetdeck-station.service"
        ]
    );
    let rig2 = Rig::systemd();
    rig2.put_installed(Kind::Station);
    let (result, out) = rig2.run(&ServiceCommand::Stop {
        kind: Some(Kind::Station),
    });
    result.unwrap();
    assert_eq!(
        out,
        "Stopped the station service. It starts again at the next boot or login.\n"
    );
    assert_eq!(
        rig2.fake.lines(),
        ["systemctl --user stop fleetdeck-station.service"]
    );
}

/// A station that an install made, then a `fleetdeck` with other bytes whose
/// modification time is `exe_secs`; the copy has the time 1000.
fn station_with_other_exe(exe_secs: u64) -> (Rig, PathBuf) {
    let rig = Rig::systemd();
    rig.sockets(Kind::Station, &[false, false, true]);
    rig.install(Kind::Station).0.unwrap();
    let root = rig.ctx.dirs.station().join("bin");
    let copy = std::fs::read_link(root.join("current")).unwrap();
    std::fs::write(&rig.ctx.exe, b"binary-two").unwrap();
    set_mtime(&copy, 1000);
    set_mtime(&rig.ctx.exe, exe_secs);
    (rig, copy)
}

#[test]
fn start_installs_a_newer_program_first() {
    let (rig, old_copy) = station_with_other_exe(2000);
    // One probe of the start, then the probes of the install.
    rig.sockets(Kind::Station, &[false, false, false, true]);
    let before = rig.fake.changes().len();
    let (result, out) = rig.run(&ServiceCommand::Start {
        kind: Some(Kind::Station),
    });
    result.unwrap();
    assert!(
        out.starts_with(
            "This fleetdeck is newer than the program of the station service. Installing it.\n\
             Installed the station service: "
        ),
        "{out}"
    );
    assert!(out.contains("Started.\n"));
    // No second start after the install.
    assert!(!out.contains("Started the station service."));
    let root = rig.ctx.dirs.station().join("bin");
    let copy = std::fs::read_link(root.join("current")).unwrap();
    assert_ne!(copy, old_copy);
    assert_eq!(std::fs::read(&copy).unwrap(), b"binary-two");
    let changes = rig.fake.changes();
    assert_eq!(
        changes[before..].last().unwrap(),
        "systemctl --user restart fleetdeck-station.service"
    );
    assert!(changes[before..].iter().all(|c| !c.contains(" start ")));
}

#[test]
fn start_with_a_newer_program_and_another_process_changes_nothing() {
    let (rig, copy) = station_with_other_exe(2000);
    rig.sockets(Kind::Station, &[true]);
    let before = rig.fake.changes().len();
    let (result, _) = rig.run(&ServiceCommand::Start {
        kind: Some(Kind::Station),
    });
    assert!(result.is_err());
    let root = rig.ctx.dirs.station().join("bin");
    assert_eq!(std::fs::read_link(root.join("current")).unwrap(), copy);
    assert_eq!(rig.fake.changes().len(), before);
}

#[test]
fn start_keeps_the_copy_when_this_fleetdeck_is_older() {
    let (rig, copy) = station_with_other_exe(500);
    rig.sockets(Kind::Station, &[false, true]);
    let before = rig.fake.changes().len();
    let (result, out) = rig.run(&ServiceCommand::Start {
        kind: Some(Kind::Station),
    });
    result.unwrap();
    assert_eq!(
        out,
        format!(
            "Note: this fleetdeck ({}) is older than the program of the station service. The \
             service keeps its program. Run `fleetdeck service install` to use this one.\n\
             Started the station service.\n",
            rig.ctx.exe.display()
        )
    );
    let root = rig.ctx.dirs.station().join("bin");
    assert_eq!(std::fs::read_link(root.join("current")).unwrap(), copy);
    assert_eq!(std::fs::read(&copy).unwrap(), BUILD);
    assert_eq!(
        rig.fake.changes()[before..],
        ["systemctl --user start fleetdeck-station.service"]
    );
}

#[test]
fn start_with_another_process_fails_and_a_silent_service_times_out() {
    let rig = Rig::systemd();
    rig.put_installed(Kind::Both);
    rig.sockets(Kind::Both, &[true]);
    assert_eq!(
        text(rig.run(&ServiceCommand::Start { kind: None }).0),
        "Fleet Deck runs now in another process. Stop it (Ctrl-C in its terminal), then run \
         `fleetdeck service start`."
    );
    assert!(rig.fake.changes().is_empty());
    let rig = Rig::systemd();
    rig.put_installed(Kind::Both);
    assert_eq!(
        text(rig.run(&ServiceCommand::Start { kind: None }).0),
        "The service did not start. Look at its log: journalctl --user -u fleetdeck-up.service"
    );
}

#[test]
fn status_text_and_json() {
    let mut rig = wsl_rig();
    rig.put_installed(Kind::Both);
    rig.fake
        .always("systemctl", &["--user", "is-active"], ok("active\n"));
    rig.fake.always("loginctl", &["show-user"], ok("yes\n"));
    rig.fake.always(POWERSHELL, &[], ok("state=Running\n"));
    let root = rig.ctx.dirs.station().join("bin");
    program_link(&root);
    let (result, out) = rig.run(&ServiceCommand::Status { json: false });
    result.unwrap();
    let file = rig.file(Kind::Both);
    let expect = format!(
        "both: installed\n  file: {}\n  data directory: {}\n  program: {} -> {}\n  running: yes\n  \
         enabled: yes\n  logs: journalctl --user -u fleetdeck-up.service\n\
         deck: not installed\nstation: not installed\nlinger: on\nWindows logon task: Running\n",
        file.display(),
        rig.data(),
        root.join("current").display(),
        root.join("releases/x/fleetdeck").display(),
    );
    assert_eq!(out, expect);
    // Only installed kinds run commands; the status script runs once.
    assert_eq!(
        rig.fake.lines(),
        [
            "systemctl --user is-active fleetdeck-up.service",
            "systemctl --user is-enabled fleetdeck-up.service",
            "loginctl show-user 1000 --property=Linger --value",
            &format!(
                "{POWERSHELL} -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command -"
            ),
        ]
    );
    let (result, out) = rig.run(&ServiceCommand::Status { json: true });
    result.unwrap();
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["platform"], "systemd");
    assert_eq!(value["linger"], true);
    assert_eq!(value["wsl_task"], "Running");
    assert_eq!(value["services"][0]["kind"], "both");
    assert_eq!(value["services"][0]["installed"], true);
    assert_eq!(value["services"][0]["active"], true);
    assert_eq!(value["services"][0]["enabled"], true);
    assert_eq!(value["services"][0]["data_dir"], rig.data());
    assert_eq!(value["services"][1]["kind"], "deck");
    assert_eq!(value["services"][1]["installed"], false);
    assert_eq!(value["services"][1]["data_dir"], serde_json::Value::Null);
    assert_eq!(value["services"][1]["enabled"], serde_json::Value::Null);
    assert_eq!(value["services"][1]["active"], false);
    assert_eq!(value["services"][2]["kind"], "station");
    assert_eq!(
        value["logs"]["both"],
        "journalctl --user -u fleetdeck-up.service"
    );
    assert!(value["logs"].get("deck").is_none());
    // A kind without a program link: `missing`; no marker: `unknown`.
    rig.ctx.wsl = false;
    rig.put_file(Kind::Deck, None);
    let (result, out) = rig.run(&ServiceCommand::Status { json: false });
    result.unwrap();
    assert!(out.contains("  data directory: unknown\n"));
    assert!(out.contains("deck/bin/current -> missing\n"));
    assert!(!out.contains("Windows logon task"));
}

#[test]
fn status_says_when_this_fleetdeck_is_newer_than_the_service_program() {
    let rig = Rig::systemd();
    rig.put_installed(Kind::Station);
    let root = rig.ctx.dirs.station().join("bin");
    program_link(&root);
    // The copy has the bytes `x`; the build is later.
    set_mtime(&root.join("releases/x/fleetdeck"), 1000);
    set_mtime(&rig.ctx.exe, 2000);
    let (result, out) = rig.run(&ServiceCommand::Status { json: false });
    result.unwrap();
    assert!(
        out.contains(
            "  running: no\n  Update: this fleetdeck is newer than the service program. Run \
             `fleetdeck service start` or `fleetdeck service install`.\n"
        ),
        "{out}"
    );
    assert_eq!(out.matches("Update:").count(), 1);
    let (result, out) = rig.run(&ServiceCommand::Status { json: true });
    result.unwrap();
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["services"][2]["kind"], "station");
    assert_eq!(value["services"][2]["program_update"], "newer");
    // A kind that is not installed has no field.
    assert!(value["services"][1].get("program_update").is_none());
}

fn program_link(root: &std::path::Path) {
    let target = root.join("releases/x/fleetdeck");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, b"x").unwrap();
    std::os::unix::fs::symlink(&target, root.join("current")).unwrap();
}

#[test]
fn status_of_the_task_is_unknown_without_powershell_or_with_another_answer() {
    let mut rig = wsl_rig();
    rig.ctx.powershell = None;
    let (result, out) = rig.run(&ServiceCommand::Status { json: false });
    result.unwrap();
    assert!(out.ends_with("Windows logon task: unknown\n"));
    let rig = wsl_rig();
    rig.fake.always(POWERSHELL, &[], ok("absent\n"));
    assert!(
        rig.run(&ServiceCommand::Status { json: false })
            .1
            .ends_with("Windows logon task: absent\n")
    );
    let rig = wsl_rig();
    rig.fake.always(POWERSHELL, &[], ok("odd\n"));
    assert!(
        rig.run(&ServiceCommand::Status { json: false })
            .1
            .ends_with("Windows logon task: unknown\n")
    );
    let rig = wsl_rig();
    rig.fake.always(POWERSHELL, &[], fail(1, "x"));
    assert!(
        rig.run(&ServiceCommand::Status { json: false })
            .1
            .ends_with("Windows logon task: unknown\n")
    );
    // `loginctl` fails: linger unknown.
    let rig = Rig::systemd();
    rig.fake.always("loginctl", &["show-user"], fail(1, "x"));
    assert!(
        rig.run(&ServiceCommand::Status { json: false })
            .1
            .ends_with("linger: unknown\n")
    );
}

#[test]
fn the_dry_run_changes_no_file_and_runs_only_read_only_commands() {
    let mut rig = wsl_rig();
    rig.fake.always("loginctl", &["show-user"], ok("no\n"));
    let before = snapshot(rig.tmp.path());
    let (result, out) = rig.dry_run(Kind::Both);
    result.unwrap();
    assert_eq!(snapshot(rig.tmp.path()), before);
    for line in rig.fake.lines() {
        assert!(is_read_only(&line), "{line}");
    }
    let root = rig.ctx.dirs.station().join("bin");
    let file = rig.file(Kind::Both);
    let unit =
        crate::service::systemd::render(&rig.ctx, Kind::Both, &root.join("current")).unwrap();
    let expect = format!(
        "{}\n{unit}\
         $ (copy {} to {} and point {} at it)\n\
         $ systemctl --user daemon-reload\n\
         $ systemctl --user enable fleetdeck-up.service\n\
         $ loginctl --no-ask-password enable-linger 1000\n\
         $ {POWERSHELL} -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command -   \
         (registers and starts the Windows logon task \"\\Fleet Deck\\WSL Ubuntu <SID>\")\n\
         $ systemctl --user restart fleetdeck-up.service\n",
        file.display(),
        rig.ctx.exe.display(),
        root.join("releases/1.2.3-svc-<ulid>").display(),
        root.join("current").display(),
    );
    assert_eq!(out, expect);
    // The WSL task that would be skipped prints its warning.
    rig.ctx.powershell = None;
    let (_, out) = rig.dry_run(Kind::Both);
    assert!(out.contains("Fleet Deck could not add the Windows logon task (the Windows PowerShell program did not pass the path checks)."));
    assert!(!out.contains("(registers and starts"));
}

#[test]
fn the_dry_run_has_no_copy_line_for_the_same_binary_and_no_restart_for_another_process() {
    let rig = Rig::systemd();
    rig.sockets(Kind::Both, &[true]);
    crate::service::program::ensure(
        &rig.ctx.dirs.station().join("bin"),
        &rig.ctx.exe,
        "1.2.3",
        "01aaa",
    )
    .unwrap();
    let (result, out) = rig.dry_run(Kind::Both);
    result.unwrap();
    assert!(!out.contains("(copy"));
    assert!(!out.contains("restart"));
    assert!(out.contains("$ systemctl --user enable fleetdeck-up.service\n"));
    // Refusals apply to a dry run too.
    let rig = Rig::systemd();
    rig.put_installed(Kind::Deck);
    assert!(text(rig.dry_run(Kind::Both).0).starts_with("The deck service is installed."));
}

/// Every path under `root` with its bytes.
fn snapshot(root: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                todo.push(path.clone());
                found.push((path, Vec::new()));
            } else {
                found.push((path.clone(), std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    found.sort();
    found
}
