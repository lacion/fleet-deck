//! MCP Apps from the deck to the station (B15 T9, design
//! `docs/v2/design/mcp-apps.md` §4.7 and T9): a real deck and a real station
//! process on loopback, the fake Codex under the scenario `mcp_app` (the demo
//! MCP server with its app, `fleetdeck-testkit` `sim_codex_mcp.rs`) and the
//! owner's cookie login with its CSRF token.
//!
//! One candidate of the fake provider becomes a `tool.app` event that reaches
//! the deck timeline; the view route and the document route answer; the call
//! route round-trips through the station's bridge and leaves settled
//! `mcp_app_calls` rows; the call route without the CSRF header is 403.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::deck::{Deck, Reply};
use common::{Station, WAIT, open_store, stored_events};
use fleetdeck_deck::http::tls::pinned_client;
use fleetdeck_deck::store::DeckStore;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::Event;
use fleetdeck_station::store::ReadResult;
use fleetdeck_testkit::{Det, FakeAgent};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use serde_json::{Value, json};

/// The station name of the test; it is also the `{station}` of the routes.
const STATION: &str = "apps";
/// The start of the bootstrap script that the deck puts into a document.
const BOOTSTRAP_START: &str = "<script>(()=>{\"use strict\";";

/// A logged-in board client of the deck (cookie and CSRF token).
struct Owner {
    addr: SocketAddr,
    cookie: String,
    csrf: String,
}

impl Owner {
    fn login(deck: &Deck) -> Self {
        let (addr, token) = deck.board();
        let r = deck.https(
            addr,
            "POST",
            "/api/auth/link",
            &[],
            Some(json!({ "token": token })),
        );
        assert_eq!(r.status, 200, "{}", r.body);
        let line = r
            .head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
            .unwrap();
        let cookie = line
            .split_once(':')
            .unwrap()
            .1
            .trim()
            .split(';')
            .next()
            .unwrap();
        Self {
            addr,
            cookie: cookie.to_owned(),
            csrf: r.body["csrf"].as_str().unwrap().to_owned(),
        }
    }

