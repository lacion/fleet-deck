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
        workspace: workspace.map(Box::new),
        setup_cmd: None,
        batch: None,
        issue: None,
        origin_text: false,
        fleet_git: None,
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
        .accepted(start_cmd(
            &ws,
            Some(WorkspaceRequest::Worktree {
                branch: None,
                base: None,
            }),
        ))
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

fn worktree(branch: Option<&str>, base: Option<&str>) -> Option<WorkspaceRequest> {
    Some(WorkspaceRequest::Worktree {
        branch: branch.map(str::to_owned),
        base: base.map(str::to_owned),
    })
}

/// The message of the first `start_failed` error.
async fn start_failure(st: &Station) -> String {
    st.wait_for("start failure", |e| match e {
        Event::Error { code, message, .. } if code == "start_failed" => Some(message.clone()),
        _ => None,
    })
    .await
}

#[tokio::test]
async fn a_worktree_start_on_an_existing_branch_checks_it_out_and_names_the_ticket() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    git(Path::new(&ws), &["branch", "feature/PROJ-123-login"]);
    let d = st
        .accepted(start_cmd(
            &ws,
            worktree(Some("feature/PROJ-123-login"), None),
        ))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    let sum = summary(&st, &s).await;
    let callsign = sum.callsign.clone().unwrap();
    let (animal, key) = callsign.split_once('-').unwrap();
    assert!(crate::workspace::provision::CALLSIGNS.contains(&animal));
    assert_eq!(key, "PROJ-123");
    assert_eq!(sum.branch.as_deref(), Some("feature/PROJ-123-login"));
    st.wait_for("started", |e| {
        matches!(e, Event::SessionStarted { .. }).then_some(())
    })
    .await;
    let path = sum.worktree.unwrap();
    assert!(path.ends_with(&format!("app--fd-{callsign}")), "{path}");
    assert_eq!(
        git(Path::new(&path), &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature/PROJ-123-login"
    );
    // The same branch again: Git refuses a branch checked out elsewhere.
    let d = st
        .accepted(start_cmd(
            &ws,
            worktree(Some("feature/PROJ-123-login"), None),
        ))
        .await;
    let s2 = d.receipt.result.unwrap().session_id.unwrap();
    let message = start_failure(&st).await;
    assert!(
        message.starts_with("the workspace was not created"),
        "{message}"
    );
    assert_eq!(st.state(&s2).await, SessionState::Exited);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_worktree_start_on_a_new_branch_starts_from_the_base() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    let first = git(Path::new(&ws), &["rev-parse", "HEAD"]);
    std::fs::write(Path::new(&ws).join("b.txt"), "two\n").unwrap();
    git(Path::new(&ws), &["add", "b.txt"]);
    git(Path::new(&ws), &["commit", "-qm", "two"]);
    let d = st
        .accepted(start_cmd(&ws, worktree(Some("topic/new"), Some(&first))))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    st.wait_for("started", |e| {
        matches!(e, Event::SessionStarted { .. }).then_some(())
    })
    .await;
    let sum = summary(&st, &s).await;
    assert_eq!(sum.branch.as_deref(), Some("topic/new"));
    // No ticket key in the branch: today's callsign.
    assert_eq!(sum.callsign.unwrap().split_once('-').unwrap().1.len(), 4);
    let path = sum.worktree.unwrap();
    let path = Path::new(&path);
    assert_eq!(
        git(path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "topic/new"
    );
    assert_eq!(git(path, &["rev-parse", "HEAD"]), first);
    assert!(!path.join("b.txt").exists());
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_bad_base_or_a_base_on_an_existing_branch_fails_the_start() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    git(Path::new(&ws), &["branch", "old"]);
    let d = st
        .accepted(start_cmd(
            &ws,
            worktree(Some("topic/x"), Some("no-such-ref")),
        ))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    let message = start_failure(&st).await;
    assert!(
        message.contains("the base `no-such-ref` is not a commit"),
        "{message}"
    );
    assert_eq!(st.state(&s).await, SessionState::Exited);
    let sum = summary(&st, &s).await;
    assert!(!Path::new(&sum.worktree.unwrap()).exists());
    assert_eq!(git(Path::new(&ws), &["branch", "--list", "topic/x"]), "");

    let d = st
        .accepted(start_cmd(&ws, worktree(Some("old"), Some("HEAD"))))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    st.settled(&s).await;
    let events = st.events().await;
    let found = events.iter().any(|e| {
        matches!(&e.event, fleetdeck_proto::event::EventBody::Known(Event::Error { message, .. })
            if message.contains("the branch `old` exists; a base applies only to a new branch"))
    });
    assert!(found, "{:#?}", events);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_bad_branch_choice_is_refused_in_the_decision() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    for (branch, base) in [
        (Some("-x"), None),
        (Some("a b"), None),
        (Some("a..b"), None),
        (Some("a/"), None),
        (Some("x.lock"), None),
        (None, Some("main")),
        (Some("ok"), Some("-x")),
        (Some("ok"), Some("a b")),
    ] {
        let code = st.rejected(start_cmd(&ws, worktree(branch, base))).await;
        assert_eq!(code, RejectCode::InvalidCommand, "{branch:?} {base:?}");
    }
    let projects = Path::new(&ws).parent().unwrap();
    assert_eq!(std::fs::read_dir(projects).unwrap().count(), 1);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_start_in_place_takes_the_ticket_key_from_the_current_branch() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    git(Path::new(&ws), &["checkout", "-qb", "feature/PROJ-9-x"]);
    let s = st.start(&ws, None).await;
    let callsign = summary(&st, &s).await.callsign.unwrap();
    assert_eq!(callsign.split_once('-').unwrap().1, "PROJ-9");
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
        .rejected(start_cmd(
            &ws,
            Some(WorkspaceRequest::Worktree {
                branch: None,
                base: None,
            }),
        ))
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
async fn a_clone_start_clones_into_a_new_directory_and_reuses_it_the_second_time() {
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
    // The same target again: it is a clone of the same repository now, so
    // the start is accepted and runs in place.
    let again = st
        .accepted(start_cmd(
            &target.to_string_lossy(),
            Some(WorkspaceRequest::Clone {
                url: format!("file://{ws}"),
            }),
        ))
        .await;
    let second = again.receipt.result.unwrap().session_id.unwrap();
    let existing = target
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(summary(&st, &second).await.cwd.as_deref(), Some(&*existing));
    assert_eq!(summary(&st, &second).await.worktree, None);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_clone_start_into_a_clone_of_the_same_repository_starts_in_place_without_a_plan() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let existing = Path::new(&ws).parent().unwrap().join("tools");
    std::fs::create_dir(&existing).unwrap();
    git(&existing, &["init", "-q"]);
    git(
        &existing,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/tools.git",
        ],
    );
    // An ssh request of the same repository reuses the https checkout.
    let d = st
        .accepted(start_cmd(
            &existing.to_string_lossy(),
            Some(WorkspaceRequest::Clone {
                url: "git@github.com:acme/tools.git".into(),
            }),
        ))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    let path = existing
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(summary(&st, &s).await.cwd.as_deref(), Some(&*path));
    let effects = st.manager.store().session_effects(s.clone()).await.unwrap();
    let start = effects
        .iter()
        .find_map(|e| match &e.payload {
            crate::store::EffectPayload::Start { workspace, .. } => Some(workspace.clone()),
            _ => None,
        })
        .expect("a start effect");
    assert_eq!(start, None);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_clone_start_into_a_clone_of_another_repository_is_refused() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let existing = Path::new(&ws).parent().unwrap().join("tools");
    std::fs::create_dir(&existing).unwrap();
    git(&existing, &["init", "-q"]);
    git(
        &existing,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/other.git",
        ],
    );
    let code = st
        .rejected(start_cmd(
            &existing.to_string_lossy(),
            Some(WorkspaceRequest::Clone {
                url: "https://github.com/acme/tools.git".into(),
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
    let list = workspaces::list(&st.manager.policy(), &query).unwrap();
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
        .policy()
        .as_ref()
        .clone()
        .with_repo_groups(&BTreeMap::from([(canonical, "fleet".to_owned())]));
    let list = workspaces::list(&policy, &query).unwrap();
    let app = list.entries.iter().find(|e| e.name == "app").unwrap();
    assert_eq!(app.group.as_deref(), Some("fleet"));
}
