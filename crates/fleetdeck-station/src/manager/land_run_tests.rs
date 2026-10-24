//! The land engine and steps 1 to 4 of design §11.5 (FG-8) with real Git and a
//! bare remote. The run is driven by hand (`land_run::run`), so each test
//! controls what happens between the steps; one test uses the poll task.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::command::{CommandOrigin, ReceiptState};
use fleetdeck_proto::fleet_git::{LandPhase, LandState};
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_proto::{Event, EventBody};
use tokio::sync::watch;

use super::LandEngine;
use super::rig::*;
use crate::manager::Policy;
use crate::manager::land_temp_cleanup;
use crate::store::ReadResult;
use crate::store::land_temps::LandTempState;

#[tokio::test]
async fn a_clean_run_goes_through_every_phase_to_checked_and_keeps_the_hold() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    let wt = rig.session("s1", false).await;
    rig.admit("s1", "land-0123456789ab").await;
    let remote_before = rig.refs();
    let tip = rig.git(&wt, &["rev-parse", "HEAD"]);
    rig.git(&rig.home, &["checkout", "-q", "main"]);
    let moved = {
        // The remote moves after the admission: the capture takes the new tip.
        let other = rig.root.join("other");
        rig.git(&rig.root, &["clone", "-q", &rig.url, "other"]);
        rig.commit(&other, "c.txt", "elsewhere");
        rig.git(&other, &["push", "-q", "origin", "main"]);
        rig.git(&other, &["rev-parse", "HEAD"])
    };
    let row = rig.run("land-0123456789ab").await;
    assert_eq!(
        (row.state, row.phase),
        (LandState::Running, LandPhase::Checked)
    );
    assert_eq!(row.source_oid.as_deref(), Some(tip.as_str()));
    assert_eq!(row.base_oid.as_deref(), Some(moved.as_str()));
    assert_eq!(row.default_ref.as_deref(), Some("main"));
    assert_eq!(row.branch_lease.as_deref(), Some(""));
    let rebased = row.rebased_oid.clone().unwrap();
    assert_ne!(rebased, tip);
    assert_eq!(
        rig.git(&rig.home, &["rev-parse", &format!("{rebased}^")]),
        moved
    );
    // Each phase change has its event, in order.
    let phases: Vec<LandPhase> = rig.phases().await.into_iter().map(|p| p.1).collect();
    assert_eq!(
        phases,
        vec![
            LandPhase::Admitted,
            LandPhase::Captured,
            LandPhase::WorktreeReady,
            LandPhase::Rebased,
            LandPhase::Checked
        ]
    );
    // The hold stays; nothing was pushed (the remote changed only by `other`).
    assert!(rig.store.session_hold("s1".into()).await.unwrap().is_some());
    assert_ne!(rig.refs(), remote_before);
    assert!(!rig.refs().contains("fd/alpha"));
    // The temporary folder is inside the allowed root, and stays at `checked`.
    let temp = rig
        .store
        .land_temp("land-0123456789ab".to_owned(), 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(temp.state, LandTempState::Present);
    assert!(Path::new(&temp.path).starts_with(rig.root.join(".fleetdeck-land")));
    assert_eq!(land_temp_cleanup::run_once(&rig.store).await.unwrap(), 0);
    assert!(Path::new(&temp.path).exists());
}

#[tokio::test]
async fn a_commit_added_after_the_capture_is_not_landed() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    let wt = rig.session("s1", false).await;
    rig.admit("s1", "land-1").await;
    let row = rig.run("land-1").await;
    let tip = rig.git(&wt, &["rev-parse", "HEAD"]);
    rig.commit(&wt, "later.txt", "later");
    let later = rig.git(&wt, &["rev-parse", "HEAD"]);
    assert_ne!(tip, later);
    assert_eq!(row.source_oid.as_deref(), Some(tip.as_str()));
    let rebased = row.rebased_oid.unwrap();
    let files = rig.git(&rig.home, &["ls-tree", "-r", "--name-only", &rebased]);
    assert!(
        files.contains("work.txt") && !files.contains("later.txt"),
        "{files}"
    );
}

