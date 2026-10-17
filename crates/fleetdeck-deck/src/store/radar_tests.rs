#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::event::{Event, EventBody, FileChange};
use fleetdeck_proto::fleet_git::{GitHomeState, GitSettingsPut, ProjectHome, RadarPair};
use fleetdeck_proto::integrations::ProjectInput;
use fleetdeck_proto::session::RepoInfo;
use fleetdeck_proto::{EventEnvelope, IncarnationId, SessionSummary};
use fleetdeck_testkit::Det;
use serde_json::json;

use super::*;
use crate::store::projects_git::GitWrite;
use crate::store::stations::NewStation;

async fn store() -> DeckStore {
    let det = Det::new();
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

async fn pin(s: &DeckStore, name: &str) -> StationId {
    s.pin_station(NewStation {
        key: Identity::generate().unwrap().public_key(),
        name: name.to_owned(),
        scopes: BTreeSet::new(),
        dial_url: None,
    })
    .await
    .unwrap()
    .station_id
}

/// A sync project "App" with a home at `/work/app` on `station`.
async fn sync_project(s: &DeckStore, station: &StationId, name: &str) -> Project {
    let made = s
        .create_project(
            station.as_str().to_owned(),
            ProjectInput {
                name: name.into(),
                station: "x".into(),
                path: "/work/app".into(),
                forge: None,
                trackers: vec![],
                prompt_template: None,
                review_prompt: None,
            },
        )
        .await
        .unwrap();
    let settings = fleetdeck_proto::fleet_git::GitSettings {
        sync: true,
        ..Default::default()
    };
    let out = s
        .set_project_git(
            made.project_id.clone(),
            GitSettingsPut {
                settings,
                revision: 0,
                remote_url: Some("https://github.com/acme/app.git".to_owned()),
            },
        )
        .await
        .unwrap();
    assert!(matches!(out, GitWrite::Saved(_)), "{out:?}");
    s.project(made.project_id).await.unwrap().unwrap()
}

fn home(station: &str, position: u32) -> ProjectHome {
    ProjectHome {
        station_id: StationId::from(station.to_owned()),
        generation: 1,
        position,
        state: GitHomeState::Ready,
        path: Some("/work/app".to_owned()),
        reserved_path: None,
        attempt_id: None,
        sync_owner: true,
        code: None,
        message: None,
    }
}

fn labels(stations: &[&str]) -> BTreeSet<String> {
    stations.iter().map(|s| (*s).to_owned()).collect()
}

#[tokio::test]
async fn the_default_radar_home_is_the_first_always_on_home_else_the_first_home() {
    let s = store().await;
    let a = pin(&s, "a").await;
    let mut p = sync_project(&s, &a, "App").await;
    p.homes = vec![home("c", 2), home("a", 0), home("b", 1)];
    let none = BTreeSet::new();
    // The first home by position, not by list order.
    assert_eq!(effective_radar_home(&p, &none).unwrap().as_str(), "a");
    // An `always_on` home wins; the first of them by position.
    let on = labels(&["c", "b"]);
    assert_eq!(effective_radar_home(&p, &on).unwrap().as_str(), "b");
    // The owner's choice wins over both.
    p.git.radar_home = Some(StationId::from("c".to_owned()));
    assert_eq!(effective_radar_home(&p, &on).unwrap().as_str(), "c");
    p.git.radar_home = None;
    p.homes.clear();
    assert!(effective_radar_home(&p, &on).is_none());
}

fn card(id: &str, station: &StationId) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.cwd = Some("/work/app".to_owned());
    s.callsign = Some(id.to_owned());
    s.title = Some(format!("Title {id}"));
    s.repo = Some(RepoInfo {
        root: "/work/app".to_owned(),
        origin: Some("github.com/acme/app".to_owned()),
    });
    let _ = station;
    s
}

fn pair(a: &str, b: &str, conflict: bool, files: &[&str]) -> RadarPair {
    RadarPair {
        a: a.to_owned(),
        b: b.to_owned(),
        conflict,
        files: files.iter().map(|f| (*f).to_owned()).collect(),
        files_truncated: false,
    }
}

fn result(at: i64, pairs: Vec<RadarPair>) -> RadarResult {
    RadarResult {
        checked_at: at,
        base_oid: None,
        pairs,
        skipped: 0,
        code: None,
        text: None,
    }
}

fn sides(station: &StationId, ids: &[&str]) -> BTreeMap<String, String> {
    ids.iter()
        .map(|id| ((*id).to_owned(), station.as_str().to_owned()))
        .collect()
}

