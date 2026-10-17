#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::StationId;
use fleetdeck_proto::integrations::{ForgeBinding, ForgeKind, ProjectInput};
use fleetdeck_proto::session::{CardStatus, RepoInfo};
use fleetdeck_testkit::Det;

use super::*;

fn project(id: &str, station: &str, path: &str, forge: Option<(&str, &str)>) -> Project {
    Project {
        project_id: id.into(),
        name: id.into(),
        station_id: station.into(),
        path: path.into(),
        forge: forge.map(|(host, repo)| ForgeBinding {
            kind: ForgeKind::Github,
            host: host.into(),
            repo: repo.into(),
        }),
        trackers: vec![],
        prompt_template: None,
        review_prompt: None,
        created_at: 0,
        ..Project::default()
    }
}

fn card(station: &str, id: &str, cwd: &str) -> DeckSession {
    let mut summary = SessionSummary::new(id.into());
    summary.cwd = Some(cwd.to_owned());
    DeckSession {
        station_id: StationId::from(station.to_owned()),
        summary,
        status: CardStatus::Idle,
        unread: None,
        project_id: None,
        review_rev: None,
    }
}

fn worktree_card(station: &str, id: &str, repo_dir: &str, callsign: &str) -> DeckSession {
    let path = format!("{repo_dir}--fd-{callsign}");
    let mut c = card(station, id, &path);
    c.summary.callsign = Some(callsign.to_owned());
    c.summary.worktree = Some(path);
    c
}

fn of(projects: &[Project], card: &DeckSession) -> Option<String> {
    project_of(projects, &Assignments::new(), card)
}

#[test]
fn a_session_belongs_to_the_project_of_its_folder_or_a_parent() {
    let projects = [project("a", "st", "/work/app", None)];
    assert_eq!(
        of(&projects, &card("st", "s", "/work/app")),
        Some("a".into())
    );
    assert_eq!(
        of(&projects, &card("st", "s", "/work/app/crates/x")),
        Some("a".into())
    );
    assert_eq!(
        of(&projects, &card("st", "s", "/work/app/")),
        Some("a".into())
    );
    assert_eq!(of(&projects, &card("st", "s", "/work/app2")), None);
    assert_eq!(of(&projects, &card("st", "s", "/work")), None);
}

#[test]
fn the_longest_path_wins_and_an_equal_path_keeps_the_first_project() {
    let projects = [
        project("outer", "st", "/work", None),
        project("inner", "st", "/work/app", None),
        project("twin", "st", "/work/app/", None),
    ];
    assert_eq!(
        of(&projects, &card("st", "s", "/work/app/src")),
        Some("inner".into())
    );
    assert_eq!(
        of(&projects, &card("st", "s", "/work/other")),
        Some("outer".into())
    );
}

#[test]
fn a_project_of_another_station_is_ignored() {
    let projects = [project("a", "other", "/work/app", None)];
    assert_eq!(of(&projects, &card("st", "s", "/work/app")), None);
}

#[test]
fn a_manual_choice_wins_over_the_folder_and_none_means_no_project() {
    let projects = [
        project("a", "st", "/work/app", None),
        project("b", "st", "/work/lib", None),
    ];
    let c = card("st", "s", "/work/app");
    let key = ("st".to_owned(), "s".to_owned());
    let mut manual = Assignments::new();
    manual.insert(key.clone(), Some("b".into()));
    assert_eq!(project_of(&projects, &manual, &c), Some("b".into()));
    manual.insert(key, None);
    assert_eq!(project_of(&projects, &manual, &c), None);
    // A choice for another session changes nothing.
    let other = Assignments::from([(("st".to_owned(), "x".to_owned()), None)]);
    assert_eq!(project_of(&projects, &other, &c), Some("a".into()));
}

