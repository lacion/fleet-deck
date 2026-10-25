//! The rig of the Omarchy installer tests: a fake `$HOME`, a fake `PATH` of
//! logging scripts and a fake station socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

pub const WAIT: Duration = Duration::from_secs(20);
pub const MANIFEST: &str = "plugins/fleetdeck.fleet/manifest.json";
pub const HOOKS: [&str; 3] = [
    "hooks/post-boot.d/fleetdeck",
    "hooks/post-update.d/fleetdeck",
    "hooks/theme-set.d/fleetdeck",
];

pub struct Station {
    pub summary: Arc<Mutex<bool>>,
    pub ops: Arc<Mutex<Vec<Value>>>,
}

pub struct Rig {
    pub dir: tempfile::TempDir,
    pub station: Option<Station>,
}

impl Rig {
    pub fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("fdo").tempdir().unwrap();
        for sub in ["bin", "data/station", "home/.config", "omarchy-root/bin"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        let omarchy = dir.path().join("omarchy-root/bin/omarchy");
        std::fs::write(&omarchy, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&omarchy, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.path().join("omarchy-root/version"), "4.0.0-test\n").unwrap();
        let rig = Self { dir, station: None };
        rig.tool("omarchy-shell", "ok", 0);
        rig.tool("omarchy", "", 0);
        rig
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn cfg(&self) -> PathBuf {
        self.root().join("home/.config")
    }

    pub fn omarchy_dir(&self) -> PathBuf {
        self.cfg().join("omarchy")
    }

    pub fn fd_dir(&self) -> PathBuf {
        self.cfg().join("fleetdeck")
    }

    /// A fake program on the `PATH` that logs its argv and prints `stdout`.
    pub fn tool(&self, name: &str, stdout: &str, exit: i32) {
        self.script(
            name,
            &format!(
                "echo \"== {name}\" >> {log}\nfor a in \"$@\"; do echo \"$a\" >> {log}; done\n\
                 [ -z '{stdout}' ] || echo '{stdout}'\nexit {exit}\n",
                log = self.root().join(format!("{name}.log")).display()
            ),
        );
    }

    pub fn script(&self, name: &str, body: &str) {
        let path = self.root().join("bin").join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    pub fn calls(&self, name: &str) -> Vec<Vec<String>> {
        let Ok(text) = std::fs::read_to_string(self.root().join(format!("{name}.log"))) else {
            return Vec::new();
        };
        let mut calls: Vec<Vec<String>> = Vec::new();
        for line in text.lines() {
            if line == format!("== {name}") {
                calls.push(Vec::new());
            } else if let Some(last) = calls.last_mut() {
                last.push(line.to_owned());
            }
        }
        calls
    }

    /// Starts the fake station. `summary` is the value of `fleet_summary`.
    pub fn station(&mut self, summary: bool) {
        let listener = UnixListener::bind(self.root().join("data/station/station.sock")).unwrap();
        let state = Arc::new(Mutex::new(summary));
        let ops = Arc::new(Mutex::new(Vec::new()));
        let (state2, ops2) = (Arc::clone(&state), Arc::clone(&ops));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut line = String::new();
                if BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap_or(0)
                    == 0
                {
                    continue;
                }
                let request: Value = serde_json::from_str(line.trim()).unwrap();
                ops2.lock().unwrap().push(request.clone());
                let answer = match request["op"].as_str() {
                    Some("settings_get") => json!({
                        "status": "settings",
                        "view": {"values": {"fleet_summary": *state2.lock().unwrap()},
                                 "fields": [], "revision": "r1", "overrides": []}
                    }),
                    Some("settings_set") => {
                        let on = request["change"]["changes"]["fleet_summary"]
                            .as_bool()
                            .unwrap();
                        *state2.lock().unwrap() = on;
                        json!({
                            "status": "settings_saved",
                            "saved": {"values": {}, "applied": ["fleet_summary"],
                                      "restart_needed": [], "unapplied": [],
                                      "revision": "r2", "durability": "confirmed"}
                        })
                    }
                    _ => json!({"status": "error", "message": "unexpected"}),
                };
                writeln!(stream, "{answer}").unwrap();
            }
        });
        self.station = Some(Station {
            summary: state,
            ops,
        });
    }

    pub fn summary(&self) -> bool {
        *self.station.as_ref().unwrap().summary.lock().unwrap()
    }

    /// The `settings_set` values of `fleet_summary` that the station got.
    pub fn sets(&self) -> Vec<bool> {
        self.station
            .as_ref()
            .unwrap()
            .ops
            .lock()
            .unwrap()
            .iter()
            .filter(|op| op["op"] == "settings_set")
            .map(|op| op["change"]["changes"]["fleet_summary"].as_bool().unwrap())
            .collect()
    }

    pub fn hypr(&self, text: &str) {
        std::fs::create_dir_all(self.cfg().join("hypr")).unwrap();
        std::fs::write(self.cfg().join("hypr/hyprland.lua"), text).unwrap();
    }

    pub fn hypr_text(&self) -> String {
        std::fs::read_to_string(self.cfg().join("hypr/hyprland.lua")).unwrap()
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fleetdeck"));
        cmd.arg("--data-dir")
            .arg(self.root().join("data"))
            .args(args)
            .env_clear()
            .env("HOME", self.root().join("home"))
            .env("PATH", self.root().join("bin"))
            .env("OMARCHY_PATH", self.root().join("omarchy-root"))
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null());
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    pub fn install(&self) -> Output {
        self.run(&["omarchy", "install", "--yes"])
    }

    pub fn record(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.fd_dir().join("omarchy-install.json")).unwrap())
            .unwrap()
    }
}

pub fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

pub fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

pub fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

pub const LINE_START: &str = "pcall(dofile, \"";
pub const LINE_END: &str = "\") -- fleetdeck: managed line";

pub fn managed_line(rig: &Rig) -> String {
    format!(
        "{LINE_START}{}{LINE_END}",
        rig.fd_dir().join("omarchy/hypr.lua").display()
    )
}

pub fn sleep_path() -> String {
    std::env::split_paths(&std::env::var_os("PATH").expect("PATH is set"))
        .map(|dir| dir.join("sleep"))
        .find(|p| p.is_file())
        .expect("sleep is on PATH")
        .display()
        .to_string()
}