#[tokio::test]
async fn each_run_replaces_the_rows_and_the_groups_show_only_conflicts() {
    let s = store().await;
    let st = pin(&s, "alpha").await;
    let p = sync_project(&s, &st, "App").await;
    let inc = IncarnationId::from("inc");
    s.apply_snapshot(
        st.clone(),
        inc,
        0,
        vec![card("s1", &st), card("s2", &st), card("s3", &st)],
    )
    .await
    .unwrap();
    let id = p.project_id.clone();
    let two = sides(&st, &["s1", "s2", "s3"]);
    let first = result(
        7_000,
        vec![
            pair("s1", "@default", true, &["a.rs"]),
            pair("s1", "s2", true, &["b.rs", "c.rs"]),
            pair("s2", "s3", false, &[]),
            // An unknown key is left out.
            pair("s9", "s1", true, &["z.rs"]),
        ],
    );
    s.radar_replace(id.clone(), first, two.clone())
        .await
        .unwrap();
    let groups = s.radar_groups().await.unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    let g = &groups[0];
    assert_eq!(g.project_id.as_deref(), Some(id.as_str()));
    assert_eq!(g.group_key, "origin:github.com/acme/app");
    assert_eq!(g.will_conflict.len(), 2);
    let default = g
        .will_conflict
        .iter()
        .find(|w| w.b == RadarSide::Default)
        .unwrap();
    assert_eq!(default.files, ["a.rs"]);
    assert_eq!(serde_json::to_value(&default.b).unwrap(), json!("default"));
    let sessions = g
        .will_conflict
        .iter()
        .find(|w| matches!(w.b, RadarSide::Session(_)))
        .unwrap();
    assert_eq!(sessions.files, ["b.rs", "c.rs"]);
    assert_eq!(
        serde_json::to_value(&sessions.a).unwrap(),
        json!({
            "station_id": st.as_str(), "session_id": "s1",
            "callsign": "s1", "title": "Title s1"
        })
    );
    // The default branch is stored with the station `''`.
    let stored: Vec<(String, String)> = s
        .db()
        .call(|conn| {
            let mut stmt = conn.prepare(
                "SELECT b_station, b_session FROM radar_pairs WHERE conflict = 1 ORDER BY b_session",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    assert_eq!(
        stored,
        [
            (String::new(), "@default".to_owned()),
            (st.as_str().to_owned(), "s2".to_owned())
        ]
    );
    // The next run replaces all of them.
    s.radar_replace(
        id.clone(),
        result(8_000, vec![pair("s2", "s3", true, &["d.rs"])]),
        two,
    )
    .await
    .unwrap();
    let groups = s.radar_groups().await.unwrap();
    assert_eq!(groups[0].will_conflict.len(), 1);
    assert_eq!(groups[0].will_conflict[0].checked_at, 8_000);
    // A session that is gone from the read model leaves no row in the answer.
    s.radar_replace(
        id.clone(),
        result(9_000, vec![pair("s2", "s3", true, &["d.rs"])]),
        sides(&st, &["s2", "s3"]),
    )
    .await
    .unwrap();
    s.apply_snapshot(
        st.clone(),
        IncarnationId::from("inc"),
        0,
        vec![card("s2", &st)],
    )
    .await
    .unwrap();
    assert!(s.radar_groups().await.unwrap().is_empty());
    // Delete and prune.
    assert!(s.radar_delete(id.clone()).await.unwrap());
    assert!(!s.radar_delete(id.clone()).await.unwrap());
    s.radar_replace(
        id.clone(),
        result(1, vec![pair("s2", "@default", true, &[])]),
        sides(&st, &["s2"]),
    )
    .await
    .unwrap();
    s.radar_prune(vec![id.clone()]).await.unwrap();
    assert!(s.radar_delete(id.clone()).await.unwrap());
    s.radar_replace(
        id.clone(),
        result(1, vec![pair("s2", "@default", true, &[])]),
        sides(&st, &["s2"]),
    )
    .await
    .unwrap();
    s.radar_prune(vec![]).await.unwrap();
    assert!(!s.radar_delete(id).await.unwrap());
}

fn touched(seq: u64, session: &str, at: i64, path: &str) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::FileChanged {
            path: path.to_owned(),
            change: FileChange::Modified,
            node_id: None,
        }),
    }
}

