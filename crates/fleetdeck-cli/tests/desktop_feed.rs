//! `fleetdeck desktop feed`, `--notify` and `desktop open` (R13 P4 task B2,
//! `omarchy-p4.md` §3.3) against a fake station socket (a `UnixListener`
//! that answers scripted lines) and fake programs on a private `PATH` that
//! log their argv, one argument per line, as the P3 desktop rig does.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(20);
const UNKNOWN: &str = r#"{"v":1,"deck":"unknown"}"#;
const BOARD: &str = "https://deck.example.ts.net:7420";

/// What the fake station does with one request.
enum Step {
    Reply(String),
    /// Closes the connection without an answer.
    Close,
}

/// A temp node: the data directory with the station socket, and a `bin`
/// directory that is the whole `PATH` of the commands.
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

    /// Starts the fake station. After the script it holds every call open,
    /// like the long poll of the real station.
    fn station(&self, script: Vec<Step>) {
        let listener =
            UnixListener::bind(self.dir.path().join("data/station/station.sock")).unwrap();
        let script = Mutex::new(VecDeque::from(script));
        let requests = Arc::clone(&self.requests);
        std::thread::spawn(move || {
            let mut parked = Vec::new();
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let mut line = String::new();
                let read = BufReader::new(stream.try_clone().unwrap()).read_line(&mut line);
                if read.unwrap_or(0) == 0 {
                    continue;
                }
                requests.lock().unwrap().push(line.trim().to_owned());
                let step = script.lock().unwrap().pop_front();
                match step {
                    Some(Step::Reply(text)) => {
                        let mut stream = stream;
                        writeln!(stream, "{text}").unwrap();
                    }
                    Some(Step::Close) => {}
                    None => parked.push(stream),
                }
            }
        });
    }

    /// A fake program that logs its argv to `<name>.log`.
    fn tool(&self, name: &str) {
        let log = self.dir.path().join(format!("{name}.log"));
        let path = self.dir.path().join("bin").join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\n{{ echo \"== {name}\"; for a in \"$@\"; do echo \"$a\"; done; }} >> {}\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn tools(&self, names: &[&str]) {
        for name in names {
            self.tool(name);
        }
    }

    /// The calls that a fake program logged, one argv for each call.
    fn calls(&self, name: &str) -> Vec<Vec<String>> {
        let Ok(text) = std::fs::read_to_string(self.dir.path().join(format!("{name}.log"))) else {
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

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn stderr_path(&self) -> PathBuf {
        self.dir.path().join("stderr.txt")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fleetdeck"));
        cmd.arg("--data-dir")
            .arg(self.dir.path().join("data"))
            .args(args)
            .env("HOME", self.dir.path().join("home"))
            .env("PATH", self.dir.path().join("bin"))
            .env("RUST_LOG", "warn")
            .env_remove("FLEETDECK_DATA_DIR")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn feed(&self, args: &[&str]) -> Feed {
        let mut cmd = self.command(args);
        let stderr = std::fs::File::create(self.stderr_path()).unwrap();
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send((Instant::now(), line)).is_err() {
                    break;
                }
            }
        });
        Feed { child, lines }
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(self.stderr_path()).unwrap_or_default()
    }
}

struct Feed {
    child: Child,
    lines: Receiver<(Instant, String)>,
}

impl Feed {
    fn line(&self) -> (Instant, String) {
        self.lines.recv_timeout(WAIT).expect("a feed line")
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.line().1).unwrap()
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_until(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !ready() {
        assert!(Instant::now() < deadline, "timeout: {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn item(n: u32, age_s: u32) -> Value {
    json!({"station": "srv", "station_id": "st1", "session": format!("s{n}"),
           "request": format!("r{n}"), "callsign": "lynx-12", "kind": "tool", "age_s": age_s})
}

fn summary(at: &str, items: Vec<Value>, board: Value) -> Value {
    json!({
        "v": 1, "at": at, "board": board, "deck": "up",
        "counts": {"working": 1, "idle": 0, "needs_you": items.len(), "blocked": 0, "lost": 0},
        "needs_you": items,
        "usage": {"label": "claude · 5 h", "pct": 63, "resets_in_s": 100},
        "stations": [{"name": "srv", "up": true, "cpu_load_pct": 41}]
    })
}

fn answer(seq: u64, link: &str, held_s: u64, summary: &Value) -> Step {
    Step::Reply(
        json!({"status": "fleet_summary", "seq": seq, "link": link, "held_s": held_s,
               "summary": summary})
        .to_string(),
    )
}

fn off() -> Step {
    Step::Reply(json!({"status": "fleet_summary_off"}).to_string())
}

fn plain(at: &str) -> Value {
    summary(at, vec![item(1, 95)], json!(BOARD))
}

// ---- the feed lines ----

#[test]
fn desktop_feed_prints_a_corrected_line_for_each_answer() {
    let rig = Rig::new();
    rig.station(vec![
        answer(1, "up", 0, &plain("t1")),
        answer(2, "down", 10, &plain("t2")),
        answer(3, "up", 151, &plain("t3")),
    ]);
    let feed = rig.feed(&["desktop", "feed"]);
    let first = feed.json();
    assert_eq!(
        first,
        plain("t1"),
        "an unchanged summary is printed as it is"
    );
    let second = feed.json();
    assert_eq!(second["deck"], "down");
    assert_eq!(second["needs_you"][0]["age_s"], 105);
    assert_eq!(second["usage"]["resets_in_s"], 90);
    let third = feed.json();
    assert_eq!(third["deck"], "unknown", "stale: held_s above 150");
    assert_eq!(third["needs_you"][0]["age_s"], 246);
    assert_eq!(third["usage"]["resets_in_s"], 0, "not below 0");
    wait_until("the fourth call", || rig.requests().len() >= 4);
    assert_eq!(
        rig.requests(),
        [
            r#"{"op":"fleet_summary","after":0}"#,
            r#"{"op":"fleet_summary","after":1}"#,
            r#"{"op":"fleet_summary","after":2}"#,
            r#"{"op":"fleet_summary","after":3}"#,
        ]
    );
}

#[test]
fn desktop_feed_a_line_has_no_newline_inside() {
    let rig = Rig::new();
    rig.station(vec![answer(1, "up", 0, &plain("t1"))]);
    let feed = rig.feed(&["desktop", "feed"]);
    let (_, line) = feed.line();
    assert!(line.starts_with(r#"{"v":1,"#), "{line}");
    assert!(
        !line.contains("\": ") && !line.contains(", \""),
        "compact: {line}"
    );
}

#[test]
fn desktop_feed_no_summary_yet_prints_the_unknown_line() {
    let rig = Rig::new();
    rig.station(vec![
        answer(0, "down", 0, &Value::Null),
        answer(1, "up", 0, &plain("t1")),
    ]);
    let feed = rig.feed(&["desktop", "feed"]);
    assert_eq!(feed.line().1, UNKNOWN);
    assert_eq!(feed.json()["deck"], "up");
}

#[test]
fn desktop_feed_setting_off_prints_unknown_and_waits_5_seconds() {
    let rig = Rig::new();
    rig.station(vec![off(), answer(1, "up", 0, &plain("t1"))]);
    let feed = rig.feed(&["desktop", "feed"]);
    let (first_at, first) = feed.line();
    assert_eq!(first, UNKNOWN);
    let (second_at, second) = feed.line();
    assert!(second.contains(r#""deck":"up""#), "{second}");
    let gap = second_at - first_at;
    assert!(
        gap >= Duration::from_millis(4500),
        "a fixed 5 s wait: {gap:?}"
    );
    assert!(gap < Duration::from_secs(9), "{gap:?}");
    assert_eq!(
        rig.requests(),
        [
            r#"{"op":"fleet_summary","after":0}"#,
            r#"{"op":"fleet_summary","after":0}"#
        ]
    );
}

#[test]
fn desktop_feed_a_failure_prints_unknown_backs_off_and_asks_with_after_0() {
    let rig = Rig::new();
    rig.station(vec![
        answer(5, "up", 0, &plain("t1")),
        Step::Close,
        Step::Reply("this is not json".to_owned()),
        answer(2, "up", 0, &plain("t2")),
    ]);
    let feed = rig.feed(&["desktop", "feed"]);
    assert_eq!(feed.json()["deck"], "up");
    let (closed_at, closed) = feed.line();
    assert_eq!(closed, UNKNOWN, "the socket closes");
    let (invalid_at, invalid) = feed.line();
    assert_eq!(invalid, UNKNOWN, "an invalid answer");
    let (good_at, good) = feed.line();
    assert!(good.contains(r#""deck":"up""#), "{good}");
    let first_wait = invalid_at - closed_at;
    let second_wait = good_at - invalid_at;
    assert!(
        first_wait >= Duration::from_millis(900),
        "1 s: {first_wait:?}"
    );
    assert!(
        first_wait < Duration::from_millis(1900),
        "1 s: {first_wait:?}"
    );
    assert!(
        second_wait >= Duration::from_millis(1900),
        "2 s: {second_wait:?}"
    );
    assert!(
        second_wait < Duration::from_millis(3500),
        "2 s: {second_wait:?}"
    );
    wait_until("the fifth call", || rig.requests().len() >= 5);
    let after: Vec<String> = rig.requests();
    assert_eq!(
        after,
        [
            r#"{"op":"fleet_summary","after":0}"#,
            r#"{"op":"fleet_summary","after":5}"#,
            r#"{"op":"fleet_summary","after":0}"#,
            r#"{"op":"fleet_summary","after":0}"#,
            r#"{"op":"fleet_summary","after":2}"#,
        ],
        "after a failure the next call asks with after 0"
    );
}

#[test]
fn desktop_feed_without_a_station_prints_unknown_and_retries() {
    let rig = Rig::new();
    let feed = rig.feed(&["desktop", "feed"]);
    let (first_at, first) = feed.line();
    let (second_at, second) = feed.line();
    assert_eq!((first.as_str(), second.as_str()), (UNKNOWN, UNKNOWN));
    assert!(second_at - first_at >= Duration::from_millis(900));
}

#[test]
fn desktop_feed_ends_with_exit_0_when_stdout_is_closed() {
    let rig = Rig::new();
    let mut child = rig
        .command(&["desktop", "feed"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), UNKNOWN);
    drop(stdout);
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the feed did not end");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(0));
}

// ---- --notify ----

const THREE_TOOLS: [&str; 3] = ["omarchy-notification-send", "busctl", "jq"];

fn exe() -> String {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_fleetdeck"))
        .unwrap()
        .display()
        .to_string()
}

fn strings(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|a| (*a).to_owned()).collect()
}

fn argv_of(n: u32) -> Vec<String> {
    strings(&[
        "--app-name",
        "Fleet Deck",
        "-u",
        "normal",
        "lynx-12 needs you",
        "Approval on srv",
        "--exec",
        &exe(),
        "desktop",
        "open",
        &format!("st1/s{n}"),
        "--request",
        &format!("r{n}"),
    ])
}

fn more_argv(text: &str) -> Vec<String> {
    strings(&[
        "--app-name",
        "Fleet Deck",
        "-u",
        "normal",
        "Fleet Deck",
        text,
        "--exec",
        &exe(),
        "desktop",
        "open",
    ])
}

fn needs(items: Vec<Value>, at: &str) -> Value {
    summary(at, items, json!(BOARD))
}

#[test]
fn desktop_feed_notify_baseline_dedup_age_rule_and_burst_of_six() {
    let rig = Rig::new();
    rig.tools(&THREE_TOOLS);
    let burst: Vec<Value> = [item(1, 5), item(2, 5), item(3, 61)]
        .into_iter()
        .chain((11..=16).map(|n| item(n, 5)))
        .collect();
    rig.station(vec![
        // The first good summary is only the baseline.
        answer(1, "up", 0, &needs(vec![item(1, 5)], "t1")),
        answer(2, "up", 0, &needs(vec![item(1, 5), item(2, 5)], "t2")),
        // A new key above 60 s does not notify.
        answer(
            3,
            "up",
            0,
            &needs(vec![item(1, 5), item(2, 5), item(3, 61)], "t3"),
        ),
        // The same keys again: nothing.
        answer(
            4,
            "up",
            0,
            &needs(vec![item(1, 5), item(2, 5), item(3, 61)], "t4"),
        ),
        // Six new keys: five notifications and one "more".
        answer(5, "up", 0, &needs(burst, "t5")),
    ]);
    let feed = rig.feed(&["desktop", "feed", "--notify"]);
    for _ in 0..5 {
        feed.line();
    }
    wait_until("seven notifications", || {
        rig.calls("omarchy-notification-send").len() >= 7
    });
    std::thread::sleep(Duration::from_millis(400));
    let calls = rig.calls("omarchy-notification-send");
    assert_eq!(calls.len(), 7, "{calls:?}");
    assert_eq!(calls[0], argv_of(2));
    for (call, n) in calls[1..6].iter().zip(11..=15) {
        assert_eq!(*call, argv_of(n));
    }
    assert_eq!(calls[6], more_argv("1 more sessions need you"));
    assert!(calls.iter().flatten().all(|a| a != "--"));
    assert!(rig.stderr().is_empty(), "{}", rig.stderr());
}

#[test]
fn desktop_feed_notify_argv_for_a_dash_callsign_an_option_like_name_and_a_bad_id() {
    let rig = Rig::new();
    rig.tools(&THREE_TOOLS);
    let dash = json!({"station": "srv", "station_id": "st1", "session": "sess-12345678",
        "request": "q1", "callsign": "-rf", "kind": "command", "age_s": 5});
    let option_like = json!({"station": "--exec", "station_id": "st1", "session": "abc",
        "request": "q2", "callsign": null, "kind": "question", "age_s": 5});
    let bad_id = json!({"station": "a<b>&c", "station_id": "st1", "session": "s3",
        "request": "a b", "callsign": "lynx-12", "kind": "file_change", "age_s": 5});
    rig.station(vec![
        answer(1, "up", 0, &needs(vec![], "t1")),
        answer(2, "up", 0, &needs(vec![dash, option_like, bad_id], "t2")),
    ]);
    let feed = rig.feed(&["desktop", "feed", "--notify"]);
    feed.line();
    feed.line();
    wait_until("three notifications", || {
        rig.calls("omarchy-notification-send").len() >= 3
    });
    let calls = rig.calls("omarchy-notification-send");
    let exec = |call: &[String]| call[6..].to_vec();
    assert_eq!(
        calls[0][4], "sess-123 needs you",
        "a callsign that starts with `-`"
    );
    assert_eq!(calls[0][5], "Approval on srv");
    assert_eq!(
        exec(&calls[0]),
        strings(&[
            "--exec",
            &exe(),
            "desktop",
            "open",
            "st1/sess-12345678",
            "--request",
            "q1"
        ])
    );
    assert_eq!(calls[1][4], "abc needs you");
    assert_eq!(
        calls[1][5], "Question on --exec",
        "a name that looks like an option"
    );
    assert_eq!(calls[2][4], "lynx-12 needs you");
    assert_eq!(calls[2][5], "Approval on abc");
    assert_eq!(calls[2].len(), 6, "an id outside the grammar: no --exec");
    for call in &calls {
        assert!(
            !call[4].starts_with('-') && !call[5].starts_with('-'),
            "{call:?}"
        );
    }
}

#[test]
fn desktop_feed_notify_warns_once_for_each_missing_tool_and_goes_on() {
    let rig = Rig::new();
    rig.tool("omarchy-notification-send");
    rig.station(vec![
        answer(1, "up", 0, &needs(vec![], "t1")),
        answer(2, "up", 0, &needs(vec![item(1, 5)], "t2")),
        answer(3, "up", 0, &needs(vec![item(1, 5), item(2, 5)], "t3")),
    ]);
    let feed = rig.feed(&["desktop", "feed", "--notify"]);
    for _ in 0..3 {
        feed.line();
    }
    wait_until("the third call", || rig.requests().len() >= 4);
    std::thread::sleep(Duration::from_millis(300));
    let stderr = rig.stderr();
    assert_eq!(
        stderr
            .matches("fleetdeck: busctl is not installed; notifications are off.")
            .count(),
        1,
        "{stderr}"
    );
    assert_eq!(
        stderr
            .matches("fleetdeck: jq is not installed; notifications are off.")
            .count(),
        1,
        "{stderr}"
    );
    assert!(
        !stderr.contains("omarchy-notification-send is not installed"),
        "{stderr}"
    );
    assert!(
        rig.calls("omarchy-notification-send").is_empty(),
        "notifications stay off"
    );
}

#[test]
fn desktop_feed_notify_warns_for_all_three_tools_when_none_is_installed() {
    let rig = Rig::new();
    rig.station(vec![
        answer(1, "up", 0, &needs(vec![], "t1")),
        answer(2, "up", 0, &needs(vec![item(1, 5)], "t2")),
    ]);
    let feed = rig.feed(&["desktop", "feed", "--notify"]);
    feed.line();
    feed.line();
    wait_until("the warnings", || rig.stderr().lines().count() >= 3);
    let stderr = rig.stderr();
    for tool in THREE_TOOLS {
        assert_eq!(
            stderr
                .matches(&format!(
                    "fleetdeck: {tool} is not installed; notifications are off."
                ))
                .count(),
            1,
            "{stderr}"
        );
    }
}

#[test]
fn desktop_feed_a_baseline_alone_prints_no_tool_warning() {
    let rig = Rig::new();
    rig.station(vec![answer(1, "up", 0, &needs(vec![item(1, 5)], "t1"))]);
    let feed = rig.feed(&["desktop", "feed", "--notify"]);
    feed.line();
    wait_until("the second call", || rig.requests().len() >= 2);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        rig.stderr().is_empty(),
        "the lookup runs before the first send: {}",
        rig.stderr()
    );
}

#[test]
fn desktop_feed_without_notify_sends_nothing() {
    let rig = Rig::new();
    rig.tools(&THREE_TOOLS);
    rig.station(vec![
        answer(1, "up", 0, &needs(vec![], "t1")),
        answer(2, "up", 0, &needs(vec![item(1, 5)], "t2")),
    ]);
    let feed = rig.feed(&["desktop", "feed"]);
    feed.line();
    feed.line();
    wait_until("the third call", || rig.requests().len() >= 3);
    std::thread::sleep(Duration::from_millis(300));
    assert!(rig.calls("omarchy-notification-send").is_empty());
    assert!(rig.stderr().is_empty(), "{}", rig.stderr());
}

// ---- desktop open ----

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn opened(rig: &Rig) -> Vec<Vec<String>> {
    wait_until("xdg-open", || !rig.calls("xdg-open").is_empty());
    rig.calls("xdg-open")
}

#[test]
fn desktop_open_without_an_argument_opens_the_board() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    rig.station(vec![answer(1, "up", 0, &plain("t1"))]);
    let out = rig.run(&["desktop", "open"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert_eq!(opened(&rig), [[format!("{BOARD}/")]]);
    assert_eq!(rig.requests(), [r#"{"op":"fleet_summary","after":0}"#]);
}

#[test]
fn desktop_open_a_session_and_a_request_go_into_the_fragment() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    rig.station(vec![
        answer(1, "up", 0, &plain("t1")),
        answer(1, "up", 0, &plain("t1")),
    ]);
    let out = rig.run(&["desktop", "open", "st1/s2"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let out = rig.run(&["desktop", "open", "st1/s2", "--request", "r3"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    wait_until("two xdg-open calls", || rig.calls("xdg-open").len() >= 2);
    assert_eq!(
        rig.calls("xdg-open"),
        [
            [format!("{BOARD}/#/s/st1/s2")],
            [format!("{BOARD}/#/s/st1/s2?request=r3")]
        ]
    );
}

#[test]
fn desktop_open_refuses_a_bad_argument() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    for args in [
        vec!["desktop", "open", "st1"],
        vec!["desktop", "open", "st1/s 1"],
        vec!["desktop", "open", "--request", "r1"],
        vec!["desktop", "open", "st1/s1", "--request", "r?1"],
    ] {
        let out = rig.run(&args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(
            stderr_of(&out),
            "fleetdeck: Use <station_id>/<session>.\n",
            "{args:?}"
        );
    }
    assert!(rig.requests().is_empty(), "no call for a bad argument");
    assert!(rig.calls("xdg-open").is_empty());
}

#[test]
fn desktop_open_without_a_summary_or_with_the_setting_off_says_to_run_the_feed() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    rig.station(vec![answer(1, "down", 0, &Value::Null), off()]);
    for _ in 0..2 {
        let out = rig.run(&["desktop", "open"]);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(
            stderr_of(&out),
            "fleetdeck: No board address is known. Run the feed first.\n"
        );
    }
    assert!(rig.calls("xdg-open").is_empty());
}

#[test]
fn desktop_open_gives_up_after_5_seconds_when_the_station_holds_the_call() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    rig.station(vec![]);
    let started = Instant::now();
    let out = rig.run(&["desktop", "open"]);
    let took = started.elapsed();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: No board address is known. Run the feed first.\n"
    );
    assert!(took >= Duration::from_millis(4500), "{took:?}");
    assert!(took < Duration::from_secs(12), "{took:?}");
}

#[test]
fn desktop_open_a_null_board_has_its_own_text() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    rig.station(vec![answer(
        1,
        "up",
        0,
        &summary("t1", vec![], Value::Null),
    )]);
    let out = rig.run(&["desktop", "open"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "fleetdeck: The deck did not tell its board address.\n"
    );
}

#[test]
fn desktop_open_without_xdg_open_says_so() {
    let rig = Rig::new();
    rig.station(vec![answer(1, "up", 0, &plain("t1"))]);
    let out = rig.run(&["desktop", "open"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr_of(&out), "fleetdeck: xdg-open is not installed.\n");
}

#[test]
fn desktop_open_without_a_station_fails_with_exit_1() {
    let rig = Rig::new();
    rig.tool("xdg-open");
    let out = rig.run(&["desktop", "open"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr_of(&out).starts_with("fleetdeck: "),
        "{}",
        stderr_of(&out)
    );
    assert!(rig.calls("xdg-open").is_empty());
}