#[test]
fn a_worktree_session_belongs_to_the_project_of_the_main_checkout() {
    let projects = [project("a", "st", "/work/app", None)];
    let c = worktree_card("st", "s", "/work/app", "ada");
    assert_eq!(c.summary.cwd.as_deref(), Some("/work/app--fd-ada"));
    assert_eq!(of(&projects, &c), Some("a".into()));
    // A project that holds the parent of the main checkout matches too.
    let parent = [project("p", "st", "/work", None)];
    assert_eq!(of(&parent, &c), Some("p".into()));
    // A session started in a subfolder of the worktree has no `worktree`
    // field; its own folder is not a worktree folder, so no match.
    let mut sub = c.clone();
    sub.summary.worktree = None;
    sub.summary.cwd = Some("/work/app--fd-ada/src".into());
    assert_eq!(of(&projects, &sub), None);
    // Without `worktree`, the cwd that ends with the suffix is enough.
    let mut no_field = c.clone();
    no_field.summary.worktree = None;
    assert_eq!(of(&projects, &no_field), Some("a".into()));
}

#[test]
fn only_the_callsign_of_the_session_marks_its_worktree() {
    let projects = [project("a", "st", "/work/app", None)];
    let mut c = worktree_card("st", "s", "/work/app", "ada");
    c.summary.callsign = Some("bob".into());
    assert_eq!(of(&projects, &c), None);
    c.summary.callsign = None;
    assert_eq!(of(&projects, &c), None);
    assert_eq!(main_path("/--fd-ada", "ada"), None);
}

#[test]
fn the_origin_rule_runs_when_no_path_matches() {
    let projects = [
        project("a", "st", "/work/app", Some(("GitHub.com", "Acme/App"))),
        project("b", "st", "/work/lib", Some(("github.com", "acme/lib"))),
        project("c", "other", "/x", Some(("github.com", "acme/app"))),
    ];
    let mut c = card("st", "s", "/elsewhere/app-clone");
    c.summary.repo = Some(RepoInfo {
        root: "/elsewhere/app-clone".into(),
        origin: Some("github.com/acme/app".into()),
    });
    assert_eq!(of(&projects, &c), Some("a".into()));
    // The path rule comes first.
    let mut by_path = c.clone();
    by_path.summary.cwd = Some("/work/lib/src".into());
    assert_eq!(of(&projects, &by_path), Some("b".into()));
    // A repository without an origin matches nothing.
    c.summary.repo = Some(RepoInfo {
        root: "/elsewhere/app-clone".into(),
        origin: None,
    });
    assert_eq!(of(&projects, &c), None);
}

#[test]
fn two_projects_with_the_same_origin_give_no_automatic_project() {
    let projects = [
        project("a", "st", "/work/app", Some(("github.com", "acme/app"))),
        project("b", "st", "/work/app2", Some(("github.com", "acme/app"))),
    ];
    let mut c = card("st", "s", "/elsewhere/app-clone");
    c.summary.repo = Some(RepoInfo {
        root: "/elsewhere/app-clone".into(),
        origin: Some("github.com/acme/app".into()),
    });
    assert_eq!(of(&projects, &c), None);
}

