//! `fleetdeck desktop wall next | mode next | show` (R13 P4 task E, design
//! `omarchy-p4.md` §3.6) against a fake station socket (a `UnixListener`
//! that answers one scripted line) and a fake `hyprctl` on a private `PATH`
//! that logs its argv, one argument per line.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

struct Rig {
    dir: tempfile::TempDir,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("fd").tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::create_dir_all(dir.path().join("data/station")).unwrap();
        std::fs::create_dir_all(dir.path().join("home")).unwrap();
        Self {
            dir,
            requests: Arc::default(),
        }
    }

    /// Starts the fake station: it answers each request with the next line.
    fn station(&self, answers: &[&str]) {
        let listener =
            UnixListener::bind(self.dir.path().join("data/station/station.sock")).unwrap();
        let mut script: VecDeque<String> = answers.iter().map(|a| (*a).to_owned()).collect();
        let requests = Arc::clone(&self.requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let mut line = String::new();
                let read = BufReader::new(stream.try_clone().unwrap()).read_line(&mut line);
                if read.unwrap_or(0) == 0 {
                    continue;
                }
                requests.lock().unwrap().push(line.trim().to_owned());
                if let Some(text) = script.pop_front() {
                    let mut stream = stream;
                    writeln!(stream, "{text}").unwrap();
                }
            }
        });
    }

    /// A fake `hyprctl`: it logs its argv and prints `clients.json` for
    /// `clients -j` and `active.json` for `activeworkspace -j`.
    fn hyprctl(&self, clients: &str, active: &str) {
        let root = self.dir.path();
        std::fs::write(root.join("clients.json"), clients).unwrap();
        std::fs::write(root.join("active.json"), active).unwrap();
        let log = root.join("hyprctl.log");
        let path = root.join("bin/hyprctl");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\n{{ echo \"== hyprctl\"; for a in \"$@\"; do echo \"$a\"; done; }} >> {log}\n\
                 case \"$*\" in\n\
                 \"clients -j\") /bin/cat {r}/clients.json;;\n\
                 \"activeworkspace -j\") /bin/cat {r}/active.json;;\n\
                 *) echo ok;;\n\
                 esac\n",
                log = log.display(),
                r = root.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn calls(&self) -> Vec<Vec<String>> {
        let Ok(text) = std::fs::read_to_string(self.dir.path().join("hyprctl.log")) else {
            return Vec::new();
        };
        let mut calls: Vec<Vec<String>> = Vec::new();
        for line in text.lines() {
            if line == "== hyprctl" {
                calls.push(Vec::new());
            } else if let Some(last) = calls.last_mut() {
                last.push(line.to_owned());
            }
        }
        calls
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
            .arg("--data-dir")
            .arg(self.dir.path().join("data"))
            .args(args)
            .env("HOME", self.dir.path().join("home"))
            .env("PATH", self.dir.path().join("bin"))
            .env("RUST_LOG", "warn")
            .env_remove("FLEETDECK_DATA_DIR")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap()
    }
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const ACCEPTED: &str =
    r#"{"status":"wall_directive","results":[{"display_id":"d1","code":null,"text":null}]}"#;

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

#[test]
fn wall_next_and_mode_next_send_their_intent_and_print_nothing_on_success() {
    let rig = Rig::new();
    rig.station(&[ACCEPTED, ACCEPTED]);
    let out = rig.run(&["desktop", "wall", "next"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty(), "{}", stderr_of(&out));
    let out = rig.run(&["desktop", "wall", "mode", "next"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty(), "{}", stderr_of(&out));
    assert_eq!(
        rig.requests(),
        [
            r#"{"op":"wall_directive","intent":"next_needs_you"}"#,
            r#"{"op":"wall_directive","intent":"next_mode"}"#,
        ]
    );
}

#[test]
fn a_refused_result_prints_its_code_and_text_and_exits_with_1() {
    let rig = Rig::new();
    rig.station(&[
        r#"{"status":"wall_directive","results":[
            {"display_id":"d1","code":null,"text":null},
            {"display_id":"d2","code":"nothing_to_show","text":"No session of this station needs you."},
            {"display_id":"d3","code":"not_bound","text":"This station has no wall display bound to it."}]}"#
            .replace('\n', "")
            .as_str(),
        r#"{"status":"wall_directive","results":[{"display_id":"d1","code":"deck_down","text":"The deck is not connected."}]}"#,
    ]);
    let out = rig.run(&["desktop", "wall", "next"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "nothing_to_show: No session of this station needs you.\n"
    );
    let out = rig.run(&["desktop", "wall", "mode", "next"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr_of(&out), "deck_down: The deck is not connected.\n");
}

#[test]
fn a_station_with_no_wall_prints_not_bound() {
    let rig = Rig::new();
    rig.station(&[r#"{"status":"wall_directive","results":[]}"#]);
    let out = rig.run(&["desktop", "wall", "next"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "not_bound: This station has no wall display bound to it.\n"
    );
}

#[test]
fn without_a_station_socket_the_commands_say_so() {
    let rig = Rig::new();
    for args in [
        ["desktop", "wall", "next"].as_slice(),
        ["desktop", "wall", "mode", "next"].as_slice(),
    ] {
        let out = rig.run(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(
            stderr_of(&out),
            "unavailable: The local station is not running.\n",
            "{args:?}"
        );
    }
}

#[test]
fn wall_show_focuses_the_first_wall_window() {
    let rig = Rig::new();
    rig.hyprctl(
        r#"[{"class":"chromium","address":"0x1","workspace":{"id":1}},
            {"class":"fleetdeck-preview-a","address":"0x2","workspace":{"id":1}},
            {"class":"fleetdeck-wall-b","address":"0x3f","workspace":{"id":5}},
            {"class":"fleetdeck-wall-c","address":"0x40","workspace":{"id":1}}]"#,
        r#"{"id":1,"name":"1"}"#,
    );
    let out = rig.run(&["desktop", "wall", "show"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(out.stdout.is_empty());
    assert_eq!(
        rig.calls(),
        [
            argv(&["clients", "-j"]),
            argv(&["activeworkspace", "-j"]),
            argv(&["dispatch", "focuswindow", "address:0x3f"]),
        ]
    );
}

#[test]
fn wall_show_goes_back_when_the_wall_is_on_the_active_workspace() {
    let rig = Rig::new();
    rig.hyprctl(
        r#"[{"class":"fleetdeck-wall-b","address":"0x3f","workspace":{"id":5}}]"#,
        r#"{"id":5,"name":"5"}"#,
    );
    let out = rig.run(&["desktop", "wall", "show"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert_eq!(
        rig.calls(),
        [
            argv(&["clients", "-j"]),
            argv(&["activeworkspace", "-j"]),
            argv(&["dispatch", "workspace", "previous"]),
        ]
    );
}

#[test]
fn wall_show_without_a_wall_window_or_a_valid_address_exits_with_1() {
    let rig = Rig::new();
    rig.hyprctl(
        r#"[{"class":"chromium","address":"0x1","workspace":{"id":1}},
            {"class":"fleetdeck-wall-b","address":"0xZZ","workspace":{"id":5}},
            {"class":"fleetdeck-wall-c","address":"--help","workspace":{"id":5}}]"#,
        r#"{"id":1,"name":"1"}"#,
    );
    let out = rig.run(&["desktop", "wall", "show"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: No Fleet Deck wall window is open.\n"
    );
    // Only the window list was read; nothing was dispatched.
    assert_eq!(rig.calls(), [argv(&["clients", "-j"])]);
}

#[test]
fn wall_show_without_hyprctl_says_so() {
    let rig = Rig::new();
    let out = rig.run(&["desktop", "wall", "show"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr_of(&out), "fleetdeck: hyprctl is not installed.\n");
}
