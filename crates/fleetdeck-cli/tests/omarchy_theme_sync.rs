//! `fleetdeck omarchy theme-sync` (R13 P4 task F1, design `omarchy-p4.md`
//! §3.7) against a fake `$HOME` with the generated theme file and a fake
//! station socket (a `UnixListener` that answers one scripted line).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

const GOOD: &str = r##"{
  "mode": "dark",
  "background": "#1a1b26",
  "foreground": "#C0CAF5",
  "accent": "#7aa2f7",
  "muted": "#565f89",
  "dark_background": "#16161e",
  "dark_foreground": "#a9b1d6",
  "bright_foreground": "#c0caf5",
  "selection": "#283457",
  "red": "#f7768e",
  "yellow": "#e0af68"
}"##;

const MISSING: &str = "Fleet Deck: no generated theme file; nothing was sent.\n";
const UNUSABLE: &str = "Fleet Deck: the theme file is not usable; nothing was sent.\n";
const SENT: &str = r#"{"status":"palette_stored","sent":true}"#;
const NOT_SENT: &str = r#"{"status":"palette_stored","sent":false}"#;

struct Rig {
    dir: tempfile::TempDir,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("fdt").tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("data/station")).unwrap();
        std::fs::create_dir_all(dir.path().join("home")).unwrap();
        Self {
            dir,
            requests: Arc::default(),
        }
    }

    fn theme_path(&self) -> PathBuf {
        self.dir
            .path()
            .join("home/.local/state/omarchy/current/theme/fleetdeck.json")
    }

    /// Writes the generated theme file.
    fn theme(&self, text: &str) {
        let path = self.theme_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Starts the fake station: it answers each request with the next line.
    fn station(&self, answers: &[&str]) {
        let listener =
            UnixListener::bind(self.dir.path().join("data/station/station.sock")).unwrap();
        let mut script: std::collections::VecDeque<String> =
            answers.iter().map(|a| (*a).to_owned()).collect();
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

    fn run(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
            .arg("--data-dir")
            .arg(self.dir.path().join("data"))
            .args(["omarchy", "theme-sync"])
            .env("HOME", self.dir.path().join("home"))
            .env("PATH", "/usr/bin:/bin")
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

/// A bad file sends nothing, says so on stderr, and exits 0.
fn unusable(text: &str) {
    let rig = Rig::new();
    rig.station(&[SENT]);
    rig.theme(text);
    let out = rig.run();
    assert_eq!(out.status.code(), Some(0), "{text}: {}", stderr_of(&out));
    assert!(out.stdout.is_empty());
    assert_eq!(stderr_of(&out), UNUSABLE, "{text}");
    assert!(rig.requests().is_empty(), "{text}");
}

#[test]
fn a_missing_file_sends_nothing_and_exits_0() {
    let rig = Rig::new();
    rig.station(&[SENT]);
    let out = rig.run();
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(out.stdout.is_empty());
    assert_eq!(stderr_of(&out), MISSING);
    assert!(rig.requests().is_empty());
}

#[test]
fn a_template_that_still_holds_a_mapping_is_not_usable() {
    unusable(&GOOD.replace("#7aa2f7", "{{ accent }}"));
    unusable("{{");
}

#[test]
fn a_file_that_is_not_json_is_not_usable() {
    unusable("not json");
    unusable("");
    unusable("[]");
}

#[test]
fn a_file_above_4_kib_is_not_usable() {
    let pad = |n: usize| format!("{GOOD}{}", " ".repeat(n - GOOD.len()));
    unusable(&pad(4097));
}

#[test]
fn a_bad_mode_is_not_usable() {
    unusable(&GOOD.replace("\"dark\"", "\"blue\""));
    unusable(&GOOD.replace("\"dark\"", "\"DARK\""));
}

#[test]
fn a_bad_color_is_not_usable() {
    for bad in ["#7aa2f", "7aa2f7", "#7aa2fg", "#7aa2f7f", "red", ""] {
        unusable(&GOOD.replace("#7aa2f7", bad));
    }
}

#[test]
fn a_missing_or_extra_key_is_not_usable() {
    unusable(&GOOD.replace("  \"red\": \"#f7768e\",\n", ""));
    unusable(&GOOD.replace("\"mode\"", "\"color3\": \"#000000\", \"mode\""));
}

#[test]
fn a_valid_file_sends_the_palette_to_the_station_and_prints_nothing() {
    let rig = Rig::new();
    rig.station(&[SENT]);
    rig.theme(GOOD);
    let out = rig.run();
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty(), "{}", stderr_of(&out));
    let requests = rig.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let got: Value = serde_json::from_str(&requests[0]).unwrap();
    assert_eq!(
        got,
        json!({"op": "palette_set", "palette": {"v": 1, "mode": "dark", "colors": {
            "background": "#1a1b26", "foreground": "#C0CAF5", "accent": "#7aa2f7",
            "muted": "#565f89", "dark_background": "#16161e",
            "dark_foreground": "#a9b1d6", "bright_foreground": "#c0caf5",
            "selection": "#283457", "red": "#f7768e", "yellow": "#e0af68"}}})
    );
}

#[test]
fn a_light_theme_is_sent_as_light() {
    let rig = Rig::new();
    rig.station(&[SENT]);
    rig.theme(&GOOD.replace("\"dark\"", "\"light\""));
    let out = rig.run();
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let got: Value = serde_json::from_str(&rig.requests()[0]).unwrap();
    assert_eq!(got["palette"]["mode"], "light");
}

#[test]
fn a_deck_that_is_not_connected_exits_with_1() {
    let rig = Rig::new();
    rig.station(&[NOT_SENT]);
    rig.theme(GOOD);
    let out = rig.run();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(
        stderr_of(&out),
        "The deck is not connected. The palette is sent when the link is up again.\n"
    );
    assert_eq!(rig.requests().len(), 1);
}

#[test]
fn without_a_station_socket_the_command_says_so() {
    let rig = Rig::new();
    rig.theme(GOOD);
    let out = rig.run();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: The local station is not running.\n"
    );
}

#[test]
fn a_refusal_of_the_station_exits_with_1() {
    let rig = Rig::new();
    rig.station(&[r#"{"status":"error","message":"The palette is not valid."}"#]);
    rig.theme(GOOD);
    let out = rig.run();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: the station refused the palette: The palette is not valid.\n"
    );
}
