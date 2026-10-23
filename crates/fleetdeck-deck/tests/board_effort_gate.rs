//! An effort command needs a station with `session_effort.v1` (B18 Phase 2,
//! design `docs/v2/design/effort.md` §9.2 item 1): the deck refuses it at
//! admission with 409 `effort_unsupported` and at every send of a stored
//! command. A link that is down waits. A command without an effort is not
//! affected.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod share_support;

use std::time::Duration;

use common::*;
use fleetdeck_deck::link::CommandKind;
use fleetdeck_proto::command::{CommandOrigin, DeckCommandState, ReceiptState, SessionCommand};
use serde_json::{Value, json};
use share_support::*;

const START: &str = "/api/commands/session.start";
const IMPORT: &str = "/api/commands/session.import";
const SET_EFFORT: &str = "/api/commands/session.set_effort";
const NATIVE: &str = "0b6c1d2e-aaaa-4bbb-8ccc-123456789abc";
const TEXT: &str = "This station cannot set the effort. Update it.";

fn spec(effort: Option<&str>) -> Value {
    json!({
        "provider_instance": "claude", "cwd": "/w", "mode": null,
        "model": null, "title": null, "effort": effort,
    })
}

fn start_body(id: &str, effort: Option<&str>) -> Value {
    json!({
        "station": "fake",
        "command_id": id,
        "command": {"op": "start", "spec": spec(effort), "prompt": null},
    })
}

fn import_body(id: &str, effort: Option<&str>) -> Value {
    json!({
        "station": "fake",
        "command_id": id,
        "command": {
            "op": "import", "spec": spec(effort),
            "native_session_id": NATIVE, "prompt": null,
        },
    })
}

fn set_body(id: &str) -> Value {
    json!({
        "station": "fake",
        "command_id": id,
        "command": {"op": "set_effort", "session_id": "s1", "effort": "high"},
    })
}

fn command(body: &Value) -> SessionCommand {
    serde_json::from_value(body["command"].clone()).unwrap()
}

fn kind(body: &Value) -> CommandKind {
    CommandKind::Session {
        command: command(body),
    }
}

#[tokio::test]
async fn a_start_an_import_and_a_set_effort_to_a_station_without_the_feature_are_409() {
    let (f, mut station, c, h) = setup().await;
    f.store.set_link_features(&f.station, 98, vec![]);
    for (path, body) in [
        (START, start_body("e1", Some("high"))),
        (IMPORT, import_body("e2", Some("high"))),
        (SET_EFFORT, set_body("e3")),
    ] {
        let r = post(&c, &h, path, body).await;
        expect(&r, 409, "effort_unsupported");
        assert_eq!(r.body["message"], TEXT, "{path}");
    }
    assert!(f.store.commands().await.unwrap().is_empty());
    no_request(&mut station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn a_start_without_an_effort_to_a_station_without_the_feature_is_forwarded() {
    let (f, mut station, c, h) = setup().await;
    f.store.set_link_features(&f.station, 98, vec![]);
    let call = post(&c, &h, START, start_body("e4", None));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "session.start");
}

#[tokio::test]
async fn an_effort_command_to_a_station_with_the_feature_is_forwarded() {
    let (_f, mut station, c, h) = setup().await;
    for (path, body, method) in [
        (START, start_body("e5", Some("high")), "session.start"),
        (SET_EFFORT, set_body("e6"), "session.set_effort"),
    ] {
        let call = post(&c, &h, path, body);
        let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(req.method, method);
    }
}

#[tokio::test]
async fn a_stored_effort_command_is_refused_at_the_send_when_the_link_lost_the_feature() {
    for (n, body) in [
        start_body("s1", Some("high")),
        import_body("s2", Some("high")),
        set_body("s3"),
    ]
    .into_iter()
    .enumerate()
    {
        let (mut f, station, _c, _h) = setup().await;
        // Admitted while the link negotiated the feature; the station is
        // down when the command is stored. It comes back as an older build.
        f.drop_link(station).await;
        f.store.set_link_features(&f.station, 98, vec![]);
        let stored = f
            .ctx
            .submit(f.station.clone(), CommandOrigin::Board, kind(&body))
            .await
            .unwrap();
        let mut station = f.connect().await;
        let done = f.settled(&stored.command_id).await;
        assert_eq!(done.state, DeckCommandState::Failed, "{n}: {done:?}");
        assert_eq!(done.reason.as_deref(), Some("effort_unsupported"), "{n}");
        assert_eq!(done.attempts, 0, "{n}: nothing was sent");
        no_request(&mut station, Duration::from_millis(300)).await;
    }
}

#[tokio::test]
async fn an_effort_command_for_a_station_that_is_down_waits_and_is_sent_when_it_links() {
    for (n, (body, method)) in [
        (start_body("d1", Some("high")), "session.start"),
        (import_body("d2", Some("high")), "session.import"),
        (set_body("d3"), "session.set_effort"),
    ]
    .into_iter()
    .enumerate()
    {
        let (mut f, station, _c, _h) = setup().await;
        f.drop_link(station).await;
        let stored = f
            .ctx
            .submit(f.station.clone(), CommandOrigin::Board, kind(&body))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let waiting = f.command(&stored.command_id).await;
        assert_eq!(waiting.state, DeckCommandState::Pending, "{n}");
        assert_eq!(waiting.attempts, 0, "{n}");

        let mut station = f.connect().await;
        let req = Fixture::answer(&mut station, accepted).await;
        assert_eq!(req.method, method, "{n}");
        let done = f.settled(&stored.command_id).await;
        assert_eq!(done.state, DeckCommandState::Done, "{n}: {done:?}");
        assert_eq!(done.receipt.unwrap().state, ReceiptState::Accepted);
    }
}
