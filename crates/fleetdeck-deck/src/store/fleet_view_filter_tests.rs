//! The `station` filter of the fleet view (gaps G2, design
//! `docs/v2/design/fleet-gaps.md` §2). The helpers are in
//! `fleet_view_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_proto::StationId;
use serde_json::Value;

use super::super::fleet_view_tests::{World, ask, info, refusal, summary, world};
use super::UNKNOWN_STATION;

/// `count` sessions `<prefix>-0` .. on `station`.
async fn sessions_on(w: &mut World, station: &StationId, prefix: &str, count: usize) {
    let list = (0..count)
        .map(|i| summary(&format!("{prefix}-{i:03}")))
        .collect();
    w.sessions(station, list).await;
}

fn session_ids(items: &Value) -> BTreeSet<String> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["session_id"].as_str().unwrap().to_owned())
        .collect()
}

fn name_filter(name: &str) -> StationId {
    StationId::from(name.to_owned())
}

#[tokio::test]
async fn the_station_filter_keeps_the_sessions_of_one_station_by_id_or_name() {
    let mut w = world().await;
    let alpha = w.pin("alpha", Some(info("alpha", vec![]))).await;
    let beta = w.pin("beta", Some(info("beta", vec![]))).await;
    sessions_on(&mut w, &alpha, "a", 3).await;
    sessions_on(&mut w, &beta, "b", 2).await;
    let expected: BTreeSet<String> = ["b-000", "b-001"].map(str::to_owned).into();

    for filter in [beta.clone(), name_filter("beta"), name_filter("BETA")] {
        let page = ask(&w, &alpha, "a-000", "sessions", None, Some(&filter))
            .await
            .unwrap();
        assert_eq!(session_ids(&page.items), expected, "{filter:?}");
        assert!(page.next_cursor.is_none());
        let items = page.items.as_array().unwrap();
        assert!(items.iter().all(|i| i["station_id"] == beta.as_str()));
    }
}

#[tokio::test]
async fn the_station_filter_keeps_one_station_in_the_stations_view() {
    let mut w = world().await;
    let alpha = w.pin("alpha", Some(info("alpha", vec![]))).await;
    let _beta = w.pin("beta", Some(info("beta", vec![]))).await;
    sessions_on(&mut w, &alpha, "a", 1).await;

    let page = ask(
        &w,
        &alpha,
        "a-000",
        "stations",
        None,
        Some(&name_filter("alpha")),
    )
    .await
    .unwrap();
    let items = page.items.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["station_id"], alpha.as_str());
}

#[tokio::test]
async fn the_station_filter_of_the_projects_view_takes_a_name() {
    let mut w = world().await;
    let alpha = w.pin("alpha", Some(info("alpha", vec![]))).await;
    let beta = w.pin("beta", Some(info("beta", vec![]))).await;
    sessions_on(&mut w, &alpha, "a", 1).await;
    w.project(&alpha, "on-alpha", "/p/a").await;
    let on_beta = w.project(&beta, "on-beta", "/p/b").await;

    let page = ask(
        &w,
        &alpha,
        "a-000",
        "projects",
        None,
        Some(&name_filter("beta")),
    )
    .await
    .unwrap();
    let items = page.items.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["project_id"], on_beta.as_str());
}

#[tokio::test]
async fn an_unknown_or_ambiguous_station_filter_is_refused() {
    let mut w = world().await;
    let alpha = w.pin("alpha", Some(info("alpha", vec![]))).await;
    // The deck refuses two stations with the same exact name, so the names
    // differ in case: the exact match finds none and the case-insensitive
    // match finds two.
    let _dup1 = w.pin("Dup", Some(info("Dup", vec![]))).await;
    let _dup2 = w.pin("dUp", Some(info("dUp", vec![]))).await;
    sessions_on(&mut w, &alpha, "a", 1).await;

    for (view, filter) in [
        ("sessions", "nope"),
        ("stations", "nope"),
        ("projects", "nope"),
        ("sessions", "dup"),
    ] {
        let result = ask(&w, &alpha, "a-000", view, None, Some(&name_filter(filter))).await;
        assert_eq!(refusal(result), UNKNOWN_STATION, "{view} {filter}");
    }
}

#[tokio::test]
async fn the_me_view_ignores_the_station_filter() {
    let mut w = world().await;
    let alpha = w.pin("alpha", Some(info("alpha", vec![]))).await;
    sessions_on(&mut w, &alpha, "a", 1).await;

    let page = ask(&w, &alpha, "a-000", "me", None, Some(&name_filter("nope")))
        .await
        .unwrap();
    assert_eq!(page.items["session_id"], "a-000");
}

#[tokio::test]
async fn paging_with_the_station_filter_lists_every_session_of_the_station() {
    let mut w = world().await;
    let alpha = w.pin("alpha", Some(info("alpha", vec![]))).await;
    let beta = w.pin("beta", Some(info("beta", vec![]))).await;
    sessions_on(&mut w, &alpha, "a", 130).await;
    sessions_on(&mut w, &beta, "b", 20).await;

    let mut ids: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut first_had_next = false;
    let mut done = false;
    for page_no in 0..10 {
        let page = ask(
            &w,
            &alpha,
            "a-000",
            "sessions",
            cursor.as_deref(),
            Some(&name_filter("alpha")),
        )
        .await
        .unwrap();
        ids.extend(session_ids(&page.items));
        if page_no == 0 {
            first_had_next = page.next_cursor.is_some();
        }
        cursor = page.next_cursor;
        // The projections are rebuilt per page, as in `fleet_view_paging_tests.rs`.
        w.s.fleet_view_expire().await;
        if cursor.is_none() {
            done = true;
            break;
        }
    }
    assert!(done, "more than 10 pages");
    assert!(first_had_next);
    let unique: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(ids.len(), 130);
    assert_eq!(unique.len(), 130);
    assert!(ids.iter().all(|id| id.starts_with("a-")));
}
