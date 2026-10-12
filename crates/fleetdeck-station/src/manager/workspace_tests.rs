//! Workspace provisioning (M3 6-1, DL-200, DL-201): a start can create a
//! `<repo>--fd-<callsign>` worktree on `fd/<callsign>` or clone a URL; each
//! session gets a unique callsign; refused plans create nothing.

use std::process::Command;

use fleetdeck_proto::command::WorkspaceRequest;

use super::*;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-qm", "one"]);
}

fn start_cmd(cwd: &str, workspace: Option<WorkspaceRequest>) -> SessionCommand {
    SessionCommand::Start {
        spec: spec(cwd, "claude"),
        prompt: Some(text("hello")),
        workspace,
    }
}

async fn summary(st: &Station, s: &SessionId) -> fleetdeck_proto::SessionSummary {
    st.manager
        .store()
        .summary(s.clone())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn a_worktree_start_creates_the_callsign_worktree_on_its_branch() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    let d = st
        .accepted(start_cmd(&ws, Some(WorkspaceRequest::Worktree)))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    let sum = summary(&st, &s).await;
    let callsign = sum.callsign.clone().unwrap();
    let (animal, short) = callsign.split_once('-').unwrap();
    assert!(crate::workspace::provision::CALLSIGNS.contains(&animal));
    assert_eq!(short.len(), 4);
    let canonical = Path::new(&ws).canonicalize().unwrap();
    let expected = canonical
        .parent()
        .unwrap()
        .join(format!("app--fd-{callsign}"));
    assert_eq!(sum.worktree.as_deref(), expected.to_str());
    assert_eq!(sum.cwd, Some(expected.to_string_lossy().into_owned()));
    assert_eq!(
        sum.branch.as_deref(),
        Some(format!("fd/{callsign}").as_str())
    );
    // The effect creates the worktree before the provider starts.
    st.wait_for("started", |e| {
        matches!(e, Event::SessionStarted { .. }).then_some(())
    })
    .await;
    assert_eq!(
        git(&expected, &["rev-parse", "--abbrev-ref", "HEAD"]),
        format!("fd/{callsign}")
    );
    assert!(expected.join("a.txt").is_file());
    // The callsign reaches every reader as a `session.named` event.
    let named = st
        .wait_for("named", |e| match e {
            Event::SessionNamed { callsign, worktree } => {
                Some((callsign.clone(), worktree.clone()))
            }
            _ => None,
        })
        .await;
    assert_eq!(named, (callsign.clone(), sum.worktree.clone()));
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn every_session_gets_its_own_callsign() {
    let mut st = Station::new(8).await;
    let ws = st.workspace();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..4 {
        let s = st.start(&ws, None).await;
        let callsign = summary(&st, &s).await.callsign.unwrap();
        assert!(seen.insert(callsign.clone()), "{callsign} twice");
        // A plain start creates no worktree.
        assert_eq!(summary(&st, &s).await.worktree, None);
    }
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_worktree_start_outside_a_repository_is_refused_and_creates_nothing() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let code = st
        .rejected(start_cmd(&ws, Some(WorkspaceRequest::Worktree)))
        .await;
    assert_eq!(code, RejectCode::InvalidCommand);
    let projects = Path::new(&ws).parent().unwrap();
    let names: Vec<_> = std::fs::read_dir(projects)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("app")]);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_clone_url_with_credentials_or_outside_the_roots_is_forbidden() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let target = Path::new(&ws).parent().unwrap().join("new");
    let outside = st.root.path().join("elsewhere");
    init_repo(&outside);
    let cases = [
        "https://user:token@example.com/o/r.git",
        "ssh://user:pw@example.com/o/r.git",
        "ext::sh -c touch% /tmp/x",
        &format!("file://{}", outside.display()),
    ];
    for url in cases {
        let code = st
            .rejected(start_cmd(
                &target.to_string_lossy(),
                Some(WorkspaceRequest::Clone {
                    url: url.to_owned(),
                }),
            ))
            .await;
        assert!(
            matches!(code, RejectCode::Forbidden | RejectCode::InvalidCommand),
            "{url}: {code:?}"
        );
    }
    // A target outside the roots is forbidden too.
    let code = st
        .rejected(start_cmd(
            &st.root.path().join("elsewhere/new").to_string_lossy(),
            Some(WorkspaceRequest::Clone {
                url: "https://example.com/o/r.git".into(),
            }),
        ))
        .await;
    assert_eq!(code, RejectCode::Forbidden);
    assert!(!target.exists());
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_clone_start_clones_into_a_new_directory_inside_the_roots() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    let target = Path::new(&ws).parent().unwrap().join("copy");
    let d = st
        .accepted(start_cmd(
            &target.to_string_lossy(),
            Some(WorkspaceRequest::Clone {
                url: format!("file://{ws}"),
            }),
        ))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    st.wait_for("started", |e| {
        matches!(e, Event::SessionStarted { .. }).then_some(())
    })
    .await;
    assert!(target.join("a.txt").is_file());
    assert!(summary(&st, &s).await.callsign.is_some());
    // The same target again: it exists now, so the start is refused.
    let code = st
        .rejected(start_cmd(
            &target.to_string_lossy(),
            Some(WorkspaceRequest::Clone {
                url: format!("file://{ws}"),
            }),
        ))
        .await;
    assert_eq!(code, RejectCode::InvalidCommand);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_repo_catalog_shows_origin_default_branch_and_group() {
    let st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    git(
        Path::new(&ws),
        &[
            "remote",
            "add",
            "origin",
            "https://bot:secret@GitHub.com/Acme/App.git",
        ],
    );
    let projects = Path::new(&ws)
        .parent()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let query = fleetdeck_proto::link::WorkspaceQuery {
        path: Some(projects),
    };
    let list = workspaces::list(&st.manager.policy, &query).unwrap();
    let app = list.entries.iter().find(|e| e.name == "app").unwrap();
    assert!(app.repository);
    assert_eq!(
        app.origin.as_deref(),
        Some("https://GitHub.com/Acme/App.git")
    );
    assert_eq!(app.default_branch.as_deref(), Some("main"));
    assert_eq!(app.group.as_deref(), Some("github.com/acme/app"));
    // An owner group wins over the heuristic.
    let canonical = Path::new(&ws).canonicalize().unwrap();
    let policy = st
        .manager
        .policy
        .as_ref()
        .clone()
        .with_repo_groups(&BTreeMap::from([(canonical, "fleet".to_owned())]));
    let list = workspaces::list(&policy, &query).unwrap();
    let app = list.entries.iter().find(|e| e.name == "app").unwrap();
    assert_eq!(app.group.as_deref(), Some("fleet"));
}
