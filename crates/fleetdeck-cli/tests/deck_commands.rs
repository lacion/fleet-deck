//! `fleetdeck deck command` and `deck commands` through the deck's command
//! router (M2 P3-7a): the deck starts a session on a dialed station with a
//! fake agent, stores the command with its receipt, and the session shows
//! up on the deck's board read model.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Instant;

use common::deck::Deck;
use common::{Station, WAIT};
use fleetdeck_deck::store::DeckCommand;
use fleetdeck_proto::command::{DeckCommandState, ReceiptState};
use fleetdeck_testkit::FakeAgent;
use serde_json::json;

#[test]
fn the_deck_starts_a_session_on_a_station_and_keeps_the_receipt() {
    let deck = Deck::start();
    let mut alpha = Station::named("alpha");
    alpha.spawn(None);
    let inv = alpha.ok(&["station", "invite", "--listen", "127.0.0.1:0"]);
    deck.ok_stdin(&["deck", "add-station", "--stdin"], inv.trim());

    let start = json!({
        "op": "start",
        "spec": {
            "provider_instance": FakeAgent::Claude.instance_id(),
            "cwd": alpha.workspace.to_str().unwrap(),
            "mode": null,
            "model": null,
            "title": null
        },
        "prompt": { "text": "hello from the deck" }
    })
    .to_string();
    // The first sync gives the deck the station incarnation; until then
    // the router refuses to address the station.
    let deadline = Instant::now() + WAIT;
    let cmd: DeckCommand = loop {
        let out = deck.cli(&["deck", "command", "alpha", &start, "--json"]);
        if out.status.success() {
            break serde_json::from_slice(&out.stdout).unwrap();
        }
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(err.contains("has not synced"), "{err}");
        assert!(Instant::now() < deadline, "the deck did not sync");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(cmd.state, DeckCommandState::Done, "{cmd:?}");
    assert_eq!(cmd.method, "session.start");
    assert_eq!(cmd.attempts, 1);
    let receipt = cmd.receipt.clone().unwrap();
    assert_eq!(receipt.state, ReceiptState::Accepted);
    let session = receipt.result.unwrap().session_id.unwrap();

    deck.wait_for("the started session on the deck", |cards| {
        cards.iter().any(|c| c.summary.session_id == session)
    });
    assert!(alpha.sessions().iter().any(|s| s.session_id == session));
    let listed: Vec<DeckCommand> =
        serde_json::from_str(&deck.ok(&["deck", "commands", "--json"])).unwrap();
    assert_eq!(listed, vec![cmd]);
    let text = deck.ok(&["deck", "commands"]);
    assert!(text.contains("session.start  done  accepted"), "{text}");

    let bad = deck.cli(&["deck", "command", "alpha", "{\"op\":\"nope\"}"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("not valid JSON"));
}
