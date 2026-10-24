//! Rendering: the unit file, the plist and the PowerShell scripts.

use std::path::{Path, PathBuf};

use super::{Rig, text};
use crate::service::{Kind, launchd, systemd, wsl_task};

fn program() -> PathBuf {
    PathBuf::from("/data/station/bin/current")
}

fn line<'a>(text: &'a str, start: &str) -> Vec<&'a str> {
    text.lines().filter(|l| l.starts_with(start)).collect()
}

#[test]
fn systemd_quotes_the_program_the_arguments_and_the_environment() {
    let mut rig = Rig::systemd();
    rig.ctx.data_dir_flag = true;
    rig.ctx.dirs.data = PathBuf::from("/d $x %y \"z\" \\w");
    rig.ctx.path = Some("/a b:\"q\":\\:%:$".to_owned());
    let tricky = Path::new("/p q/\"r\"/\\s/%t/$u/current");
    let unit = systemd::render(&rig.ctx, Kind::Both, tricky).unwrap();
    // The program: `\` `"` `%` are escaped; `$` stays.
    // The arguments: `$` is `$$` too.
    assert_eq!(
        line(&unit, "ExecStart="),
        [
            "ExecStart=\"/p q/\\\"r\\\"/\\\\s/%%t/$u/current\" \"--data-dir\" \
             \"/d $$x %%y \\\"z\\\" \\\\w\" \"up\""
        ]
    );
    // The environment: the whole `NAME=value`; `$` stays.
    assert_eq!(
        line(&unit, "Environment="),
        [
            "Environment=\"PATH=/a b:\\\"q\\\":\\\\:%%:$\"",
            "Environment=\"FLEETDECK_LOG_FORMAT=json\""
        ]
    );
}

