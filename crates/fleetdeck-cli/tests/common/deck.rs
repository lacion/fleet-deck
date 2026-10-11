//! A `fleetdeck deck serve` process on a temporary node (M2 P3-6b), with a
//! fixed loopback link port so that a restarted deck listens on the same
//! address for the stations that dial it.

use std::net::TcpListener;
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use fleetdeck_deck::DeckSession;
use fleetdeck_deck::local::StationView;
use fleetdeck_testkit::TempNode;

use super::{WAIT, fleetdeck, with_stdin};

pub struct Deck {
    pub node: TempNode,
    pub link_port: u16,
    pub board_port: u16,
    child: Option<Child>,
}

/// A loopback port that is free now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Deck {
    /// Writes deck.toml (board on a free port, link listener on another)
    /// and starts the deck.
    pub fn start() -> Self {
        let node = TempNode::new("deck").unwrap();
        let link_port = free_port();
        let board_port = free_port();
        node.write_deck_toml(&format!(
            "name = \"deck\"\nlisten = [\"127.0.0.1:{board_port}\"]\nlink_listen = [\"127.0.0.1:{link_port}\"]\n",
        ))
        .unwrap();
        let mut deck = Self {
            node,
            link_port,
            board_port,
            child: None,
        };
        deck.spawn();
        deck
    }

    /// Starts `fleetdeck deck serve` and waits until its socket answers.
    pub fn spawn(&mut self) {
        assert!(self.child.is_none(), "a deck process runs already");
        let child = fleetdeck(&self.node)
            .args(["deck", "serve"])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        self.child = Some(child);
        let deadline = Instant::now() + WAIT;
        while !self.cli(&["deck", "stations"]).status.success() {
            assert!(Instant::now() < deadline, "the deck did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn cli(&self, args: &[&str]) -> Output {
        fleetdeck(&self.node).args(args).output().unwrap()
    }

    pub fn ok(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "fleetdeck {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    pub fn ok_stdin(&self, args: &[&str], input: &str) -> String {
        with_stdin(&self.node, args, input)
    }

    pub fn stations(&self) -> Vec<StationView> {
        serde_json::from_str(&self.ok(&["deck", "stations", "--json"])).unwrap()
    }

    pub fn sessions(&self) -> Vec<DeckSession> {
        serde_json::from_str(&self.ok(&["deck", "sessions", "--json"])).unwrap()
    }

    /// Polls until `f` holds for the deck's cards.
    pub fn wait_for(&self, what: &str, f: impl Fn(&[DeckSession]) -> bool) -> Vec<DeckSession> {
        let deadline = Instant::now() + WAIT;
        loop {
            let cards = self.sessions();
            if f(&cards) {
                return cards;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {cards:#?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// SIGTERM; the deck must exit 0.
    pub fn stop(&mut self) {
        let mut child = self.child.take().unwrap();
        let status = std::process::Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "deck exit: {status:?}");
                return;
            }
            assert!(Instant::now() < deadline, "the deck did not exit");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Deck {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
