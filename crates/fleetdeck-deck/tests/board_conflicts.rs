//! The conflict radar across stations (M3 6-7, DL-306): two sessions on
//! two stations that change the same file of a repository with the same
//! single origin are a conflict; a repository without an origin stays on
//! its station; a touch older than the window is no conflict.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::board::board;
use common::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_proto::conflicts::CONFLICT_WINDOW_MS;
use fleetdeck_proto::event::{Event, EventBody, FileChange, ToolStatus};
use fleetdeck_proto::session::RepoInfo;
use fleetdeck_proto::{EventEnvelope, IncarnationId, SessionSummary};

fn env(seq: u64, session: &str, at: i64, event: Event) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(event),
    }
}

fn session(id: &str, title: &str, cwd: &str, root: &str, origin: Option<&str>) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.title = Some(title.to_owned());
    s.cwd = Some(cwd.to_owned());
    s.repo = Some(RepoInfo {
        root: root.to_owned(),
        origin: origin.map(str::to_owned),
    });
    s
}

fn changed(seq: u64, session: &str, at: i64, path: &str) -> EventEnvelope {
    env(
        seq,
        session,
        at,
        Event::FileChanged {
            path: path.to_owned(),
            change: FileChange::Modified,
            node_id: None,
        },
    )
}

#[tokio::test]
async fn the_same_file_changed_on_two_stations_is_one_conflict_inside_the_window() {
    let f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let s = &f.store;
    let beta = s
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "beta".to_owned(),
            scopes: BTreeSet::new(),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id;
    let origin = Some("github.com/o/app");
    s.apply_snapshot(
        f.station.clone(),
        f.inc.clone(),
        0,
        vec![session(
            "s1",
            "Alpha fix",
            "/home/x/app",
            "/home/x/app",
            origin,
        )],
    )
    .await
    .unwrap();
    let beta_inc = IncarnationId::from("beta-db");
    s.apply_snapshot(
        beta.clone(),
        beta_inc.clone(),
        0,
        vec![
            session("s2", "Beta fix", "/srv/app/src", "/srv/app", origin),
            // Same relative path, but a repository without an origin.
            session("s3", "Other", "/srv/other", "/srv/other", None),
        ],
    )
    .await
    .unwrap();
    let now = i64::try_from(f.det.now_ms()).unwrap();
    s.apply_events(
        f.station.clone(),
        f.inc.clone(),
        vec![changed(1, "s1", now - 2_000, "/home/x/app/src/lib.rs")],
    )
    .await
    .unwrap();
    s.apply_events(
        beta.clone(),
        beta_inc,
        vec![
            // A relative path is relative to the session's cwd.
            env(
                1,
                "s2",
                now - 1_000,
                Event::ToolCompleted {
                    node_id: "n1".into(),
                    status: ToolStatus::Ok,
                    files: vec!["lib.rs".to_owned()],
                    exit_code: None,
                },
            ),
            changed(2, "s3", now - 500, "/srv/other/src/lib.rs"),
        ],
    )
    .await
    .unwrap();

    let (cookie, _) = c.login().await;
    let r = c
        .get("/api/conflicts", &[("Cookie", cookie.as_str())])
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let list = r.body.as_array().unwrap();
    assert_eq!(list.len(), 1, "{}", r.body);
    assert_eq!(list[0]["repo"], "github.com/o/app");
    assert_eq!(list[0]["file"], "src/lib.rs");
    let touches = list[0]["touches"].as_array().unwrap();
    assert_eq!(touches.len(), 2);
    assert_eq!(touches[0]["session_title"], "Beta fix", "newest first");
    assert_eq!(touches[0]["path"], "lib.rs");
    assert_eq!(touches[1]["station_id"], f.station.as_str());

    // Outside the window the touches are no conflict.
    f.det.advance(Duration::from_millis(
        u64::try_from(CONFLICT_WINDOW_MS).unwrap(),
    ));
    assert!(s.conflicts().await.unwrap().is_empty());
    // Without a login the radar is closed.
    let r = c.get("/api/conflicts", &[]).await;
    assert_eq!(r.status, 401);
}
