//! N−1 version skew (PLAN Phase 7, M3 7-3, DL-128): a deck and a station
//! one build apart link, use the features that both have, refuse the newer
//! methods with `feature_not_negotiated`, and read the stored events and
//! summaries of the other build.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_link::methods::{authorize, code};
use fleetdeck_link::tls::Role;
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::version::{VersionOffer, feature, negotiate};
use fleetdeck_proto::{SessionId, SessionSummary};
use serde_json::json;

/// The offer of the previous build: this build without its newest feature.
fn previous_build() -> VersionOffer {
    let mut offer = VersionOffer::this_build();
    offer.features.retain(|f| f != feature::AUTO_CONTINUE);
    offer
}

#[test]
fn a_deck_and_a_station_one_build_apart_negotiate_the_shared_features() {
    let this = VersionOffer::this_build();
    let old = previous_build();
    let a = negotiate(&this, &old).unwrap();
    let b = negotiate(&old, &this).unwrap();
    assert_eq!(a, b, "both sides compute the same result");
    assert!(!a.has(feature::AUTO_CONTINUE));
    for f in &old.features {
        assert!(a.has(f), "{f}");
    }
}

#[test]
fn a_method_of_the_newer_build_is_refused_by_an_older_peer_and_the_shared_ones_work() {
    let negotiated = negotiate(&VersionOffer::this_build(), &previous_build()).unwrap();
    let err = authorize(
        "session.set_auto_continue",
        Role::Deck,
        Role::Station,
        Some("c1"),
        &negotiated,
        None,
    )
    .unwrap_err();
    assert_eq!(err.code, code::FEATURE_NOT_NEGOTIATED);
    authorize(
        "session.rename",
        Role::Deck,
        Role::Station,
        Some("c2"),
        &negotiated,
        None,
    )
    .unwrap();
}

#[test]
fn an_event_of_a_newer_build_is_kept_as_unknown_and_written_back_unchanged() {
    let raw = json!({ "kind": "session.from_the_future", "detail": { "n": 1 } });
    let body: EventBody = serde_json::from_value(raw.clone()).unwrap();
    assert!(matches!(&body, EventBody::Unknown { kind, .. } if kind == "session.from_the_future"));
    assert_eq!(serde_json::to_value(&body).unwrap(), raw);
}

#[test]
fn a_stored_event_and_summary_of_the_previous_build_decode_with_defaults() {
    // A summary of the previous build has no `auto_continue` field.
    let mut old =
        serde_json::to_value(SessionSummary::new(SessionId::from("s1".to_owned()))).unwrap();
    let fields = old.as_object_mut().unwrap();
    fields.remove("auto_continue");
    let summary: SessionSummary = serde_json::from_value(old).unwrap();
    assert!(!summary.auto_continue, "a missing preference is off");

    // A stored event of the previous build decodes as the known event.
    let body: EventBody =
        serde_json::from_value(json!({ "kind": "session.renamed", "title": "t" })).unwrap();
    assert!(matches!(
        body,
        EventBody::Known(Event::SessionRenamed { .. })
    ));
}
