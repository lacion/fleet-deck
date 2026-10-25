//! `fleetdeck desktop crash --last` (R13 P4 task H, `omarchy-p4.md` §3.9)
//! against a fake station socket (a `UnixListener` that answers scripted
//! lines) and fake programs on a private `PATH`: `coredumpctl` prints a file,
//! `omarchy-notification-send` logs its argv, one argument per line.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

const ACCEPTED: &str = r#"{"status":"receipt","receipt":{"command_id":"c1","state":"accepted","station_seq":1,"result":{"session_id":"01SESSION","input_id":null},"reject":null,"at":1}}"#;
const REJECTED: &str = r#"{"status":"receipt","receipt":{"command_id":"c1","state":"rejected","station_seq":null,"result":null,"reject":{"code":"forbidden","message":"Not allowed here.","unsupported":null},"at":1}}"#;

struct Rig {
    dir: tempfile::TempDir,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("fd").tempdir().unwrap();
        for sub in ["bin", "data/station", "home", "omarchy/bin"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        let rig = Self {
            dir,
            requests: Arc::default(),
        };
        rig.script("omarchy/bin/omarchy", "exit 0");
        rig
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.dir.path().join(sub)
    }

    fn script(&self, sub: &str, body: &str) {
        let path = self.path(sub);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A fake `coredumpctl` that prints `json`.
    fn coredumpctl(&self, json: &str) {
        std::fs::write(self.path("list.json"), json).unwrap();
        self.script(
            "bin/coredumpctl",
            &format!("/bin/cat {}", self.path("list.json").display()),
        );
    }

    /// A fake `omarchy-notification-send` that logs its argv.
    fn notification_send(&self) {
        self.script(
            "bin/omarchy-notification-send",
            &format!(
                "for a in \"$@\"; do echo \"$a\"; done >> {}",
                self.path("send.log").display()
            ),
        );
    }

    fn sent(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("send.log"))
            .map(|t| t.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    fn home(&self) -> PathBuf {
        std::fs::canonicalize(self.path("home")).unwrap()
    }

    fn station_toml(&self, text: &str) {
        std::fs::write(self.path("data/station.toml"), text).unwrap();
    }

    /// Starts the fake station: it answers each request with the next line.
    fn station(&self, answers: &[&str]) {
        let listener = UnixListener::bind(self.path("data/station/station.sock")).unwrap();
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

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
            .arg("--data-dir")
            .arg(self.path("data"))
            .args(args)
            .env("HOME", self.path("home"))
            .env("PATH", self.path("bin"))
            .env("OMARCHY_PATH", self.path("omarchy"))
            .env("RUST_LOG", "warn")
            .env_remove("FLEETDECK_DATA_DIR")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap()
    }

    fn crash(&self) -> Output {
        self.run(&["desktop", "crash", "--last"])
    }
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < end, "timeout waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

const THREE: &str = r#"[
  {"time":1700000000000000,"pid":10,"sig":6,"exe":"/usr/bin/old"},
  {"time":1792299429000000,"pid":4242,"sig":11,"exe":"/usr/bin/newest","comm":"newest"},
  {"time":1750000000000000,"pid":30,"sig":4,"exe":"/usr/bin/mid"}
]"#;

#[test]
fn the_newest_crash_starts_a_session_prints_its_id_and_notifies() {
    let rig = Rig::new();
    rig.coredumpctl(THREE);
    rig.notification_send();
    rig.station_toml(&format!(
        "allowed_roots = [\"{}\"]\n\n[[provider_instance]]\nid = \"first\"\ndriver = \"claude\"\n\n[[provider_instance]]\nid = \"second\"\ndriver = \"codex\"\n",
        rig.home().display()
    ));
    rig.station(&[ACCEPTED]);
    let out = rig.crash();
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out), "01SESSION\n");
    assert!(out.stderr.is_empty(), "{}", stderr_of(&out));

    let requests = rig.requests();
    assert_eq!(requests.len(), 1);
    let request: Value = serde_json::from_str(&requests[0]).unwrap();
    assert_eq!(request["op"], "command");
    assert_eq!(request["command_id"], Value::Null);
    let command = &request["command"];
    assert_eq!(command["op"], "start");
    let spec = &command["spec"];
    assert_eq!(spec["provider_instance"], "first");
    assert_eq!(spec["cwd"], rig.home().to_str().unwrap());
    assert_eq!(spec["mode"], Value::Null);
    let omarchy = rig.path("omarchy");
    let expected = format!(
        "A process crashed on this Omarchy machine and I want to know why.\n\
         \n\
         What systemd-coredump recorded:\n\
         \x20 process:  newest\n\
         \x20 PID:      4242\n\
         \x20 binary:   /usr/bin/newest\n\
         \x20 signal:   11\n\
         \x20 time:     2026-10-18 04:57:09 UTC\n\
         \n\
         Use the diagnose-crash skill: it covers how to investigate, what to report, and\n\
         when a crash is worth reporting upstream to Omarchy. If your harness has no skill\n\
         mechanism, read the skill files directly and follow them instead:\n\
         \n\
         \x20 {}/default/agents/skills/diagnose-crash/SKILL.md\n",
        omarchy.display()
    );
    assert_eq!(command["prompt"]["text"], expected.as_str());

