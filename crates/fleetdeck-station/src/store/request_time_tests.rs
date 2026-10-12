//! Approval deadlines across a station restart and adapter checkpoints in
//! the shim transactions (ADR-005 §2.1, §6 "Approvals across a restart",
//! test 15; DL-108). A child of `effects_tests`, so it uses its helpers.

use std::path::PathBuf;

use fleetdeck_providers::checkpoint::AdapterCheckpoint;
use fleetdeck_providers::host::shim::wire::ProviderIdentity;
use serde_json::json;

use super::*;
use crate::store::{NewShim, OutputCommit, OutputMeta, REASON_REQUEST_EXPIRED, RequestTime};

const SHIM: &str = "shim-1";

fn now(det: &Det) -> i64 {
    i64::try_from(det.now_ms()).unwrap()
}

/// A running shim of `s1` with generation 1.
async fn shim(s: &Store) {
    s.shim_starting(NewShim {
        shim_id: SHIM.to_owned(),
        session_id: S1.into(),
        socket_path: PathBuf::from("/run/x/shim-1.sock"),
    })
    .await
    .unwrap();
    assert!(
        s.shim_running(
            SHIM.to_owned(),
            ProviderIdentity {
                pid: 42,
                start_time: 7,
                boot_id: "boot".to_owned(),
            },
        )
        .await
        .unwrap()
    );
    assert_eq!(
        s.shim_next_generation(SHIM.to_owned()).await.unwrap(),
        Some(1)
    );
}

fn checkpoint(n: u64) -> AdapterCheckpoint {
    AdapterCheckpoint {
        provider: ProviderKind::Claude,
        version: 1,
        state: json!({ "n": n }),
    }
}

/// The output transaction of one request line read at `read_at`.
async fn replay(s: &Store, cursor: u64, event: Event, read_at: RequestTime) -> OutputCommit {
    s.shim_commit_output_with(
        SHIM.to_owned(),
        1,
        cursor,
        vec![event.into()],
        OutputMeta {
            read_at,
            checkpoint: Some(checkpoint(cursor)),
            frames: Vec::new(),
        },
    )
    .await
    .unwrap()
}

async fn stored(s: &Store) -> Option<AdapterCheckpoint> {
    s.shim_checkpoint(SHIM.to_owned())
        .await
        .unwrap()
        .map(|t| AdapterCheckpoint::from_json(&t).unwrap())
}

async fn claim(s: &Store, det: &Det, id: &str, req: &str, option: &str) -> AnswerClaim {
    let (tx, rx) = std::sync::mpsc::channel();
    let (req, option) = (req.to_owned(), option.to_owned());
    s.decide(envelope(det, id), Caller::local(), move |d| {
        let claim = d.claim_answer(&req.as_str().into(), &option, ResolvedBy::Board)?;
        if claim == AnswerClaim::First {
            d.enqueue(&S1.into(), answer(&req, &option))?;
        }
        tx.send(claim).unwrap();
        Ok(accepted())
    })
    .await
    .unwrap();
    rx.recv().unwrap()
}

fn deny_of(sent: &[Effect]) -> Vec<String> {
    sent.iter()
        .map(|e| match &e.payload {
            EffectPayload::Answer {
                request_id,
                decision,
                ..
            } => format!("{request_id}:{}", decision.option_id),
            other => panic!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn a_replayed_request_keeps_its_proven_read_time_and_its_deadline_does_not_restart() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::DenyAfter { after_ms: 1_000 }).await;
    shim(&s).await;
    let read_at = now(&det);
    // The station was down for 5 s; the replay commits the request line.
    det.advance(Duration::from_secs(5));
    let commit = replay(
        &s,
        10,
        approval("req-1", "n1", &["allow", "deny"]),
        RequestTime::Read(read_at),
    )
    .await;
    assert!(matches!(commit, OutputCommit::Committed(_)));
    let req = s.runtime_request("req-1".into()).await.unwrap().unwrap();
    assert_eq!(
        req.requested_at, read_at,
        "replay time is never the request time"
    );
    assert_eq!(req.deny_at, Some(read_at + 1_000));
    assert!(req.time_proven);
    // Overdue at once: the safe deny goes out, never an allow.
    assert_eq!(deny_of(&s.deny_overdue().await.unwrap()), ["req-1:deny"]);
    // The adapter checkpoint was committed with the events and the cursor.
    assert_eq!(stored(&s).await, Some(checkpoint(10)));
}

#[tokio::test]
async fn a_request_without_a_proven_time_expires_with_the_deny_and_an_allow_is_refused() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::DenyAfter { after_ms: 60_000 }).await;
    shim(&s).await;
    replay(
        &s,
        5,
        approval("req-1", "n1", &["allow", "deny"]),
        RequestTime::Unprovable,
    )
    .await;
    let req = s.runtime_request("req-1".into()).await.unwrap().unwrap();
    assert!(!req.time_proven);
    assert_eq!(req.deny_at, Some(req.requested_at), "due at once");
    // An allow arriving before the sweep is refused as expired.
    assert_eq!(
        claim(&s, &det, "c1", "req-1", "allow").await,
        AnswerClaim::Closed(RequestState::Expired)
    );
    assert_eq!(deny_of(&s.deny_overdue().await.unwrap()), ["req-1:deny"]);
}

