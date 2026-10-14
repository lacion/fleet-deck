//! The systemd plans: the commands in the order that they run (§10).

use fleetdeck_core::config::HostChoice;
use fleetdeck_core::tailscale_wsl::POWERSHELL;

use super::{Rig, text, wsl_rig};
use crate::service::Kind;
use crate::service::fake::{fail, ok};
use crate::service::runner::RunError;

const UNIT: &str = "fleetdeck-up.service";

#[test]
fn the_first_install_of_both_copies_reloads_enables_restarts_and_turns_linger_on() {
    let rig = Rig::systemd();
    rig.fake.always("loginctl", &["show-user"], ok("no\n"));
    rig.sockets(Kind::Both, &[false, false, true]);
    let (result, out) = rig.install(Kind::Both);
    result.unwrap();
    assert_eq!(
        rig.fake.lines(),
        [
            "systemctl --user show --property=UnitPath --value",
            "systemctl --user is-active fleetdeck-up.service",
            "systemctl --user daemon-reload",
            "systemctl --user enable fleetdeck-up.service",
            "loginctl show-user 1000 --property=Linger --value",
            "loginctl --no-ask-password enable-linger 1000",
            "systemctl --user is-active fleetdeck-up.service",
            "systemctl --user restart fleetdeck-up.service",
        ]
    );
    // The file and the program copy exist.
    let file = rig.file(Kind::Both);
    let unit = std::fs::read_to_string(&file).unwrap();
    let root = rig.ctx.dirs.station().join("bin");
    assert!(unit.contains(&format!(
        "ExecStart=\"{}\" \"up\"\n",
        root.join("current").display()
    )));
    let copy = std::fs::read_link(root.join("current")).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), super::BUILD);
    let expect = format!(
        "Installed the both service: {}\n\
         Program: {} -> {}\n\
         Data directory: {}\n\
         Provider host: direct. The station ends its agents when the service stops. Set host = \
         \"shim\" in station.toml to keep them running.\n\
         Linger is on for luis: your user services start at boot, without a login.\n\
         Started.\n\
         Logs: journalctl --user -u fleetdeck-up.service\n\
         The service runs a copy of this program. Run `fleetdeck service install` again after \
         you build or install a new version.\n\
         The service uses the PATH of this shell. Run `fleetdeck service install` again after \
         you install a provider CLI in a new directory.\n",
        file.display(),
        root.join("current").display(),
        copy.display(),
        rig.data(),
    );
    assert_eq!(out, expect);
}

#[test]
fn an_install_again_with_the_same_binary_makes_no_copy_and_restarts() {
    let rig = Rig::systemd();
    rig.sockets(Kind::Both, &[false, false, true]);
    rig.install(Kind::Both).0.unwrap();
    let root = rig.ctx.dirs.station().join("bin");
    let first = std::fs::read_link(root.join("current")).unwrap();
    // Now the service is active and its sockets answer.
    rig.sockets(Kind::Both, &[true]);
    rig.fake
        .always("systemctl", &["--user", "is-active"], ok("active\n"));
    let before = rig.fake.lines().len();
    let (result, out) = rig.install(Kind::Both);
    result.unwrap();
    assert_eq!(std::fs::read_link(root.join("current")).unwrap(), first);
    assert_eq!(std::fs::read_dir(root.join("releases")).unwrap().count(), 1);
    let again = &rig.fake.lines()[before..];
    assert_eq!(
        again.last().unwrap(),
        "systemctl --user restart fleetdeck-up.service"
    );
    assert_eq!(
        again.iter().filter(|l| l.contains("restart")).count(),
        1,
        "{again:?}"
    );
    assert!(out.contains("Started.\n"));
}

#[test]
fn the_host_line_names_shim_and_deck_has_no_host_line() {
    let mut rig = Rig::systemd();
    rig.ctx.host = Some(HostChoice::Shim);
    rig.sockets(Kind::Station, &[false, false, true]);
    let (result, out) = rig.install(Kind::Station);
    result.unwrap();
    assert!(
        out.contains(
            "Provider host: shim. Agents keep running through a service stop or restart.\n"
        )
    );
    let rig = Rig::systemd();
    rig.sockets(Kind::Deck, &[false, false, true]);
    let (result, out) = rig.install(Kind::Deck);
    result.unwrap();
    assert!(!out.contains("Provider host"));
    // No host known: the line is left out.
    let mut rig = Rig::systemd();
    rig.ctx.host = None;
    rig.sockets(Kind::Station, &[false, false, true]);
    assert!(!rig.install(Kind::Station).1.contains("Provider host"));
}

