#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The land queue on the deck store (design §11.2, §11.12; FG-8): the
//! migration, the request, admission and the fold of events and statuses.

use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::fleet_git::{
    GitSettings, GitSettingsPut, LandMode, LandPhase, LandState, LandStatus, LandStatusAnswer,
};
use fleetdeck_proto::integrations::{ForgeBinding, ForgeKind};
use fleetdeck_proto::version::feature;
use std::time::Duration;

use super::super::land_world::*;
use super::super::schema::migrations;
use super::*;

// ----- the migration -----

#[test]
fn the_three_partial_unique_indexes_allow_one_running_land_for_each_project_and_lock_key() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    migrations().to_latest(&mut conn).unwrap();
    let insert = |id: &str, project: &str, lock: &str, session: &str, state: &str| {
        conn.execute(
            "INSERT INTO land_queue (land_id, command_id, requested_by, request_command_id,
                 project_id, lock_key, station_id, session_id, state, created_at)
             VALUES (?1, ?1, 'owner', ?1, ?2, ?3, 'st', ?4, ?5, 1)",
            rusqlite::params![id, project, lock, session, state],
        )
    };
    insert("l1", "p1", "r1", "s1", "running").unwrap();
    // One running land for a project, and for a lock key.
    assert!(insert("l2", "p1", "r2", "s2", "running").is_err());
    assert!(insert("l3", "p2", "r1", "s3", "running").is_err());
    // A queued land of the same project, a land of another project and
    // lock key, and an ended land of the same session are all fine.
    insert("l4", "p1", "r1", "s4", "queued").unwrap();
    insert("l5", "p2", "r2", "s5", "running").unwrap();
    insert("l6", "p9", "r9", "s1", "landed").unwrap();
    // One open land for a session.
    assert!(insert("l7", "p3", "r3", "s4", "queued").is_err());
}

// ----- the request -----

#[tokio::test]
async fn a_request_queues_a_land_and_the_same_command_id_gives_the_first_answer() {
    let w = world(&["/work/app"]).await;
    w.show(vec![
        session("s1", "/work/app", SessionState::Exited),
        session("s2", "/work/app", SessionState::Exited),
    ])
    .await;
    let first = w.queued("c1", "s1").await;
    assert_eq!(w.queued("c1", "s1").await, first);
    // The same command id for another session is not the same request.
    assert_eq!(refused(w.ask("c1", "s2").await).0, "command_id_reused");
    // The session has an open land.
    assert_eq!(refused(w.ask("c2", "s1").await).0, "land_open");
    // Another session queues behind it: position 1.
    let LandRequested::Queued { position, .. } = w.ask("c3", "s2").await else {
        panic!("not queued");
    };
    assert_eq!(position, 1);
    let row = w.row(&first).await;
    assert_eq!(
        (row.state.as_str(), row.requested_by.as_str()),
        ("queued", "owner")
    );
    assert!(row.land_id.starts_with("land-") && row.command_id.starts_with("landcmd-"));
    assert_eq!(row.lock_key, "github.com/acme/app");
}

#[tokio::test]
async fn a_request_that_breaks_a_rule_of_the_design_is_refused_with_its_text() {
    let w = world(&["/work/app"]).await;
    let mut working = session("w", "/work/app", SessionState::Running);
    working.current_run = Some("r1".into());
    let mut bare = session("b", "/work/app", SessionState::Exited);
    bare.worktree = None;
    let mut idle = session("i", "/work/app", SessionState::Running);
    idle.current_run = None;
    let elsewhere = session("e", "/other", SessionState::Exited);
    w.show(vec![working, bare, idle, elsewhere]).await;
    let (code, text) = refused(w.ask("c", "nope").await);
    assert_eq!(
        (code, text.as_str()),
        ("unknown_session", "The station does not know this session.")
    );
    let (code, text) = refused(w.ask("c", "e").await);
    assert_eq!(
        (code, text.as_str()),
        ("land_not_ready", "This session is not in a project.")
    );
    let (code, text) = refused(w.ask("c", "w").await);
    assert_eq!(
        (code, text.as_str()),
        (
            "land_not_ready",
            "The session is working. Land it when it is idle or ended."
        )
    );
    let (code, text) = refused(w.ask("c", "b").await);
    assert_eq!(
        (code, text.as_str()),
        ("land_not_ready", "This session has no Fleet Git branch.")
    );
    // An idle session passes.
    w.queued("c", "i").await;
    // A station without the feature.
    w.s.clear_link_features(&w.st, 1);
    w.s.set_link_features(&w.st, 2, vec!["control".to_owned()]);
    w.show(vec![session("x", "/work/app", SessionState::Exited)])
        .await;
    let (code, text) = refused(w.ask("c3", "x").await);
    assert_eq!(
        (code, text.as_str()),
        (
            "fleet_git_unsupported",
            "This station does not support Fleet Git. Update it first."
        )
    );
}