#[tokio::test]
async fn a_queued_allow_after_the_deadline_is_never_dispatched_and_the_deny_follows() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::DenyAfter { after_ms: 1_000 }).await;
    emit(&s, approval("req-1", "n1", &["allow", "deny"])).await;
    det.advance(Duration::from_millis(500));
    assert_eq!(
        claim(&s, &det, "c1", "req-1", "allow").await,
        AnswerClaim::First
    );
    // The station stops before the worker claims the allow; it restarts
    // after the deadline with the same live process (a reattached shim).
    det.advance(Duration::from_secs(2));
    s.recover(vec![P1.into()]).await.unwrap();
    let req = s.runtime_request("req-1".into()).await.unwrap().unwrap();
    assert_eq!(
        req.state,
        RequestState::Open,
        "the request survived the restart"
    );
    let fence = s.register_worker().await.unwrap();
    assert!(
        s.claim_next(fence).await.unwrap().is_none(),
        "the allow is not dispatched"
    );
    let e = s
        .session_effects(S1.into())
        .await
        .unwrap()
        .into_iter()
        .find(|e| matches!(&e.payload, EffectPayload::Answer { .. }))
        .unwrap();
    assert_eq!(e.state, EffectState::RetiredNotDispatched);
    assert_eq!(e.reason.as_deref(), Some(REASON_REQUEST_EXPIRED));
    assert_eq!(deny_of(&s.deny_overdue().await.unwrap()), ["req-1:deny"]);
}

#[tokio::test]
async fn an_open_request_keeps_requested_at_and_deny_at_across_a_restart() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::DenyAfter { after_ms: 10_000 }).await;
    emit(&s, approval("req-1", "n1", &["allow", "deny"])).await;
    let before = s.runtime_request("req-1".into()).await.unwrap().unwrap();
    det.advance(Duration::from_secs(3));
    s.recover(vec![P1.into()]).await.unwrap();
    let after = s.runtime_request("req-1".into()).await.unwrap().unwrap();
    assert_eq!(after.requested_at, before.requested_at);
    assert_eq!(after.deny_at, before.deny_at);
    assert!(s.deny_overdue().await.unwrap().is_empty());
    // Before the deadline an allow is still admitted.
    assert_eq!(
        claim(&s, &det, "c1", "req-1", "allow").await,
        AnswerClaim::First
    );
}

#[tokio::test]
async fn the_checkpoint_is_stored_only_by_an_accepted_claim_or_output_commit() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    shim(&s).await;
    let e = queue(&s, &det, "c1", prompt("in-1")).await;
    let fence = s.register_worker().await.unwrap();
    s.claim_next(fence).await.unwrap().unwrap();
    // A stale generation stores nothing.
    let stale = s
        .shim_claim_write_with(
            SHIM.to_owned(),
            9,
            Some(e.effect_id.clone()),
            0,
            1,
            Some(checkpoint(1)),
        )
        .await
        .unwrap();
    assert!(stale.is_none());
    assert_eq!(stored(&s).await, None);
    let tag = s
        .shim_claim_write_with(
            SHIM.to_owned(),
            1,
            Some(e.effect_id.clone()),
            0,
            1,
            Some(checkpoint(2)),
        )
        .await
        .unwrap();
    assert!(tag.is_some());
    assert_eq!(
        stored(&s).await,
        Some(checkpoint(2)),
        "committed with the claim"
    );
    // A stale output commit changes neither the events nor the checkpoint.
    let old = s
        .shim_commit_output_with(
            SHIM.to_owned(),
            0,
            1,
            vec![started("in-1").into()],
            OutputMeta {
                read_at: RequestTime::Now,
                checkpoint: Some(checkpoint(3)),
                frames: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(old, OutputCommit::StaleGeneration);
    assert_eq!(stored(&s).await, Some(checkpoint(2)));
}

#[tokio::test]
async fn a_proven_unwritten_shim_answer_reopens_the_request_with_its_original_deadline() {
    use fleetdeck_providers::host::shim::settle::{EffectResult, SHIM_NOT_RECEIVED};
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::DenyAfter { after_ms: 10_000 }).await;
    shim(&s).await;
    emit(&s, approval("req-1", "n1", &["allow", "deny"])).await;
    let before = s.runtime_request("req-1".into()).await.unwrap().unwrap();
    det.advance(Duration::from_secs(2));
    assert_eq!(
        claim(&s, &det, "c1", "req-1", "allow").await,
        AnswerClaim::First
    );
    let claimed = s.runtime_request("req-1".into()).await.unwrap().unwrap();
    assert!(claimed.receipt.is_some());
    assert_eq!(claimed.deny_at, None, "an answered request has no deadline");
    let fence = s.register_worker().await.unwrap();
    let effect = s.claim_next(fence).await.unwrap().unwrap();
    s.shim_claim_write(SHIM.to_owned(), 1, Some(effect.effect_id.clone()), 0, 1)
        .await
        .unwrap()
        .unwrap();
    // Recovery: the shim proves that no byte of the frame was received.
    assert!(
        s.shim_settle_write(
            SHIM.to_owned(),
            effect.effect_id.clone(),
            EffectResult::Retired(SHIM_NOT_RECEIVED),
        )
        .await
        .unwrap()
    );
    let e = s.effect(effect.effect_id.clone()).await.unwrap().unwrap();
    assert_eq!(e.state, EffectState::RetiredNotDispatched);
    let after = s.runtime_request("req-1".into()).await.unwrap().unwrap();
    assert_eq!(after.state, RequestState::Open);
    assert!(after.receipt.is_none(), "the answer slot is open again");
    assert_eq!(after.deny_at, before.deny_at, "the original deadline");
    // The first command keeps its receipt; a new command can answer.
    let repeat = s
        .decide(envelope(&det, "c1"), Caller::local(), |_| Ok(accepted()))
        .await
        .unwrap();
    assert!(repeat.duplicate, "the first command keeps its receipt");
    assert!(repeat.effects.is_empty());
    assert_eq!(
        claim(&s, &det, "c2", "req-1", "deny").await,
        AnswerClaim::First
    );
}
