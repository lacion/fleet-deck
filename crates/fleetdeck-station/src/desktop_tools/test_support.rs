//! Test support: a fake [`Exec`], a temporary desktop and a temporary
//! `/etc`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::identity::Fingerprint;
use fleetdeck_proto::desktop::DesktopKind;
use futures_util::future::BoxFuture;
use tempfile::TempDir;

use super::etc_check::EtcRoot;
use super::facts::PACMAN;
use super::probe::{Exec, ProbeRun};
use crate::desktop::detect::Found;

/// One child that the fake saw.
#[derive(Debug, Clone)]
pub struct Call {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub limit: Duration,
}

type Handler = Box<dyn Fn(&Call) -> ProbeRun + Send + Sync>;

/// A fake runner: `handler` answers each child, after `delay` (a delay that
/// is longer than the limit of the child is a `Timeout`).
pub struct FakeExec {
    handler: Handler,
    pacman: bool,
    delay: Duration,
    running: AtomicUsize,
    max_running: AtomicUsize,
    calls: Mutex<Vec<Call>>,
}

impl FakeExec {
    pub fn new(handler: impl Fn(&Call) -> ProbeRun + Send + Sync + 'static) -> Self {
        Self {
            handler: Box::new(handler),
            pacman: true,
            delay: Duration::ZERO,
            running: AtomicUsize::new(0),
            max_running: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// `/usr/bin/pacman` does not exist.
    pub fn without_pacman(mut self) -> Self {
        self.pacman = false;
        self
    }

    pub fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub fn max_running(&self) -> usize {
        self.max_running.load(Ordering::SeqCst)
    }
}

impl Exec for FakeExec {
    fn run(
        &self,
        program: &Path,
        args: &[OsString],
        env: &[(OsString, OsString)],
        limit: Duration,
    ) -> BoxFuture<'_, ProbeRun> {
        let text = |s: &OsString| s.to_string_lossy().into_owned();
        let call = Call {
            program: program.to_owned(),
            args: args.iter().map(text).collect(),
            env: env.iter().map(|(k, v)| (text(k), text(v))).collect(),
            limit,
        };
        Box::pin(async move {
            self.calls.lock().unwrap().push(call.clone());
            let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_running.fetch_max(now, Ordering::SeqCst);
            let reply = if self.delay > limit {
                tokio::time::sleep(limit).await;
                ProbeRun::Timeout
            } else {
                tokio::time::sleep(self.delay).await;
                (self.handler)(&call)
            };
            self.running.fetch_sub(1, Ordering::SeqCst);
            reply
        })
    }

    fn exists(&self, path: &Path) -> bool {
        if path == Path::new(PACMAN) {
            self.pacman
        } else {
            path.exists()
        }
    }
}

pub fn exit(code: i32, stdout: &str) -> ProbeRun {
    ProbeRun::Exit {
        code,
        stdout: stdout.as_bytes().to_vec(),
    }
}

/// A deck fingerprint made of the byte `byte`.
pub fn peer(byte: u8) -> Fingerprint {
    format!("{byte:02x}").repeat(32).parse().unwrap()
}

/// A temporary desktop: a `bin` folder on `PATH` and a `HOME`.
pub struct Rig {
    pub dir: TempDir,
    pub bin: PathBuf,
    pub home: PathBuf,
}

impl Rig {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let home = dir.path().join("home");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&home).unwrap();
        Self { dir, bin, home }
    }

    /// An executable file `name` in `folder`.
    pub fn exe_in(folder: &Path, name: &str) -> PathBuf {
        fs::create_dir_all(folder).unwrap();
        let path = folder.join(name);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// An executable `name` on the plain `PATH`.
    pub fn tool(&self, name: &str) -> PathBuf {
        Self::exe_in(&self.bin, name)
    }

    /// The environment of the station: `PATH` and `HOME`.
    pub fn base(&self) -> ChildEnv {
        let mut env = ChildEnv::empty();
        env.set("PATH", &self.bin).unwrap();
        env.set("HOME", &self.home).unwrap();
        env
    }

    /// A found Omarchy desktop; its `bin` folder exists.
    pub fn omarchy(&self) -> Found {
        let root = self.dir.path().join("omarchy");
        fs::create_dir_all(root.join("bin")).unwrap();
        Found {
            kind: DesktopKind::Omarchy,
            signature: "sig".to_owned(),
            runtime_dir: self.dir.path().join("run"),
            wayland: "wayland-1".to_owned(),
            omarchy: Some(root),
            omarchy_version: None,
        }
    }

    /// An executable in the `bin` folder of the Omarchy desktop.
    pub fn omarchy_exe(&self, name: &str) -> PathBuf {
        Self::exe_in(&self.dir.path().join("omarchy/bin"), name)
    }

    /// A found Hyprland desktop without Omarchy.
    pub fn hyprland(&self) -> Found {
        Found {
            omarchy: None,
            kind: DesktopKind::Hyprland,
            ..self.omarchy()
        }
    }

    /// The plugin folder below `HOME`.
    pub fn plugin_dir(&self) -> PathBuf {
        let dir = self.home.join(".config/omarchy/plugins/fleetdeck.fleet");
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}

/// A temporary `/etc` that passes the check, owned by the test user.
pub struct Etc {
    pub dir: TempDir,
    pub root: EtcRoot,
}

impl Etc {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc");
        fs::create_dir_all(etc.join("pacman.d")).unwrap();
        fs::write(etc.join("pacman.conf"), "[options]\n[core]\n[extra]\n").unwrap();
        fs::write(
            etc.join("pacman.d/mirrorlist"),
            "Server = https://x/$repo\n",
        )
        .unwrap();
        let mode =
            |p: &Path, m: u32| fs::set_permissions(p, fs::Permissions::from_mode(m)).unwrap();
        mode(&etc, 0o755);
        mode(&etc.join("pacman.d"), 0o755);
        mode(&etc.join("pacman.conf"), 0o644);
        mode(&etc.join("pacman.d/mirrorlist"), 0o644);
        let owner = fs::metadata(dir.path()).unwrap().uid();
        let root = EtcRoot {
            path: dir.path().to_owned(),
            owner,
        };
        Self { dir, root }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join("etc").join(rel)
    }

    pub fn chmod(&self, rel: &str, mode: u32) {
        fs::set_permissions(self.path(rel), fs::Permissions::from_mode(mode)).unwrap();
    }
}