#[tokio::test]
async fn same_file_rows_join_the_project_of_their_newest_touch_and_no_project_is_last() {
    let s = store().await;
    let st = pin(&s, "alpha").await;
    let p = sync_project(&s, &st, "App").await;
    let inc = IncarnationId::from("inc");
    s.apply_snapshot(
        st.clone(),
        inc.clone(),
        0,
        vec![
            card("s1", &st),
            card("s2", &st),
            card("s3", &st),
            card("s4", &st),
        ],
    )
    .await
    .unwrap();
    // s3 and s4 say "no project".
    for id in ["s3", "s4"] {
        s.set_session_project(st.as_str().to_owned(), id.to_owned(), None)
            .await
            .unwrap();
    }
    let now = s.now_ms();
    s.apply_events(
        st.clone(),
        inc,
        vec![
            touched(1, "s1", now - 4_000, "/work/app/x.rs"),
            touched(2, "s2", now - 3_000, "/work/app/x.rs"),
            touched(3, "s3", now - 2_000, "/work/app/y.rs"),
            touched(4, "s4", now - 1_000, "/work/app/y.rs"),
        ],
    )
    .await
    .unwrap();
    let groups = s.radar_groups().await.unwrap();
    assert_eq!(groups.len(), 2, "{groups:?}");
    assert_eq!(groups[0].project_id.as_deref(), Some(p.project_id.as_str()));
    assert_eq!(groups[0].group_key, "origin:github.com/acme/app");
    assert_eq!(groups[0].same_files.len(), 1);
    assert_eq!(groups[0].same_files[0].file, "x.rs");
    assert!(groups[0].will_conflict.is_empty());
    assert_eq!(groups[1].project_id, None);
    assert_eq!(groups[1].group_key, "origin:github.com/acme/app");
    assert_eq!(groups[1].same_files[0].file, "y.rs");
    // The older view of `conflicts()` is unchanged.
    assert_eq!(s.conflicts().await.unwrap().len(), 2);
}

fn input(offline: bool, last: Option<i64>, branches: usize) -> RadarInput {
    RadarInput {
        project_id: "p".to_owned(),
        station: None,
        home_name: "alpha".to_owned(),
        offline,
        last_checked: last,
        branches: (0..branches)
            .map(|n| {
                (
                    RadarBranch {
                        key: format!("s{n}"),
                        ref_name: format!("fd/x/s{n}"),
                    },
                    StationId::from("st".to_owned()),
                )
            })
            .collect(),
    }
}

#[test]
fn the_offline_line_names_the_station_and_the_time_of_the_last_results() {
    let at = (13 * 3600 + 5 * 60) * 1000;
    let n = offline_notice(&input(true, Some(at), 1)).unwrap();
    assert_eq!(
        (n.code.as_str(), n.text.as_str()),
        (
            "station_offline",
            "Radar: alpha is offline; results from 13:05 UTC."
        )
    );
    // Without results the line has no time.
    let n = offline_notice(&input(true, None, 2)).unwrap();
    assert_eq!(n.text, "Radar: alpha is offline.");
    // Online, or with nothing to show: no line.
    assert!(offline_notice(&input(false, Some(at), 1)).is_none());
    assert!(offline_notice(&input(true, None, 0)).is_none());
    assert_eq!(clock_text(-1_000), "23:59 UTC");
}

#[tokio::test]
async fn the_inputs_hold_the_live_pushed_branches_and_the_offline_line_comes_at_read_time() {
    use fleetdeck_proto::fleet_git::{PushInfo, PushState};
    let s = store().await;
    let st = pin(&s, "alpha").await;
    let p = sync_project(&s, &st, "App").await;
    let mut pushed = card("s1", &st);
    pushed.push = Some(PushInfo {
        state: PushState::Pushed,
        ref_name: "fd/x/s1".to_owned(),
        oid: Some("a".repeat(40)),
        desired_oid: None,
        stop_code: None,
        attempts: 0,
        next_attempt_at: None,
        code: None,
        text: None,
    });
    s.apply_snapshot(
        st.clone(),
        IncarnationId::from("inc"),
        0,
        vec![pushed, card("s2", &st)],
    )
    .await
    .unwrap();
    let inputs = s.radar_inputs().await.unwrap();
    assert_eq!(inputs.len(), 1);
    let i = &inputs[0];
    assert_eq!(i.project_id, p.project_id);
    assert_eq!(i.home_name, "alpha");
    assert_eq!(i.branches.len(), 1);
    assert_eq!(i.branches[0].0.key, "s1");
    // No link: the radar cannot run, the home is offline.
    assert!(i.station.is_none());
    assert!(i.offline);
    let notices = s.radar_offline().await.unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].text, "Radar: alpha is offline.");
}