#[test]
fn systemd_writes_the_marker_the_order_and_the_fixed_lines() {
    let rig = Rig::systemd();
    let unit = systemd::render(&rig.ctx, Kind::Both, &program()).unwrap();
    let data = rig.data();
    let expect = format!(
        "# Written by fleetdeck service install. fleetdeck service uninstall removes it.\n\
         # fleetdeck-data: {data}\n\
         [Unit]\n\
         Description=Fleet Deck deck and local station\n\
         Documentation=https://github.com/lacion/fleet-deck/blob/v2/docs/v2/operations.md\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=\"/data/station/bin/current\" \"up\"\n\
         Environment=\"PATH=/usr/bin:/home/luis/.local/bin\"\n\
         Environment=\"FLEETDECK_LOG_FORMAT=json\"\n\
         KillMode=process\n\
         KillSignal=SIGTERM\n\
         TimeoutStopSec=30\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         UMask=0077\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    );
    assert_eq!(unit, expect);
    assert_eq!(systemd::marker(&unit), Some(data));
}

#[test]
fn kill_mode_is_for_station_and_both_and_no_new_privileges_for_deck_only() {
    let rig = Rig::systemd();
    for (kind, kill, privileges, exec) in [
        (Kind::Both, true, false, "\"up\""),
        (Kind::Station, true, false, "\"station\" \"serve\""),
        (Kind::Deck, false, true, "\"deck\" \"serve\""),
    ] {
        let unit = systemd::render(&rig.ctx, kind, &program()).unwrap();
        assert_eq!(unit.contains("KillMode=process\n"), kill, "{kind:?}");
        assert_eq!(
            unit.contains("NoNewPrivileges=true\n"),
            privileges,
            "{kind:?}"
        );
        assert!(unit.contains(&format!("current\" {exec}\n")), "{kind:?}");
        assert!(unit.contains(&format!("Description={}\n", kind.description())));
    }
}

#[test]
fn the_environment_has_only_the_listed_variables() {
    let mut rig = Rig::systemd();
    rig.ctx.xdg_config_home = Some("/xc".to_owned());
    rig.ctx.xdg_data_home = Some("/xd".to_owned());
    let unit = systemd::render(&rig.ctx, Kind::Deck, &program()).unwrap();
    assert_eq!(
        line(&unit, "Environment="),
        [
            "Environment=\"PATH=/usr/bin:/home/luis/.local/bin\"",
            "Environment=\"XDG_CONFIG_HOME=/xc\"",
            "Environment=\"XDG_DATA_HOME=/xd\"",
            "Environment=\"FLEETDECK_LOG_FORMAT=json\"",
        ]
    );
    // Without PATH and XDG values: the log format only.
    rig.ctx.path = None;
    rig.ctx.xdg_config_home = None;
    rig.ctx.xdg_data_home = None;
    let unit = systemd::render(&rig.ctx, Kind::Deck, &program()).unwrap();
    assert_eq!(
        line(&unit, "Environment="),
        ["Environment=\"FLEETDECK_LOG_FORMAT=json\""]
    );
}

#[test]
fn a_control_character_or_a_relative_xdg_value_is_refused() {
    for platform in [Rig::systemd, Rig::launchd] {
        let base = platform();
        let render = |ctx: &crate::service::Context, program: &Path| match ctx.platform {
            crate::service::Platform::Systemd => systemd::render(ctx, Kind::Both, program),
            crate::service::Platform::Launchd => launchd::render(ctx, Kind::Both, program),
        };
        let refuse = |ctx: crate::service::Context, program: PathBuf, what: &str| {
            let message = text(render(&ctx, &program).map(drop));
            assert_eq!(
                message,
                format!("{what} contains a control character. Fix it, then run the command again.")
            );
        };
        let mut ctx = base.ctx.clone();
        ctx.path = Some("/a\nb".to_owned());
        refuse(ctx, program(), "PATH");
        let mut ctx = base.ctx.clone();
        ctx.xdg_config_home = Some("/a\u{7f}b".to_owned());
        refuse(ctx, program(), "XDG_CONFIG_HOME");
        let mut ctx = base.ctx.clone();
        ctx.xdg_data_home = Some("/a\tb".to_owned());
        refuse(ctx, program(), "XDG_DATA_HOME");
        refuse(
            base.ctx.clone(),
            PathBuf::from("/a\nb/current"),
            "The program path",
        );
        let mut ctx = base.ctx.clone();
        ctx.dirs.data = PathBuf::from("/a\nb");
        refuse(ctx, program(), "The data directory");
        let mut ctx = base.ctx.clone();
        ctx.home = PathBuf::from("/h\nx");
        if ctx.platform == crate::service::Platform::Launchd {
            refuse(ctx, program(), "The home directory");
        }
        let mut ctx = base.ctx.clone();
        ctx.xdg_data_home = Some("rel/dir".to_owned());
        assert_eq!(
            text(render(&ctx, &program()).map(drop)),
            "XDG_DATA_HOME must be an absolute path (it is \"rel/dir\"). Fix it or unset it, \
             then run the command again."
        );
    }
}

#[test]
fn the_plist_has_the_keys_the_escapes_and_the_marker() {
    let mut rig = Rig::launchd();
    rig.ctx.data_dir_flag = true;
    rig.ctx.path = Some("/a&b:<c>:\"d\":'e'".to_owned());
    let plist = launchd::render(&rig.ctx, Kind::Station, &program()).unwrap();
    let data = rig.data();
    let hex: String = data.bytes().map(|b| format!("{b:02x}")).collect();
    let logs = rig.ctx.home.join("Library/Logs/fleetdeck/station.log");
    let logs = logs.display();
    let expect = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <!-- fleetdeck-data-hex: {hex} -->\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20   <key>Label</key>\n\
         \x20   <string>dev.fleetdeck.station</string>\n\
         \x20   <key>ProgramArguments</key>\n\
         \x20   <array>\n\
         \x20       <string>/data/station/bin/current</string>\n\
         \x20       <string>--data-dir</string>\n\
         \x20       <string>{data}</string>\n\
         \x20       <string>station</string>\n\
         \x20       <string>serve</string>\n\
         \x20   </array>\n\
         \x20   <key>EnvironmentVariables</key>\n\
         \x20   <dict>\n\
         \x20       <key>PATH</key>\n\
         \x20       <string>/a&amp;b:&lt;c&gt;:&quot;d&quot;:&apos;e&apos;</string>\n\
         \x20       <key>FLEETDECK_LOG_FORMAT</key>\n\
         \x20       <string>json</string>\n\
         \x20   </dict>\n\
         \x20   <key>StandardOutPath</key>\n\
         \x20   <string>{logs}</string>\n\
         \x20   <key>StandardErrorPath</key>\n\
         \x20   <string>{logs}</string>\n\
         \x20   <key>RunAtLoad</key>\n\
         \x20   <true/>\n\
         \x20   <key>KeepAlive</key>\n\
         \x20   <dict>\n\
         \x20       <key>SuccessfulExit</key>\n\
         \x20       <false/>\n\
         \x20   </dict>\n\
         \x20   <key>ExitTimeOut</key>\n\
         \x20   <integer>30</integer>\n\
         \x20   <key>Umask</key>\n\
         \x20   <integer>63</integer>\n\
         \x20   <key>ProcessType</key>\n\
         \x20   <string>Background</string>\n\
         \x20   <key>AbandonProcessGroup</key>\n\
         \x20   <true/>\n\
         </dict>\n\
         </plist>\n"
    );
    assert_eq!(plist, expect);
    assert_eq!(launchd::marker(&plist), Some(data));
}