#[test]
fn a_conflict_changes_nothing_and_runs_no_command() {
    let rig = Rig::systemd();
    rig.put_installed(Kind::Deck);
    let (result, out) = rig.install(Kind::Both);
    assert_eq!(
        text(result),
        "The deck service is installed. Remove it first: `fleetdeck service uninstall deck`."
    );
    assert_eq!(out, "");
    assert!(rig.fake.changes().is_empty());
    assert!(!rig.file(Kind::Both).exists());
    // `deck` over `both`.
    let rig = Rig::systemd();
    rig.put_installed(Kind::Both);
    assert_eq!(
        text(rig.install(Kind::Deck).0),
        "The both service is installed. Remove it first: `fleetdeck service uninstall both`."
    );
    assert!(rig.fake.changes().is_empty());
    assert!(!rig.file(Kind::Deck).exists());
}

#[test]
fn another_data_directory_changes_nothing_and_runs_no_command() {
    let rig = Rig::systemd();
    rig.put_file(Kind::Station, Some("/elsewhere"));
    let (result, _) = rig.install(Kind::Station);
    assert_eq!(
        text(result),
        "The station service runs Fleet Deck with the data directory /elsewhere. Remove it \
         first: `fleetdeck service uninstall station`."
    );
    assert!(rig.fake.changes().is_empty());
    // A file without a marker line is another data directory: "unknown".
    let rig = Rig::systemd();
    rig.put_file(Kind::Station, None);
    assert_eq!(
        text(rig.install(Kind::Station).0),
        "The station service runs Fleet Deck with the data directory unknown. Remove it \
         first: `fleetdeck service uninstall station`."
    );
    assert!(rig.fake.changes().is_empty());
    assert!(!rig.ctx.dirs.station().join("bin").exists());
}

#[test]
fn another_process_that_answers_gets_the_files_and_enable_but_no_start() {
    let rig = Rig::systemd();
    rig.sockets(Kind::Both, &[true]);
    let (result, out) = rig.install(Kind::Both);
    result.unwrap();
    assert!(rig.file(Kind::Both).exists());
    let changes = rig.fake.changes();
    assert_eq!(
        changes,
        [
            "systemctl --user daemon-reload",
            "systemctl --user enable fleetdeck-up.service"
        ]
    );
    assert!(out.contains(
        "Installed for automatic start. Fleet Deck runs now in another process. Stop it \
         (Ctrl-C in its terminal), then run `fleetdeck service start`.\n"
    ));
    assert!(!out.contains("Started."));
    assert!(out.contains("Logs: journalctl --user -u fleetdeck-up.service\n"));
}

#[test]
fn a_service_that_never_answers_is_an_error_after_the_summary() {
    let rig = Rig::systemd();
    let (result, out) = rig.install(Kind::Deck);
    assert_eq!(
        text(result),
        "The service did not start. Look at its log: journalctl --user -u fleetdeck-deck.service"
    );
    // The limit is 1 s and the step 200 ms: five tries.
    assert_eq!(rig.fake.sleeps(), 5);
    assert!(out.starts_with("Installed the deck service: "));
    assert!(!out.contains("Started."));
    assert!(rig.file(Kind::Deck).exists());
}

#[test]
fn without_systemd_the_text_depends_on_wsl() {
    let mut rig = Rig::systemd();
    rig.ctx.systemd_booted = false;
    assert_eq!(
        text(rig.install(Kind::Both).0),
        "This machine does not run systemd. Run `fleetdeck up` under your own process supervisor."
    );
    rig.ctx.wsl_kernel = true;
    assert_eq!(
        text(rig.install(Kind::Both).0),
        "systemd is off in this WSL distro. Add these two lines to /etc/wsl.conf (needs sudo): \
         `[boot]` and `systemd=true`. Then run `wsl.exe --shutdown` in Windows and open the \
         distro again."
    );
    assert!(rig.fake.calls().is_empty());
}

