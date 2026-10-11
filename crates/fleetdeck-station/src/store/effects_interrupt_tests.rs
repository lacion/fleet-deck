//! Only the provider's answer to an interrupt's own control request settles
//! the interrupt effect (ADR-001 §3.1).

use super::*;

pub(super) fn interrupted(effect: &Effect, accepted: bool) -> Event {
    Event::InterruptResult {
        control_id: Some(effect.effect_id.as_str().to_owned()),
        accepted,
        message: (!accepted).then(|| "no turn to interrupt".to_owned()),
    }
}

/// Every effect claimed and sent; returns the fence.
async fn send_all(s: &Store) -> u64 {
    let fence = s.register_worker().await.unwrap();
    while let Some(e) = s.claim_next(fence).await.unwrap() {
        s.settle(e.effect_id, fence, Dispatch::Sent).await.unwrap();
    }
    fence
}

#[tokio::test]
async fn a_turn_or_run_that_ends_does_not_confirm_an_interrupt() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    let interrupt = queue(&s, &det, "c1", EffectPayload::Interrupt).await;
    send_all(&s).await;
    for status in [
        TurnStatus::Done,
        TurnStatus::Interrupted,
        TurnStatus::Failed,
    ] {
        emit(
            &s,
            Event::TurnCompleted {
                turn_id: "turn-1".into(),
                status,
                usage: None,
            },
        )
        .await;
        emit(
            &s,
            Event::RunCompleted {
                run_id: "run-1".into(),
                status,
                background_pending: false,
            },
        )
        .await;
    }
    // An answer without a control id, or to another effect, is no evidence.
    emit(
        &s,
        Event::InterruptResult {
            control_id: None,
            accepted: true,
            message: None,
        },
    )
    .await;
    emit(
        &s,
        Event::InterruptResult {
            control_id: Some("effect-of-another-command".to_owned()),
            accepted: true,
            message: None,
        },
    )
    .await;
    assert_eq!(state(&s, &interrupt).await, EffectState::Claimed);
    emit(&s, interrupted(&interrupt, true)).await;
    assert_eq!(state(&s, &interrupt).await, EffectState::Confirmed);
}

#[tokio::test]
async fn each_interrupt_answer_settles_only_its_own_effect() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    let first = queue(&s, &det, "c1", EffectPayload::Interrupt).await;
    let second = queue(&s, &det, "c2", EffectPayload::Interrupt).await;
    send_all(&s).await;
    emit(&s, interrupted(&second, false)).await;
    assert_eq!(state(&s, &first).await, EffectState::Claimed);
    let refused = s.effect(second.effect_id.clone()).await.unwrap().unwrap();
    assert_eq!(refused.state, EffectState::RetiredNotDispatched);
    assert_eq!(refused.reason.as_deref(), Some("refused_by_provider"));
    emit(&s, interrupted(&first, true)).await;
    assert_eq!(state(&s, &first).await, EffectState::Confirmed);
    // A repeated answer changes nothing.
    emit(&s, interrupted(&second, true)).await;
    assert_eq!(state(&s, &second).await, EffectState::RetiredNotDispatched);
}

#[tokio::test]
async fn a_late_interrupt_answer_confirms_an_outcome_unknown_effect_after_a_worker_restart() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    let interrupt = queue(&s, &det, "c1", EffectPayload::Interrupt).await;
    send_all(&s).await;
    // The worker restarts while the process lives: the claim is unknown.
    s.register_worker().await.unwrap();
    assert_eq!(state(&s, &interrupt).await, EffectState::OutcomeUnknown);
    emit(&s, interrupted(&interrupt, true)).await;
    assert_eq!(state(&s, &interrupt).await, EffectState::Confirmed);
}