    wait_until("the notification", || rig.sent().len() >= 10);
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_fleetdeck")).unwrap();
    assert_eq!(
        rig.sent(),
        [
            "--app-name",
            "Fleet Deck",
            "-u",
            "normal",
            "Fleet Deck",
            "A crash diagnosis session started",
            "--exec",
            exe.to_str().unwrap(),
            "desktop",
            "open"
        ]
    );
}

#[test]
fn a_hostile_exe_and_a_pid_that_is_not_digits() {
    let rig = Rig::new();
    rig.notification_send();
    rig.station(&[ACCEPTED]);
    let hostile = format!(
        "/usr/bin/ev\\u001b[31mil\\n\\u202egnp\\u2066x\\u2069{}",
        "z".repeat(500)
    );
    rig.coredumpctl(&format!(
        r#"[{{"time":5,"pid":"77","sig":"SIGSEGV","exe":"{hostile}"}}]"#
    ));
    let out = rig.crash();
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let requests = rig.requests();
    let request: Value = serde_json::from_str(&requests[0]).unwrap();
    let text = request["command"]["prompt"]["text"].as_str().unwrap();
    assert!(!text.chars().any(|c| c.is_control() && c != '\n'), "{text}");
    assert!(!text.contains(['\u{202e}', '\u{2066}', '\u{2069}']));
    let binary = text.lines().find(|l| l.contains("binary:")).unwrap();
    assert_eq!(binary.chars().count(), "  binary:   ".len() + 200);
    assert!(binary.contains("/usr/bin/ev[31milgnpxzzz"), "{binary}");
    assert!(text.contains("  signal:   SIGSEGV\n"));
    assert!(text.contains("  process:  ev[31milgnpxzzz"), "{text}");

    // A pid that is not digits: the entry is refused and nothing starts.
    let rig = Rig::new();
    rig.coredumpctl(r#"[{"time":5,"pid":"7;rm","exe":"/usr/bin/x"}]"#);
    let out = rig.crash();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr_of(&out), "fleetdeck: No crash was found.\n");
    assert!(out.stdout.is_empty());
}

#[test]
fn the_list_problems_have_their_own_texts() {
    for (json, text) in [
        ("[]", "No crash was found."),
        ("not json", "coredumpctl is not available."),
    ] {
        let rig = Rig::new();
        rig.coredumpctl(json);
        let out = rig.crash();
        assert_eq!(out.status.code(), Some(1), "{json}");
        assert_eq!(stderr_of(&out), format!("fleetdeck: {text}\n"), "{json}");
        assert!(out.stdout.is_empty());
    }
    // No tool on the PATH.
    let rig = Rig::new();
    let out = rig.crash();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: coredumpctl is not available.\n"
    );
}

#[test]
fn a_failing_coredumpctl_is_not_available() {
    let rig = Rig::new();
    rig.script("bin/coredumpctl", "echo '[]'; exit 1");
    let out = rig.crash();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: coredumpctl is not available.\n"
    );
}

#[test]
fn without_omarchy_the_command_says_so() {
    let rig = Rig::new();
    rig.coredumpctl(THREE);
    std::fs::remove_file(rig.path("omarchy/bin/omarchy")).unwrap();
    let out = rig.crash();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: Omarchy was not found on this machine.\n"
    );
    assert!(rig.requests().is_empty());
}

#[test]
fn a_home_outside_allowed_roots_is_refused_before_any_start() {
    let rig = Rig::new();
    rig.coredumpctl(THREE);
    rig.station(&[ACCEPTED]);
    rig.station_toml(&format!(
        "allowed_roots = [\"{}\"]\n",
        rig.path("elsewhere").display()
    ));
    let out = rig.crash();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: Add your home directory to allowed_roots to diagnose a crash.\n"
    );
    assert!(out.stdout.is_empty());
    assert!(rig.requests().is_empty());
}

#[test]
fn a_station_without_a_provider_instance_is_refused_before_any_start() {
    let rig = Rig::new();
    rig.coredumpctl(THREE);
    rig.station(&[ACCEPTED]);
    rig.station_toml("provider_instance = []\n");
    let out = rig.crash();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: No provider is configured on this station.\n"
    );
    assert!(rig.requests().is_empty());
}

#[test]
fn a_station_that_is_not_running_is_named() {
    let rig = Rig::new();
    rig.coredumpctl(THREE);
    let out = rig.crash();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: The local station is not running.\n"
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn a_refused_start_prints_the_code_and_the_message() {
    let rig = Rig::new();
    rig.coredumpctl(THREE);
    rig.notification_send();
    rig.station(&[REJECTED]);
    let out = rig.crash();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr_of(&out), "fleetdeck: forbidden: Not allowed here.\n");
    assert!(out.stdout.is_empty());
    assert!(rig.sent().is_empty());
}

#[test]
fn a_missing_notification_tool_is_a_warning_and_the_exit_is_0() {
    let rig = Rig::new();
    rig.coredumpctl(THREE);
    rig.station(&[ACCEPTED]);
    let out = rig.crash();
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out), "01SESSION\n");
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: the notification was not sent.\n"
    );
}

#[test]
fn without_last_the_usage_is_printed_with_exit_2() {
    let rig = Rig::new();
    rig.coredumpctl(THREE);
    rig.station(&[ACCEPTED]);
    let out = rig.run(&["desktop", "crash"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_of(&out).contains("--last"), "{}", stderr_of(&out));
    assert!(rig.requests().is_empty());
}
