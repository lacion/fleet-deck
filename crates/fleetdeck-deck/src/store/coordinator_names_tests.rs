//! A station named by its name in a coordinator call or a fleet view acts
//! as the station id (`coordinator_names.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::ReceiptState;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::CoordinatorOp;
use fleetdeck_proto::{RejectCode, StationId};

use super::names::resolve_station;
use super::tests::{World, code, decide, env, grant, grant_with, spawn, target, world};
use crate::store::fleet_view_tests as view;
use crate::store::grants::GrantClass;
use crate::store::stations::NewStation;
use crate::store::{DeckStore, call};

/// The `station` of every outbox item, in order.
async fn outbox_stations(w: &World) -> Vec<StationId> {
    w.s.outbox()
        .await
        .unwrap()
        .into_iter()
        .map(|item| item.station_id)
        .collect()
}

async fn pin(s: &DeckStore, name: &str) -> StationId {
    s.pin_station(NewStation {
        key: Identity::generate().unwrap().public_key(),
        name: name.to_owned(),
        scopes: BTreeSet::from([Scope::Read, Scope::Coordinate]),
        dial_url: None,
    })
    .await
    .unwrap()
    .station_id
}

async fn resolve(s: &DeckStore, value: &str) -> Option<String> {
    let value = value.to_owned();
    call(&s.inner.db, move |conn| resolve_station(conn, &value))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_spawn_by_station_name_goes_to_the_station_id() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Spawn], 5, &[("claude", "default")]).await;
    let by_name = spawn(&StationId::from("alpha"), "claude", None);
    let r = decide(&w, env(&w, &b, "k1", by_name.clone())).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    assert_eq!(outbox_stations(&w).await, vec![w.alpha.clone()]);
    // The same call again is a replay of the same receipt: the receipt
    // keeps the op as received.
    let again = decide(&w, env(&w, &b, "k1", by_name)).await;
    assert_eq!(again, r);
    assert_eq!(w.s.coordinator_receipts().await.unwrap(), 1);
    assert_eq!(w.s.coordinator_starts().await.unwrap(), 1);
    // The station id still works.
    let r = decide(&w, env(&w, &b, "k2", spawn(&w.alpha, "claude", None))).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
}

#[tokio::test]
async fn a_station_name_matches_without_regard_to_ascii_case() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Spawn], 5, &[("claude", "default")]).await;
    let r = decide(
        &w,
        env(
            &w,
            &b,
            "k1",
            spawn(&StationId::from("ALPHA"), "claude", None),
        ),
    )
    .await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    assert_eq!(outbox_stations(&w).await, vec![w.alpha.clone()]);
}

#[tokio::test]
async fn an_unknown_station_name_is_refused_with_the_text_of_an_id() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Spawn], 5, &[("claude", "default")]).await;
    let r = decide(
        &w,
        env(
            &w,
            &b,
            "k1",
            spawn(&StationId::from("nowhere"), "claude", None),
        ),
    )
    .await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
    assert_eq!(r.reject.unwrap().message, "the station is not in the grant");
    assert!(w.s.outbox().await.unwrap().is_empty());
    // A station that exists but is not in the grant: the same text.
    let r = decide(
        &w,
        env(
            &w,
            &b,
            "k2",
            spawn(&StationId::from("beta"), "claude", None),
        ),
    )
    .await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
    assert_eq!(r.reject.unwrap().message, "the station is not in the grant");
    assert!(w.s.outbox().await.unwrap().is_empty());
}

#[tokio::test]
async fn two_stations_that_match_keep_the_value_and_the_call_is_refused() {
    let w = world().await;
    let upper = pin(&w.s, "DEV").await;
    let mixed = pin(&w.s, "Dev").await;
    let b = grant_with(&w, &[GrantClass::Spawn], 5, &[("claude", "default")], |g| {
        g.stations.insert(upper.clone());
        g.stations.insert(mixed.clone());
    })
    .await;
    // `dev` matches both stations when case is ignored: it stays as it is.
    assert_eq!(resolve(&w.s, "dev").await, None);
    let r = decide(
        &w,
        env(&w, &b, "k1", spawn(&StationId::from("dev"), "claude", None)),
    )
    .await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden), "{r:?}");
    assert_eq!(r.reject.unwrap().message, "the station is not in the grant");
    assert!(w.s.outbox().await.unwrap().is_empty());
    // An exact name wins over the case-insensitive matches.
    assert_eq!(resolve(&w.s, "DEV").await, Some(upper.as_str().to_owned()));
    assert_eq!(resolve(&w.s, "Dev").await, Some(mixed.as_str().to_owned()));
    // A station id stays as it is; an empty value matches nothing.
    assert_eq!(resolve(&w.s, upper.as_str()).await, None);
    assert_eq!(resolve(&w.s, "").await, None);
}

#[tokio::test]
async fn send_and_interrupt_by_station_name_reach_the_right_station() {
    let w = world().await;
    let b = grant_with(&w, &[GrantClass::Mail, GrantClass::Drive], 0, &[], |g| {
        g.stations.insert(w.beta.clone());
    })
    .await;
    let named = |station: &str| target(&StationId::from(station), "other-1");
    let send = CoordinatorOp::Send {
        to: named("beta"),
        text: "hi".to_owned(),
        delivery: fleetdeck_proto::mutation::CoordinatorDelivery::FollowUp,
    };
    let r = decide(&w, env(&w, &b, "k1", send)).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    assert_eq!(outbox_stations(&w).await, vec![w.beta.clone()]);
    // `interrupt` needs a session of the lineage.
    w.s.add_lineage(
        b.grant_id.clone(),
        w.beta.clone(),
        "other-1".into(),
        "start-1".to_owned(),
    )
    .await
    .unwrap();
    let interrupt = CoordinatorOp::Interrupt {
        station: StationId::from("Beta"),
        session_id: "other-1".into(),
    };
    let r = decide(&w, env(&w, &b, "k2", interrupt)).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    assert_eq!(outbox_stations(&w).await, [w.beta.clone(), w.beta.clone()]);
}

#[tokio::test]
async fn a_projects_filter_by_station_name_lists_the_items_of_the_station_id() {
    let mut w = view::world().await;
    let alpha = w.pin("alpha", None).await;
    let beta = w.pin("beta", None).await;
    w.sessions(&alpha, vec![view::summary("coord-1")]).await;
    w.project(&alpha, "app", "/p/app").await;
    w.project(&beta, "lib", "/p/lib").await;
    let ask = |filter: &str| {
        let params = serde_json::json!({"caller": "coord-1", "view": "projects",
                                        "station": filter});
        let (s, alpha) = (w.s.clone(), alpha.clone());
        async move { s.fleet_view(&alpha, params).await.unwrap().items }
    };
    let by_id = ask(beta.as_str()).await;
    assert_eq!(by_id.as_array().unwrap().len(), 1, "{by_id}");
    assert_eq!(by_id[0]["name"], "lib");
    assert_eq!(ask("beta").await, by_id);
    assert_eq!(ask("BETA").await, by_id);
    // An unknown name stays as it is and matches no project.
    assert_eq!(ask("nowhere").await, serde_json::json!([]));
}
