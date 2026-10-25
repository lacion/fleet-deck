//! B26 PRE-PR review fixes over a fake station link (design
//! `grants-update.md`, "Build answers"): a `coordinator.policy_now` answer
//! holds only for the caller links that were settled when the deck asked,
//! and the link start puts back a `coordinate` scope that a rebuild during
//! the gap removed, before the first request of the returning link.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::fleet::*;
use common::*;
use fleetdeck_deck::store::GrantClass;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::mutation::{AuthorityCheck, CoordinatorQuery};
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};

/// Every feature of this build: a second link with it changes no link check
/// of the station, only its settle state.
fn all_features() -> Vec<String> {
    feature::ALL.iter().map(|f| (*f).to_owned()).collect()
}

/// A rig with a sent `stop` of `w1` by `coord-1` and its authority check.
async fn checked() -> (Rig, AuthorityCheck) {
    let mut r = rig().await;
    let b = bound(&r.f, "coord-1", &[GrantClass::Drive], true).await;
    let receipt = call(&r.f, &b, "k1", stop(&r.f, "w1")).await;
    assert!(receipt.reject.is_none(), "{receipt:?}");
    let (id, _) = wire(&mut r).await;
    let check = AuthorityCheck {
        grant_id: b.grant_id.clone(),
        generation: 1,
        caller_station: r.f.station.clone(),
        caller_session: "coord-1".into(),
        target_session: "w1".into(),
        command_id: id,
        launch: None,
    };
    (r, check)
}

/// A second link of the caller station starts while the deck waits for the
/// policy answer of the first one (`DeckLink::up` order: the mark, then the
/// features). The answer is stale: the check is invalid.
#[tokio::test]
async fn a_caller_link_that_returns_during_the_policy_wait_makes_the_check_invalid() {
    let (mut r, check) = checked().await;
    let live = json!({"fleet_control": true, "bound": true});
    let got = authority_check_held(&mut r, &check, live.clone(), async {}).await;
    assert_eq!(got, json!({"valid": true}));
    let (store, station) = (r.f.store.clone(), r.f.station.clone());
    let returns = async move {
        store.mark_unsettled(&station, 9_001);
        store.set_link_features(&station, 9_001, all_features());
    };
    let got = authority_check_held(&mut r, &check, live, returns).await;
    assert_eq!(
        got,
        json!({"valid": false, "reason": "caller_unreachable"}),
        "{got}"
    );
}

/// The returning link also settles before the old answer arrives: the
/// station is settled again, but the answer belongs to an earlier epoch and
/// stays stale. A new check after the return is valid.
#[tokio::test]
async fn an_answer_asked_before_a_return_stays_stale_after_the_return_check() {
    let (mut r, check) = checked().await;
    let live = json!({"fleet_control": true, "bound": true});
    let (store, station) = (r.f.store.clone(), r.f.station.clone());
    let returns_and_settles = async move {
        store.mark_unsettled(&station, 9_002);
        store.set_link_features(&station, 9_002, all_features());
        // The station never answered `station.info`: no stored answer, so
        // this check is neither an update nor a reset and keeps the grant.
        let resolved = store
            .resolve_grant_holds(station, 9_002, "inc-r".to_owned().into(), None)
            .await
            .unwrap();
        assert!(resolved.settled);
        assert_eq!(resolved.dropped, 0);
    };
    let got = authority_check_held(&mut r, &check, live.clone(), returns_and_settles).await;
    assert_eq!(
        got,
        json!({"valid": false, "reason": "caller_unreachable"}),
        "{got}"
    );
    let got = authority_check_held(&mut r, &check, live, async {}).await;
    assert_eq!(got, json!({"valid": true}), "{got}");
}

async fn has_coordinate(f: &Fixture) -> bool {
    f.store
        .station(f.station.clone())
        .await
        .unwrap()
        .unwrap()
        .scopes
        .contains(&Scope::Coordinate)
}

async fn ask_query(
    station: &fleetdeck_link::LiveLink,
    f: &Fixture,
    b: &fleetdeck_proto::mutation::CoordinatorBinding,
) -> Outcome {
    let params = json!(query(f, b, CoordinatorQuery::ListSessions));
    tokio::time::timeout(
        T,
        station.control.request("coordinator.query", None, params),
    )
    .await
    .expect("an answer")
    .expect("a response")
}

fn accepted(out: &Outcome) {
    let Outcome::Ok(v) = out else {
        panic!("{out:?}")
    };
    assert!(
        !matches!(v, Value::Object(o) if o.contains_key("reject")),
        "{v}"
    );
}

/// A grant that is idle by its own clock during the gap loses its scope to a
/// rebuild of another change; the hold at the link start makes it in effect
/// again, and the first query of the returning link passes the link layer
/// and the store.
#[tokio::test]
async fn the_first_query_after_a_return_passes_when_a_rebuild_in_the_gap_removed_the_scope() {
    let mut f = fixture_with(coordinate()).await;
    let first = f.connect().await;
    let st = f.station.clone();
    let b = bound_edit(&f, &st, "coord-1", &[GrantClass::Read], |g| {
        g.idle_ttl_ms = 60_000;
    })
    .await;
    // Unbound, so never idle: it holds the scope until the owner revokes it.
    let keeper = f
        .store
        .create_grant(fleetdeck_deck::store::NewGrant::new(
            st.clone(),
            "keeper".into(),
        ))
        .await
        .unwrap();
    accepted(&ask_query(&first, &f, &b).await);
    f.det.advance(Duration::from_secs(55));
    f.drop_link(first).await;
    f.det.advance(Duration::from_secs(6));
    // The rebuild of an unrelated change: `coord-1` is idle by its own clock
    // and still `active`.
    assert!(
        f.store
            .revoke_grant(keeper.grant_id, "owner".to_owned())
            .await
            .unwrap()
    );
    f.ctx.peers.refresh_changed().await.unwrap();
    assert!(!has_coordinate(&f).await);
    f.det.advance(Duration::from_secs(4));
    let second = f.connect().await;
    assert!(has_coordinate(&f).await);
    accepted(&ask_query(&second, &f, &b).await);
}
