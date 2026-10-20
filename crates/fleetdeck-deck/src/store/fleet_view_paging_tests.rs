//! The fleet view on the deck store, test 2 (MCP P3 design §6): paging with
//! concurrent changes, the byte budget and the field limits. The helpers are
//! in `fleet_view_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_view::{FleetView, decode_cursor, encode_cursor};
use fleetdeck_proto::link::StationInfo;
use serde_json::Value;

use super::fleet_view::BAD_CURSOR;
use super::fleet_view_tests::{World, ask, info, provider, refusal, summary, world};

/// Control characters escape to six bytes each: a way to make fat items.
fn fat(n: usize) -> String {
    "\u{1}".repeat(n)
}

fn fat_info(name: &str, instance_chars: usize) -> StationInfo {
    let providers = (0..17)
        .map(|k| {
            provider(
                &format!("{}{k:02}", fat(instance_chars)),
                &fat(instance_chars),
            )
        })
        .collect();
    info(name, providers)
}

/// Pages through `view` and returns every item; `between` runs after each
/// page (concurrent changes), then the projections are dropped.
async fn drain(
    w: &mut World,
    caller_station: &StationId,
    view: &str,
    filter: Option<&StationId>,
    mut between: impl AsyncFnMut(&mut World, usize),
) -> Vec<Value> {
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    for page_no in 0..100 {
        let page = ask(
            w,
            caller_station,
            "caller-0",
            view,
            cursor.as_deref(),
            filter,
        )
        .await
        .unwrap();
        items.extend(page.items.as_array().unwrap().iter().cloned());
        let Some(next) = page.next_cursor else {
            return items;
        };
        // The cursor is valid for the view.
        let parts = decode_cursor(
            match view {
                "stations" => FleetView::Stations,
                "projects" => FleetView::Projects,
                _ => FleetView::Sessions,
            },
            &next,
        )
        .unwrap();
        assert!(!parts.is_empty());
        cursor = Some(next);
        between(w, page_no).await;
        w.s.fleet_view_expire().await;
        // A fresh caller per page: the session limit is not the subject.
    }
    panic!("a view of more than 100 pages");
}

fn unique(items: &[Value], key: &str) -> Vec<String> {
    let ids: Vec<String> = items
        .iter()
        .map(|i| i[key].as_str().unwrap().to_owned())
        .collect();
    let set: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(set.len(), ids.len(), "a repeated {key}");
    ids
}

