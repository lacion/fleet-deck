//! `fleetdeck omarchy install | uninstall | status` and the Omarchy lines of
//! `fleetdeck doctor` (R13 P4 task C, `omarchy-p4.md` §3.4, decisions P4-7
//! and P4-13). A fake `$HOME`, a fake `PATH` of logging scripts (`omarchy`,
//! `omarchy-shell`, `omarchy-sudo-passwordless`) and a fake station socket.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

#[path = "omarchy_install/doctor.rs"]
mod doctor;
#[path = "omarchy_install/hyprland.rs"]
mod hyprland;
#[path = "omarchy_install/rig.rs"]
mod rig;

use std::os::unix::fs::symlink;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use rig::{HOOKS, MANIFEST, Rig, WAIT, err, mode, out, sleep_path};

// ---- install ----

#[test]
fn omarchy_install_writes_the_files_the_record_and_runs_the_commands() {
    let mut rig = Rig::new();
    rig.station(false);
    rig.hypr("-- user\n");
    let o = rig.install();
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    let omarchy = rig.omarchy_dir();
    for path in std::iter::once(MANIFEST)
        .chain(HOOKS)
        .chain(["themed/fleetdeck.json.tpl", "extensions/omarchy-menu.jsonc"])
    {
        assert!(omarchy.join(path).is_file(), "{path}");
    }
    assert_eq!(mode(&omarchy.join(HOOKS[0])), 0o755);
    assert_eq!(mode(&omarchy.join(MANIFEST)), 0o644);
    assert_eq!(mode(&omarchy.join("plugins")), 0o755);
    assert_eq!(mode(&rig.fd_dir().join("omarchy/hypr.lua")), 0o644);
    assert_eq!(mode(&rig.fd_dir().join("omarchy-install.json")), 0o600);
    let record = rig.record();
    assert_eq!(record["v"], 1);
    assert_eq!(record["files"].as_array().unwrap().len(), 12);
    assert_eq!(record["files"][0]["path"], MANIFEST);
    assert_eq!(record["files"][0]["base"], "omarchy");
    assert_eq!(record["files"][0]["existed_before"], false);
    assert_eq!(record["files"][0]["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(record["files"][10]["base"], "fleetdeck");
    assert_eq!(record["plugin_enabled"], true);
    assert_eq!(record["fleet_summary_set_by_install"], true);
    assert_eq!(record["menu_file_written"], true);
    assert_eq!(record["screensaver"]["enabled"], false);
    assert_eq!(record["hyprland_line"]["added"], true);
    assert_eq!(record["hyprland_line"]["newline_added"], false);
    assert_eq!(rig.calls("omarchy-shell"), [["shell", "rescanPlugins"]]);
    assert_eq!(
        rig.calls("omarchy"),
        [["plugin", "enable", "fleetdeck.fleet", "--section", "right"]]
    );
    assert_eq!(rig.sets(), [true]);
    assert!(rig.summary());
    assert!(err(&o).contains("fleetdeck is not on PATH"), "{}", err(&o));
    assert!(!out(&o).contains("Add these rows"), "{}", out(&o));
}

#[test]
fn omarchy_install_uninstall_and_install_again() {
    let mut rig = Rig::new();
    rig.station(false);
    rig.hypr("-- user\n");
    assert_eq!(rig.install().status.code(), Some(0));
    let o = rig.run(&["omarchy", "uninstall"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    let omarchy = rig.omarchy_dir();
    for path in std::iter::once(MANIFEST)
        .chain(HOOKS)
        .chain(["themed/fleetdeck.json.tpl", "extensions/omarchy-menu.jsonc"])
    {
        assert!(!omarchy.join(path).exists(), "{path}");
    }
    assert!(!omarchy.join("plugins/fleetdeck.fleet").exists());
    assert!(!omarchy.join("hooks/post-boot.d").exists());
    // The folders of Omarchy stay.
    for kept in ["plugins", "hooks", "themed", "extensions"] {
        assert!(omarchy.join(kept).is_dir(), "{kept}");
    }
    assert!(!rig.fd_dir().join("omarchy").exists());
    assert!(!rig.fd_dir().join("omarchy-install.json").exists());
    assert!(rig.fd_dir().is_dir());
    assert_eq!(rig.hypr_text(), "-- user\n");
    assert_eq!(
        rig.calls("omarchy").last().unwrap(),
        &["plugin", "disable", "fleetdeck.fleet"]
    );
    assert_eq!(rig.sets(), [true, false]);
    // And again: the plugin is enabled again, the line is added again.
    assert_eq!(rig.install().status.code(), Some(0));
    assert_eq!(rig.calls("omarchy").len(), 3);
    assert_eq!(rig.sets(), [true, false, true]);
    assert!(rig.hypr_text().contains("managed line"));
}

#[test]
fn omarchy_install_keeps_and_names_a_changed_file() {
    let mut rig = Rig::new();
    rig.station(false);
    assert_eq!(rig.install().status.code(), Some(0));
    let hook = rig.omarchy_dir().join(HOOKS[1]);
    std::fs::write(&hook, "#!/bin/bash\n# mine\nexit 0\n").unwrap();
    let o = rig.install();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains(&format!("kept (changed): {}", hook.display())),
        "{}",
        out(&o)
    );
    assert_eq!(
        std::fs::read_to_string(&hook).unwrap(),
        "#!/bin/bash\n# mine\nexit 0\n"
    );
    let o = rig.run(&["omarchy", "uninstall"]);
    assert!(
        out(&o).contains(&format!("kept (changed): {}", hook.display())),
        "{}",
        out(&o)
    );
    assert!(hook.is_file());
    assert!(!rig.omarchy_dir().join(HOOKS[0]).exists());
    // The record stays with the file that stayed.
    let record = rig.record();
    assert_eq!(record["files"].as_array().unwrap().len(), 1);
    assert_eq!(record["files"][0]["path"], HOOKS[1]);
}

#[test]
fn omarchy_install_refuses_a_conflicting_file_before_any_write() {
    let mut rig = Rig::new();
    rig.station(false);
    let hook = rig.omarchy_dir().join(HOOKS[2]);
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/bash\necho theirs\n").unwrap();
    let o = rig.install();
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains(&format!("fleetdeck: conflict: {}", hook.display())),
        "{}",
        err(&o)
    );
    assert!(!rig.omarchy_dir().join(MANIFEST).exists());
    assert!(!rig.fd_dir().exists());
    assert_eq!(
        std::fs::read_to_string(&hook).unwrap(),
        "#!/bin/bash\necho theirs\n"
    );
    assert!(rig.calls("omarchy-shell").is_empty());
    assert!(rig.sets().is_empty());
}

#[test]
fn omarchy_install_adopts_an_identical_file() {
    let mut rig = Rig::new();
    rig.station(false);
    assert_eq!(rig.install().status.code(), Some(0));
    let hook = rig.omarchy_dir().join(HOOKS[0]);
    let text = std::fs::read(&hook).unwrap();
    // A record that does not name the file: remove the record, then install.
    std::fs::remove_file(rig.fd_dir().join("omarchy-install.json")).unwrap();
    assert_eq!(rig.install().status.code(), Some(0));
    assert_eq!(std::fs::read(&hook).unwrap(), text);
    let record = rig.record();
    assert_eq!(record["files"][6]["existed_before"], true);
}

#[test]
fn omarchy_install_never_writes_an_existing_menu_file() {
    let mut rig = Rig::new();
    rig.station(false);
    let menu = rig.omarchy_dir().join("extensions/omarchy-menu.jsonc");
    std::fs::create_dir_all(menu.parent().unwrap()).unwrap();
    std::fs::write(&menu, "// mine\n{\n}\n").unwrap();
    let o = rig.install();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(std::fs::read_to_string(&menu).unwrap(), "// mine\n{\n}\n");
    assert!(
        out(&o).contains(&format!(
            "Add these rows inside the object of {}",
            menu.display()
        )),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("\"fleet.board\""), "{}", out(&o));
    let record = rig.record();
    assert_eq!(record["menu_file_written"], false);
    assert_eq!(record["files"].as_array().unwrap().len(), 11);
    let o = rig.run(&["omarchy", "uninstall"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(std::fs::read_to_string(&menu).unwrap(), "// mine\n{\n}\n");
}

#[test]
fn omarchy_install_rerun_does_not_enable_the_plugin_again() {
    let mut rig = Rig::new();
    rig.station(false);
    assert_eq!(rig.install().status.code(), Some(0));
    assert_eq!(rig.install().status.code(), Some(0));
    assert_eq!(rig.calls("omarchy").len(), 1);
    assert_eq!(rig.calls("omarchy-shell").len(), 2);
    // The summary was on already: the second install changes nothing.
    assert_eq!(rig.sets(), [true]);
    assert_eq!(rig.record()["fleet_summary_set_by_install"], true);
}

#[test]
fn omarchy_install_a_failed_rescan_and_a_failed_enable_are_warnings() {
    let mut rig = Rig::new();
    rig.station(false);
    rig.tool("omarchy-shell", "unknown", 0);
    rig.tool("omarchy", "", 1);
    let o = rig.install();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let stderr = err(&o);
    assert!(
        stderr.contains("fleetdeck: warning: the plugins were not rescanned; run: omarchy-shell shell rescanPlugins"),
        "{stderr}"
    );
    assert!(
        stderr.contains("fleetdeck: warning: the plugin is not enabled; run: omarchy plugin enable fleetdeck.fleet --section right"),
        "{stderr}"
    );
    assert_eq!(rig.record()["plugin_enabled"], false);
    // A rerun with working tools enables the plugin.
    rig.tool("omarchy-shell", "ok", 0);
    rig.tool("omarchy", "", 0);
    let o = rig.install();
    assert_eq!(o.status.code(), Some(0));
    assert!(!err(&o).contains("rescanned"), "{}", err(&o));
    assert_eq!(rig.record()["plugin_enabled"], true);
}

#[test]
fn omarchy_install_a_missing_omarchy_command_is_a_warning() {
    let mut rig = Rig::new();
    rig.station(false);
    std::fs::remove_file(rig.root().join("bin/omarchy-shell")).unwrap();
    let o = rig.install();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        err(&o).contains("run: omarchy-shell shell rescanPlugins"),
        "{}",
        err(&o)
    );
}

#[test]
fn omarchy_install_the_station_not_running_is_a_warning() {
    let rig = Rig::new();
    let o = rig.install();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        err(&o).contains("run: fleetdeck settings set --station fleet_summary true"),
        "{}",
        err(&o)
    );
    assert_eq!(rig.record()["fleet_summary_set_by_install"], false);
}

#[test]
fn omarchy_install_does_not_claim_a_summary_that_was_on() {
    let mut rig = Rig::new();
    rig.station(true);
    assert_eq!(rig.install().status.code(), Some(0));
    assert!(rig.sets().is_empty());
    assert_eq!(rig.record()["fleet_summary_set_by_install"], false);
    assert_eq!(rig.run(&["omarchy", "uninstall"]).status.code(), Some(0));
    assert!(rig.sets().is_empty());
    assert!(rig.summary());
}

#[test]
fn omarchy_install_without_a_terminal_and_without_yes_fails() {
    let rig = Rig::new();
    let o = rig.run(&["omarchy", "install"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("fleetdeck: Run with --yes to confirm."),
        "{}",
        err(&o)
    );
    assert!(
        out(&o).contains("Fleet Deck will write these files:"),
        "{}",
        out(&o)
    );
    assert!(!rig.omarchy_dir().exists());
    assert!(!rig.fd_dir().exists());
}

#[test]
fn omarchy_install_without_omarchy_fails() {
    let rig = Rig::new();
    let o = rig
        .command(&["omarchy", "install", "--yes"])
        .env("OMARCHY_PATH", rig.root().join("nowhere"))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("fleetdeck: Omarchy was not found on this machine."),
        "{}",
        err(&o)
    );
}

#[test]
fn omarchy_install_refuses_a_symlink_parent_and_changes_nothing() {
    let rig = Rig::new();
    let outside = tempfile::Builder::new().prefix("out").tempdir().unwrap();
    std::fs::create_dir_all(rig.omarchy_dir()).unwrap();
    symlink(outside.path(), rig.omarchy_dir().join("plugins")).unwrap();
    let o = rig.install();
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("is a link or is not yours; change nothing."),
        "{}",
        err(&o)
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    assert!(!rig.omarchy_dir().join(HOOKS[0]).exists());
    assert!(!rig.fd_dir().exists());
}

#[test]
fn omarchy_install_uses_xdg_config_home_when_it_is_absolute() {
    let rig = Rig::new();
    let xdg = rig.root().join("xdg");
    std::fs::create_dir_all(&xdg).unwrap();
    let o = rig
        .command(&["omarchy", "install", "--yes"])
        .env("XDG_CONFIG_HOME", &xdg)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(xdg.join("omarchy").join(MANIFEST).is_file());
    assert!(xdg.join("fleetdeck/omarchy-install.json").is_file());
    assert!(!rig.omarchy_dir().exists());
}

#[test]
fn omarchy_install_screensaver_sets_only_the_record_switch() {
    let mut rig = Rig::new();
    rig.station(false);
    let o = rig.run(&["omarchy", "install", "--yes", "--screensaver"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let record = rig.record();
    assert_eq!(record["screensaver"]["enabled"], true);
    assert_eq!(record["screensaver"]["had_original"], false);
    assert!(record["screensaver"]["backup_sha256"].is_null());
    // A rerun without the flag keeps the switch.
    assert_eq!(rig.install().status.code(), Some(0));
    assert_eq!(rig.record()["screensaver"]["enabled"], true);
}

#[test]
fn omarchy_install_the_record_after_an_interrupted_install() {
    let mut rig = Rig::new();
    rig.station(false);
    // The rescan blocks: the installer is killed after the files and before
    // the plugin enable.
    rig.script(
        "omarchy-shell",
        &format!(
            "echo started > {}\nexec {} 30\n",
            rig.root().join("blocked").display(),
            sleep_path()
        ),
    );
    let mut child = rig
        .command(&["omarchy", "install", "--yes"])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + WAIT;
    while !rig.root().join("blocked").exists() {
        assert!(Instant::now() < deadline, "the rescan never started");
        std::thread::sleep(Duration::from_millis(25));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let record = rig.record();
    assert_eq!(record["files"].as_array().unwrap().len(), 12);
    assert_eq!(record["plugin_enabled"], false);
    assert_eq!(record["fleet_summary_set_by_install"], false);
    // The status sees a consistent install.
    rig.tool("omarchy-shell", "ok", 0);
    let o = rig.run(&["omarchy", "status"]);
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));
    assert!(out(&o).contains("plugin: not enabled"), "{}", out(&o));
    // A rerun finishes the install.
    assert_eq!(rig.install().status.code(), Some(0));
    assert_eq!(rig.record()["plugin_enabled"], true);
}

// ---- status ----

#[test]
fn omarchy_status_without_a_record_is_not_installed_with_exit_1() {
    let rig = Rig::new();
    let o = rig.run(&["omarchy", "status"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(out(&o).trim(), "not installed");
    let o = rig.run(&["omarchy", "status", "--json"]);
    assert_eq!(o.status.code(), Some(1));
    let v: Value = serde_json::from_str(out(&o).trim()).unwrap();
    assert_eq!(
        v,
        json!({"installed": false, "files": [], "plugin_enabled": false,
               "summary": "unknown", "hyprland_line": "missing"})
    );
}

#[test]
fn omarchy_status_reports_drift_and_its_exit_code() {
    let mut rig = Rig::new();
    rig.station(false);
    assert_eq!(rig.install().status.code(), Some(0));
    let o = rig.run(&["omarchy", "status"]);
    assert_eq!(o.status.code(), Some(0), "{}", out(&o));
    let text = out(&o);
    assert!(
        text.contains(&format!(
            "ok {}",
            rig.omarchy_dir().join(MANIFEST).display()
        )),
        "{text}"
    );
    assert!(text.contains("plugin: enabled"), "{text}");
    assert!(text.contains("summary: on"), "{text}");
    std::fs::write(rig.omarchy_dir().join(HOOKS[0]), "changed\n").unwrap();
    std::fs::remove_file(rig.omarchy_dir().join("themed/fleetdeck.json.tpl")).unwrap();
    let o = rig.run(&["omarchy", "status"]);
    assert_eq!(o.status.code(), Some(1));
    let text = out(&o);
    assert!(
        text.contains(&format!(
            "changed {}",
            rig.omarchy_dir().join(HOOKS[0]).display()
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "missing {}",
            rig.omarchy_dir()
                .join("themed/fleetdeck.json.tpl")
                .display()
        )),
        "{text}"
    );
    let o = rig.run(&["omarchy", "status", "--json"]);
    assert_eq!(o.status.code(), Some(1));
    let v: Value = serde_json::from_str(out(&o).trim()).unwrap();
    assert_eq!(v["installed"], true);
    assert_eq!(v["plugin_enabled"], true);
    assert_eq!(v["summary"], "on");
    let states: Vec<&str> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["state"].as_str().unwrap())
        .collect();
    assert_eq!(states.iter().filter(|s| **s == "changed").count(), 1);
    assert_eq!(states.iter().filter(|s| **s == "missing").count(), 1);
}

#[test]
fn omarchy_status_summary_is_off_or_unknown() {
    let mut rig = Rig::new();
    rig.station(false);
    assert_eq!(rig.install().status.code(), Some(0));
    let o = rig.run(&["omarchy", "uninstall"]);
    assert_eq!(o.status.code(), Some(0));
    let o = rig.run(&["omarchy", "status"]);
    assert!(out(&o).contains("not installed"));
    let json = rig.run(&["omarchy", "status", "--json"]);
    let v: Value = serde_json::from_str(out(&json).trim()).unwrap();
    assert_eq!(v["summary"], "off");
    let plain = Rig::new();
    let json = plain.run(&["omarchy", "status", "--json"]);
    let v: Value = serde_json::from_str(out(&json).trim()).unwrap();
    assert_eq!(v["summary"], "unknown");
}

#[test]
fn omarchy_uninstall_without_a_record_changes_nothing() {
    let rig = Rig::new();
    let o = rig.run(&["omarchy", "uninstall"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).contains("Nothing to remove"), "{}", out(&o));
}

#[test]
fn omarchy_uninstall_with_the_station_down_keeps_the_record_until_summary_is_off() {
    let mut rig = Rig::new();
    rig.station(false);
    assert_eq!(rig.install().status.code(), Some(0));
    assert_eq!(rig.record()["fleet_summary_set_by_install"], true);
    // The station goes away: its socket file is removed.
    std::fs::remove_file(rig.root().join("data/station/station.sock")).unwrap();
    let o = rig.run(&["omarchy", "uninstall"]);
    assert_eq!(o.status.code(), Some(1), "{}{}", out(&o), err(&o));
    assert!(
        err(&o).contains(
            "fleetdeck: warning: the station is not running or did not accept the change, \
             so fleet_summary stays on; run uninstall again when the station runs."
        ),
        "{}",
        err(&o)
    );
    // The files are gone; the record stays with the pending flag.
    assert!(!rig.omarchy_dir().join(MANIFEST).exists());
    let record = rig.record();
    assert_eq!(record["files"].as_array().unwrap().len(), 0);
    assert_eq!(record["fleet_summary_set_by_install"], true);
    assert!(rig.summary(), "the setting was not changed");

    // The station runs again. `status` reads the summary and reports no file
    // problem.
    rig.station(true);
    let o = rig.run(&["omarchy", "status", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    let v: Value = serde_json::from_str(out(&o).trim()).unwrap();
    assert_eq!(v["installed"], true);
    assert_eq!(v["summary"], "on");
    assert_eq!(v["files"], json!([]));

    // A later uninstall with the station up turns the setting off and removes
    // the record.
    let o = rig.run(&["omarchy", "uninstall"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    assert_eq!(rig.sets(), [false]);
    assert!(!rig.summary());
    assert!(!rig.fd_dir().join("omarchy-install.json").exists());
}
