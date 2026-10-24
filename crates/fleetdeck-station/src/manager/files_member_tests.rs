//! The member file deny list (R11.9, DL-166): a call that the deck marks as
//! made for a member never lists, reads, searches or diffs a sensitive file,
//! and a member browses the workspace of a session only. The owner's calls
//! are not changed.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use fleetdeck_proto::TurnId;
use fleetdeck_proto::event::InputState;
use fleetdeck_proto::files::{
    FileListQuery, FileReadQuery, FileSearchQuery, SearchMode, TurnDiffQuery, code,
};
use fleetdeck_proto::member_deny::MemberDeny;
use fleetdeck_testkit::Det;

use super::files_tests::{session, store_with_session, ws};
use super::{diff_as, list_as, list_routed, read_as, read_routed, search_as, search_routed};
use crate::manager::policy::Policy;
use crate::manager::turns::turns_tests::{
    S1, completed, dispatch, git, input, repo, setup, started,
};
use crate::store::OfflineApprovalPolicy;
use crate::workspace::{GitOptions, PrivatePaths};

/// A policy whose deny list has one more pattern.
fn deny_policy(root: &Path, extra: &[&str]) -> Policy {
    let mut p = Policy::new(
        vec![root.to_path_buf()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    );
    let extra: Vec<String> = extra.iter().map(|s| (*s).to_owned()).collect();
    p.set_member_deny(MemberDeny::new(&extra));
    p
}

fn tree() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() { token_here(); }\n").unwrap();
    std::fs::write(root.join(".env"), "token_here=abc\n").unwrap();
    std::fs::write(root.join("src/server.pem"), "token_here pem\n").unwrap();
    std::fs::create_dir_all(root.join("secrets")).unwrap();
    std::fs::write(root.join("secrets/db.txt"), "token_here db\n").unwrap();
    std::fs::create_dir_all(root.join("private")).unwrap();
    std::fs::write(root.join("private/plan.txt"), "token_here plan\n").unwrap();
    std::fs::write(root.join("README.md"), "token_here readme\n").unwrap();
    (tmp, root)
}

