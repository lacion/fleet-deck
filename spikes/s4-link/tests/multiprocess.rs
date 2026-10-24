//! Several `fd-link-node` processes on loopback, each with its own data
//! directory and port (PLAN §13 link tests; no Docker): two stations without a
//! listener (the NAT case) pair with a deck and dial it, the deck restarts,
//! and the stations reconnect with a higher link epoch. The mirror direction
//! (a station invites, the deck joins and dials) runs too.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_fd-link-node");
const T: Duration = Duration::from_secs(20);

/// A running node; killed on drop.
struct Node {
    name: &'static str,
    child: Child,
    lines: mpsc::Receiver<String>,
    /// Every line so far (for failure messages).
    seen: Vec<String>,
    /// Lines that no `expect` consumed yet; events of different links may
    /// interleave in any order.
    pending: Vec<String>,
}

impl Node {
    fn start(name: &'static str, args: &[&str]) -> Self {
        let mut child = Command::new(BIN)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            name,
            child,
            lines,
            seen: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// Takes the oldest unconsumed line that starts with `prefix`, waiting
    /// for it if needed.
    fn expect(&mut self, prefix: &str) -> String {
        if let Some(i) = self.pending.iter().position(|l| l.starts_with(prefix)) {
            return self.pending.remove(i);
        }
        let deadline = Instant::now() + T;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    self.seen.push(line.clone());
                    if line.starts_with(prefix) {
                        return line;
                    }
                    self.pending.push(line);
                }
                Err(e) => panic!(
                    "{}: no `{prefix}` line ({e}); seen: {:#?}",
                    self.name, self.seen
                ),
            }
        }
    }

    /// Waits for the process to exit by itself.
    fn wait_exit(&mut self) -> i32 {
        let deadline = Instant::now() + T;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                while let Ok(line) = self.lines.try_recv() {
                    self.seen.push(line);
                }
                return status.code().unwrap_or(-1);
            }
            assert!(Instant::now() < deadline, "{} did not exit", self.name);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.stop();
    }
}

fn word(line: &str, n: usize) -> &str {
    line.split_whitespace().nth(n).unwrap()
}

fn field(line: &str, key: &str) -> u64 {
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(prefix.as_str()))
        .unwrap()
        .parse()
        .unwrap()
}

fn dir(d: &tempfile::TempDir) -> &str {
    d.path().to_str().unwrap()
}

fn pinned(d: &Path) -> usize {
    let text = std::fs::read_to_string(d.join("peers.json")).unwrap_or_else(|_| "[]".into());
    serde_json::from_str::<Vec<serde_json::Value>>(&text)
        .unwrap()
        .len()
}

const FAST: [&str; 4] = ["--backoff-ms", "100", "--backoff-cap-ms", "800"];

fn station_args<'a>(data: &'a str, invite: &'a str, addr: &'a str) -> Vec<&'a str> {
    let mut a = vec![
        "--role", "station", "--data", data, "--join", invite, "--dial", addr,
    ];
    a.extend(FAST);
    a
}