#[test]
fn a_user_manager_that_does_not_answer_is_named() {
    let rig = Rig::systemd();
    rig.fake.once(
        "systemctl",
        &["--user", "show"],
        fail(1, "\nFailed to connect to bus: No medium found\nmore\n"),
    );
    assert_eq!(
        text(rig.install(Kind::Both).0),
        "The systemd user manager does not answer (Failed to connect to bus: No medium found). \
         Log in to this user once (not with `su`), then run the command again."
    );
    assert!(rig.fake.changes().is_empty());
    // Empty stderr: the detail of the failure; a time-out too.
    let rig = Rig::systemd();
    rig.fake.once("systemctl", &["--user", "show"], fail(1, ""));
    assert!(
        text(rig.install(Kind::Both).0)
            .starts_with("The systemd user manager does not answer (exit 1).")
    );
    let rig = Rig::systemd();
    rig.fake
        .once("systemctl", &["--user", "show"], Err(RunError::TimedOut));
    assert!(text(rig.install(Kind::Both).0).contains("(systemctl did not answer in 5 s)"));
}

#[test]
fn a_unit_directory_that_systemd_does_not_read_is_refused() {
    let rig = Rig::systemd();
    rig.fake.once(
        "systemctl",
        &["--user", "show"],
        ok("/etc/systemd/user /usr/lib/systemd/user\n"),
    );
    let dir = rig.unit_dir();
    assert_eq!(
        text(rig.install(Kind::Both).0),
        format!(
            "systemd does not read {}; set XDG_CONFIG_HOME the same for your shell and your \
             login, or unset it.",
            dir.display()
        )
    );
    assert!(rig.fake.changes().is_empty());
}

#[test]
fn a_relative_xdg_config_home_is_refused_before_any_change() {
    let mut rig = Rig::systemd();
    rig.ctx.xdg_config_home = Some("rel".to_owned());
    assert_eq!(
        text(rig.install(Kind::Both).0),
        "XDG_CONFIG_HOME must be an absolute path (it is \"rel\"). Fix it or unset it, then run \
         the command again."
    );
    assert!(rig.fake.changes().is_empty());
}

#[test]
fn an_xdg_config_home_names_the_unit_directory() {
    let mut rig = Rig::systemd();
    let xdg = rig.tmp.path().join("xdg");
    rig.ctx.xdg_config_home = Some(xdg.display().to_string());
    let dir = xdg.join("systemd/user");
    rig.fake.always(
        "systemctl",
        &["--user", "show"],
        ok(&format!("{}\n", dir.display())),
    );
    rig.sockets(Kind::Both, &[false, false, true]);
    rig.install(Kind::Both).0.unwrap();
    assert_eq!(rig.file(Kind::Both), dir.join(UNIT));
    assert!(dir.join(UNIT).exists());
    assert!(
        std::fs::read_to_string(dir.join(UNIT))
            .unwrap()
            .contains("XDG_CONFIG_HOME=")
    );
}

#[test]
fn linger_that_fails_is_a_warning_and_unknown_runs_no_command() {
    let rig = Rig::systemd();
    rig.fake.always("loginctl", &["show-user"], ok("no\n"));
    rig.fake
        .always("loginctl", &["--no-ask-password"], fail(1, "denied"));
    rig.sockets(Kind::Both, &[false, false, true]);
    let (result, out) = rig.install(Kind::Both);
    result.unwrap();
    let warning = "Your services start when luis logs in. To start them at boot, run: \
                   `sudo loginctl enable-linger luis`.\n";
    assert!(out.contains(warning));
    assert!(out.contains("Started.\n"));
    // `loginctl show-user` fails: unknown, no `enable-linger`.
    let rig = Rig::systemd();
    rig.fake
        .always("loginctl", &["show-user"], fail(1, "no user"));
    rig.sockets(Kind::Both, &[false, false, true]);
    let (result, out) = rig.install(Kind::Both);
    result.unwrap();
    assert!(out.contains(warning));
    assert!(!rig.fake.lines().iter().any(|l| l.contains("enable-linger")));
}

