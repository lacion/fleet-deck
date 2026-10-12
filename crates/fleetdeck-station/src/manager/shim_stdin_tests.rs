//! A partial stdin write blocks the session's input in the decision
//! transaction; interrupt and stop pass (ADR-005 §4.3 test 30, station
//! half; slice 5, DL-105).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, RETRY_WINDOW_MS, Receipt, ReceiptState, RejectCode,
    SessionCommand, StopMode, UserInput,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::host::shim::settle::EffectResult;
use fleetdeck_providers::host::shim::wire::ProviderIdentity;
use fleetdeck_testkit::Det;

use crate::manager::decide::decide;
use crate::manager::policy::Policy;
use crate::store::{
    Caller, EffectState, NewSession, NewShim, OfflineApprovalPolicy, Store, StoreConfig,
};

async fn rig(det: &Det) -> (Store, Policy) {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s.create_session(NewSession {
        session_id: "s1".into(),
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        cwd: "/w".to_owned(),
        worktree: None,
        branch: None,
        mode: None,
        title: None,
        offline_approval_policy: OfflineApprovalPolicy::Wait,
    })
    .await
    .unwrap();
    s.process_started("s1".into(), "proc-1".to_owned().into(), None)
        .await
        .unwrap();
    let policy = Policy::new(
        Vec::new(),
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
    );
    (s, policy)
}

async fn send(det: &Det, s: &Store, policy: &Policy, id: &str, command: SessionCommand) -> Receipt {
    let envelope = CommandEnvelope {
        command_id: id.into(),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: command.clone(),
    };
    let policy = policy.clone();
    s.decide(envelope, Caller::local(), move |d| {
        decide(d, &policy, CommandOrigin::Board, command)
    })
    .await
    .unwrap()
    .receipt
}

fn prompt(text: &str) -> SessionCommand {
    SessionCommand::Prompt {
        session_id: "s1".into(),
        input: UserInput::new(text.to_owned()),
    }
}

#[tokio::test]
async fn a_partial_stdin_write_blocks_input_and_still_admits_interrupt_and_stop() {
    let det = Det::new();
    let (s, policy) = rig(&det).await;
    s.shim_starting(NewShim {
        shim_id: "a".to_owned(),
        session_id: "s1".into(),
        socket_path: PathBuf::from("/run/x/a.sock"),
    })
    .await
    .unwrap();
    s.shim_running(
        "a".to_owned(),
        ProviderIdentity {
            pid: 10,
            start_time: 1,
            boot_id: "b".to_owned(),
        },
    )
    .await
    .unwrap();
    let generation = s
        .shim_next_generation("a".to_owned())
        .await
        .unwrap()
        .unwrap();
    let first = send(&det, &s, &policy, "c1", prompt("one")).await;
    assert_eq!(first.state, ReceiptState::Accepted, "{first:?}");
    let fence = s.register_worker().await.unwrap();
    let effect = s.claim_next(fence).await.unwrap().unwrap();
    // A tag for a stale generation or a settled effect is refused.
    assert!(
        s.shim_claim_write(
            "a".to_owned(),
            generation + 1,
            Some(effect.effect_id.clone()),
            0,
            1
        )
        .await
        .unwrap()
        .is_none()
    );
    let tag = s
        .shim_claim_write(
            "a".to_owned(),
            generation,
            Some(effect.effect_id.clone()),
            0,
            1,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tag.write_id, format!("{}:0", effect.effect_id));
    assert_eq!(
        s.shim_writes_to_settle("a".to_owned()).await.unwrap().len(),
        1
    );
    assert!(
        s.shim_settle_write(
            "a".to_owned(),
            effect.effect_id.clone(),
            EffectResult::Unknown { block_input: true }
        )
        .await
        .unwrap()
    );
    let settled = s.effect(effect.effect_id.clone()).await.unwrap().unwrap();
    assert_eq!(settled.state, EffectState::OutcomeUnknown);
    assert_eq!(settled.reason.as_deref(), Some("stdin_partial_write"));
    // A settled effect is never settled again and gets no new frame.
    assert!(
        !s.shim_settle_write(
            "a".to_owned(),
            effect.effect_id.clone(),
            EffectResult::Retired("shim_not_received")
        )
        .await
        .unwrap()
    );
    assert!(
        s.shim_claim_write(
            "a".to_owned(),
            generation,
            Some(effect.effect_id.clone()),
            1,
            2
        )
        .await
        .unwrap()
        .is_none()
    );
    assert!(
        s.shim_writes_to_settle("a".to_owned())
            .await
            .unwrap()
            .is_empty()
    );

    let refused = send(&det, &s, &policy, "c2", prompt("two")).await;
    let reject = refused.reject.expect("rejected");
    assert_eq!(reject.code, RejectCode::InvalidState);
    assert!(
        reject.message.contains("stdin_partial_write"),
        "{}",
        reject.message
    );
    // Interrupt passes the block (it is refused only because no turn runs).
    let interrupt = send(
        &det,
        &s,
        &policy,
        "c3",
        SessionCommand::Interrupt {
            session_id: "s1".into(),
        },
    )
    .await;
    assert!(
        interrupt
            .reject
            .as_ref()
            .is_none_or(|r| !r.message.contains("stdin_partial_write")),
        "{interrupt:?}"
    );
    let stop = send(
        &det,
        &s,
        &policy,
        "c4",
        SessionCommand::Stop {
            session_id: "s1".into(),
            how: StopMode::Graceful,
        },
    )
    .await;
    assert_eq!(stop.state, ReceiptState::Accepted, "{stop:?}");
}

// DL-107: the complete frame count is stored with frame 0, the frames of
// one effect are claimed in order, and the count never changes.
#[tokio::test]
async fn the_frame_count_is_stored_with_frame_0_and_claims_stay_in_order() {
    let det = Det::new();
    let (s, policy) = rig(&det).await;
    s.shim_starting(NewShim {
        shim_id: "a".to_owned(),
        session_id: "s1".into(),
        socket_path: PathBuf::from("/run/x/a.sock"),
    })
    .await
    .unwrap();
    let generation = s
        .shim_next_generation("a".to_owned())
        .await
        .unwrap()
        .unwrap();
    let first = send(&det, &s, &policy, "c1", prompt("one")).await;
    assert_eq!(first.state, ReceiptState::Accepted, "{first:?}");
    let fence = s.register_worker().await.unwrap();
    let effect = s.claim_next(fence).await.unwrap().unwrap();
    let id = || Some(effect.effect_id.clone());
    let claim = |ordinal: u32, frames: u32| {
        s.shim_claim_write("a".to_owned(), generation, id(), ordinal, frames)
    };
    // An ordinal outside the count, or frame 1 before frame 0, is refused
    // and writes no row.
    assert!(claim(2, 2).await.is_err());
    assert!(claim(1, 2).await.is_err());
    assert!(
        s.shim_writes_to_settle("a".to_owned())
            .await
            .unwrap()
            .is_empty()
    );
    claim(0, 3).await.unwrap().unwrap();
    let pending = s.shim_writes_to_settle("a".to_owned()).await.unwrap();
    assert_eq!((pending[0].tags.len(), pending[0].frames), (1, 3));
    // The count of frame 0 cannot change, and frame 0 cannot repeat.
    assert!(claim(1, 2).await.is_err());
    assert!(claim(0, 3).await.is_err());
    claim(1, 3).await.unwrap().unwrap();
    let pending = s.shim_writes_to_settle("a".to_owned()).await.unwrap();
    assert_eq!((pending[0].tags.len(), pending[0].frames), (2, 3));
}
