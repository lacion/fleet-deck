//! A pinned import needs a station that verifies the pin (B7 PRE-PR r1): the
//! deck refuses it at admission with 409 `import_pin_unsupported` and again at
//! every send of a stored command. An unpinned import is not affected.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod share_support;

use std::time::Duration;

use common::*;
use fleetdeck_deck::link::CommandKind;
use fleetdeck_proto::command::{CommandOrigin, DeckCommandState, SessionCommand};
use serde_json::{Value, json};
use share_support::*;

const IMPORT: &str = "/api/commands/session.import";
const NATIVE: &str = "0b6c1d2e-aaaa-4bbb-8ccc-123456789abc";
const TEXT: &str = "This station cannot verify an import. Update it.";

fn import_body(id: &str, pinned: bool) -> Value {
    let pin = pinned.then(|| json!({"home": "/home/u/.claude", "dev": 7, "ino": 99}));
    json!({
        "station": "fake",
        "command_id": id,
        "command": {
            "op": "import",
            "spec": {
                "provider_instance": "claude", "cwd": "/w", "mode": null,
                "model": null, "title": null,
            },
            "native_session_id": NATIVE,
            "prompt": null,
            "transcript": pin,
        },
    })
}

#[tokio::test]
async fn a_pinned_import_to_a_station_without_the_feature_is_409_and_not_forwarded() {
    let (f, mut station, c, h) = setup().await;
    f.store.set_link_features(&f.station, 98, vec![]);
    let r = post(&c, &h, IMPORT, import_body("p1", true)).await;
    expect(&r, 409, "import_pin_unsupported");
    assert_eq!(r.body["message"], TEXT);
    assert!(f.store.commands().await.unwrap().is_empty());
    no_request(&mut station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn a_pinned_import_to_a_station_with_the_feature_is_forwarded() {
    let (_f, mut station, c, h) = setup().await;
    let call = post(&c, &h, IMPORT, import_body("p2", true));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "session.import");
    assert_eq!(req.params["command"]["transcript"]["ino"], 99);
}

#[tokio::test]
async fn an_unpinned_import_to_a_station_without_the_feature_is_forwarded() {
    let (f, mut station, c, h) = setup().await;
    f.store.set_link_features(&f.station, 98, vec![]);
    let call = post(&c, &h, IMPORT, import_body("p3", false));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "session.import");
    assert!(req.params["command"]["transcript"].is_null());
}

#[tokio::test]
async fn a_stored_pinned_import_is_refused_at_the_send_when_the_station_lost_the_feature() {
    let (f, mut station, _c, _h) = setup().await;
    // The command is stored while the link has no `import_pin.v1`: the send
    // gate settles it and sends nothing.
    f.store.set_link_features(&f.station, 98, vec![]);
    let command = SessionCommand::Import {
        spec: serde_json::from_value(json!({
            "provider_instance": "claude", "cwd": "/w", "mode": null,
            "model": null, "title": null,
        }))
        .unwrap(),
        native_session_id: NATIVE.to_owned(),
        prompt: None,
        transcript: Some(
            serde_json::from_value(json!({"home": "/home/u/.claude", "dev": 7, "ino": 99}))
                .unwrap(),
        ),
    };
    let stored = f
        .ctx
        .submit(
            f.station.clone(),
            CommandOrigin::Board,
            CommandKind::Session { command },
        )
        .await
        .unwrap();
    let done = f.settled(&stored.command_id).await;
    assert_eq!(done.state, DeckCommandState::Failed, "{done:?}");
    assert_eq!(done.reason.as_deref(), Some("import_pin_unsupported"));
    no_request(&mut station, Duration::from_millis(300)).await;
}
