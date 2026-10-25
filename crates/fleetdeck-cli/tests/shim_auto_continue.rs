//! Auto-continue on the shim host (DL-188 step 3, design `shim-parity.md`
//! §2 G4): a turn that a loss interrupted under a shim is continued when the
//! station holds the DL-182 proof that the run ended, and waits for the
//! owner without it. Real shims and the fake agents.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use fleetdeck_proto::Event;
use fleetdeck_proto::command::StopMode;
use fleetdeck_proto::event::{RunOrigin, SessionState};
use fleetdeck_station::store::{EffectPayload, ShimState};

mod shim_resume_rig;
use shim_resume_rig::*;

/// A reboot proves the run ended: the lost turn is continued under a new
/// shim with the continuation input.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_lost_before_a_reboot_is_continued_under_a_new_shim() {
    let mut rig = Rig::new();
    let (session, shim_id) = lost_under_a_dead_turn(&mut rig, true, "wait for it", true).await;
    // The test clock is manual: the intent must be newer than the start.
    rig.det.advance(std::time::Duration::from_secs(1));
    let st = rig.station().await;
    let native = starts(&st.store, &session).await[0].1.clone();
    wait_for(&st.store, &session, "a continuation run", |e| {
        e.iter().any(|e| {
            matches!(
                e,
                Event::RunStarted {
                    origin: RunOrigin::Continuation,
                    ..
                }
            )
        })
    })
    .await;
    wait_turns(&st.store, &session, 1).await;
    let after = starts(&st.store, &session).await;
    assert_eq!(after.len(), 2, "{after:?}");
    assert_eq!(
        (after[1].0, after[1].1.as_str()),
        (Some(true), native.as_str()),
        "the board shows `resumed`"
    );
    let events = events(&st.store, &session).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::AutoContinueSkipped { .. })),
        "{events:?}"
    );
    let rows = st.store.shim_rows(session.clone()).await.unwrap();
    assert_eq!(rows.len(), 2, "a new shim");
    assert_eq!(rows[0].shim_id, shim_id);
    assert_eq!(rows[0].state, ShimState::Lost, "the old row stays");
    assert_eq!(rows[0].proof.as_deref(), Some("ended_by_reboot"));
    assert_eq!(rows[1].state, ShimState::Running);
    let effects = st.store.session_effects(session.clone()).await.unwrap();
    let intents = effects
        .iter()
        .filter(|e| {
            matches!(
                e.payload,
                EffectPayload::Resume {
                    continue_by: Some(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(intents, 1, "{effects:?}");
    st.manager.shutdown(StopMode::Graceful).await;
    drop(st);
}

/// On the same boot nothing proves that the run ended: the session stays
/// `lost` and the station says why.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_lost_on_the_same_boot_waits_for_the_owner() {
    let mut rig = Rig::new();
    let (session, shim_id) = lost_under_a_dead_turn(&mut rig, false, "wait for it", true).await;
    let st = rig.station().await;
    let all = events(&st.store, &session).await;
    let skips: Vec<_> = all
        .iter()
        .filter_map(|e| match e {
            Event::AutoContinueSkipped { reason } => Some(reason.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(skips, vec!["execution_unknown"], "{all:?}");
    assert_eq!(state(&st.store, &session).await, SessionState::Lost);
    assert_eq!(starts(&st.store, &session).await.len(), 1);
    assert!(!all.iter().any(|e| matches!(
        e,
        Event::RunStarted {
            origin: RunOrigin::Continuation,
            ..
        }
    )));
    let row = st.store.shim(shim_id).await.unwrap().unwrap();
    assert_eq!(row.proof, None);
    assert_eq!(st.store.shim_rows(session.clone()).await.unwrap().len(), 1);
    let effects = st.store.session_effects(session).await.unwrap();
    assert!(
        effects
            .iter()
            .all(|e| !matches!(e.payload, EffectPayload::Resume { .. })),
        "{effects:?}"
    );
    st.manager.shutdown(StopMode::Graceful).await;
    drop(st);
}