#[tokio::test]
async fn a_pull_request_land_needs_the_branch_on_the_forge() {
    let w = world(&["/work/app"]).await;
    w.show(vec![session("s1", "/work/app", SessionState::Exited)])
        .await;
    let (id, _) = w.projects[0].clone();
    let p = w.s.project(id.clone()).await.unwrap().unwrap();
    // A WIP remote on another repository, with the default mode `pr`.
    let put = GitSettingsPut {
        settings: GitSettings {
            sync: true,
            wip_remote: Some("https://github.com/acme/wip.git".to_owned()),
            ..p.git.clone()
        },
        revision: p.git_revision,
        remote_url: None,
    };
    w.s.set_project_git(id, put).await.unwrap();
    let (code, text) = refused(w.ask("c", "s1").await);
    assert_eq!(code, "land_pr_needs_forge_branch");
    assert_eq!(
        text,
        "Pull request mode needs the branch on github.com/acme/app, but pushes go to github.com/acme/wip. Choose Merge in the project settings, or clear the WIP remote."
    );
}

#[tokio::test]
async fn a_pull_request_land_is_admitted_for_a_mixed_case_forge() {
    let w = world(&["/work/app"]).await;
    w.show(vec![session("s1", "/work/app", SessionState::Exited)])
        .await;
    let (id, folder) = w.projects[0].clone();
    // The forge keeps its case; the remote is the scp-like URL of the same
    // repository.
    let mut forge = input("App0", &folder);
    forge.forge = Some(ForgeBinding {
        kind: ForgeKind::Github,
        host: "github.com".into(),
        repo: "Acme/App".into(),
    });
    w.s.update_project(id.clone(), w.st.0.clone(), forge)
        .await
        .unwrap()
        .unwrap();
    w.s.db()
        .call(move |conn| {
            conn.execute(
                "UPDATE projects SET remote_url = 'github.com:acme/app.git',
                     remote_ssh_user = 'git' WHERE project_id = ?1",
                [id],
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    let a = w.queued("c1", "s1").await;
    w.admit().await;
    let row = w.row(&a).await;
    assert_eq!(row.state, "running");
    let plan = row.plan.unwrap();
    assert_eq!(plan.mode, LandMode::Pr);
    assert_eq!(plan.remote.repo, "github.com/acme/app");
}

// ----- admission -----

#[tokio::test]
async fn admission_runs_one_land_for_each_project_and_pins_the_plan_and_the_outbox() {
    let w = world(&["/work/app"]).await;
    w.show(vec![
        session("s1", "/work/app", SessionState::Exited),
        session("s2", "/work/app", SessionState::Exited),
    ])
    .await;
    let a = w.queued("c1", "s1").await;
    w.det.advance(Duration::from_millis(5));
    let b = w.queued("c2", "s2").await;
    w.admit().await;
    let (ra, rb) = (w.row(&a).await, w.row(&b).await);
    assert_eq!(
        (ra.state.as_str(), rb.state.as_str()),
        ("running", "queued")
    );
    assert_eq!(ra.attempt, 1);
    let plan = ra.plan.unwrap();
    assert_eq!(plan.branch_ref, "fd/alpha/s1");
    assert_eq!(plan.publish_ref, "fd/alpha/s1");
    assert_eq!(plan.fetch_remote, "origin");
    assert_eq!(plan.remote.repo, "github.com/acme/app");
    assert_eq!(plan.pr_title, "Work of s1");
    assert_eq!(plan.pr_body, "Landed by Fleet Deck from session s1.");
    // The outbox row waits with the command fixed at admission.
    let due = w.s.land_outbox_due().await.unwrap();
    assert_eq!(due, vec![ra.command_id.clone()]);
    // The first land ends: the second is admitted.
    w.event(1, "s1", info(&a, LandState::Landed, LandPhase::Done))
        .await;
    assert_eq!(w.row(&a).await.state, "landed");
    w.admit().await;
    assert_eq!(w.row(&b).await.state, "running");
}

#[tokio::test]
async fn two_projects_with_one_repo_run_one_land_at_a_time() {
    let w = world(&["/work/app", "/work/app2"]).await;
    w.show(vec![
        session("s1", "/work/app", SessionState::Exited),
        session("s2", "/work/app2", SessionState::Exited),
    ])
    .await;
    let a = w.queued("c1", "s1").await;
    w.det.advance(Duration::from_millis(5));
    let b = w.queued("c2", "s2").await;
    assert_ne!(w.row(&a).await.project_id, w.row(&b).await.project_id);
    w.admit().await;
    assert_eq!(
        (
            w.row(&a).await.state.as_str(),
            w.row(&b).await.state.as_str()
        ),
        ("running", "queued")
    );
}

#[tokio::test]
async fn a_queued_land_waits_without_a_link_and_fails_when_the_request_no_longer_passes() {
    let w = world(&["/work/app"]).await;
    w.show(vec![
        session("s1", "/work/app", SessionState::Exited),
        session("s2", "/work/app", SessionState::Exited),
    ])
    .await;
    let a = w.queued("c1", "s1").await;
    w.s.clear_link_features(&w.st, 1);
    w.admit().await;
    assert_eq!(w.row(&a).await.state, "queued");
    w.s.set_link_features(&w.st, 3, vec![feature::FLEET_GIT_LAND.to_owned()]);
    // The session starts a turn meanwhile.
    let mut busy = session("s1", "/work/app", SessionState::Running);
    busy.current_turn = Some("t".into());
    w.show(vec![busy, session("s2", "/work/app", SessionState::Exited)])
        .await;
    w.admit().await;
    let row = w.row(&a).await;
    assert_eq!(
        (row.state.as_str(), row.code.as_deref()),
        ("failed", Some("land_not_ready"))
    );
    assert_eq!(
        row.message.as_deref(),
        Some("The session is working. Land it when it is idle or ended.")
    );
    assert!(row.ended_at.is_some());
}

// ----- the fold -----

#[tokio::test]
async fn events_fold_into_a_running_row_and_an_unknown_state_keeps_it_running() {
    let w = world(&["/work/app"]).await;
    w.show(vec![session("s1", "/work/app", SessionState::Exited)])
        .await;
    let a = w.queued("c1", "s1").await;
    w.admit().await;
    let mut step = info(&a, LandState::Running, LandPhase::Captured);
    step.base_oid = Some("b".repeat(40));
    w.event(1, "s1", step).await;
    let row = w.row(&a).await;
    assert_eq!(
        (
            row.state.as_str(),
            row.phase.as_deref(),
            row.base_oid.as_deref()
        ),
        ("running", Some("captured"), Some("b".repeat(40).as_str()))
    );
    // The event proves that the station took the command.
    assert!(w.s.land_outbox_due().await.unwrap().is_empty());
    // A state of a newer station frees nothing.
    w.event(2, "s1", info(&a, LandState::Unknown, LandPhase::Unknown))
        .await;
    assert_eq!(w.row(&a).await.state, "running");
    // `reconciling` keeps the row running.
    let mut rec = info(&a, LandState::Running, LandPhase::BranchPushIntent);
    rec.reconciling = true;
    w.event(3, "s1", rec).await;
    assert!(w.row(&a).await.reconciling);
    // A conflict ends it with its files.
    let mut end = info(&a, LandState::Conflict, LandPhase::Rebased);
    end.files = vec!["a.rs".to_owned()];
    end.code = Some("conflict".to_owned());
    end.text = Some("Conflict in 1 files.".to_owned());
    w.event(4, "s1", end).await;
    let row = w.row(&a).await;
    assert_eq!(
        (
            row.state.as_str(),
            row.files.clone(),
            row.message.as_deref()
        ),
        (
            "conflict",
            vec!["a.rs".to_owned()],
            Some("Conflict in 1 files.")
        )
    );
    // A late event of an ended land changes nothing.
    w.event(5, "s1", info(&a, LandState::Landed, LandPhase::Done))
        .await;
    assert_eq!(w.row(&a).await.state, "conflict");
}

#[tokio::test]
async fn a_status_answer_applies_like_an_event_and_the_two_markers_follow_the_design() {
    let w = world(&["/work/app"]).await;
    w.show(vec![
        session("s1", "/work/app", SessionState::Exited),
        session("s2", "/work/app", SessionState::Exited),
    ])
    .await;
    let a = w.queued("c1", "s1").await;
    w.admit().await;
    // Unknown with a pending outbox: nothing changes.
    assert!(
        !w.s.land_apply_status(a.clone(), LandStatusAnswer::Unknown)
            .await
            .unwrap()
    );
    assert_eq!(w.row(&a).await.phase, None);
    // A known status ends the row and accepts the outbox.
    let status = LandStatus {
        state: LandState::Landed,
        phase: LandPhase::Done,
        reconciling: false,
        attempt: 1,
        source_oid: None,
        base_oid: None,
        rebased_oid: None,
        branch_published_oid: None,
        default_published_oid: Some("d".repeat(40)),
        pr_url: None,
        files: Vec::new(),
        code: None,
        text: None,
    };
    w.s.land_apply_status(a.clone(), LandStatusAnswer::Known(Box::new(status)))
        .await
        .unwrap();
    assert_eq!(w.row(&a).await.state, "landed");
    // Unknown with an accepted outbox: the station lost its record.
    w.admit().await;
    let b = w.queued("c2", "s2").await;
    w.admit().await;
    w.event(1, "s2", info(&b, LandState::Running, LandPhase::Admitted))
        .await;
    w.s.land_apply_status(b.clone(), LandStatusAnswer::Unknown)
        .await
        .unwrap();
    let row = w.row(&b).await;
    assert_eq!(
        (row.state.as_str(), row.phase.as_deref()),
        ("running", Some("lost"))
    );
    // The fence marker ends the row `unresolved`.
    w.s.land_apply_status(b.clone(), LandStatusAnswer::Fenced)
        .await
        .unwrap();
    let row = w.row(&b).await;
    assert_eq!(
        (
            row.state.as_str(),
            row.code.as_deref(),
            row.abandon_write.as_deref()
        ),
        ("unresolved", Some("abandoned"), Some("unknown"))
    );
}
