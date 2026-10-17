//! `session.history` on the link (DL-189): the method table row. The deck
//! must negotiate `history.v1` first, and a `read` grant is enough.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_link::methods::{authorize, code, method};
use fleetdeck_link::tls::Role;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::version::{Negotiated, PROTO_MAX, feature};

fn negotiated(features: &[&str]) -> Negotiated {
    Negotiated {
        version: PROTO_MAX,
        features: features.iter().map(|f| (*f).to_owned()).collect(),
    }
}

fn read() -> BTreeSet<Scope> {
    [Scope::Read].into_iter().collect()
}

#[test]
fn the_method_row_is_a_read_method_of_the_history_feature() {
    let spec = method("session.history").unwrap();
    assert_eq!(spec.scope, Scope::Read);
    assert_eq!(spec.feature, Some(feature::HISTORY));
    assert!(!spec.mutating);
    assert_eq!(feature::HISTORY, "history.v1");
    assert!(feature::ALL.contains(&feature::HISTORY));
}

#[test]
fn it_is_refused_without_history_v1_and_served_with_it() {
    let without = negotiated(&[feature::CONTROL, feature::SESSION_DIFF]);
    let with = negotiated(&[feature::CONTROL, feature::HISTORY]);
    let call = |n: &Negotiated, caller, receiver| {
        authorize("session.history", caller, receiver, None, n, Some(&read()))
            .map(|_| ())
            .map_err(|e| e.code)
    };
    assert_eq!(
        call(&without, Role::Deck, Role::Station),
        Err(code::FEATURE_NOT_NEGOTIATED.to_owned())
    );
    assert_eq!(call(&with, Role::Deck, Role::Station), Ok(()));
    // A station never sends it.
    assert_eq!(
        call(&with, Role::Station, Role::Deck),
        Err(code::WRONG_ROLE.to_owned())
    );
    // A grant without `read` is refused.
    let none = BTreeSet::new();
    let denied = authorize(
        "session.history",
        Role::Deck,
        Role::Station,
        None,
        &with,
        Some(&none),
    );
    assert_eq!(denied.unwrap_err().code, code::FORBIDDEN);
}