#[test]
fn nat_stations_pair_dial_and_reconnect_after_a_deck_restart() {
    let (deck_dir, s1_dir, s2_dir, s3_dir) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let mut deck = Node::start(
        "deck",
        &[
            "--role",
            "deck",
            "--data",
            dir(&deck_dir),
            "--listen",
            "127.0.0.1:0",
            "--invites",
            "2",
        ],
    );
    let deck_fp = word(&deck.expect("node "), 2).to_owned();
    let addr = word(&deck.expect("listening "), 1).to_owned();
    let inv1 = word(&deck.expect("invite "), 1).to_owned();
    let inv2 = word(&deck.expect("invite "), 1).to_owned();
    assert!(inv1.starts_with("fdinv1:wss://127.0.0.1:"));
    assert!(inv1.contains(&format!("#{deck_fp}.")));

    let mut s1 = Node::start("s1", &station_args(dir(&s1_dir), &inv1, &addr));
    assert_eq!(word(&s1.expect("joined "), 2), deck_fp);
    let first1 = field(&s1.expect("linked dialer"), "epoch");
    let mut s2 = Node::start("s2", &station_args(dir(&s2_dir), &inv2, &addr));
    s2.expect("joined ");
    let first2 = field(&s2.expect("linked dialer"), "epoch");

    // The deck sees two paired stations, links to both and drives each.
    deck.expect("paired station");
    deck.expect("paired station");
    for _ in 0..2 {
        deck.expect("reply ");
    }
    s1.expect("served station.info");
    s2.expect("served station.info");
    assert_eq!(pinned(deck_dir.path()), 2);

    // A used invitation pairs nothing.
    let mut s3 = Node::start("s3", &station_args(dir(&s3_dir), &inv1, &addr));
    assert_ne!(s3.wait_exit(), 0, "{:?}", s3.seen);
    assert!(
        s3.seen.iter().any(|l| l.starts_with("error ")),
        "{:?}",
        s3.seen
    );
    assert_eq!(pinned(deck_dir.path()), 2);
    assert_eq!(pinned(s3_dir.path()), 0);

    // The deck stops; the stations lose the link and retry with backoff.
    deck.stop();
    s1.expect("lost ");
    s2.expect("lost ");
    s1.expect("retry ");

    // The deck restarts with the same data directory and port and no
    // invitations: the pinned stations come back on their own.
    let mut deck = Node::start(
        "deck2",
        &[
            "--role",
            "deck",
            "--data",
            dir(&deck_dir),
            "--listen",
            &addr,
        ],
    );
    assert_eq!(word(&deck.expect("node "), 2), deck_fp, "same identity");
    let again1 = field(&s1.expect("linked dialer"), "epoch");
    let again2 = field(&s2.expect("linked dialer"), "epoch");
    assert!(again1 > first1 && again2 > first2);
    deck.expect("linked listener");
    deck.expect("linked listener");
    deck.expect("reply ");
    deck.expect("reply ");
}

#[test]
fn the_deck_joins_a_station_invitation_and_dials_it() {
    let (deck_dir, st_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut station = Node::start(
        "station",
        &[
            "--role",
            "station",
            "--data",
            dir(&st_dir),
            "--listen",
            "127.0.0.1:0",
            "--invites",
            "1",
        ],
    );
    let station_fp = word(&station.expect("node "), 2).to_owned();
    let addr = word(&station.expect("listening "), 1).to_owned();
    let invite = word(&station.expect("invite "), 1).to_owned();
    let mut args = vec![
        "--role",
        "deck",
        "--data",
        dir(&deck_dir),
        "--join",
        &invite,
        "--dial",
        &addr,
    ];
    args.extend(FAST);
    let mut deck = Node::start("deck", &args);
    assert_eq!(word(&deck.expect("joined "), 2), station_fp);
    station.expect("paired deck");
    let linked = deck.expect("linked dialer");
    assert!(linked.contains("peer=station"));
    let reply = deck.expect("reply ");
    assert!(reply.contains(&station_fp), "{reply}");
    station.expect("served station.info");
}

#[test]
fn public_and_wildcard_addresses_are_refused() {
    let d = tempfile::tempdir().unwrap();
    for listen in ["0.0.0.0:0", "8.8.8.8:7700"] {
        let mut n = Node::start(
            "public",
            &["--role", "deck", "--data", dir(&d), "--listen", listen],
        );
        assert_ne!(n.wait_exit(), 0, "{listen} must be refused: {:?}", n.seen);
    }
    let mut n = Node::start(
        "joiner",
        &[
            "--role",
            "station",
            "--data",
            dir(&d),
            "--join",
            "fdinv1:wss://8.8.8.8:7700#00.00.0",
        ],
    );
    assert_ne!(n.wait_exit(), 0);
    assert!(
        n.seen.iter().any(|l| l.starts_with("error ")),
        "{:?}",
        n.seen
    );
}
