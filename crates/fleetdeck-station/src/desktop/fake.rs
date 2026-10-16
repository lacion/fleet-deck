//! A fake desktop for the tests (R13 P3a): a temp `PATH` with shell-script
//! fakes that log their argv (one argument per line), a fake Hyprland
//! instance folder with a live `.socket.sock`, and an event socket that
//! announces a window for each `--class=` that the fake `chromium` logged.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use super::Desktop;

pub const SIGNATURE: &str = "sig_a_1";

const MONITORS: &str = r#"[
 {"id":0,"name":"DP-1","description":"Ultrawide","width":3440,"height":1440,"refreshRate":143.99800,"scale":1.25,"focused":true,"dpmsStatus":true,"activeWorkspace":{"id":1,"name":"1"}},
 {"id":1,"name":"HDMI-A-1","description":"Side","width":1920,"height":1080,"refreshRate":60.0,"scale":1.0,"focused":false,"dpmsStatus":false,"activeWorkspace":{"id":5,"name":"5"}}
]"#;

/// Which parts of the fake desktop exist.
#[derive(Clone, Copy)]
pub struct Opts {
    pub omarchy: bool,
    pub uwsm: bool,
    pub instance: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            omarchy: true,
            uwsm: false,
            instance: true,
        }
    }
}

/// One logged program call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub program: String,
    pub args: Vec<String>,
}

pub fn call(program: &str, args: &[&str]) -> Call {
    Call {
        program: program.to_owned(),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
    }
}

pub struct Rig {
    pub dir: tempfile::TempDir,
    pub runtime: PathBuf,
    pub data: PathBuf,
    pub desktop: Arc<Desktop>,
    /// Lines for the event socket; `None` closes the connection.
    events: Option<mpsc::UnboundedSender<Option<String>>>,
}