async fn store() -> DeckStore {
    let det = Det::new();
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

fn input() -> ProjectInput {
    ProjectInput {
        name: "App".into(),
        station: "laptop".into(),
        path: "/work/app".into(),
        forge: None,
        trackers: vec![],
        prompt_template: None,
        review_prompt: None,
    }
}

#[tokio::test]
async fn assign_clear_and_auto_change_the_manual_rows() {
    let s = store().await;
    let p = s.create_project("st".into(), input()).await.unwrap();
    assert!(s.project_assignments().await.unwrap().is_empty());

    s.set_session_project("st".into(), "s1".into(), Some(p.project_id.clone()))
        .await
        .unwrap();
    s.set_session_project("st".into(), "s2".into(), None)
        .await
        .unwrap();
    let key = |id: &str| ("st".to_owned(), id.to_owned());
    let rows = s.project_assignments().await.unwrap();
    assert_eq!(rows.get(&key("s1")), Some(&Some(p.project_id.clone())));
    assert_eq!(rows.get(&key("s2")), Some(&None), "a NULL row is kept");

    // A second choice replaces the first.
    s.set_session_project("st".into(), "s1".into(), None)
        .await
        .unwrap();
    assert_eq!(
        s.project_assignments().await.unwrap().get(&key("s1")),
        Some(&None)
    );

    assert!(
        s.reset_session_project("st".into(), "s1".into())
            .await
            .unwrap()
    );
    assert!(
        !s.reset_session_project("st".into(), "s1".into())
            .await
            .unwrap()
    );
    let rows = s.project_assignments().await.unwrap();
    assert!(!rows.contains_key(&key("s1")) && rows.contains_key(&key("s2")));
}

#[tokio::test]
async fn deleting_a_project_returns_its_sessions_to_the_automatic_match() {
    let s = store().await;
    let p = s.create_project("st".into(), input()).await.unwrap();
    s.set_session_project("st".into(), "s1".into(), Some(p.project_id.clone()))
        .await
        .unwrap();
    s.set_session_project("st".into(), "s2".into(), None)
        .await
        .unwrap();
    assert!(s.delete_project(p.project_id).await.unwrap());
    let rows = s.project_assignments().await.unwrap();
    assert_eq!(rows.len(), 1, "only the NULL row is left: {rows:?}");
    assert!(rows.contains_key(&("st".to_owned(), "s2".to_owned())));
}

// ----- FG-5: homes on many stations (design §8.3, decisions B5c, B17) -----

use fleetdeck_proto::fleet_git::{GitHomeState, ProjectHome};

fn home(station: &str, path: Option<&str>, position: u32) -> ProjectHome {
    ProjectHome {
        station_id: StationId::from(station.to_owned()),
        generation: 1,
        position,
        state: GitHomeState::Ready,
        path: path.map(str::to_owned),
        reserved_path: None,
        attempt_id: None,
        sync_owner: false,
        code: None,
        message: None,
    }
}

fn homed(id: &str, homes: Vec<ProjectHome>) -> Project {
    let mut p = project(id, "copy-station", "/copy", None);
    p.homes = homes;
    p
}

fn with_origin(mut card: DeckSession, origin: &str) -> DeckSession {
    card.summary.repo = Some(RepoInfo {
        root: "/anywhere".into(),
        origin: Some(origin.into()),
    });
    card
}

#[test]
fn a_session_belongs_to_the_project_of_any_home_on_its_station() {
    let projects = [homed(
        "a",
        vec![
            home("st-1", Some("/work/app"), 0),
            home("st-2", Some("/srv/app"), 1),
        ],
    )];
    assert_eq!(
        of(&projects, &card("st-2", "s", "/srv/app/src")),
        Some("a".into())
    );
    assert_eq!(
        of(&projects, &card("st-1", "s", "/work/app")),
        Some("a".into())
    );
    // A folder of the other station's home does not match here.
    assert_eq!(of(&projects, &card("st-1", "s", "/srv/app")), None);
    // The copy does not count when the project has homes.
    assert_eq!(of(&projects, &card("copy-station", "s", "/copy")), None);
    // A home that still clones has no path to match.
    let cloning = [homed("c", vec![home("st-3", None, 0)])];
    assert_eq!(of(&cloning, &card("st-3", "s", "/srv/c")), None);
}

#[test]
fn a_project_without_homes_matches_through_its_copy() {
    let projects = [project("a", "st", "/work/app", None)];
    assert!(projects[0].homes.is_empty());
    assert_eq!(
        of(&projects, &card("st", "s", "/work/app/x")),
        Some("a".into())
    );
    assert_eq!(of(&projects, &card("other", "s", "/work/app/x")), None);
}

#[test]
fn the_repo_of_a_project_matches_a_session_on_any_station_without_case() {
    let mut a = homed("a", vec![home("st-1", Some("/work/app"), 0)]);
    a.repo = Some("github.com/Acme/App".into());
    let mut b = homed("b", vec![home("st-1", Some("/work/other"), 0)]);
    b.repo = Some("github.com/acme/other".into());
    let elsewhere = with_origin(card("st-9", "s", "/anywhere"), "GitHub.com/acme/app");
    assert_eq!(of(&[a.clone(), b.clone()], &elsewhere), Some("a".into()));
    // Two projects with the same repo give none.
    let mut twin = homed("t", vec![home("st-1", Some("/work/twin"), 0)]);
    twin.repo = a.repo.clone();
    assert_eq!(of(&[a.clone(), twin, b.clone()], &elsewhere), None);
    // The path rule comes first: the folder of `b` wins over the repo of `a`.
    let on_path = with_origin(card("st-1", "s", "/work/other/src"), "github.com/acme/app");
    assert_eq!(of(&[a, b], &on_path), Some("b".into()));
}