#[test]
fn a_failed_write_command_is_an_error_that_stops_at_once() {
    let rig = Rig::systemd();
    rig.fake.once(
        "systemctl",
        &["--user", "enable"],
        fail(1, "Failed to enable unit: boom\n"),
    );
    assert_eq!(
        text(rig.install(Kind::Both).0),
        "`systemctl --user enable fleetdeck-up.service` failed (exit 1): Failed to enable unit: boom"
    );
    assert!(!rig.fake.lines().iter().any(|l| l.contains("restart")));
    let rig = Rig::systemd();
    rig.fake.once(
        "systemctl",
        &["--user", "daemon-reload"],
        Ok(crate::service::runner::Output {
            status: None,
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    assert_eq!(
        text(rig.install(Kind::Both).0),
        "`systemctl --user daemon-reload` failed (ended by a signal)"
    );
}

#[test]
fn the_wsl_task_is_added_after_linger_and_before_the_start() {
    let rig = wsl_rig();
    rig.sockets(Kind::Both, &[false, false, true]);
    let (result, out) = rig.install(Kind::Both);
    result.unwrap();
    let lines = rig.fake.lines();
    let at = |needle: &str| lines.iter().position(|l| l.starts_with(needle)).unwrap();
    assert!(at("loginctl show-user") < at(POWERSHELL));
    assert!(at(POWERSHELL) < at("systemctl --user restart"));
    let call = rig
        .fake
        .calls()
        .into_iter()
        .find(|c| c.program == POWERSHELL)
        .unwrap();
    assert_eq!(
        call.args,
        [
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "-"
        ]
    );
    assert_eq!(
        call.stdin.unwrap(),
        crate::service::wsl_task::install_script("Ubuntu")
    );
    assert!(out.contains(
        "Windows logon task: added (WSL starts when you sign in to Windows).\nStarted.\n"
    ));
}

#[test]
fn a_wsl_task_that_fails_is_a_warning_and_the_exit_is_ok() {
    let warning = |reason: &str| {
        format!(
            "The services start when WSL starts. Fleet Deck could not add the Windows logon \
             task ({reason}).\n"
        )
    };
    // The script prints an error line.
    let rig = wsl_rig();
    rig.fake
        .always(POWERSHELL, &[], fail(1, "\nAccess is denied\n"));
    rig.sockets(Kind::Both, &[false, false, true]);
    let (result, out) = rig.install(Kind::Both);
    result.unwrap();
    assert!(out.contains(&warning("Access is denied")));
    assert!(out.contains("Started.\n"));
    // Exit 0 without `ok` as the last line: exit status text is not used; the
    // result is a failure with the status text.
    let rig = wsl_rig();
    rig.fake.always(POWERSHELL, &[], ok("done\n"));
    rig.sockets(Kind::Both, &[false, false, true]);
    assert!(rig.install(Kind::Both).1.contains(&warning("exit 0")));
    // No stderr: the status.
    let rig = wsl_rig();
    rig.fake.always(POWERSHELL, &[], fail(5, ""));
    rig.sockets(Kind::Both, &[false, false, true]);
    assert!(rig.install(Kind::Both).1.contains(&warning("exit 5")));
    // The program does not start.
    let rig = wsl_rig();
    rig.fake.always(
        POWERSHELL,
        &[],
        Err(RunError::Start("cannot start x: gone".to_owned())),
    );
    rig.sockets(Kind::Both, &[false, false, true]);
    assert!(
        rig.install(Kind::Both)
            .1
            .contains(&warning("cannot start x: gone"))
    );
}

#[test]
fn a_wsl_distro_that_is_refused_or_missing_skips_the_task_with_a_warning() {
    let mut rig = wsl_rig();
    rig.ctx.distro = Some("Ubuntu;rm".to_owned());
    rig.sockets(Kind::Both, &[false, false, true]);
    let (result, out) = rig.install(Kind::Both);
    result.unwrap();
    assert!(out.contains("(the distro name has other characters)."));
    assert!(!rig.fake.lines().iter().any(|l| l.starts_with(POWERSHELL)));
    let mut rig = wsl_rig();
    rig.ctx.distro = None;
    rig.sockets(Kind::Both, &[false, false, true]);
    assert!(
        rig.install(Kind::Both)
            .1
            .contains("(WSL_DISTRO_NAME is not set).")
    );
    let mut rig = wsl_rig();
    rig.ctx.powershell = None;
    rig.sockets(Kind::Both, &[false, false, true]);
    let (result, out) = rig.install(Kind::Both);
    result.unwrap();
    assert!(out.contains("(the Windows PowerShell program did not pass the path checks)."));
    assert!(!rig.fake.lines().iter().any(|l| l.starts_with(POWERSHELL)));
}
