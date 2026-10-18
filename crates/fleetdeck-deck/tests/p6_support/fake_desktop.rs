//! The fake desktop of the P6 end-to-end proofs: a temp `PATH` of shell
//! scripts that log their argv (one argument per line) to `calls.log`, a fake
//! Hyprland instance folder with a live `.socket.sock`, and an event socket
//! that announces a window for each `--class=` that the fake `chromium`
//! logged.
//!
//! The station crate has the same fake as a `#[cfg(test)]` module, which a
//! test of another crate cannot use; this is its copy for the deck tests, with
//! two additions: a gate that holds the next `hyprctl instances -j` (the
//! station runs it at the start of an action, before its final checks, so a
//! test can change the world while the target waits), and fixed sentinel texts
//! in the raw monitor and window data (proofs 23 to 26).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_station::desktop::Desktop;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};

pub const SIGNATURE: &str = "sig_a_1";
/// A sentinel in the raw monitor description of the fake desktop.
pub const MONITOR_SENTINEL: &str = "SENTINEL-MONITOR-DESC-91ab";
/// A sentinel in the raw title of every window of the fake desktop.
pub const WINDOW_SENTINEL: &str = "SENTINEL-WINDOW-TITLE-4c1e";

fn monitors() -> String {
    format!(
        r#"[
 {{"id":0,"name":"DP-1","description":"{MONITOR_SENTINEL}","width":3440,"height":1440,"refreshRate":143.99800,"scale":1.25,"focused":true,"dpmsStatus":true,"activeWorkspace":{{"id":1,"name":"1"}}}},
 {{"id":1,"name":"HDMI-A-1","description":"Side","width":1920,"height":1080,"refreshRate":60.0,"scale":1.0,"focused":false,"dpmsStatus":false,"activeWorkspace":{{"id":5,"name":"5"}}}}
]"#
    )
}

/// One logged program call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub program: String,
    pub args: Vec<String>,
}

pub struct Rig {
    pub dir: tempfile::TempDir,
    pub desktop: Arc<Desktop>,
}

fn write_script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Default for Rig {
    fn default() -> Self {
        Self::new()
    }
}

