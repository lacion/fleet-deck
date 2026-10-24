//! PRE-PR round 2 of the session move (FG-9 property 1): at no step do the
//! source and the target both accept a prompt. A move excludes the import
//! and the resume of its native conversation, and a source needs proof that
//! its previous execution ended.

use std::path::PathBuf;

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::host::shim::wire::ProviderIdentity;

use super::{Fx, NATIVE, finish, out, state, undo};
use crate::store::resumes::Purpose;
use crate::store::session_holds::KIND_MOVE_IN;
use crate::store::{NewShim, RunningFacts, ShimState};

const WHY_LOST: &str = "Another execution of this conversation may still run \
                        (shim sh1 is lost (execution_unknown)).";

/// Why an import of `NATIVE` is blocked now (`None`: it is free).
async fn import_blocked(fx: &Fx) -> Option<String> {
    fx.rig
        .store
        .execution_exclusion(
            None,
            ProviderKind::Claude,
            Some(NATIVE.to_owned()),
            None,
            Purpose::Import,
        )
        .await
        .unwrap()
        .blocked
}

/// A shim row of `session` that runs now (state `running`).
async fn running_row(fx: &Fx, session: &str, shim_id: &str) {
    let store = &fx.rig.store;
    store
        .shim_starting(NewShim {
            shim_id: shim_id.to_owned(),
            session_id: session.into(),
            socket_path: PathBuf::from(format!("/run/x/{shim_id}.sock")),
            launch_boot_id: Some("boot-1".to_owned()),
            native_target: None,
        })
        .await
        .unwrap();
    store
        .shim_running_with(
            shim_id.to_owned(),
            ProviderIdentity {
                pid: 10,
                start_time: 5,
                boot_id: "boot-1".to_owned(),
            },
            RunningFacts::default(),
        )
        .await
        .unwrap();
}

/// A shim row of `session` that was lost and has no proof of its end.
async fn lost_row(fx: &Fx, session: &str, shim_id: &str) {
    running_row(fx, session, shim_id).await;
    fx.rig
        .store
        .shim_end(
            shim_id.to_owned(),
            ShimState::Lost,
            "execution_unknown".to_owned(),
            0,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn an_import_of_the_source_conversation_is_refused_during_and_after_a_move_until_the_undo() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("s1", true).await;
    fx.end("s1").await;
    // Before the move the ended source is free.
    assert_eq!(import_blocked(&fx).await, None);
    // The move holds the source.
    fx.accepted("c1", out("s1", "m1")).await;
    assert_eq!(
        import_blocked(&fx).await.as_deref(),
        Some("session `s1` is being moved to another station")
    );
    // The move is done: the source has `moved_to` and no hold.
    fx.accepted("c2", finish("s1", "m1")).await;
    assert!(fx.summary("s1").await.moved_to.is_some());
    assert!(
        fx.rig
            .store
            .session_hold("s1".into())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        import_blocked(&fx).await.as_deref(),
        Some("session `s1` moved this conversation to another station")
    );
    // After the undo nothing excludes the conversation any more.
    fx.accepted("c3", undo("s1", "m1")).await;
    assert!(fx.summary("s1").await.moved_to.is_none());
    assert_eq!(import_blocked(&fx).await, None);
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn an_import_of_the_conversation_is_refused_on_the_target_while_it_holds_move_in() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("t1", true).await;
    fx.end("t1").await;
    assert_eq!(import_blocked(&fx).await, None);
    fx.rig
        .store
        .session_hold_insert("t1".into(), KIND_MOVE_IN, "m1".to_owned())
        .await
        .unwrap();
    assert_eq!(
        import_blocked(&fx).await.as_deref(),
        Some("session `t1` is still arriving from another station")
    );
    // The same rule blocks the resume of another session with that native id.
    let resume = fx
        .rig
        .store
        .execution_exclusion(
            None,
            ProviderKind::Claude,
            Some(NATIVE.to_owned()),
            Some("other".into()),
            Purpose::Resume,
        )
        .await
        .unwrap();
    assert_eq!(
        resume.blocked.as_deref(),
        Some("session `t1` is still arriving from another station")
    );
    assert!(
        fx.rig
            .store
            .session_hold_delete("t1".into(), "m1".to_owned())
            .await
            .unwrap()
    );
    assert_eq!(import_blocked(&fx).await, None);
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn a_source_with_an_unproven_shim_row_is_not_moved() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("s1", true).await;
    fx.end("s1").await;
    lost_row(&fx, "s1", "sh1").await;
    assert_eq!(
        fx.refused("c1", out("s1", "m1")).await,
        state(&format!("move_not_ready: {WHY_LOST}"))
    );
    assert!(
        fx.rig
            .store
            .session_hold("s1".into())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fx.rig
            .store
            .move_out_run("m1".into())
            .await
            .unwrap()
            .is_none()
    );
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn an_unproven_row_after_the_decision_makes_the_export_plan_refuse() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("s1", true).await;
    fx.end("s1").await;
    fx.accepted("c1", out("s1", "m1")).await;
    // The row appears after the decision (a shim recovery of the source).
    lost_row(&fx, "s1", "sh1").await;
    let err = fx.manager.move_export_plan("m1").await.unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        ("invalid_state", WHY_LOST)
    );
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

// PRE-PR round 3: the running row of a live idle source is the run that the
// move stops, so it does not refuse the move; an earlier unproven row does.
#[tokio::test]
async fn a_live_idle_source_with_its_running_shim_row_is_moved() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("s1", true).await;
    running_row(&fx, "s1", "sh2").await;
    fx.accepted("c1", out("s1", "m1")).await;
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn an_earlier_unproven_row_still_refuses_a_live_source() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("s1", true).await;
    lost_row(&fx, "s1", "sh1").await;
    running_row(&fx, "s1", "sh2").await;
    assert_eq!(
        fx.refused("c1", out("s1", "m1")).await,
        state(&format!("move_not_ready: {WHY_LOST}"))
    );
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}
