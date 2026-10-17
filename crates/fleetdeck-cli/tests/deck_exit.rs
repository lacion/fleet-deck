//! Phase 3 exit: real deck + two station processes, offline fake agents.
//! TCP relays cut both encrypted channels but leave the providers running.
//! Pending commands survive the outage and a deck restart with their original
//! envelopes. Full condensed timelines equal the station logs byte-for-byte.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
#[path = "common/cut_link.rs"]
mod cut_link;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use common::deck::Deck;
use common::{Station, WAIT, open_store};
use cut_link::CutLink;
use fleetdeck_deck::store::DeckStore;
use fleetdeck_proto::event::{EventEnvelope, SessionState};
use fleetdeck_station::store::ReadResult;
use fleetdeck_testkit::{Det, FakeAgent};
use serde_json::{Value, json};

fn post(deck: &Deck, auth: &str, method: &str, body: Value) -> Value {
    let (board, _) = deck.board();
    let deadline = Instant::now() + WAIT;
    loop {
        let reply = deck.https(
            board,
            "POST",
            &format!("/api/commands/{method}"),
            &[("Authorization", auth)],
            Some(body.clone()),
        );
        if reply.status == 409 && reply.body["error"] == "not_synced" {
            assert!(Instant::now() < deadline, "initial sync: {}", reply.body);
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        assert_eq!(reply.status, 200, "{}", reply.body);
        return reply.body;
    }
}

fn settled(deck: &Deck, auth: &str, id: &str) -> Value {
    let (board, _) = deck.board();
    let deadline = Instant::now() + WAIT;
    loop {
        let r = deck.https(
            board,
            "GET",
            &format!("/api/commands/{id}"),
            &[("Authorization", auth)],
            None,
        );
        assert_eq!(r.status, 200, "{}", r.body);
        if r.body["state"] == "done" {
            assert_eq!(r.body["receipt"]["state"], "accepted", "{}", r.body);
            return r.body;
        }
        assert!(Instant::now() < deadline, "command not done: {}", r.body);
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn reachability(deck: &Deck, online: bool) {
    // Reconnect's full jitter may need more than the harness's usual 20 s.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let stations = deck.stations();
        // `deck invite` pairs bravo under its fingerprint; the deck adopts
        // the station.toml name after the first link (`station.info`).
        let names: Vec<&str> = stations.iter().map(|s| s.name.as_str()).collect();
        if stations.len() == 2
            && names.contains(&"alpha")
            && names.contains(&"bravo")
            && stations.iter().all(|s| s.reachable == online)
        {
            return;
        }
        assert!(Instant::now() < deadline, "reachability: {stations:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn converged(deck: &Deck, stations: &[&Station]) {
    deck.wait_for("exact station summaries", |cards| {
        let expected: Vec<_> = stations.iter().flat_map(|s| s.sessions()).collect();
        cards.len() == expected.len()
            && expected
                .iter()
                .all(|summary| cards.iter().any(|card| card.summary == *summary))
    });
}

async fn last_seq(store: &fleetdeck_station::store::Store) -> u64 {
    let ReadResult::Events { events, .. } = store.read_after(0, 100_000).await.unwrap() else {
        panic!("unexpected retention");
    };
    events.last().unwrap().station_seq
}

fn start_body(station: &Station) -> Value {
    json!({
        "station": station.node.name(),
        "command_id": format!("start-{}", station.node.name()),
        "command": { "op": "start", "spec": {
            "provider_instance": FakeAgent::Claude.instance_id(),
            "cwd": station.workspace, "mode": null, "model": null, "title": null
        }, "prompt": { "text": format!("hello {}", station.node.name()) } }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn two_station_link_cuts_and_a_deck_restart_lose_no_events_or_commands() {
    let mut deck = Deck::start();
    let mut alpha = Station::named("alpha");
    let mut bravo = Station::named("bravo");
    alpha.spawn(None);
    bravo.spawn(None);

    let alpha_inv = alpha.ok(&["station", "invite", "--listen", "127.0.0.1:0"]);
    let alpha_addr: SocketAddr = alpha_inv
        .trim()
        .split_once('#')
        .unwrap()
        .0
        .strip_prefix("fdinv1:wss://")
        .unwrap()
        .parse()
        .unwrap();
    let alpha_link = CutLink::start(alpha_addr);
    deck.ok_stdin(
        &["deck", "add-station", "--stdin"],
        &alpha_link.invitation(&alpha_inv),
    );
    let bravo_link = CutLink::start(format!("127.0.0.1:{}", deck.link_port).parse().unwrap());
    let bravo_inv = deck.ok(&["deck", "invite"]);
    bravo.ok_stdin(
        &["station", "join", "--stdin"],
        &bravo_link.invitation(&bravo_inv),
    );
    reachability(&deck, true);
    let stations = deck.stations();
    assert_eq!(
        stations
            .iter()
            .find(|s| s.name == "alpha")
            .unwrap()
            .direction,
        "deck_dials"
    );
    assert_eq!(
        stations
            .iter()
            .find(|s| s.name == "bravo")
            .unwrap()
            .direction,
        "station_dials"
    );
    assert!(
        bravo.node.station_config().unwrap().listen.is_empty(),
        "NAT station has no listener"
    );

    let auth = format!(
        "Bearer {}",
        deck.ok(&[
            "deck", "token", "create", "driver", "--scope", "drive", "--scope", "read"
        ])
        .trim()
    );
    let mut queued = Vec::new();
    for station in [&alpha, &bravo] {
        let body = start_body(station);
        post(&deck, &auth, "session.start", body.clone());
        let first = settled(&deck, &auth, body["command_id"].as_str().unwrap());
        assert_eq!(post(&deck, &auth, "session.start", body), first);
        let session = first["receipt"]["result"]["session_id"].as_str().unwrap();
        station.wait_for("initial turn completed", session, |s| {
            s.current_turn.is_none() && s.usage.input_tokens.is_some()
        });
        queued.push(json!({
            "station": station.node.name(),
            "command_id": format!("queued-{}", station.node.name()),
            "command": { "op": "prompt", "session_id": session,
                         "input": { "text": format!("queued {}", station.node.name()) } }
        }));
    }
    converged(&deck, &[&alpha, &bravo]);

    alpha_link.set_online(false);
    bravo_link.set_online(false);
    reachability(&deck, false);
    let mut pending = Vec::new();
    for (station, body) in [&alpha, &bravo].into_iter().zip(&queued) {
        let command = post(&deck, &auth, "session.prompt", body.clone());
        assert_eq!(command["state"], "pending");
        assert_eq!(post(&deck, &auth, "session.prompt", body.clone()), command);
        assert_eq!(
            station
                .stdin()
                .count(&format!("queued {}", station.node.name())),
            0
        );
        pending.push(command);
        let local = station.run(&format!("offline {}", station.node.name()));
        station.wait_for("offline turn", &local, |s| s.usage.input_tokens.is_some());
    }

    // Restart only the deck. Both stations and all four providers stay alive.
    deck.stop();
    deck.spawn();
    for (body, original) in queued.iter().zip(&pending) {
        assert_eq!(
            post(&deck, &auth, "session.prompt", body.clone()),
            *original,
            "restart keeps the id, target incarnation, payload and deadline"
        );
    }
    alpha_link.set_online(true);
    bravo_link.set_online(true);
    reachability(&deck, true);
    for (station, body) in [&alpha, &bravo].into_iter().zip(&queued) {
        let command = settled(&deck, &auth, body["command_id"].as_str().unwrap());
        assert_eq!(post(&deck, &auth, "session.prompt", body.clone()), command);
        station.wait_for(
            "queued turn completed",
            body["command"]["session_id"].as_str().unwrap(),
            |s| s.current_turn.is_none() && s.usage.input_tokens.is_some_and(|n| n >= 6),
        );
    }
    converged(&deck, &[&alpha, &bravo]);
    assert_eq!(deck.sessions().len(), 4);
    for station in [&alpha, &bravo] {
        let log = station.stdin();
        assert_eq!(
            log.pids.len(),
            2,
            "no provider was restarted or spawned twice"
        );
        for prefix in ["hello", "queued", "offline"] {
            assert_eq!(log.count(&format!("{prefix} {}", station.node.name())), 1);
        }
    }

    // Stop the providers while links remain up. Station shutdown closes its
    // link before recording final child exits; those replay on its next boot.
    for station in [&alpha, &bravo] {
        for summary in station.sessions() {
            let id = format!("stop-{}", summary.session_id);
            post(
                &deck,
                &auth,
                "session.stop",
                json!({
                    "station": station.node.name(), "command_id": id,
                    "command": { "op": "stop", "session_id": summary.session_id,
                                 "how": "graceful" }
                }),
            );
            settled(&deck, &auth, &id);
            station.wait_for("provider stopped", &summary.session_id.0, |s| {
                s.state == SessionState::Exited
            });
        }
    }
    converged(&deck, &[&alpha, &bravo]);
    // Every provider has exited and the stations are idle: wait until the
    // deck's cursor holds each station's last event, stop the deck, then
    // compare every stored non-delta event, not only the cards. The station
    // processes keep running; SQLite WAL lets the test read their logs.
    for station in [&alpha, &bravo] {
        let last = last_seq(&open_store(&station.db()).await).await;
        let deadline = Instant::now() + WAIT;
        while deck
            .stations()
            .iter()
            .find(|s| s.name == station.node.name())
            .is_none_or(|s| s.cursor != last)
        {
            assert!(Instant::now() < deadline, "deck cursor behind {last}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    deck.stop();
    let det = Det::new();
    let store = DeckStore::open(&deck.node.dirs().deck_db(), det.clock(), det.ids())
        .await
        .unwrap();
    for station in [&alpha, &bravo] {
        let source = open_store(&station.db()).await;
        let ReadResult::Events { events, .. } = source.read_after(0, 100_000).await.unwrap() else {
            panic!("unexpected retention");
        };
        let rec = store
            .stations()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.name == station.node.name())
            .unwrap();
        assert_eq!(rec.cursor, events.last().unwrap().station_seq);
        for summary in source.snapshot().await.unwrap().sessions {
            let expected: Vec<EventEnvelope> = events
                .iter()
                .filter(|e| {
                    e.session_id == summary.session_id
                        && !matches!(e.event.kind_name(), "message.delta" | "reasoning.delta")
                })
                .cloned()
                .collect();
            let actual = store
                .timeline(rec.station_id.clone(), summary.session_id)
                .await
                .unwrap();
            assert_eq!(actual, expected, "no missing or duplicate timeline events");
        }
    }
    assert_eq!(
        store.commands().await.unwrap().len(),
        8,
        "one durable row per command"
    );
}