    /// A request with the cookie and, when `csrf` is set, the CSRF header.
    fn send(
        &self,
        deck: &Deck,
        method: &str,
        path: &str,
        body: Option<Value>,
        csrf: bool,
    ) -> Reply {
        let mut headers = vec![("Cookie", self.cookie.as_str())];
        if csrf {
            headers.push(("X-CSRF-Token", self.csrf.as_str()));
        }
        deck.https(self.addr, method, path, &headers, body)
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

/// A station whose only provider is the fake Codex under `mcp_app`, with a
/// Git workspace.
fn codex_station() -> Station {
    let mut station = Station::named(STATION);
    let bins = FakeAgent::bin_dir().unwrap();
    FakeAgent::Codex.locate().unwrap();
    let mut text = station.node.station_toml(&bins, &[FakeAgent::Codex]);
    text.push_str("env = { FLEETDECK_FAKE_SCENARIO = \"mcp_app\" }\n");
    station.node.write_station_toml(&text).unwrap();
    git(&station.workspace, &["init", "-q", "-b", "main"]);
    std::fs::write(station.workspace.join("a.txt"), "one\n").unwrap();
    git(&station.workspace, &["add", "."]);
    git(&station.workspace, &["commit", "-q", "-m", "init"]);
    station.spawn(None);
    station
}

/// `fleetdeck station run` with the fake Codex; returns the session id.
fn run_codex(station: &Station, prompt: &str) -> String {
    let cwd = station.workspace.to_str().unwrap().to_owned();
    station
        .ok(&[
            "station",
            "run",
            "--provider",
            FakeAgent::Codex.instance_id(),
            "--cwd",
            &cwd,
            prompt,
        ])
        .trim()
        .to_owned()
}

/// One HTTPS GET of the board without a login: the head and the raw body.
fn raw_get(deck: &Deck, addr: SocketAddr, path: &str) -> (String, String) {
    let pem = deck.node.dirs().deck().join("board-selfsigned.crt");
    let cert = CertificateDer::from_pem_file(pem).unwrap();
    let name = ServerName::try_from(addr.ip().to_string()).unwrap();
    let conn = rustls::ClientConnection::new(pinned_client(cert), name).unwrap();
    let tcp = TcpStream::connect(addr).unwrap();
    tcp.set_read_timeout(Some(WAIT)).unwrap();
    let mut s = rustls::StreamOwned::new(conn, tcp);
    let text = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    s.write_all(text.as_bytes()).unwrap();
    let mut raw = Vec::new();
    // The server may close without close_notify after the response.
    let _ = s.read_to_end(&mut raw);
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").expect("an HTTP response");
    (head.to_owned(), body.to_owned())
}

/// The `(tool, state)` pairs of the `mcp_app_calls` rows, oldest first. The
/// station runs on and writes; SQLite WAL lets the test read.
fn call_rows(db: &Path) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    let mut stmt = conn
        .prepare("SELECT tool, state FROM mcp_app_calls ORDER BY created_at, call_id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The path of the app route `suffix` of `session`.
fn app_path(session: &str, suffix: &str) -> String {
    format!("/api/sessions/{STATION}/{session}/{suffix}")
}

fn tools_call(tool: &str, confirmed: bool) -> Value {
    json!({
        "request": { "method": "tools/call", "name": tool, "arguments": {} },
        "confirmed": confirmed,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_app_goes_from_the_provider_to_the_board_and_back() {
    let mut deck = Deck::start();
    let mut station = codex_station();
    let invite = station.ok(&["station", "invite", "--listen", "127.0.0.1:0"]);
    let out = deck.ok_stdin(&["deck", "add-station", "--stdin"], invite.trim());
    assert!(
        out.contains(&format!("paired with station {STATION}")),
        "{out}"
    );
    let session = run_codex(&station, "show the counter");
    let owner = Owner::login(&deck);

    // The fake provider emits one candidate: one `tool.app` event.
    let app_id = {
        let deadline = Instant::now() + WAIT;
        loop {
            let events = stored_events(&open_store(&station.db()).await).await;
            let ids: Vec<String> = events
                .into_iter()
                .filter(|(s, _)| *s == session)
                .filter_map(|(_, e)| match e {
                    Event::ToolApp { app_id, .. } => Some(app_id),
                    _ => None,
                })
                .collect();
            if let Some(id) = ids.first() {
                assert_eq!(ids.len(), 1, "one candidate, one app: {ids:?}");
                break id.clone();
            }
            assert!(Instant::now() < deadline, "no tool.app event");
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    // The list route answers an empty list once the link has negotiated the
    // feature and the deck knows the session.
    let list = {
        let deadline = Instant::now() + WAIT;
        loop {
            let r = owner.send(&deck, "GET", &app_path(&session, "app-calls"), None, true);
            if r.status == 200 {
                break r;
            }
            assert!(
                Instant::now() < deadline,
                "app-calls: {} {}",
                r.status,
                r.body
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    assert_eq!(list.body, json!({ "calls": [] }));

    // The view route mints a token; the document route serves the document
    // with the bootstrap and the CSP, without a login.
    let view = owner.send(
        &deck,
        "GET",
        &app_path(&session, &format!("apps/{app_id}")),
        None,
        true,
    );
    assert_eq!(view.status, 200, "{}", view.body);
    assert_eq!(view.body["server"], "demo");
    assert_eq!(view.body["tool"], "show_counter");
    assert_eq!(view.body["resource_uri"], "ui://demo/counter.html");
    let url = view.body["url"].as_str().unwrap();
    assert!(url.starts_with("/app/"), "{url}");
    let (head, body) = raw_get(&deck, owner.addr, url);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let head = head.to_ascii_lowercase();
    assert!(
        head.contains("content-security-policy: sandbox allow-scripts; default-src 'none'"),
        "{head}"
    );
    assert!(head.contains("connect-src 'none'"), "{head}");
    assert!(body.contains(BOOTSTRAP_START), "no bootstrap");
    assert!(body.contains("textContent='ready'"), "the app document");

    // A read-only call answers at once; a non-read-only call asks first and
    // runs after the confirmation, through the station's bridge.
    let call = app_path(&session, &format!("apps/{app_id}/call"));
    let r = owner.send(
        &deck,
        "POST",
        &call,
        Some(tools_call("show_counter", false)),
        true,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["outcome"], "done", "{}", r.body);
    let r = owner.send(&deck, "POST", &call, Some(tools_call("bump", false)), true);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["outcome"], "confirm", "{}", r.body);
    assert_eq!(r.body["tool"], "bump");
    let r = owner.send(&deck, "POST", &call, Some(tools_call("bump", true)), true);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["outcome"], "done", "{}", r.body);
    assert_eq!(r.body["result"]["structuredContent"]["count"], 1);

    // Both calls left a settled row, and no call is open.
    let want = vec![
        ("show_counter".to_owned(), "done".to_owned()),
        ("bump".to_owned(), "done".to_owned()),
    ];
    let deadline = Instant::now() + WAIT;
    while call_rows(&station.db()) != want {
        assert!(
            Instant::now() < deadline,
            "rows: {:?}",
            call_rows(&station.db())
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let list = owner.send(&deck, "GET", &app_path(&session, "app-calls"), None, true);
    assert_eq!(list.status, 200, "{}", list.body);
    assert_eq!(list.body, json!({ "calls": [] }));

    // A call without the CSRF header of the login is refused, and the
    // station sees no new row.
    let r = owner.send(&deck, "POST", &call, Some(tools_call("bump", true)), false);
    assert_eq!(r.status, 403, "{}", r.body);
    assert_eq!(r.body["error"], "csrf", "{}", r.body);
    assert_eq!(call_rows(&station.db()), want);

    // The `tool.app` event reached the deck timeline, once. The deck's
    // cursor must hold the station's last event before the deck stops.
    let ReadResult::Events { events, .. } = open_store(&station.db())
        .await
        .read_after(0, 100_000)
        .await
        .unwrap()
    else {
        panic!("snapshot required")
    };
    let last = events.last().unwrap().station_seq;
    let deadline = Instant::now() + WAIT;
    let station_id = loop {
        let found = deck.stations().into_iter().find(|s| s.name == STATION);
        if let Some(st) = found.filter(|s| s.cursor >= last) {
            break st.station_id;
        }
        assert!(
            Instant::now() < deadline,
            "the deck cursor is behind {last}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    deck.stop();
    station.stop();
    let det = Det::new();
    let store = DeckStore::open(&deck.node.dirs().deck_db(), det.clock(), det.ids())
        .await
        .unwrap();
    let timeline = store
        .timeline(station_id, SessionId::from(session.as_str()))
        .await
        .unwrap();
    let apps: Vec<String> = timeline
        .iter()
        .filter_map(|e| match &e.event {
            fleetdeck_proto::event::EventBody::Known(Event::ToolApp { app_id, .. }) => {
                Some(app_id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(apps, vec![app_id], "the deck timeline has the tool.app");
}
