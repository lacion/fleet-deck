#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::fleet_git::PushInfo;
use fleetdeck_proto::fleet_git::PushState;
use fleetdeck_proto::{CardStatus, SessionSummary};

use super::*;

fn card(station: &str, id: &str, branch: &str, push_ref: Option<&str>) -> DeckSession {
    let mut summary = SessionSummary::new(id.into());
    summary.branch = Some(branch.to_owned());
    summary.callsign = Some(format!("cs-{id}"));
    summary.push = push_ref.map(|r| PushInfo {
        state: PushState::Pushed,
        ref_name: r.to_owned(),
        oid: None,
        desired_oid: None,
        stop_code: None,
        attempts: 0,
        next_attempt_at: None,
        code: None,
        text: None,
    });
    DeckSession {
        station_id: station.to_owned().into(),
        summary,
        status: CardStatus::Idle,
        unread: None,
        project_id: None,
        review_rev: None,
    }
}

fn branch(name: &str, slug: Option<&str>) -> RemoteBranch {
    RemoteBranch {
        name: name.to_owned(),
        slug: slug.map(str::to_owned),
        callsign: name.rsplit('/').next().unwrap().to_owned(),
        head: "a".repeat(40),
        ahead: 2,
        behind: 1,
        at: 1_000,
        subject: "subject".to_owned(),
    }
}

fn answer(branches: Vec<RemoteBranch>) -> RemoteBranches {
    RemoteBranches {
        remote: "origin".to_owned(),
        default_branch: "main".to_owned(),
        default_oid: None,
        fetched_at: Some(5),
        branches,
        truncated: false,
    }
}

fn names() -> HashMap<String, String> {
    HashMap::from([("st-b".to_owned(), "bravo".to_owned())])
}

fn slugs() -> Vec<SlugReservation> {
    vec![SlugReservation {
        slug: "bravo".to_owned(),
        station_id: "st-b".to_owned().into(),
    }]
}

#[test]
fn a_slug_branch_maps_to_its_station_and_to_the_session_by_publication_ref_then_branch() {
    let sessions = vec![
        // The local branch differs from the pushed ref.
        card("st-b", "s1", "local-1", Some("fd/bravo/one")),
        card("st-b", "s2", "fd/bravo/two", None),
        card("st-c", "s3", "fd/bravo/one", None),
    ];
    let (names, slugs) = (names(), slugs());
    let input = MapInput {
        slugs: &slugs,
        names: &names,
        sessions: &sessions,
        conflicts: &[],
    };
    let view = map_branches(
        answer(vec![
            branch("fd/bravo/one", Some("bravo")),
            branch("fd/bravo/two", Some("bravo")),
            branch("fd/bravo/none", Some("bravo")),
            branch("fd/ghost/x", Some("ghost")),
        ]),
        &input,
    );
    let rows = &view.branches;
    assert_eq!(rows[0].station_name.as_deref(), Some("bravo"));
    assert_eq!(rows[0].session.as_ref().unwrap().session_id.as_str(), "s1");
    assert_eq!(rows[1].session.as_ref().unwrap().session_id.as_str(), "s2");
    assert!(rows[2].session.is_none() && !rows[2].legacy);
    // An unreserved slug has no station and no session, and is not legacy.
    assert!(rows[3].station_id.is_none() && rows[3].session.is_none() && !rows[3].legacy);
    assert_eq!(
        (view.fetched_at, rows[0].conflict),
        (Some(5), Conflict::None)
    );
}

#[test]
fn a_legacy_branch_links_a_session_only_when_exactly_one_has_that_branch() {
    let (names, slugs) = (names(), slugs());
    let one = vec![card("st-a", "s1", "fd/old", None)];
    let two = vec![
        card("st-a", "s1", "fd/old", None),
        card("st-b", "s2", "fd/old", None),
    ];
    let rows = |sessions: &[DeckSession]| {
        let input = MapInput {
            slugs: &slugs,
            names: &names,
            sessions,
            conflicts: &[],
        };
        map_branches(answer(vec![branch("fd/old", None)]), &input).branches
    };
    let linked = rows(&one);
    assert!(linked[0].legacy && linked[0].station_id.is_none());
    assert_eq!(
        linked[0].session.as_ref().unwrap().session_id.as_str(),
        "s1"
    );
    assert!(rows(&two)[0].session.is_none());
}

#[test]
fn the_conflict_column_comes_from_the_radar_rows_of_either_side_with_the_default_branch() {
    let sessions = vec![
        card("st-b", "s1", "fd/bravo/one", None),
        card("st-b", "s2", "fd/bravo/two", None),
        card("st-b", "s3", "fd/bravo/three", None),
    ];
    let (names, slugs) = (names(), slugs());
    let conflicts: Vec<ConflictRow> = vec![
        (
            ("st-b".into(), "s1".into()),
            ("st-b".into(), "s2".into()),
            vec!["b.rs".into(), "a.rs".into()],
        ),
        (
            ("st-b".into(), "s2".into()),
            (String::new(), "@default".into()),
            vec!["a.rs".into(), "c.rs".into()],
        ),
    ];
    let input = MapInput {
        slugs: &slugs,
        names: &names,
        sessions: &sessions,
        conflicts: &conflicts,
    };
    let view = map_branches(
        answer(vec![
            branch("fd/bravo/one", Some("bravo")),
            branch("fd/bravo/two", Some("bravo")),
            branch("fd/bravo/three", Some("bravo")),
        ]),
        &input,
    );
    let rows = &view.branches;
    assert_eq!(rows[0].conflict, Conflict::WillConflict);
    assert_eq!(rows[0].files, ["a.rs", "b.rs"]);
    // The `b` side of one row and the `a` side of another: the union, sorted.
    assert_eq!(rows[1].files, ["a.rs", "b.rs", "c.rs"]);
    assert_eq!((rows[2].conflict, rows[2].files.len()), (Conflict::None, 0));
}

#[test]
fn the_view_serializes_without_empty_options() {
    let (names, slugs) = (names(), slugs());
    let input = MapInput {
        slugs: &slugs,
        names: &names,
        sessions: &[],
        conflicts: &[],
    };
    let view = map_branches(answer(vec![branch("fd/old", None)]), &input);
    let v = serde_json::to_value(&view).unwrap();
    assert!(v.get("default_oid").is_none());
    let row = &v["branches"][0];
    assert_eq!(row["legacy"], true);
    assert_eq!(row["conflict"], "none");
    assert!(row.get("slug").is_none() && row.get("session").is_none());
}
