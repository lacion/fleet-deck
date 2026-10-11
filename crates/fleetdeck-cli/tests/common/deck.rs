//! A `fleetdeck deck serve` process on a temporary node (M2 P3-6b), with a
//! fixed loopback link port so that a restarted deck listens on the same
//! address for the stations that dial it. The board listens on port 0;
//! [`Deck::board`] reads its address from a `fleetdeck open` login link
//! (round 26: a port picked before the start can be taken meanwhile).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use fleetdeck_deck::DeckSession;
use fleetdeck_deck::http::tls::pinned_client;
use fleetdeck_deck::local::StationView;
use fleetdeck_testkit::TempNode;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use serde_json::Value;

use super::{WAIT, fleetdeck, with_stdin};

pub struct Deck {
    pub node: TempNode,
    pub link_port: u16,
    child: Option<Child>,
    /// `deck serve`, or `up` for the all-in-one process (P4-4).
    command: &'static [&'static str],
}

/// A loopback port that is free now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// An HTTPS response: status, head, JSON body.
pub struct Reply {
    pub status: u16,
    pub head: String,
    pub body: Value,
}

impl Deck {
    /// Writes deck.toml (board on port 0, link listener on a free port)
    /// and starts the deck. When another process takes the link port
    /// first, the deck exits and the harness picks a new port.
    pub fn start() -> Self {
        Self::start_with(TempNode::new("deck").unwrap(), &["deck", "serve"])
    }

    /// `fleetdeck up` on a node whose station.toml has `fake-claude sim`
    /// (M2 P4-4). Returns the deck and the node's `~/projects/demo`.
    pub fn up(name: &str) -> (Self, std::path::PathBuf) {
        let node = TempNode::new(name).unwrap();
        let workspace = node.workspace("demo").unwrap();
        let bins = fleetdeck_testkit::FakeAgent::bin_dir().unwrap();
        let mut text = node.station_toml(&bins, &[fleetdeck_testkit::FakeAgent::Claude]);
        text.push_str("env = { FLEETDECK_FAKE_SCENARIO = \"sim\" }\n");
        node.write_station_toml(&text).unwrap();
        (Self::start_with(node, &["up"]), workspace)
    }

    fn start_with(node: TempNode, command: &'static [&'static str]) -> Self {
        let mut deck = Self {
            node,
            link_port: 0,
            child: None,
            command,
        };
        for _ in 0..10 {
            deck.link_port = free_port();
            deck.node
                .write_deck_toml(&format!(
                    "name = \"deck\"\nlisten = [\"127.0.0.1:0\"]\nlink_listen = [\"127.0.0.1:{}\"]\n",
                    deck.link_port
                ))
                .unwrap();
            if deck.try_spawn() {
                return deck;
            }
        }
        panic!("the deck did not start");
    }

    /// Starts `fleetdeck deck serve` (again) and waits until its socket
    /// answers. A restart reuses the link port; it is retried while the
    /// old listener's port is not free yet.
    pub fn spawn(&mut self) {
        for _ in 0..20 {
            if self.try_spawn() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the deck did not start");
    }

    /// `false` when the deck process exits before its socket answers.
    fn try_spawn(&mut self) -> bool {
        assert!(self.child.is_none(), "a deck process runs already");
        let mut child = fleetdeck(&self.node)
            .args(self.command)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            if child.try_wait().unwrap().is_some() {
                return false;
            }
            if self.cli(&["deck", "stations"]).status.success() {
                self.child = Some(child);
                return true;
            }
            assert!(Instant::now() < deadline, "the deck did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The board address and a fresh login token from `fleetdeck open`.
    pub fn board(&self) -> (SocketAddr, String) {
        let url = self.ok(&["open"]);
        let rest = url.trim().strip_prefix("https://").expect("an https URL");
        let (addr, token) = rest.split_once("/#login=").expect("a login link");
        (addr.parse().unwrap(), token.to_owned())
    }

    /// One HTTPS request to the board, pinned to the deck's self-signed
    /// certificate.
    pub fn https(
        &self,
        addr: SocketAddr,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Reply {
        let pem = self.node.dirs().deck().join("board-selfsigned.crt");
        let cert = CertificateDer::from_pem_file(pem).unwrap();
        let name = ServerName::try_from(addr.ip().to_string()).unwrap();
        let conn = rustls::ClientConnection::new(pinned_client(cert), name).unwrap();
        let tcp = TcpStream::connect(addr).unwrap();
        tcp.set_read_timeout(Some(WAIT)).unwrap();
        let mut s = rustls::StreamOwned::new(conn, tcp);
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let mut text = format!(
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: application/json\r\n",
            body.len()
        );
        for (k, v) in headers {
            text.push_str(&format!("{k}: {v}\r\n"));
        }
        text.push_str("\r\n");
        text.push_str(&body);
        s.write_all(text.as_bytes()).unwrap();
        let mut raw = Vec::new();
        // The server may close without close_notify after the response.
        let _ = s.read_to_end(&mut raw);
        let raw = String::from_utf8(raw).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").expect("an HTTP response");
        Reply {
            status: head.split(' ').nth(1).unwrap().parse().unwrap(),
            head: head.to_owned(),
            body: serde_json::from_str(body).unwrap_or(Value::Null),
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