#[tokio::test]
async fn paging_lists_every_station_project_and_session_above_one_page_with_concurrent_changes() {
    let mut w = world().await;
    // 70 stations with 17 providers and 80-character Unicode names; fat
    // items (about 6.6 KiB) so that the byte budget cuts a page of 32.
    let mut stations = Vec::new();
    for i in 0..70 {
        let name = format!("st{i:02}{}", "ü".repeat(76));
        assert_eq!(name.chars().count(), 80);
        let id = w.pin(&name, Some(fat_info(&name, 30))).await;
        stations.push((name, id));
    }
    // A station name of 128 four-byte characters (the longest valid key).
    let emoji = "\u{1F600}".repeat(128);
    let wide = w.pin(&emoji, Some(info(&emoji, vec![]))).await;
    // An item above 8 KiB after the cuts is left out.
    let oversized = w
        .pin("st-oversized", Some(fat_info("st-oversized", 64)))
        .await;
    let caller_station = stations[0].1.clone();

    // 250 projects: 40 on station 0, 120 with 128-character names on
    // station 1, the rest spread over other stations.
    let mut project_ids = BTreeMap::new();
    for i in 0..250 {
        let (name, station) = if i < 40 {
            (format!("p{i:03}"), &stations[0].1)
        } else if i < 160 {
            (emoji.clone(), &stations[1].1)
        } else {
            (format!("p{i:03}"), &stations[2 + i % 60].1)
        };
        let id = w.project(station, &name, &format!("/p/{i}")).await;
        project_ids.insert(id, station.clone());
    }

    // 250 sessions over the stations; a 200 KiB callsign is cut and a
    // 300-byte session id leaves its item out.
    let mut expected_sessions = BTreeSet::new();
    for (k, (_, id)) in stations.iter().enumerate().take(50) {
        let mut list = Vec::new();
        for j in 0..5 {
            let sid = format!("s{k:02}-{j}");
            let mut s = summary(&sid);
            s.title = Some(format!("{sid} {}", fat(200)));
            expected_sessions.insert(sid);
            list.push(s);
        }
        if k == 0 {
            list.push(summary("caller-0"));
            let mut long = summary("long-callsign");
            long.callsign = Some("c".repeat(200 * 1024));
            list.push(long);
            list.push(summary(&"x".repeat(300)));
        }
        w.sessions(id, list).await;
    }
    expected_sessions.insert("caller-0".to_owned());
    expected_sessions.insert("long-callsign".to_owned());

    // Stations: three pages, adds and removals between them.
    let removed = stations[60].1.clone();
    let removed_name = stations[60].0.clone();
    let added_name = format!("st30{}", "a".repeat(10));
    let mut added = None;
    let listed = drain(
        &mut w,
        &caller_station,
        "stations",
        None,
        async |w, page| {
            if page == 0 {
                w.revoke(&removed).await;
                added = Some(w.pin(&added_name, None).await);
                // Dropped from the middle of the listing.
            }
        },
    )
    .await;
    let ids = unique(&listed, "station_id");
    let names = unique(&listed, "name");
    assert!(names.windows(2).all(|p| p[0] < p[1]), "not in name order");
    assert!(
        !ids.contains(&removed.as_str().to_owned()),
        "a removed station"
    );
    assert!(!names.contains(&removed_name));
    // Every remaining station is listed once; the oversized one is not.
    for (name, id) in stations.iter().filter(|(_, id)| *id != removed) {
        assert!(ids.contains(&id.as_str().to_owned()), "{name} is missing");
    }
    assert!(ids.contains(&wide.as_str().to_owned()));
    assert!(!ids.contains(&oversized.as_str().to_owned()));
    assert!(
        ids.contains(&added.unwrap().as_str().to_owned()),
        "an added station"
    );
    // The first page was cut by the byte budget, below 32 items.
    w.s.fleet_view_expire().await;
    let first = ask(&w, &caller_station, "caller-0", "stations", None, None)
        .await
        .unwrap();
    assert!(first.items.as_array().unwrap().len() < 32);
    assert!(first.next_cursor.is_some());
    // 17 providers: 16 listed, `providers_more`.
    assert_eq!(first.items[0]["providers"].as_array().unwrap().len(), 16);
    assert_eq!(first.items[0]["providers_more"], true);

    // Projects: every project, and the 40 (and the 120 wide ones) of one
    // station with the filter.
    let listed = drain(
        &mut w,
        &caller_station,
        "projects",
        None,
        async |w, page| {
            if page == 0 {
                w.project(&stations[3].1, "p-added", "/p/added").await;
            }
        },
    )
    .await;
    let ids = unique(&listed, "project_id");
    // The projects of a revoked station have no station name: not listed.
    for (id, station) in &project_ids {
        assert_eq!(ids.contains(id), *station != removed, "{id}");
    }
    let filtered = drain(
        &mut w,
        &caller_station,
        "projects",
        Some(&stations[0].1),
        async |_, _| {},
    )
    .await;
    assert_eq!(unique(&filtered, "project_id").len(), 40);
    assert!(
        filtered
            .iter()
            .all(|p| p["station"] == stations[0].0.as_str())
    );
    let filtered = drain(
        &mut w,
        &caller_station,
        "projects",
        Some(&stations[1].1),
        async |_, _| {},
    )
    .await;
    assert_eq!(unique(&filtered, "project_id").len(), 120);
    assert!(filtered.iter().all(|p| p["name"] == emoji.as_str()));

    // Sessions: three pages; changes between them.
    let dropped: Vec<String> = (0..3).map(|j| format!("s49-{j}")).collect();
    let listed = drain(
        &mut w,
        &caller_station,
        "sessions",
        None,
        async |w, page| {
            if page == 0 {
                let id = stations[49].1.clone();
                let keep = (3..5).map(|j| summary(&format!("s49-{j}"))).collect();
                w.sessions(&id, keep).await;
            }
        },
    )
    .await;
    let ids = unique(&listed, "session_id");
    for id in &expected_sessions {
        if !dropped.contains(id) {
            assert!(ids.contains(id), "{id} is missing");
        }
    }
    assert!(
        dropped.iter().all(|id| !ids.contains(id)),
        "a removed session"
    );
    assert!(!ids.contains(&"x".repeat(300)), "a 300-byte id is left out");
    // The 200 KiB callsign is cut to 64 characters with the ellipsis.
    let long = listed
        .iter()
        .find(|i| i["session_id"] == "long-callsign")
        .unwrap();
    let callsign = long["callsign"].as_str().unwrap();
    assert_eq!(callsign.chars().count(), 64);
    assert!(callsign.ends_with('…'));
    // A title is cut at 200 characters.
    let titled = listed.iter().find(|i| i["session_id"] == "s01-0").unwrap();
    assert!(titled["title"].as_str().unwrap().chars().count() <= 200);

    // A bad, malformed or oversized cursor is `invalid_params`.
    w.s.fleet_view_expire().await;
    let good = encode_cursor(&["a".to_owned(), "b".to_owned()]);
    for (view, cursor) in [
        ("stations", "***".to_owned()),
        ("stations", "A".repeat(3000)),
        ("stations", encode_cursor(&["only-one".to_owned()])),
        (
            "stations",
            encode_cursor(&["n".repeat(129), "i".to_owned()]),
        ),
        (
            "stations",
            encode_cursor(&["n".to_owned(), "i".repeat(129)]),
        ),
        ("sessions", good.clone()),
        ("projects", format!("{good}=")),
    ] {
        let r = ask(&w, &caller_station, "caller-0", view, Some(&cursor), None).await;
        assert_eq!(refusal(r), BAD_CURSOR, "{view} {cursor:.20}");
    }
    // `me` ignores the cursor.
    ask(&w, &caller_station, "caller-0", "me", Some("***"), None)
        .await
        .unwrap();
}
