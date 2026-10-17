//! The land decision (FG-8, design §11.1, §11.3, §11.4) with real Git and a
//! bare remote on the same machine.

use std::collections::BTreeMap;
use std::sync::Arc;

use fleetdeck_proto::command::{CommandEnvelope, CommandOrigin, RETRY_WINDOW_MS, SessionCommand};
use fleetdeck_proto::fleet_git::{
    CleanupState, LandMode, LandPhase, LandPlan, LandState, PushTarget, RemoteIdentity,
};
use fleetdeck_proto::integrations::{ForgeBinding, ForgeKind};
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_proto::{CommandId, RejectCode};

use super::rig::*;
use crate::coordinator::Bindings;
use crate::manager::fleet_git_cleanup::{CleanupCtx, run_once as cleanup_once};
use crate::shell::{ShellConfig, Shells};
use crate::store::Caller;
use crate::store::session_holds::{HELD, KIND_LAND};

#[tokio::test]
async fn an_ended_session_is_admitted_with_a_run_a_hold_an_effect_and_an_event() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    let decided = rig.admit("s1", "land-1").await;
    assert_eq!(decided.effects.len(), 1);
    assert_eq!(decided.effects[0].payload.kind(), "land");
    let row = rig.row("land-1").await;
    assert_eq!(
        (row.state, row.phase, row.attempt),
        (LandState::Running, LandPhase::Admitted, 1)
    );
    assert_eq!(row.command_id, "cmd-land-1");
    let hold = rig.store.session_hold("s1".into()).await.unwrap().unwrap();
    assert_eq!(
        (hold.kind.as_str(), hold.holder_id.as_str()),
        (KIND_LAND, "land-1")
    );
    assert_eq!(
        rig.phases().await,
        vec![(LandState::Running, LandPhase::Admitted)]
    );
}

#[tokio::test]
async fn the_decision_refuses_what_the_design_names() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    rig.session("s2", true).await;
    rig.record("plain", false, None).await;
    let plan = |id: &str| rig.plan(id);
    let invalid = |t: &str| (RejectCode::InvalidCommand, format!("invalid_params: {t}"));
    let state = |c: &str, t: &str| (RejectCode::InvalidState, format!("{c}: {t}"));

    // The land id.
    assert_eq!(
        refused(&rig, "s1", "bad id", plan("s1")).await,
        invalid("The land id is not valid.")
    );
    // A busy session.
    assert_eq!(
        refused(&rig, "s2", "land-b", plan("s2")).await,
        state(
            "land_not_ready",
            "The session is busy. Wait until it is idle or ended."
        )
    );
    // A session without a Fleet Git branch.
    assert_eq!(
        refused(&rig, "plain", "land-p", plan("plain")).await,
        state(
            "land_not_ready",
            "This session has no Fleet Git branch to land."
        )
    );
    // The plan against the session.
    let mut other = plan("s1");
    other.branch_ref = "fd/alpha/other".to_owned();
    assert_eq!(
        refused(&rig, "s1", "land-m", other).await,
        invalid("The land plan does not match this session.")
    );
    let mut stale = plan("s1");
    stale.home_generation = 2;
    assert_eq!(
        refused(&rig, "s1", "land-g", stale).await,
        state("stale_generation", "The project home changed. Land again.")
    );
    let mut unknown = plan("s1");
    unknown.mode = LandMode::Unknown;
    assert_eq!(
        refused(&rig, "s1", "land-u", unknown).await,
        state(
            "unsupported_mode",
            "This station does not know the land mode. Update the station."
        )
    );
    let mut pr = plan("s1");
    pr.mode = LandMode::Pr;
    assert_eq!(
        refused(&rig, "s1", "land-pr", pr.clone()).await,
        invalid("A pull request land needs a forge.")
    );
    pr.forge = Some(ForgeBinding {
        kind: ForgeKind::Github,
        host: "github.com".to_owned(),
        repo: "acme/app".to_owned(),
    });
    let (code, text) = refused(&rig, "s1", "land-pf", pr).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(text.starts_with("land_pr_needs_forge_branch: Pull request mode needs the branch on github.com/acme/app, but pushes go to "), "{text}");
    assert!(
        text.ends_with("Choose Merge in the project settings, or clear the WIP remote."),
        "{text}"
    );
    let mut dest = plan("s1");
    dest.branch_target.dest = "https://h:443/acme/else".to_owned();
    assert_eq!(
        refused(&rig, "s1", "land-d", dest).await,
        state(
            "remote_mismatch",
            "The push destination changed since the land was requested."
        )
    );
    // A check command on a station that does not run them.
    let mut check = plan("s1");
    check.check_cmd = Some("true".to_owned());
    assert_eq!(
        refused(&rig, "s1", "land-c", check).await,
        state(
            "check_disabled",
            "This station does not run check commands (allow_check_cmd = false)."
        )
    );
    // The origin.
    let command = Rig::land_command("s1", "land-o", plan("s1"));
    let decided = rig
        .submit("cmd-o", CommandOrigin::Local, None, command)
        .await;
    assert_eq!(rejected(&decided).0, RejectCode::Forbidden);
    // None of the refusals stored a run or a hold.
    assert!(rig.store.land_runs_running().await.unwrap().is_empty());
    assert!(rig.store.session_hold("s1".into()).await.unwrap().is_none());
}