fn write_script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Rig {
    pub fn new(opts: Opts) -> Self {
        Self::without(opts, &[])
    }

    /// A rig where the programs in `missing` do not exist.
    #[allow(clippy::too_many_lines)]
    pub fn without(opts: Opts, missing: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        let runtime = root.join("run");
        let bin = root.join("bin");
        let omarchy_dir = root.join("omarchy");
        let data = root.join("data");
        for d in [&runtime, &bin, &data, &root.join("home")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(root.join("monitors.json"), MONITORS).unwrap();
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
        let script = |name: &str, body: String| {
            if !missing.contains(&name) {
                write_script(&bin, name, &body);
            }
        };
        script(
            "hyprctl",
            format!(
                "case \"$1\" in -j|instances) {reads};; *) {log};; esac\n\
                 case \"$*\" in\n\
                 \"instances -j\") /bin/cat {r}/instances.json;;\n\
                 \"-j monitors\") /bin/cat {r}/monitors.json;;\n\
                 \"-j clients\") if [ -f {r}/fail_clients_late ] && [ -s {r}/close.req ]; then exit 1; fi; /bin/cat {r}/clients.json;;\n\
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
            script(
                name,
                format!(
                    "{}\n[ -f {r}/fail_{name} ] && exit 2\nexit 0",
                    note("calls.log")
                ),
            );
        }
        script(
            "makoctl",
            format!(
                "case \"$1\" in mode) if [ $# -eq 1 ]; then {reads}; echo default; \
                 [ -f {r}/dnd ] && echo do-not-disturb; exit 0; fi;; esac\n{log}\nexit 0",
                reads = note("reads.log"),
                log = note("calls.log"),
            ),
        );
        script(
            "pgrep",
            format!(
                "{reads}\ncase \"$*\" in\n\
                 *hyprlock*) [ -f {r}/hyprlock ] && exit 0; exit 1;;\n\
                 *hypridle*) [ -f {r}/no_hypridle ] && exit 1; exit 0;;\n\
                 esac\nexit 1",
                reads = note("reads.log"),
            ),
        );
        let chromium_body = format!(
            "{log}\n[ -f {r}/chromium_fail ] && exit 3\n\
             for a in \"$@\"; do case \"$a\" in --user-data-dir=*) /bin/mkdir -p \"${{a#--user-data-dir=}}\";; esac; done\nexit 0",
            log = note("calls.log"),
        );
        script("chromium", chromium_body);
        if opts.uwsm {
            script(
                "uwsm-app",
                format!(
                    "{log}\nshift\n[ -f {r}/chromium_fail ] && exit 3\n\
                     for a in \"$@\"; do case \"$a\" in --user-data-dir=*) /bin/mkdir -p \"${{a#--user-data-dir=}}\";; esac; done\nexit 0",
                    log = note("calls.log"),
                ),
            );
        }
        let path = bin.as_os_str().to_owned();
        if opts.omarchy {
            std::fs::create_dir_all(omarchy_dir.join("bin")).unwrap();
            write_script(&omarchy_dir.join("bin"), "omarchy", "exit 0");
            std::fs::write(omarchy_dir.join("version"), "3.1.0\n").unwrap();
            for name in ["omarchy-lock-screen", "omarchy-toggle-idle"] {
                if let Ok(body) = std::fs::read_to_string(bin.join(name)) {
                    // The Omarchy scripts live in the Omarchy `bin` folder.
                    std::fs::remove_file(bin.join(name)).unwrap();
                    let body = body.trim_start_matches("#!/bin/sh\n").to_owned();
                    write_script(&omarchy_dir.join("bin"), name, &body);
                }
            }
        } else {
            for name in ["omarchy-lock-screen", "omarchy-toggle-idle"] {
                let _ = std::fs::remove_file(bin.join(name));
            }
        }
        let base = ChildEnv::scrubbed(
            [
                ("PATH", path),
                ("XDG_RUNTIME_DIR", runtime.as_os_str().to_owned()),
                ("HOME", root.join("home").into_os_string()),
                ("XDG_DATA_HOME", data.as_os_str().to_owned()),
                (
                    "OMARCHY_PATH",
                    if opts.omarchy {
                        omarchy_dir.as_os_str().to_owned()
                    } else {
                        root.join("no-omarchy").into_os_string()
                    },
                ),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v)),
        );
        let desktop = Arc::new(Desktop::with_env(base));
        Self {
            dir,
            runtime,
            data,
            desktop,
            events: None,
        }
        .with_instance(opts.instance)
    }

    fn with_instance(mut self, on: bool) -> Self {
        if !on {
            return self;
        }
        let inst = self.runtime.join("hypr").join(SIGNATURE);
        std::fs::create_dir_all(&inst).unwrap();
        let live = UnixListener::bind(inst.join(".socket.sock")).unwrap();
        tokio::spawn(async move {
            // Accept and drop: a connect succeeds.
            while let Ok((stream, _)) = live.accept().await {
                drop(stream);
            }
        });
        let events = UnixListener::bind(inst.join(".socket2.sock")).unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        self.events = Some(tx);
        tokio::spawn(serve_events(events, rx, self.dir.path().to_owned()));
        self
    }

    /// Closes the event connection (the station sees an EOF).
    pub fn drop_events_connection(&self) {
        let _ = self.events.as_ref().unwrap().send(None);
    }

    fn read_log(&self, name: &str) -> Vec<Call> {
        let text = std::fs::read_to_string(self.dir.path().join(name)).unwrap_or_default();
        let mut calls: Vec<Call> = Vec::new();
        for line in text.lines() {
            if let Some(program) = line.strip_prefix("== ") {
                calls.push(call(program, &[]));
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

    /// The read-only calls (`hyprctl -j …`, `pgrep`, `makoctl mode`).
    pub fn reads(&self) -> Vec<Call> {
        self.read_log("reads.log")
    }

    pub fn calls_of(&self, program: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.program == program)
            .collect()
    }

    /// Makes a fake program exit with 2 from now on.
    pub fn fail(&self, name: &str) {
        std::fs::write(self.dir.path().join(format!("fail_{name}")), "").unwrap();
    }

    /// Makes the fake `chromium` exit with 3.
    pub fn chromium_fails(&self) {
        std::fs::write(self.dir.path().join("chromium_fail"), "").unwrap();
    }

    /// Sets a state flag file of the fakes (`dnd`, `hyprlock`, `no_hypridle`,
    /// `hold_close`, `ignore_close`, `drop_events`, `fail_clients_late`).
    pub fn flag(&self, name: &str, on: bool) {
        let path = self.dir.path().join(name);
        if on {
            std::fs::write(path, "").unwrap();
        } else {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Removes a window from the fake `clients` list.
    pub fn forget_client(&self, address: &str) {
        let path = self.dir.path().join("clients.json");
        let mut list: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        list.retain(|c| c["address"] != address);
        std::fs::write(path, serde_json::to_string(&list).unwrap()).unwrap();
    }
}

/// Accepts the station's connection and announces a window for each
/// `--class=` in the call log; a `closewindow` dispatch makes the fake close
/// it. `lines` carries raw events from the test.
async fn serve_events(
    listener: UnixListener,
    mut lines: mpsc::UnboundedReceiver<Option<String>>,
    root: PathBuf,
) {
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
            line = lines.recv() => match line {
                Some(Some(line)) => {
                    if let Some(s) = stream.as_mut() {
                        let _ = s.write_all(format!("{line}\n").as_bytes()).await;
                    }
                }
                Some(None) => stream = None,
                None => return,
            },
            _ = tick.tick() => {
                // `drop_events`: the event connection ends once (a lost stream).
                if root.join("drop_events").exists() {
                    let _ = std::fs::remove_file(root.join("drop_events"));
                    stream = None;
                    continue;
                }
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
                        .write_all(format!("openwindow>>{address},1,{class},Title, with comma\n").as_bytes())
                        .await;
                }
                // `hold_close`: the requests wait; `ignore_close`: they are lost.
                if root.join("hold_close").exists() {
                    continue;
                }
                let requests = std::fs::read_to_string(root.join("close.req")).unwrap_or_default();
                let requests: Vec<&str> = requests.lines().collect();
                let ignore = root.join("ignore_close").exists();
                for address in requests.iter().skip(closed).filter(|_| !ignore) {
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
    list.push(serde_json::json!({"address": address, "class": class}));
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