#[tokio::test]
async fn a_member_list_leaves_out_the_hidden_files_and_the_owner_sees_all() {
    let det = Det::new();
    let (_t, root) = tree();
    let s = store_with_session(&det, &root).await;
    let p = deny_policy(&root, &["private/"]);
    let names = |l: fleetdeck_proto::files::FileListing| -> Vec<String> {
        l.entries.into_iter().map(|e| e.name).collect()
    };
    let q = |path: &str| FileListQuery {
        root: session(),
        path: path.to_owned(),
    };
    let member = names(
        list_as(&s, &p, &PrivatePaths::default(), q(""), true)
            .await
            .unwrap(),
    );
    assert_eq!(member, ["src", "README.md"]);
    let owner = names(
        list_as(&s, &p, &PrivatePaths::default(), q(""), false)
            .await
            .unwrap(),
    );
    for hidden in [".env", "secrets", "private"] {
        assert!(owner.iter().any(|n| n == hidden), "{hidden}");
    }
    let src = names(
        list_as(&s, &p, &PrivatePaths::default(), q("src"), true)
            .await
            .unwrap(),
    );
    assert_eq!(src, ["main.rs"]);
    // A hidden directory is not found, not listed as empty.
    for path in ["secrets", "private"] {
        let err = list_as(&s, &p, &PrivatePaths::default(), q(path), true)
            .await
            .unwrap_err();
        assert_eq!(err.code, code::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn a_member_read_of_a_hidden_file_is_not_found() {
    let det = Det::new();
    let (_t, root) = tree();
    let s = store_with_session(&det, &root).await;
    let p = deny_policy(&root, &["private/"]);
    let q = |path: &str| FileReadQuery {
        root: session(),
        path: path.to_owned(),
        max_bytes: None,
    };
    for path in [
        ".env",
        "src/server.pem",
        "secrets/db.txt",
        "private/plan.txt",
    ] {
        let err = read_as(&s, &p, &PrivatePaths::default(), q(path), true)
            .await
            .unwrap_err();
        assert_eq!(err.code, code::NOT_FOUND, "{path}");
        assert!(
            read_as(&s, &p, &PrivatePaths::default(), q(path), false)
                .await
                .is_ok(),
            "{path}"
        );
    }
    let ok = read_as(&s, &p, &PrivatePaths::default(), q("README.md"), true)
        .await
        .unwrap();
    assert_eq!(ok.text.as_deref(), Some("token_here readme\n"));
}

#[tokio::test]
async fn a_member_search_never_names_or_reads_a_hidden_file() {
    let det = Det::new();
    let (_t, root) = tree();
    let s = store_with_session(&det, &root).await;
    let p = deny_policy(&root, &["private/"]);
    let git = GitOptions::isolated();
    for (mode, text) in [
        (SearchMode::Content, "token_here"),
        (SearchMode::Name, "en"),
    ] {
        let q = || FileSearchQuery {
            root: session(),
            query: text.to_owned(),
            mode,
            max_hits: None,
        };
        let member = search_as(&s, &p, &PrivatePaths::default(), &git, q(), true)
            .await
            .unwrap()
            .hits;
        for hit in &member {
            for hidden in [".env", "server.pem", "secrets", "private"] {
                assert!(!hit.path.contains(hidden), "{mode:?}: {}", hit.path);
            }
        }
        let owner = search_as(&s, &p, &PrivatePaths::default(), &git, q(), false)
            .await
            .unwrap()
            .hits;
        assert!(
            owner
                .iter()
                .any(|h| h.path.contains("secrets") || h.path.contains(".env"))
        );
    }
    // A content search finds the visible files only.
    let hits = search_as(
        &s,
        &p,
        &PrivatePaths::default(),
        &git,
        FileSearchQuery {
            root: session(),
            query: "token_here".to_owned(),
            mode: SearchMode::Content,
            max_hits: None,
        },
        true,
    )
    .await
    .unwrap()
    .hits;
    let mut found: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
    found.sort_unstable();
    assert_eq!(found, ["README.md", "src/main.rs"]);
}

#[tokio::test]
async fn a_member_browses_the_session_workspace_only() {
    let det = Det::new();
    let (_t, root) = tree();
    let s = store_with_session(&det, &root).await;
    let p = deny_policy(&root, &[]);
    let err = list_as(
        &s,
        &p,
        &PrivatePaths::default(),
        FileListQuery {
            root: ws(&root),
            path: String::new(),
        },
        true,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::OUTSIDE_ROOTS);
    assert!(
        list_as(
            &s,
            &p,
            &PrivatePaths::default(),
            FileListQuery {
                root: ws(&root),
                path: String::new(),
            },
            false,
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn a_member_diff_loses_the_hunks_of_hidden_files() {
    let det = Det::new();
    let (_t, dir) = repo();
    // A checkpoint holds tracked files only: the secret is tracked.
    std::fs::write(dir.join(".env"), "TOKEN=old\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "secret"]);
    let (s, turns) = setup(&det, &dir).await;
    let p = deny_policy(&dir, &[]);
    let git = GitOptions::isolated();
    dispatch(&turns, "in-1").await;
    turns.observe(&[started("turn-1")]).await;
    std::fs::write(dir.join("a.txt"), "one\nmore\n").unwrap();
    std::fs::write(dir.join(".env"), "TOKEN=abc\n").unwrap();
    turns
        .observe(&[
            input("in-1", Some("turn-1"), InputState::Started),
            completed("turn-1"),
        ])
        .await;
    Arc::clone(&turns).capture_due().await;
    let q = || TurnDiffQuery {
        session_id: S1.into(),
        turn_id: Some(TurnId::from("turn-1")),
        checkpoint: None,
        max_bytes: None,
    };
    let owner = diff_as(&s, &p, &PrivatePaths::default(), &git, q(), false)
        .await
        .unwrap();
    assert!(owner.patch.contains("TOKEN=abc"), "{}", owner.patch);
    assert_eq!(owner.hidden_files, 0);
    let member = diff_as(&s, &p, &PrivatePaths::default(), &git, q(), true)
        .await
        .unwrap();
    assert!(member.patch.contains("+more"), "{}", member.patch);
    assert!(!member.patch.contains("TOKEN"), "{}", member.patch);
    assert!(!member.patch.contains(".env"), "{}", member.patch);
    assert_eq!(member.hidden_files, 1);
    assert_eq!(
        member.summary.files_changed + 1,
        owner.summary.files_changed
    );
}

#[tokio::test]
async fn a_publish_with_a_new_deny_pattern_hides_the_files_from_the_next_member_call() {
    let det = Det::new();
    let (_t, root) = tree();
    let s = store_with_session(&det, &root).await;
    let hub = crate::manager::policy_hub::PolicyHub::new(deny_policy(&root, &[]));
    let names = |l: fleetdeck_proto::files::FileListing| -> Vec<String> {
        l.entries.into_iter().map(|e| e.name).collect()
    };
    let q = || FileListQuery {
        root: session(),
        path: String::new(),
    };
    let before = names(
        list_as(&s, &hub.current(), &PrivatePaths::default(), q(), true)
            .await
            .unwrap(),
    );
    assert!(before.iter().any(|n| n == "private"), "{before:?}");

    let config = crate::manager::policy_hub::test_config(&root, "member_deny = [\"private/\"]\n");
    hub.swap(hub.current().reconfigured(&config));
    let after = names(
        list_as(&s, &hub.current(), &PrivatePaths::default(), q(), true)
            .await
            .unwrap(),
    );
    assert!(!after.iter().any(|n| n == "private"), "{after:?}");
    // The owner still sees it.
    let owner = names(
        list_as(&s, &hub.current(), &PrivatePaths::default(), q(), false)
            .await
            .unwrap(),
    );
    assert!(owner.iter().any(|n| n == "private"), "{owner:?}");
}

// ------------------------------------------------- the artifacts root (M11)

mod artifacts {
    use std::os::unix::fs::PermissionsExt;

    use fleetdeck_core::ChildEnv;
    use fleetdeck_proto::command::{ReceiptState, SessionCommand};
    use fleetdeck_proto::files::{FileRoot, FileSearchQuery, SearchBackend};
    use fleetdeck_proto::fleet_git::{
        FleetGitStart, GitSettings, LocalRemotes, PushMode, StartKind, remote_identity,
    };
    use fleetdeck_proto::{Event, SessionId};
    use fleetdeck_testkit::FakeAgent;

    use super::*;
    use crate::artifact_share::tests::Rig;
    use crate::manager::policy::SetupEnvs;
    use crate::manager::tests::{Station, spec, text};

    /// A rig whose session `S1` has an artifact folder with `a.txt` and `.env`.
    async fn with_folder() -> (Rig, std::path::PathBuf) {
        let rig = Rig::ready().await;
        rig.start().await;
        let folder = rig.share.join("My-App/ab12");
        std::fs::write(folder.join("a.txt"), "hello artifact\n").unwrap();
        std::fs::write(folder.join(".env"), "TOKEN=1\n").unwrap();
        (rig, folder)
    }

    fn query(path: &str) -> FileListQuery {
        FileListQuery {
            root: FileRoot::Artifacts(SessionId::from("S1")),
            path: path.to_owned(),
        }
    }

    fn names(listing: fleetdeck_proto::files::FileListing) -> Vec<String> {
        listing.entries.into_iter().map(|e| e.name).collect()
    }

    #[tokio::test]
    async fn the_owner_lists_the_artifact_folder_and_a_member_does_not_see_the_deny_list() {
        let (rig, _folder) = with_folder().await;
        let policy = deny_policy(&rig.root, &[]);
        let private = PrivatePaths::default();
        let call = |member| {
            list_routed(
                &rig.store,
                &rig.runtime,
                &policy,
                &private,
                query(""),
                member,
            )
        };
        let mut owner = names(call(false).await.unwrap());
        owner.sort();
        assert_eq!(owner, [".env", "a.txt"]);
        assert_eq!(names(call(true).await.unwrap()), ["a.txt"]);
        // A read: the owner reads `.env`, a member does not find it.
        let read = |member, path: &str| {
            read_routed(
                &rig.store,
                &rig.runtime,
                &policy,
                &private,
                FileReadQuery {
                    root: query("").root,
                    path: path.to_owned(),
                    max_bytes: None,
                },
                member,
            )
        };
        assert_eq!(
            read(false, ".env").await.unwrap().text.as_deref(),
            Some("TOKEN=1\n")
        );
        assert_eq!(read(true, ".env").await.unwrap_err().code, code::NOT_FOUND);
        assert_eq!(
            read(true, "a.txt").await.unwrap().text.as_deref(),
            Some("hello artifact\n")
        );
        // A search for the content of `.env`: the walk backend, no hit for a member.
        let git = GitOptions::isolated();
        let find = |member| {
            search_routed(
                &rig.store,
                &rig.runtime,
                &policy,
                &private,
                &git,
                FileSearchQuery {
                    root: query("").root,
                    query: "token".to_owned(),
                    mode: SearchMode::Content,
                    max_hits: None,
                },
                member,
            )
        };
        let owner = find(false).await.unwrap();
        assert_eq!(owner.backend, SearchBackend::Walk);
        assert_eq!(owner.hits.len(), 1);
        assert!(find(true).await.unwrap().hits.is_empty());
    }

    #[tokio::test]
    async fn a_member_with_a_workspace_root_is_still_refused() {
        let (rig, _folder) = with_folder().await;
        let policy = deny_policy(&rig.root, &[]);
        let denied = list_routed(
            &rig.store,
            &rig.runtime,
            &policy,
            &PrivatePaths::default(),
            FileListQuery {
                root: FileRoot::Workspace(rig.root.display().to_string()),
                path: String::new(),
            },
            true,
        )
        .await
        .unwrap_err();
        assert_eq!(denied.code, code::OUTSIDE_ROOTS);
    }

    #[tokio::test]
    async fn a_session_without_a_folder_is_unknown_and_a_replaced_folder_is_unavailable() {
        let (rig, folder) = with_folder().await;
        let policy = deny_policy(&rig.root, &[]);
        let private = PrivatePaths::default();
        let other = FileListQuery {
            root: FileRoot::Artifacts(SessionId::from("S9")),
            path: String::new(),
        };
        let e = list_routed(&rig.store, &rig.runtime, &policy, &private, other, false)
            .await
            .unwrap_err();
        assert_eq!(e.code, code::UNKNOWN_SESSION);
        std::fs::rename(&folder, rig.share.join("My-App/old")).unwrap();
        std::fs::create_dir(&folder).unwrap();
        let e = list_routed(
            &rig.store,
            &rig.runtime,
            &policy,
            &private,
            query(""),
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(
            e.code,
            fleetdeck_proto::fleet_git::code::ARTIFACTS_UNAVAILABLE
        );
        assert_eq!(
            e.message,
            "The artifact folder cannot be opened safely now."
        );
    }

    #[tokio::test]
    async fn a_share_that_is_not_authorized_now_refuses_the_next_access() {
        let (rig, _folder) = with_folder().await;
        let policy = deny_policy(&rig.root, &[]);
        let private = PrivatePaths::default();
        let listed = list_routed(
            &rig.store,
            &rig.runtime,
            &policy,
            &private,
            query(""),
            false,
        );
        assert!(listed.await.is_ok());
        // A home appears inside the share: exposed.
        rig.snapshot(Some("box/out"), Some(&rig.share.join("My-App")))
            .await;
        crate::taildrive::recompute(&rig.runtime, &rig.store)
            .await
            .unwrap();
        let e = list_routed(
            &rig.store,
            &rig.runtime,
            &policy,
            &private,
            query(""),
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(
            e.code,
            fleetdeck_proto::fleet_git::code::ARTIFACTS_UNAVAILABLE
        );
    }

    #[test]
    fn the_wire_json_of_the_artifacts_root_is_the_session_form() {
        let root = FileRoot::Artifacts(SessionId::from("abc"));
        assert_eq!(
            serde_json::to_string(&root).unwrap(),
            r#"{"artifacts":"abc"}"#
        );
        let back: FileRoot = serde_json::from_str(r#"{"artifacts":"abc"}"#).unwrap();
        assert_eq!(back, root);
    }

    /// A provider program that writes its environment next to itself, then
    /// runs the fake Claude.
    fn env_dump_program(dir: &Path) -> std::path::PathBuf {
        let fake = FakeAgent::Claude.locate().unwrap();
        let script = dir.join("provider.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nenv > \"$(dirname \"$0\")/provider-env.txt\"\nexec '{}' \"$@\"\n",
                fake.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn start_command(home: &Path, share: &str, cmd: Option<&str>) -> SessionCommand {
        let settings = GitSettings {
            sync: true,
            push: PushMode::Off,
            artifact_share: Some(share.to_owned()),
            ..GitSettings::default()
        };
        let fg = FleetGitStart {
            kind: StartKind::InPlace,
            project_id: "p1".to_owned(),
            project_name: "app".to_owned(),
            settings,
            settings_revision: 1,
            home_generation: 1,
            home_path: home.display().to_string(),
            remote: remote_identity("https://h/acme/app", LocalRemotes::Deny).unwrap(),
            slug: "alpha".to_owned(),
            forge: None,
            clone_url: "https://h/acme/app".to_owned(),
        };
        SessionCommand::Start {
            spec: spec(&home.display().to_string(), "claude"),
            prompt: Some(text("hello")),
            workspace: None,
            setup_cmd: cmd.map(str::to_owned),
            batch: None,
            issue: None,
            origin_text: false,
            fleet_git: Some(Box::new(fg)),
        }
    }

    /// A station with the fake provider, a setup environment and the rig on
    /// its store.
    async fn station(dir: &Path) -> (Station, Rig, std::path::PathBuf) {
        let st = Station::with_program(8, env_dump_program(dir)).await;
        let home = st.root.path().canonicalize().unwrap().join("projects/app");
        let policy = Policy::new(
            vec![home.parent().unwrap().to_path_buf()],
            8,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([(
                "claude".into(),
                fleetdeck_proto::event::ProviderKind::Claude,
            )]),
        )
        .with_name("Alpha")
        .with_allow_setup_cmd(true)
        .with_setup_envs(SetupEnvs::from([(
            "claude".into(),
            ChildEnv::from_process(),
        )]));
        st.manager.policy_hub().swap(policy);
        let rig = Rig::on(Det::new(), st.manager.store().clone());
        st.manager.set_fleet_git_runtime(rig.runtime.clone());
        (st, rig, home)
    }

    #[tokio::test]
    async fn a_start_gives_the_variable_to_the_setup_command_and_to_the_provider() {
        let dir = tempfile::tempdir().unwrap();
        let (mut st, rig, home) = station(dir.path()).await;
        rig.snapshot_at("box/out", &home).await;
        rig.refresh().await;
        let cmd = start_command(
            &home,
            "box/out",
            Some("printf '%s' \"$FLEETDECK_ARTIFACTS\" > art-out.txt"),
        );
        let d = st.accepted(cmd).await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted);
        let session = d.receipt.result.unwrap().session_id.unwrap();
        st.wait_for("session.started", |e| {
            matches!(e, Event::SessionStarted { .. }).then_some(())
        })
        .await;
        st.settled(&session).await;
        let store = st.manager.store();
        let summary = store.summary(session.clone()).await.unwrap().unwrap();
        let callsign = summary.callsign.clone().unwrap();
        let expected = format!("{}/app/{callsign}/", rig.share.display());
        // The setup command and the provider both see the variable.
        assert_eq!(
            std::fs::read_to_string(home.join("art-out.txt")).unwrap(),
            expected
        );
        let seen = std::fs::read_to_string(dir.path().join("provider-env.txt")).unwrap();
        let line = format!("FLEETDECK_ARTIFACTS={expected}");
        assert!(seen.lines().any(|l| l == line), "{seen}");
        assert_eq!(
            summary.artifacts_dir.unwrap().path.as_deref(),
            Some(expected.as_str())
        );
        let folder = rig.share.join("app").join(&callsign);
        let mode = std::fs::metadata(folder).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[tokio::test]
    async fn a_start_with_an_unavailable_share_still_starts_without_the_variable() {
        let dir = tempfile::tempdir().unwrap();
        let (mut st, rig, home) = station(dir.path()).await;
        rig.snapshot_at("box/unlisted", &home).await;
        rig.refresh().await;
        let cmd = start_command(&home, "box/unlisted", None);
        let d = st.accepted(cmd).await;
        let session = d.receipt.result.unwrap().session_id.unwrap();
        let code = st
            .wait_for("artifacts.unavailable", |e| match e {
                Event::ArtifactsUnavailable { code, .. } => Some(code.clone()),
                _ => None,
            })
            .await;
        assert_eq!(code, fleetdeck_proto::fleet_git::code::SHARE_UNVERIFIED);
        st.wait_for("session.started", |e| {
            matches!(e, Event::SessionStarted { .. }).then_some(())
        })
        .await;
        st.settled(&session).await;
        let seen = std::fs::read_to_string(dir.path().join("provider-env.txt")).unwrap();
        assert!(!seen.contains("FLEETDECK_ARTIFACTS"), "{seen}");
    }

    /// Stops `session` and waits until it has ended.
    async fn ended(st: &mut Station, session: &SessionId) {
        use fleetdeck_proto::command::StopMode;
        use fleetdeck_proto::event::SessionState;
        st.accepted(SessionCommand::Stop {
            session_id: session.clone(),
            how: StopMode::Graceful,
        })
        .await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while st.state(session).await != SessionState::Exited {
            assert!(tokio::time::Instant::now() < deadline, "not exited");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        st.settled(session).await;
    }

    fn ready_count(events: &[fleetdeck_proto::EventEnvelope]) -> usize {
        events
            .iter()
            .filter(|e| {
                matches!(
                    &e.event,
                    fleetdeck_proto::EventBody::Known(Event::ArtifactsReady { .. })
                )
            })
            .count()
    }

    #[tokio::test]
    async fn a_resume_sets_the_variable_again_and_without_the_share_it_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let (mut st, rig, home) = station(dir.path()).await;
        rig.snapshot_at("box/out", &home).await;
        rig.refresh().await;
        let cmd = start_command(&home, "box/out", None);
        let session = st
            .accepted(cmd)
            .await
            .receipt
            .result
            .unwrap()
            .session_id
            .unwrap();
        st.wait_for("session.started", |e| {
            matches!(e, Event::SessionStarted { .. }).then_some(())
        })
        .await;
        st.settled(&session).await;
        let first = std::fs::read_to_string(dir.path().join("provider-env.txt")).unwrap();
        assert!(first.contains("FLEETDECK_ARTIFACTS="), "{first}");
        ended(&mut st, &session).await;
        std::fs::remove_file(dir.path().join("provider-env.txt")).unwrap();
        // The resume asks for a new authorization and sets the variable again.
        st.accepted(SessionCommand::Resume {
            session_id: session.clone(),
        })
        .await;
        st.settled(&session).await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !dir.path().join("provider-env.txt").exists() {
            assert!(tokio::time::Instant::now() < deadline, "no resumed process");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let again = std::fs::read_to_string(dir.path().join("provider-env.txt")).unwrap();
        let var = first
            .lines()
            .find(|l| l.starts_with("FLEETDECK_ARTIFACTS="))
            .unwrap();
        assert!(again.lines().any(|l| l == var), "{again}");
        assert_eq!(ready_count(&st.events().await), 2);
        // The project no longer has the share: the next resume starts without it.
        ended(&mut st, &session).await;
        std::fs::remove_file(dir.path().join("provider-env.txt")).unwrap();
        rig.snapshot(None, None).await;
        st.accepted(SessionCommand::Resume {
            session_id: session.clone(),
        })
        .await;
        st.settled(&session).await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !dir.path().join("provider-env.txt").exists() {
            assert!(tokio::time::Instant::now() < deadline, "no resumed process");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let last = std::fs::read_to_string(dir.path().join("provider-env.txt")).unwrap();
        assert!(!last.contains("FLEETDECK_ARTIFACTS"), "{last}");
        let unavailable = st
            .events()
            .await
            .iter()
            .filter(|e| {
                matches!(
                    &e.event,
                    fleetdeck_proto::EventBody::Known(Event::ArtifactsUnavailable { .. })
                )
            })
            .count();
        assert_eq!(unavailable, 1);
    }
}