#[tokio::test]
async fn a_second_land_a_fenced_id_and_a_reused_id_are_refused() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    rig.session("s2", false).await;
    rig.admit("s1", "land-1").await;
    // The session is held.
    assert_eq!(
        refused(&rig, "s1", "land-2", rig.plan("s1")).await,
        (RejectCode::InvalidState, HELD.to_owned())
    );
    // The land id is used already (another command id).
    assert_eq!(
        refused(&rig, "s2", "land-1", rig.plan("s2")).await,
        (
            RejectCode::InvalidCommand,
            "invalid_params: This land id exists already.".to_owned()
        )
    );
    // A fenced id is refused before it is a run.
    fence(&rig, "land-f").await;
    assert_eq!(
        refused(&rig, "s2", "land-f", rig.plan("s2")).await,
        (
            RejectCode::InvalidState,
            "land_fenced: This land was stopped before; it does not run again.".to_owned()
        )
    );
}

#[tokio::test]
async fn a_coordinator_land_needs_the_live_fleet_control_setting() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    let authority = CommandAuthority {
        grant_id: "g1".to_owned(),
        caller_station: "other".into(),
        caller_session: "caller".into(),
        generation: 1,
    };
    rig.hub.swap(policy(&rig.root, false, false));
    let command = Rig::land_command("s1", "land-1", rig.plan("s1"));
    let decided = rig
        .submit(
            "cmd-1",
            CommandOrigin::Coordinator,
            Some(authority),
            command,
        )
        .await;
    assert_eq!(rejected(&decided).0, RejectCode::Forbidden);
}

#[tokio::test]
async fn a_direct_decision_refuses_a_land() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    let command = Rig::land_command("s1", "land-1", rig.plan("s1"));
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from("c".to_owned()),
        target_incarnation: None,
        retry_until: rig.store.now_ms() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command,
    };
    let policy = rig.hub.current();
    let decided = rig
        .store
        .decide(envelope, Caller::local(), move |d| {
            crate::manager::decide::decide(
                d,
                &policy,
                &Bindings::default(),
                CommandOrigin::Board,
                false,
                Rig::land_command("s1", "land-1", rig_plan_stub()),
            )
        })
        .await
        .unwrap();
    assert_eq!(rejected(&decided).0, RejectCode::Unsupported);
}

fn rig_plan_stub() -> LandPlan {
    LandPlan {
        project_id: "p1".to_owned(),
        settings_revision: 1,
        home_generation: 1,
        mode: LandMode::Merge,
        fetch_remote: "origin".to_owned(),
        remote: RemoteIdentity {
            repo: "h/x".to_owned(),
            dest: "https://h:443/x".to_owned(),
        },
        default_branch: None,
        branch_ref: "b".to_owned(),
        publish_ref: "b".to_owned(),
        branch_target: PushTarget {
            target: "origin".to_owned(),
            dest: "https://h:443/x".to_owned(),
        },
        forge: None,
        check_cmd: None,
        check_env: BTreeMap::new(),
        pr_title: String::new(),
        pr_body: String::new(),
    }
}

#[tokio::test]
async fn the_cleanup_loop_keeps_a_held_session_with_the_reason_held() {
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    rig.session("s1", false).await;
    rig.store
        .session_hold_insert("s1".into(), KIND_LAND, "land-1".to_owned())
        .await
        .unwrap();
    let options = rig.options();
    let ctx = CleanupCtx {
        store: rig.store.clone(),
        policy: Arc::clone(&rig.hub),
        shells: Arc::new(Shells::new(ShellConfig::default())),
        options: Arc::new(move || options.clone()),
    };
    let ended = rig
        .store
        .git_cleanup_rows()
        .await
        .unwrap()
        .iter()
        .filter_map(|r| r.ended_at)
        .min()
        .unwrap();
    let outcomes = cleanup_once(&ctx, ended + 25 * 3_600_000).await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].state, CleanupState::Kept);
    assert_eq!(outcomes[0].reason.as_deref(), Some("held"));
}

#[tokio::test]
async fn the_session_manager_routes_a_land_to_the_land_decision() {
    let st = crate::manager::tests::Station::new(4).await;
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from("wired".to_owned()),
        target_incarnation: None,
        retry_until: st.manager.store().now_ms() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Land {
            session_id: "nobody".into(),
            land_id: "land-1".to_owned(),
            plan: Box::new(rig_plan_stub()),
        },
    };
    let decided = st.manager.submit(envelope, Caller::local()).await.unwrap();
    // The land decision ran (a direct `decide` would say `unsupported`).
    assert_eq!(rejected(&decided).0, RejectCode::UnknownSession);
    st.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}