impl Rig {
    #[allow(clippy::too_many_lines)]
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        let runtime = root.join("run");
        let bin = root.join("bin");
        let omarchy_dir = root.join("omarchy");
        let data = root.join("data");
        for d in [&runtime, &bin, &data, &root.join("home")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(root.join("monitors.json"), monitors()).unwrap();
        std::fs::write(root.join("clients.json"), "[]").unwrap();
        std::fs::write(
            root.join("instances.json"),
            format!(r#"[{{"instance":"{SIGNATURE}","time":1,"pid":1,"wl_socket":"wayland-1"}}]"#),
        )
        .unwrap();
        let r = root.display();
        let note = |lg: &str| {
            format!(
                "{{ echo \"== $(/usr/bin/basename \"$0\")\"; for a in \"$@\"; do echo \"$a\"; done; }} >> {r}/{lg}"
            )
        };
        // The gate: while the file `gate` exists, `hyprctl instances -j`
        // waits and writes `entered`; the test removes `gate` to go on.
        write_script(
            &bin,
            "hyprctl",
            &format!(
                "case \"$1\" in -j|instances) {reads};; *) {log};; esac\n\
                 case \"$*\" in\n\
                 \"instances -j\") if [ -f {r}/gate ]; then : > {r}/entered; \
                 while [ -f {r}/gate ]; do /bin/sleep 0.02; done; fi; /bin/cat {r}/instances.json;;\n\
                 \"-j monitors\") /bin/cat {r}/monitors.json;;\n\
                 \"-j clients\") /bin/cat {r}/clients.json;;\n\
                 \"dispatch closewindow address:\"*) echo \"${{3#address:}}\" >> {r}/close.req; echo ok;;\n\
                 *) echo ok;;\n\
                 esac",
                reads = note("reads.log"),
                log = note("calls.log"),
            ),
        );
        for name in [
            "notify-send",
            "loginctl",
            "omarchy-lock-screen",
            "omarchy-toggle-idle",
        ] {
            write_script(
                &bin,
                name,
                &format!(
                    "{}\n[ -f {r}/fail_{name} ] && exit 2\nexit 0",
                    note("calls.log")
                ),
            );
        }
        write_script(
            &bin,
            "makoctl",
            &format!(
                "case \"$1\" in mode) if [ $# -eq 1 ]; then {reads}; echo default; \
                 [ -f {r}/dnd ] && echo do-not-disturb; exit 0; fi;; esac\n{log}\nexit 0",
                reads = note("reads.log"),
                log = note("calls.log"),
            ),
        );
        write_script(
            &bin,
            "date",
            &format!(
                "{reads}\nif [ -f {r}/offset ]; then /bin/cat {r}/offset; else echo +0200; fi",
                reads = note("reads.log"),
            ),
        );
        write_script(
            &bin,
            "pgrep",
            &format!(
                "{reads}\ncase \"$*\" in\n\
                 *hyprlock*) exit 1;;\n\
                 *hypridle*) exit 0;;\n\
                 esac\nexit 1",
                reads = note("reads.log"),
            ),
        );
        write_script(
            &bin,
            "chromium",
            &format!(
                "{log}\n\
                 for a in \"$@\"; do case \"$a\" in --user-data-dir=*) /bin/mkdir -p \"${{a#--user-data-dir=}}\";; esac; done\nexit 0",
                log = note("calls.log"),
            ),
        );
        // The fake `grim` logs its argv with the other changing programs (so
        // a test sees the order of the disclosure and `grim`) and writes the
        // bytes of `grim.out`.
        std::fs::write(root.join("grim.out"), b"").unwrap();
        write_script(
            &bin,
            "grim",
            &format!("{}\n/bin/cat {r}/grim.out", note("calls.log")),
        );
        std::fs::create_dir_all(omarchy_dir.join("bin")).unwrap();
        write_script(&omarchy_dir.join("bin"), "omarchy", "exit 0");
        std::fs::write(omarchy_dir.join("version"), "3.1.0\n").unwrap();
        for name in ["omarchy-lock-screen", "omarchy-toggle-idle"] {
            // The Omarchy scripts live in the Omarchy `bin` folder.
            let body = std::fs::read_to_string(bin.join(name)).unwrap();
            std::fs::remove_file(bin.join(name)).unwrap();
            let body = body.trim_start_matches("#!/bin/sh\n").to_owned();
            write_script(&omarchy_dir.join("bin"), name, &body);
        }
        let base = ChildEnv::scrubbed(
            [
                ("PATH", bin.as_os_str().to_owned()),
                ("XDG_RUNTIME_DIR", runtime.as_os_str().to_owned()),
                ("HOME", root.join("home").into_os_string()),
                ("XDG_DATA_HOME", data.as_os_str().to_owned()),
                ("OMARCHY_PATH", omarchy_dir.as_os_str().to_owned()),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v)),
        );
        // The station launches `grim` as `<exe> shim-exec --parent <pid> --
        // <grim> …`. The exe of a test is this script: it drops `--parent
        // <pid>` and the `--`, then runs the rest.
        write_script(
            &root,
            "shim.sh",
            "[ \"$1\" = shim-exec ] && [ \"$2\" = --parent ] || exit 3\n\
             shift 3\n[ \"$1\" = -- ] || exit 4\nshift\nexec \"$@\"",
        );
        let desktop = Arc::new(Desktop::with_env(base).with_shim_exe(root.join("shim.sh")));
        let rig = Self { dir, desktop };
        rig.start_instance(&runtime);
        rig
    }

    fn start_instance(&self, runtime: &Path) {
        let inst = runtime.join("hypr").join(SIGNATURE);
        std::fs::create_dir_all(&inst).unwrap();
        let live = UnixListener::bind(inst.join(".socket.sock")).unwrap();
        tokio::spawn(async move {
            // Accept and drop: a connect succeeds.
            while let Ok((stream, _)) = live.accept().await {
                drop(stream);
            }
        });
        let events = UnixListener::bind(inst.join(".socket2.sock")).unwrap();
        tokio::spawn(serve_events(events, self.dir.path().to_owned()));
    }

