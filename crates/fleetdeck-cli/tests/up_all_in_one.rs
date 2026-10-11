//! `fleetdeck up` (M2 P4-4; PLAN Phase 4 exit): one process runs the deck
//! and a local station. The station pairs with the deck over loopback at
//! the first start and keeps the pairing after a restart. The board serves
//! the web client (or says how to build it), the directory picker lists the
//! station's roots, and a session started from the board API runs on the
//! local fake agent.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::{Duration, Instant};

use common::WAIT;
use common::deck::Deck;
use fleetdeck_deck::http::assets::NOT_BUILT;
use fleetdeck_testkit::FakeAgent;
use serde_json::{Value, json};

fn cookie_of(head: &str) -> String {
    let line = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .unwrap();
    line.split_once(':')
        .unwrap()
        .1
        .trim()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

/// Waits until the deck lists exactly one reachable station named `name`.
fn linked(deck: &Deck, name: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let stations = deck.stations();
        if stations.len() == 1 && stations[0].name == name && stations[0].reachable {
            return;
        }
        assert!(Instant::now() < deadline, "station: {stations:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn query(path: &str) -> String {
    path.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[test]
fn up_runs_a_deck_and_a_paired_local_station_in_one_process() {
    let (mut deck, workspace) = Deck::up("home");
    linked(&deck, "home");
    // The local station answers on its own socket too.
    let local = deck.ok(&["station", "sessions", "--json"]);
    assert_eq!(serde_json::from_str::<Value>(&local).unwrap(), json!([]));

    let (board, token) = deck.board();
    let r = deck.https(
        board,
        "POST",
        "/api/auth/link",
        &[],
        Some(json!({ "token": token })),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let cookie = cookie_of(&r.head);
    let csrf = r.body["csrf"].as_str().unwrap().to_owned();
    let auth = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];

    // The board client: the built web client, or the hint to build it.
    let page = deck.https(board, "GET", "/", &[], None);
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist/index.html");
    if dist.exists() {
        assert_eq!(page.status, 200, "{}", page.head);
        assert!(
            page.head.contains("content-security-policy"),
            "{}",
            page.head
        );
    } else {
        assert_eq!(page.status, 404, "{}", page.head);
        assert!(NOT_BUILT.starts_with("This deck has no board files"));
    }

    // The directory picker of the spawn form.
    let r = deck.https(board, "GET", "/api/stations/home/workspaces", &auth, None);
    assert_eq!(r.status, 200, "{}", r.body);
    let root = r.body["entries"][0]["path"].as_str().unwrap().to_owned();
    let r = deck.https(
        board,
        "GET",
        &format!("/api/stations/home/workspaces?path={}", query(&root)),
        &auth,
        None,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let demo = r.body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "demo")
        .expect("the demo workspace")
        .clone();
    assert_eq!(
        demo["path"].as_str().unwrap(),
        workspace.canonicalize().unwrap().to_str().unwrap()
    );

    // A session started from the board runs on the local fake agent.
    let body = json!({
        "station": "home",
        "command_id": "up-start-1",
        "command": { "op": "start", "spec": {
            "provider_instance": FakeAgent::Claude.instance_id(),
            "cwd": demo["path"], "mode": null, "model": null, "title": "From up"
        }, "prompt": { "text": "hello from up" } }
    });
    let deadline = Instant::now() + WAIT;
    loop {
        let r = deck.https(
            board,
            "POST",
            "/api/commands/session.start",
            &auth,
            Some(body.clone()),
        );
        if r.status == 409 && r.body["error"] == "not_synced" {
            assert!(Instant::now() < deadline, "{}", r.body);
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        assert_eq!(r.status, 200, "{}", r.body);
        break;
    }
    deck.wait_for("the session's first turn", |cards| {
        cards.len() == 1
            && cards[0].summary.title.as_deref() == Some("From up")
            && cards[0].summary.current_turn.is_none()
            && cards[0].summary.usage.input_tokens.is_some()
    });

    // A restart keeps the pairing: the same single station links again and
    // the session is still on the board.
    deck.stop();
    deck.spawn();
    linked(&deck, "home");
    let cards = deck.wait_for("the session after the restart", |cards| cards.len() == 1);
    assert_eq!(cards[0].summary.title.as_deref(), Some("From up"));
    deck.stop();
}

#[test]
fn up_needs_a_link_listener_to_pair_the_local_station() {
    // A deck without a link listener cannot pair the local station.
    let node = fleetdeck_testkit::TempNode::new("nolink").unwrap();
    node.write_deck_toml("name = \"deck\"\nlisten = [\"127.0.0.1:0\"]\nlink_listen = []\n")
        .unwrap();
    let out = common::fleetdeck(&node).arg("up").output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("set `link_listen` in deck.toml"), "{err}");
}
