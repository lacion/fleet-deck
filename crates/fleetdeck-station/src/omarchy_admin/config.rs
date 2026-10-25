//! The configuration of the Omarchy service.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_core::paths::{Env, NodeDirs};

/// The time limit of each kind of child.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub install: Duration,
    pub uninstall: Duration,
    pub screensaver: Duration,
    pub status: Duration,
    pub tick: Duration,
    pub retry: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            install: Duration::from_secs(60),
            uninstall: Duration::from_secs(60),
            screensaver: Duration::from_secs(15),
            status: Duration::from_secs(15),
            tick: Duration::from_secs(15),
            retry: Duration::from_secs(600),
        }
    }
}

/// Where the three session variables of a child come from.
#[derive(Debug, Clone)]
pub enum SessionEnv {
    /// The desktop detection of the station, at each child.
    Detect,
    /// These variables (tests).
    Fixed(Vec<(&'static str, OsString)>),
}

/// What the service needs; tests build it by hand.
#[derive(Debug, Clone)]
pub struct Config {
    /// The station's own executable.
    pub program: PathBuf,
    /// `--data-dir`, when the station runs with an explicit data dir.
    pub data_dir: Option<PathBuf>,
    /// `<data>/station`: the provisioning state file and lock.
    pub station_dir: PathBuf,
    /// `$XDG_CONFIG_HOME` or `$HOME/.config`; `None` when neither is set.
    pub config_home: Option<PathBuf>,
    /// The environment of the station process, unchanged.
    pub env: Vec<(OsString, OsString)>,
    /// The scrubbed environment for the detection of Omarchy.
    pub omarchy_env: ChildEnv,
    pub session: SessionEnv,
    pub limits: Limits,
}

impl Config {
    /// The configuration of this process.
    #[must_use]
    pub fn from_process(dirs: &NodeDirs, program: PathBuf) -> Self {
        let env = Env::from_process();
        Self {
            program,
            data_dir: (dirs.config == dirs.data).then(|| dirs.data.clone()),
            station_dir: dirs.station(),
            config_home: env
                .xdg_config_home
                .or_else(|| env.home.map(|h| h.join(".config"))),
            env: std::env::vars_os().collect(),
            omarchy_env: ChildEnv::from_process(),
            session: SessionEnv::Detect,
            limits: Limits::default(),
        }
    }

    /// `<config home>/fleetdeck/provision.toml`.
    pub(super) fn provision_file(&self) -> Option<PathBuf> {
        Some(self.config_home.as_ref()?.join("fleetdeck/provision.toml"))
    }

    /// `<config home>/fleetdeck/omarchy-install.json`.
    pub(super) fn install_record(&self) -> Option<PathBuf> {
        Some(
            self.config_home
                .as_ref()?
                .join("fleetdeck/omarchy-install.json"),
        )
    }
}
