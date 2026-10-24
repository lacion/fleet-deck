//! Tests of `fleetdeck service` (design service.md §10). Every program goes
//! through the recording `Fake`; every path is under a temporary directory.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod manage;
mod path;
mod plans_launchd;
mod plans_systemd;
mod program;
mod render;

use std::path::PathBuf;
use std::time::Duration;

use fleetdeck_core::config::HostChoice;
use fleetdeck_core::paths::NodeDirs;
use fleetdeck_core::tailscale_wsl::POWERSHELL;

use super::fake::{Fake, ok};
use super::{Context, Kind, Platform, run};
use crate::cli::ServiceCommand;

pub const BUILD: &[u8] = b"binary-one";

/// A machine in a temporary directory.
pub struct Rig {
    pub tmp: tempfile::TempDir,
    pub ctx: Context,
    pub fake: Fake,
}

impl Rig {
    pub fn new(platform: Platform) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let exe = root.join("build/fleetdeck");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, BUILD).unwrap();
        let ctx = Context {
            platform,
            dirs: NodeDirs {
                config: root.join("cfg"),
                data: root.join("data"),
            },
            data_dir_flag: false,
            home: root.join("home"),
            uid: 1000,
            user: "luis".to_owned(),
            path: Some("/usr/bin:/home/luis/.local/bin".to_owned()),
            path_dropped: Vec::new(),
            shell_path: None,
            xdg_config_home: None,
            xdg_data_home: None,
            wsl: false,
            wsl_kernel: false,
            systemd_booted: true,
            distro: None,
            powershell: None,
            exe,
            version: "1.2.3".to_owned(),
            host: Some(HostChoice::Direct),
            ready_limit: Duration::from_secs(1),
            ready_step: Duration::from_millis(200),
        };
        let rig = Self {
            tmp,
            ctx,
            fake: Fake::default(),
        };
        match platform {
            Platform::Systemd => rig.systemd_defaults(),
            Platform::Launchd => rig.launchd_defaults(),
        }
        rig
    }

    pub fn systemd() -> Self {
        Self::new(Platform::Systemd)
    }

    pub fn launchd() -> Self {
        Self::new(Platform::Launchd)
    }

    pub fn unit_dir(&self) -> PathBuf {
        self.ctx.home.join(".config/systemd/user")
    }

    fn systemd_defaults(&self) {
        let dir = self.unit_dir();
        let paths = format!("/etc/systemd/user {}", dir.display());
        let f = &self.fake;
        f.always(
            "systemctl",
            &["--user", "show", "--property=UnitPath"],
            ok(&paths),
        );
        f.always(
            "systemctl",
            &["--user", "is-active"],
            super::fake::fail(3, ""),
        );
        f.always("systemctl", &["--user", "is-enabled"], ok("enabled"));
        for verb in [
            "daemon-reload",
            "enable",
            "restart",
            "start",
            "stop",
            "disable",
        ] {
            f.always("systemctl", &["--user", verb], ok(""));
        }
        f.always("loginctl", &["show-user"], ok("yes\n"));
        f.always("loginctl", &["--no-ask-password"], ok(""));
    }

    fn launchd_defaults(&self) {
        let f = &self.fake;
        // A graphical login exists; no job is loaded.
        f.always("launchctl", &["print", "gui/1000"], ok("domain"));
        for kind in super::KINDS {
            let target = format!("gui/1000/{}", kind.label());
            f.always(
                "launchctl",
                &["print", &target],
                super::fake::fail(113, "Could not find service"),
            );
        }
        for verb in ["enable", "bootstrap", "bootout", "kickstart"] {
            f.always("launchctl", &[verb], ok(""));
        }
    }

    /// The sockets of `kind` answer `answers` (all of them).
    pub fn sockets(&self, kind: Kind, answers: &[bool]) {
        for socket in kind.sockets(&self.ctx.dirs) {
            self.fake.socket(&socket, answers);
        }
    }

    /// Runs a command; the result and the text that it printed.
    pub fn run(&self, command: &ServiceCommand) -> (anyhow::Result<()>, String) {
        let mut out = Vec::new();
        let result = run(command, &self.ctx, &self.fake, &mut out);
        (result, String::from_utf8(out).unwrap())
    }

    pub fn install(&self, kind: Kind) -> (anyhow::Result<()>, String) {
        self.run(&ServiceCommand::Install {
            kind,
            dry_run: false,
            verbose: false,
        })
    }

    pub fn dry_run(&self, kind: Kind) -> (anyhow::Result<()>, String) {
        self.run(&ServiceCommand::Install {
            kind,
            dry_run: true,
            verbose: false,
        })
    }

    /// The service file of `kind`.
    pub fn file(&self, kind: Kind) -> PathBuf {
        kind.file(&self.ctx).unwrap()
    }

    /// Writes a service file of `kind` as an earlier install would have
    /// (systemd marker line or launchd hex marker); `data` is the data
    /// directory it records, `None` writes no marker.
    pub fn put_file(&self, kind: Kind, data: Option<&str>) {
        let path = self.file(kind);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = match (self.ctx.platform, data) {
            (Platform::Systemd, Some(d)) => format!("# fleetdeck-data: {d}\n[Unit]\n"),
            (Platform::Systemd, None) => "[Unit]\n".to_owned(),
            (Platform::Launchd, Some(d)) => {
                let hex: String = d.bytes().map(|b| format!("{b:02x}")).collect();
                format!("<!-- fleetdeck-data-hex: {hex} -->\n<plist/>\n")
            }
            (Platform::Launchd, None) => "<plist/>\n".to_owned(),
        };
        std::fs::write(path, text).unwrap();
    }

    /// The data directory that an install here records.
    pub fn data(&self) -> String {
        self.ctx.dirs.data.display().to_string()
    }

    /// Puts an installed file of `kind` for this data directory.
    pub fn put_installed(&self, kind: Kind) {
        let data = self.data();
        self.put_file(kind, Some(&data));
    }
}

/// The message of an error.
pub fn text(result: anyhow::Result<()>) -> String {
    format!("{:#}", result.unwrap_err())
}

/// Sets the modification time of `path` to `secs` seconds after the epoch.
pub fn set_mtime(path: &std::path::Path, secs: u64) {
    let time = std::time::UNIX_EPOCH + Duration::from_secs(secs);
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

/// A systemd machine under WSL with a PowerShell that answers `ok`.
pub fn wsl_rig() -> Rig {
    let mut rig = Rig::systemd();
    rig.ctx.wsl = true;
    rig.ctx.wsl_kernel = true;
    rig.ctx.distro = Some("Ubuntu".to_owned());
    rig.ctx.powershell = Some(PathBuf::from(POWERSHELL));
    rig.fake.always(POWERSHELL, &[], ok("ok\n"));
    rig
}