#[tokio::test]
async fn nothing_to_land_ends_failed_and_frees_the_session() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    // A session whose branch has no commit of its own.
    let wt = rig.wt("s1");
    rig.git(
        &rig.home,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "fd/alpha/s1",
            wt.to_str().unwrap(),
        ],
    );
    rig.record("s1", false, Some(&wt)).await;
    rig.admit("s1", "land-1").await;
    let row = rig.run("land-1").await;
    assert_eq!((row.state, row.phase), (LandState::Failed, LandPhase::Done));
    assert_eq!(row.code.as_deref(), Some("nothing_to_land"));
    assert_eq!(
        row.message.as_deref(),
        Some("The branch has no commits that are not on origin/main.\nNothing was pushed.")
    );
    assert!(row.ended_at.is_some());
    assert!(rig.store.session_hold("s1".into()).await.unwrap().is_none());
    assert!(
        rig.store
            .land_temp("land-1".to_owned(), 1)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_conflict_ends_with_its_files_and_the_exact_base() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session_with("s1", false, "clash.txt").await;
    rig.admit("s1", "land-1").await;
    let other = rig.root.join("other");
    rig.git(&rig.root, &["clone", "-q", &rig.url, "other"]);
    rig.commit(&other, "clash.txt", "remote text");
    rig.git(&other, &["push", "-q", "origin", "main"]);
    let base = rig.git(&other, &["rev-parse", "HEAD"]);
    let remote_before = rig.refs();
    let row = rig.run("land-1").await;
    assert_eq!(
        (row.state, row.phase),
        (LandState::Conflict, LandPhase::Done)
    );
    assert_eq!(row.code, None);
    assert_eq!(row.files, vec!["clash.txt"]);
    assert_eq!(
        row.message.as_deref(),
        Some(
            format!(
                "Conflict in 1 files with origin/main ({}).\nNothing was pushed.",
                &base[..7]
            )
            .as_str()
        )
    );
    assert_eq!(rig.refs(), remote_before);
    assert!(rig.store.session_hold("s1".into()).await.unwrap().is_none());
    // The event carries the files.
    let ReadResult::Events { events, .. } = rig.store.read_after(0, 1000).await.unwrap() else {
        panic!("snapshot required");
    };
    let last = events.into_iter().rev().find_map(|e| match e.event {
        EventBody::Known(Event::SessionLand(info)) => Some(info),
        _ => None,
    });
    assert_eq!(last.unwrap().files, vec!["clash.txt"]);
}

#[tokio::test]
async fn git_without_an_identity_ends_land_no_identity() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    rig.admit("s1", "land-1").await;
    rig.script(false);
    let row = rig.run("land-1").await;
    assert_eq!(row.state, LandState::Failed);
    assert_eq!(row.code.as_deref(), Some("land_no_identity"));
    assert_eq!(
        row.message.as_deref(),
        Some(
            "Git has no user.name or user.email on this station. Set them with git config --global.\nNothing was pushed."
        )
    );
}

#[tokio::test]
async fn a_land_folder_that_is_not_private_ends_land_temp_unsafe() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    rig.admit("s1", "land-1").await;
    let dir = rig.root.join(".fleetdeck-land");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let row = rig.run("land-1").await;
    assert_eq!(row.code.as_deref(), Some("land_temp_unsafe"));
    assert!(row.message.unwrap().starts_with(&format!(
        "The folder {}/.fleetdeck-land is not a private folder of the station user.",
        rig.root.display()
    )));
}

#[tokio::test]
async fn a_failed_check_ends_with_the_last_fifty_redacted_lines() {
    let Some(rig) = Rig::new(true).await else {
        return;
    };
    rig.session("s1", false).await;
    let mut plan = rig.plan("s1");
    plan.check_cmd =
        Some("seq 1 80; echo 'token ghp_abcdefghijklmnopqrstuvwxyz0123456789'; exit 3".to_owned());
    rig.admit_plan("s1", "land-1", plan).await;
    let row = rig.run("land-1").await;
    assert_eq!(
        (row.state, row.phase),
        (LandState::CheckFailed, LandPhase::Done)
    );
    assert_eq!(row.code, None);
    let text = row.message.unwrap();
    assert!(
        text.starts_with("The check command failed (exit 3).\n"),
        "{text}"
    );
    assert!(text.ends_with("\nNothing was pushed."), "{text}");
    assert!(!text.contains("ghp_abcdefghijklmnopqrstuvwxyz"), "{text}");
    let body: Vec<&str> = text.lines().skip(1).collect();
    // 50 output lines and the publication sentence.
    assert_eq!(body.len(), 51, "{text}");
    assert_eq!(body[0], "32");
    assert!(!text.contains("\n31\n"));
}

#[tokio::test]
async fn a_check_ends_at_the_check_limit_of_the_policy() {
    let Some(rig) = Rig::new(true).await else {
        return;
    };
    // A land id of its own: the check marker kills every process that carries it.
    rig.session("s1", false).await;
    let mut plan = rig.plan("s1");
    plan.check_cmd = Some("sleep 60".to_owned());
    rig.admit_plan("s1", "land-limit", plan).await;
    let timeouts = crate::manager::policy_limits::StationTimeouts {
        land_check: Duration::from_secs(1),
        ..Default::default()
    };
    rig.hub
        .swap(rig.hub.current().as_ref().clone().with_timeouts(timeouts));
    let row = rig.run("land-limit").await;
    assert_eq!(row.state, LandState::CheckFailed, "{row:?}");
    let text = row.message.unwrap();
    assert!(
        text.starts_with("The check command timed out after 1 s.\n"),
        "{text}"
    );
}

