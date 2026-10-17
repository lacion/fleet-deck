//! `fleetdeck settings get|set|secret` (DL-172, design 1.3) against a real
//! `deck serve` and a real `station serve` process, through their
//! owner-only local sockets. The holder is the only writer of its config
//! file while it runs.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use common::deck::Deck;
use common::{Station, run_stdin};
use fleetdeck_deck::local::{DeckClient, DeckRequest, DeckResponse};
use fleetdeck_proto::settings::{SettingsChange, SettingsView};
use fleetdeck_station::local::{LocalClient, LocalRequest, LocalResponse, socket_path};
use fleetdeck_testkit::FakeAgent;
use serde_json::{Map, json};

const CANARY: &str = "canary-cli-secret-4417";

fn view(text: &str) -> SettingsView {
    serde_json::from_str(text).unwrap()
}

fn change(key: &str, value: serde_json::Value, revision: &str) -> SettingsChange {
    let mut changes = Map::new();
    changes.insert(key.to_owned(), value);
    SettingsChange {
        changes,
        revision: revision.to_owned(),
        elevation: None,
    }
}

#[test]
fn the_deck_settings_are_read_and_saved_through_the_deck_socket() {
    let deck = Deck::start();
    let table = deck.ok(&["settings", "get"]);
    let first = table.lines().next().unwrap();
    assert!(
        first.starts_with("key") && first.ends_with("apply"),
        "{table}"
    );
    let row = |key: &str| {
        table
            .lines()
            .find(|l| l.starts_with(key))
            .unwrap_or_else(|| panic!("no row `{key}` in\n{table}"))
            .to_owned()
    };
    assert!(row("conflict_mail").ends_with("live"));
    assert!(row("link_listen").ends_with("restart"));
    let before = view(&deck.ok(&["settings", "get", "--json"]));

    // A live key applies at once.
    let said = deck.ok(&["settings", "set", "conflict_mail", "false"]);
    assert_eq!(said.trim(), "Saved and applied.");
    let after = view(&deck.ok(&["settings", "get", "--json"]));
    assert_eq!(after.values["conflict_mail"], json!(false));
    assert_ne!(after.revision, before.revision);
    assert!(row_of(&deck.ok(&["settings", "get"]), "conflict_mail").contains("false"));

    // A plain string and a restart key.
    let said = deck.ok(&["settings", "set", "tailnet_hostname", "deck.example.ts.net"]);
    assert_eq!(
        said.trim(),
        "Saved. Applies after a restart: tailnet_hostname."
    );
    let after = view(&deck.ok(&["settings", "get", "--json"]));
    assert_eq!(
        after.values["tailnet_hostname"],
        json!("deck.example.ts.net")
    );

    // An error: the code and the message, exit 1.
    let out = deck.cli(&["settings", "set", "conflict_mail", "\"maybe\""]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("invalid: "), "{err}");
    let out = deck.cli(&["settings", "set", "no_such_key", "1"]);
    assert_eq!(out.status.code(), Some(1));

    // The deck has no secrets; the CLI refuses before it reads stdin.
    let out = run_stdin(&deck.node, &["settings", "secret", "k"], CANARY);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains("use --station") && !err.contains(CANARY),
        "{err}"
    );

    // The audit rows hold the actor `local` and the keys, never a value.
    let audit = deck.ok(&["deck", "audit", "--json"]);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&audit).unwrap();
    let saves: Vec<_> = rows
        .iter()
        .filter(|r| r["action"] == "settings.deck_set")
        .collect();
    assert!(saves.len() >= 3, "{audit}");
    assert!(
        saves
            .iter()
            .all(|r| r["actor"] == "local" && r["target"] == "deck")
    );
    assert!(
        saves
            .iter()
            .any(|r| r["detail"] == "conflict_mail" && r["result"] == "ok")
    );
    assert!(
        saves
            .iter()
            .any(|r| r["result"] == "failed"
                && r["detail"].as_str().unwrap().starts_with("invalid: "))
    );
    assert!(!audit.contains("deck.example.ts.net"), "{audit}");
}