    fn read_log(&self, name: &str) -> Vec<Call> {
        let text = std::fs::read_to_string(self.dir.path().join(name)).unwrap_or_default();
        let mut calls: Vec<Call> = Vec::new();
        for line in text.lines() {
            if let Some(program) = line.strip_prefix("== ") {
                calls.push(Call {
                    program: program.to_owned(),
                    args: Vec::new(),
                });
            } else if let Some(last) = calls.last_mut() {
                last.args.push(line.to_owned());
            }
        }
        calls
    }

    /// The programs that changed something, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.read_log("calls.log")
    }

    pub fn calls_of(&self, program: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.program == program)
            .collect()
    }

    /// The names of the changing programs, in order.
    pub fn order(&self) -> Vec<String> {
        self.calls().into_iter().map(|c| c.program).collect()
    }

    /// The text of every log of the fakes (a sentinel scan).
    pub fn logs(&self) -> String {
        ["calls.log", "reads.log"]
            .iter()
            .map(|n| std::fs::read_to_string(self.dir.path().join(n)).unwrap_or_default())
            .collect()
    }

    /// Sets the bytes that the fake `grim` writes.
    pub fn set_grim(&self, bytes: &[u8]) {
        std::fs::write(self.dir.path().join("grim.out"), bytes).unwrap();
    }

    /// Closes the gate: the next `hyprctl instances -j` waits.
    pub fn close_gate(&self) {
        let _ = std::fs::remove_file(self.dir.path().join("entered"));
        std::fs::write(self.dir.path().join("gate"), "").unwrap();
    }

    /// Opens the gate: the waiting `hyprctl` goes on.
    pub fn open_gate(&self) {
        let _ = std::fs::remove_file(self.dir.path().join("gate"));
    }

    /// Waits until a `hyprctl instances -j` waits at the closed gate.
    pub async fn entered(&self) {
        let path = self.dir.path().join("entered");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no program waits at the gate"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// Accepts the station's connection and announces a window for each
/// `--class=` in the call log; a `closewindow` dispatch makes the fake close
/// it.
async fn serve_events(listener: UnixListener, root: PathBuf) {
    let mut announced: Vec<String> = Vec::new();
    let mut closed = 0_usize;
    let mut stream: Option<UnixStream> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((s, _)) = accepted {
                    stream = Some(s);
                }
            }
            _ = tick.tick() => {
                let Some(s) = stream.as_mut() else { continue };
                let calls = std::fs::read_to_string(root.join("calls.log")).unwrap_or_default();
                for class in calls.lines().filter_map(|l| l.strip_prefix("--class=")) {
                    if announced.iter().any(|c| c == class) {
                        continue;
                    }
                    announced.push(class.to_owned());
                    let address = format!("{:x}", 0x5a3e_8a40_u64 + announced.len() as u64);
                    add_client(&root, &format!("0x{address}"), class);
                    let _ = s
                        .write_all(format!("openwindow>>{address},1,{class},{WINDOW_SENTINEL}\n").as_bytes())
                        .await;
                }
                let requests = std::fs::read_to_string(root.join("close.req")).unwrap_or_default();
                let requests: Vec<&str> = requests.lines().collect();
                for address in requests.iter().skip(closed) {
                    remove_client(&root, address);
                    let bare = address.trim_start_matches("0x");
                    let _ = s.write_all(format!("closewindow>>{bare}\n").as_bytes()).await;
                }
                closed = requests.len();
            }
        }
    }
}

fn clients(root: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(root.join("clients.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn add_client(root: &Path, address: &str, class: &str) {
    let mut list = clients(root);
    list.push(serde_json::json!({"address": address, "class": class, "title": WINDOW_SENTINEL}));
    std::fs::write(
        root.join("clients.json"),
        serde_json::to_string(&list).unwrap(),
    )
    .unwrap();
}

fn remove_client(root: &Path, address: &str) {
    let mut list = clients(root);
    list.retain(|c| c["address"] != address);
    std::fs::write(
        root.join("clients.json"),
        serde_json::to_string(&list).unwrap(),
    )
    .unwrap();
}