#[tokio::test]
async fn a_check_that_edits_a_tracked_file_or_commits_is_check_mutated() {
    let Some(rig) = Rig::new(true).await else {
        return;
    };
    for (n, cmd) in [
        "echo more >> work.txt",
        "echo more >> work.txt; git -c user.name=t -c user.email=t@t commit -qam more",
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("s{n}");
        rig.session_with(&id, false, &format!("w{n}.txt")).await;
        let mut plan = rig.plan(&id);
        plan.check_cmd = Some(cmd.replace("work.txt", &format!("w{n}.txt")));
        rig.admit_plan(&id, &format!("land-{n}"), plan).await;
        let row = rig.run(&format!("land-{n}")).await;
        assert_eq!(row.state, LandState::CheckMutated, "{cmd}");
        assert_eq!(row.code.as_deref(), Some("check_mutated"));
        assert_eq!(
            row.message.as_deref(),
            Some(
                "The check command changed the tested commit or tracked files.\nNothing was pushed."
            )
        );
    }
    assert!(!rig.refs().contains("fd/alpha"));
}

#[tokio::test]
async fn a_good_check_runs_with_the_marker_and_the_environment_and_reaches_checked() {
    let Some(rig) = Rig::new(true).await else {
        return;
    };
    rig.session("s1", false).await;
    let out = rig.root.join("check.out");
    let mut plan = rig.plan("s1");
    plan.check_cmd = Some(format!(
        "echo \"$FLEETDECK_SETUP_ID $LAND_FLAG\" > {}; pwd >> {}",
        out.display(),
        out.display()
    ));
    plan.check_env = BTreeMap::from([("LAND_FLAG".to_owned(), "yes".to_owned())]);
    rig.admit_plan("s1", "land-1", plan).await;
    let row = rig.run("land-1").await;
    assert_eq!(row.phase, LandPhase::Checked);
    let text = fs::read_to_string(&out).unwrap();
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("land-1-1 yes"));
    let cwd = lines.next().unwrap();
    assert!(
        cwd.starts_with(&rig.root.join(".fleetdeck-land").display().to_string()),
        "{cwd}"
    );
    let phases: Vec<LandPhase> = rig.phases().await.into_iter().map(|p| p.1).collect();
    assert!(phases.contains(&LandPhase::Checking));
    assert!(rig.store.setup_runs().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_check_without_a_provider_environment_fails_with_internal() {
    let Some(rig) = Rig::new(true).await else {
        return;
    };
    rig.session("s1", false).await;
    let mut plan = rig.plan("s1");
    plan.check_cmd = Some("true".to_owned());
    rig.admit_plan("s1", "land-1", plan).await;
    rig.hub.swap(Policy::from_config(&config(&format!(
        "allowed_roots = [\"{}\"]\nallow_check_cmd = true\nallow_local_remotes = true\n",
        rig.root.display()
    ))));
    let row = rig.run("land-1").await;
    assert_eq!(row.code.as_deref(), Some("internal"));
    assert!(
        row.message
            .unwrap()
            .starts_with("The check command has no environment for this provider instance.")
    );
}

#[tokio::test]
async fn the_temporary_folder_and_its_admin_entry_are_deleted_for_a_failed_run() {
    let Some(rig) = Rig::new(true).await else {
        return;
    };
    let sibling = rig.session("s1", false).await;
    rig.session_with("s2", false, "two.txt").await;
    let mut plan = rig.plan("s2");
    plan.check_cmd = Some("exit 1".to_owned());
    rig.admit_plan("s2", "land-1", plan).await;
    rig.run("land-1").await;
    let temp = rig
        .store
        .land_temp("land-1".to_owned(), 1)
        .await
        .unwrap()
        .unwrap();
    let admin = temp.admin_dir.clone().unwrap();
    assert!(Path::new(&temp.path).exists() && Path::new(&admin).exists());
    assert_eq!(land_temp_cleanup::run_once(&rig.store).await.unwrap(), 1);
    assert!(!Path::new(&temp.path).exists());
    assert!(!Path::new(&admin).exists());
    let temp = rig
        .store
        .land_temp("land-1".to_owned(), 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(temp.state, LandTempState::Removed);
    // A sibling worktree of the home and its admin entry stay (no prune).
    assert!(sibling.exists());
    let list = rig.git(&rig.home, &["worktree", "list", "--porcelain"]);
    assert!(list.contains(&sibling.display().to_string()), "{list}");
    assert!(rig.home.join(".git/worktrees/work--s1").exists());
}

#[tokio::test]
async fn a_replaced_temporary_folder_is_kept() {
    let Some(rig) = Rig::new(true).await else {
        return;
    };
    rig.session("s1", false).await;
    let mut plan = rig.plan("s1");
    plan.check_cmd = Some("exit 1".to_owned());
    rig.admit_plan("s1", "land-1", plan).await;
    rig.run("land-1").await;
    let temp = rig
        .store
        .land_temp("land-1".to_owned(), 1)
        .await
        .unwrap()
        .unwrap();
    // Replace the folder: another inode at the same path.
    let keep = rig.root.join("moved-away");
    fs::rename(&temp.path, &keep).unwrap();
    fs::create_dir(&temp.path).unwrap();
    fs::write(Path::new(&temp.path).join("mine.txt"), "x").unwrap();
    land_temp_cleanup::run_once(&rig.store).await.unwrap();
    let temp = rig
        .store
        .land_temp("land-1".to_owned(), 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(temp.state, LandTempState::Kept);
    assert_eq!(temp.code.as_deref(), Some("temp_changed"));
    assert!(Path::new(&temp.path).join("mine.txt").exists());
    assert!(keep.exists());
    assert_eq!(rig.store.land_temps_kept().await.unwrap().len(), 1);
}

#[tokio::test]
async fn no_git_hook_runs_during_a_land() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    let marker = rig.root.join("hook-ran");
    let hooks = rig.home.join(".git/hooks");
    for name in [
        "pre-rebase",
        "post-checkout",
        "post-rewrite",
        "reference-transaction",
    ] {
        let hook = hooks.join(name);
        fs::write(
            &hook,
            format!("#!/bin/sh\necho {name} >> {}\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    rig.session("s1", false).await;
    rig.admit("s1", "land-1").await;
    // Take a baseline: the hooks ran for the session worktree of the rig.
    let _ = fs::remove_file(&marker);
    let row = rig.run("land-1").await;
    assert_eq!(row.phase, LandPhase::Checked);
    assert!(!marker.exists(), "{:?}", fs::read_to_string(&marker));
}

#[tokio::test]
async fn a_coordinator_land_without_a_live_setting_or_an_answer_ends_with_the_authority_code() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    for (id, live, code, text) in [
        (
            "s1",
            false,
            "authority_lost",
            "The coordinator lost its permission during the land (the coordinator's authority ended before this action ran).\nNothing was pushed.",
        ),
        (
            "s2",
            true,
            "authority_unknown",
            "Fleet Deck could not confirm the coordinator's permission, so the land stopped.\nNothing was pushed.",
        ),
    ] {
        rig.session_with(id, false, &format!("{id}.txt")).await;
        let authority = CommandAuthority {
            grant_id: "g1".to_owned(),
            caller_station: "other".into(),
            caller_session: "caller".into(),
            generation: 1,
        };
        rig.hub.swap(policy(&rig.root, false, true));
        let command = Rig::land_command(id, &format!("land-{id}"), rig.plan(id));
        let decided = rig
            .submit(
                &format!("cmd-{id}"),
                CommandOrigin::Coordinator,
                Some(authority),
                command,
            )
            .await;
        assert_eq!(decided.receipt.state, ReceiptState::Accepted);
        assert!(rig.row(&format!("land-{id}")).await.authority.is_some());
        rig.hub.swap(policy(&rig.root, false, live));
        let row = rig.run(&format!("land-{id}")).await;
        assert_eq!(row.state, LandState::Failed);
        assert_eq!(row.code.as_deref(), Some(code));
        assert_eq!(row.message.as_deref(), Some(text));
        // No fetch happened: the run never captured.
        assert_eq!(row.base_oid, None);
    }
}

#[tokio::test]
async fn the_poll_runs_an_admitted_land_and_recovers_the_runs_of_an_earlier_process() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("old", false).await;
    rig.session_with("new", false, "new.txt").await;
    rig.admit("old", "land-old").await;
    // The old run was admitted before this process started.
    rig.det.advance(Duration::from_secs(10));
    let (stop, rx) = watch::channel(false);
    let options = rig.options();
    let (engine, tasks): (LandEngine, _) = super::start(
        rig.store.clone(),
        Arc::clone(&rig.hub),
        Arc::new(move || options.clone()),
        rx,
        rig.deps(),
    );
    rig.admit("new", "land-new").await;
    engine.wake();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if rig.row("land-new").await.state == LandState::Landed {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the land did not run"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The run of the earlier process is recovered, not run: nothing external
    // ran in it (design §11.5, the reconciliation table).
    let old = rig.row("land-old").await;
    assert_eq!(old.state, LandState::Failed);
    assert_eq!(old.code.as_deref(), Some("station_restarted"));
    stop.send(true).unwrap();
    for task in tasks {
        let _ = task.await;
    }
}