fn row_of(table: &str, key: &str) -> String {
    table
        .lines()
        .find(|l| l.starts_with(key))
        .unwrap()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_revision_on_the_deck_socket_is_settings_changed() {
    let deck = Deck::start();
    let path = fleetdeck_deck::local::socket_path(&deck.node.dirs().deck());
    let mut client = DeckClient::connect(&path).await.unwrap();
    let DeckResponse::Settings { view } = client.call(&DeckRequest::SettingsGet).await.unwrap()
    else {
        panic!("not a settings view");
    };
    let saved = client
        .call(&DeckRequest::SettingsSet {
            change: change("conflict_mail", json!(false), &view.revision),
        })
        .await
        .unwrap();
    assert!(
        matches!(saved, DeckResponse::SettingsSaved { .. }),
        "{saved:?}"
    );
    // The first revision is old now.
    let stale = client
        .call(&DeckRequest::SettingsSet {
            change: change("conflict_mail", json!(true), &view.revision),
        })
        .await
        .unwrap();
    let DeckResponse::SettingsError { code, .. } = stale else {
        panic!("not an error: {stale:?}");
    };
    assert_eq!(code, "settings_changed");
    // The CLI reads the new revision and saves.
    let said = deck.ok(&["settings", "set", "conflict_mail", "true"]);
    assert_eq!(said.trim(), "Saved and applied.");
}

#[test]
fn the_station_settings_are_read_and_saved_through_the_station_socket() {
    let station = Station::start();
    let table = station.ok(&["settings", "get", "--station"]);
    let row = |key: &str| row_of(&table, key);
    assert!(row("max_sessions").contains('4') && row("max_sessions").ends_with("live"));
    assert!(row("host").ends_with("restart"));

    let said = station.ok(&["settings", "set", "max_sessions", "6", "--station"]);
    assert_eq!(said.trim(), "Saved and applied.");
    let after = view(&station.ok(&["settings", "get", "--station", "--json"]));
    assert_eq!(after.values["max_sessions"], json!(6));

    let said = station.ok(&["settings", "set", "host", "shim", "--station"]);
    assert_eq!(said.trim(), "Saved. Applies after a restart: host.");

    let err = station.fails(&["settings", "set", "max_sessions", "0", "--station"]);
    assert!(err.contains("invalid: "), "{err}");
}

#[test]
fn a_station_secret_comes_from_stdin_and_reaches_no_output_or_file() {
    let station = Station::start();
    let key = format!(
        "provider_instance.{}.env.ANTHROPIC_API_KEY",
        FakeAgent::Claude.instance_id()
    );
    let out = run_stdin(
        &station.node,
        &["settings", "secret", &key, "--station"],
        CANARY,
    );
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();
    let stderr = String::from_utf8(out.stderr.clone()).unwrap();
    assert!(out.status.success(), "{stderr}");
    assert_eq!(stdout.trim(), "Saved.");
    assert!(!stdout.contains(CANARY) && !stderr.contains(CANARY));

    // The view shows `(set)` and no value; the config file holds a
    // reference, not the value.
    let table = station.ok(&["settings", "get", "--station"]);
    assert!(table.contains("(set)"), "{table}");
    let json_view = station.ok(&["settings", "get", "--station", "--json"]);
    assert!(!table.contains(CANARY) && !json_view.contains(CANARY));
    let toml = std::fs::read_to_string(station.node.dirs().station_config()).unwrap();
    assert!(!toml.contains(CANARY), "{toml}");

    // An empty line is refused without a request.
    let out = run_stdin(
        &station.node,
        &["settings", "secret", &key, "--station"],
        "",
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("no value was given")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_revision_on_the_station_socket_is_settings_changed() {
    let station = Station::start();
    let mut client = LocalClient::connect(&socket_path(&station.node.dirs().station()))
        .await
        .unwrap();
    let LocalResponse::Settings { view } = client.call(&LocalRequest::SettingsGet).await.unwrap()
    else {
        panic!("not a settings view");
    };
    let saved = client
        .call(&LocalRequest::SettingsSet {
            change: change("max_sessions", json!(5), &view.revision),
        })
        .await
        .unwrap();
    assert!(
        matches!(saved, LocalResponse::SettingsSaved { .. }),
        "{saved:?}"
    );
    let stale = client
        .call(&LocalRequest::SettingsSet {
            change: change("max_sessions", json!(3), &view.revision),
        })
        .await
        .unwrap();
    let LocalResponse::SettingsError { code, .. } = stale else {
        panic!("not an error: {stale:?}");
    };
    assert_eq!(code, "settings_changed");
    // A secret with a stale revision is refused the same way, and its
    // value is in no answer.
    let key = format!(
        "provider_instance.{}.env.ANTHROPIC_API_KEY",
        FakeAgent::Claude.instance_id()
    );
    let stale = client
        .call(&LocalRequest::SettingsSecret {
            secret: fleetdeck_proto::settings::SecretSet {
                key,
                value: CANARY.to_owned(),
            },
            revision: view.revision,
        })
        .await
        .unwrap();
    assert!(
        matches!(&stale, LocalResponse::SettingsError { code, .. } if code == "settings_changed"),
        "{stale:?}"
    );
    assert!(!format!("{stale:?}").contains(CANARY));
}

#[test]
fn the_settings_commands_say_when_no_holder_runs() {
    let node = fleetdeck_testkit::TempNode::new("idle").unwrap();
    for args in [&["settings", "get"][..], &["settings", "get", "--station"]] {
        let out = common::fleetdeck(&node).args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(1));
        let err = String::from_utf8(out.stderr).unwrap();
        assert!(err.contains("no ") && err.contains("answers"), "{err}");
    }
}