#[test]
fn abandon_process_group_is_for_station_and_both_only() {
    let rig = Rig::launchd();
    for (kind, abandon) in [
        (Kind::Both, true),
        (Kind::Station, true),
        (Kind::Deck, false),
    ] {
        let plist = launchd::render(&rig.ctx, kind, &program()).unwrap();
        assert_eq!(plist.contains("AbandonProcessGroup"), abandon, "{kind:?}");
        assert!(plist.contains(&format!("<string>{}</string>", kind.label())));
        assert!(plist.contains(&format!("/Library/Logs/fleetdeck/{}.log", kind.log_name())));
    }
}

#[test]
fn a_launchd_marker_that_does_not_decode_is_no_marker() {
    assert_eq!(launchd::marker("<!-- fleetdeck-data-hex: zz -->"), None);
    assert_eq!(launchd::marker("<!-- fleetdeck-data-hex: abc -->"), None);
    assert_eq!(launchd::marker("<!-- fleetdeck-data-hex: ff -->"), None);
    assert_eq!(launchd::marker("<!-- fleetdeck-data-hex:  -->"), None);
    assert_eq!(launchd::marker("<plist/>"), None);
    assert_eq!(
        launchd::marker("<!-- fleetdeck-data-hex: 2f64 -->"),
        Some("/d".to_owned())
    );
}

#[test]
fn the_powershell_scripts_have_the_distro_as_their_only_variable_part() {
    for build in [
        wsl_task::install_script,
        wsl_task::status_script,
        wsl_task::remove_script,
    ] {
        let a = build("Ubuntu-22.04");
        let b = build("Debian_1.x");
        assert!(a.contains("Ubuntu-22.04"));
        assert!(!a.contains("Debian_1.x"));
        // The same script with the distro replaced by one marker word.
        assert_eq!(a.replace("Ubuntu-22.04", "@"), b.replace("Debian_1.x", "@"));
        assert!(a.contains("'WSL Ubuntu-22.04 ' + $id.User.Value"));
        assert!(a.starts_with("$ErrorActionPreference = 'Stop'\n"));
    }
    let install = wsl_task::install_script("Ubuntu");
    assert!(install.contains("-d Ubuntu --exec /bin/sleep infinity"));
    assert!(install.ends_with("'ok'\n"));
    assert!(wsl_task::status_script("Ubuntu").contains("'state=' + $t.State"));
    assert!(wsl_task::remove_script("Ubuntu").contains("Unregister-ScheduledTask"));
}

#[test]
fn a_distro_name_with_other_characters_is_refused_and_runs_nothing() {
    for bad in [
        "Ubuntu;rm",
        "a b",
        "",
        "x'y",
        "$(id)",
        &"a".repeat(65),
        "Ubuntu\n",
    ] {
        assert!(!wsl_task::distro_ok(bad), "{bad:?}");
    }
    for good in ["Ubuntu", "Ubuntu-22.04", "a_b", &"a".repeat(64)] {
        assert!(wsl_task::distro_ok(good), "{good:?}");
    }
    let mut rig = Rig::systemd();
    rig.ctx.wsl = true;
    rig.ctx.distro = Some("Ubuntu;rm".to_owned());
    rig.ctx.powershell = Some(PathBuf::from(fleetdeck_core::tailscale_wsl::POWERSHELL));
    let line = wsl_task::install(&rig.ctx, &rig.fake);
    assert_eq!(
        line,
        "The services start when WSL starts. Fleet Deck could not add the Windows logon task \
         (the distro name has other characters)."
    );
    assert!(rig.fake.calls().is_empty());
}
