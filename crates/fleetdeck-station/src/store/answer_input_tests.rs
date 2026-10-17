//! GATE-M2 P2-b: a Codex message-channel answer goes to the provider as a
//! user message under the input id `answer-<request>`. Before the fix no
//! event confirmed its effect: the tool-node join never matches a request
//! without a node. Now the provider's `input.updated started` for that
//! input confirms the answer effect, and a refusal retires it.

use fleetdeck_providers::adapter::answer_input_id;

use super::*;

fn question(request: &str) -> Event {
    Event::ApprovalRequested {
        request_id: request.into(),
        node_id: None,
        request_kind: RequestKind::Question,
        title: "Which colour?".to_owned(),
        details: String::new(),
        input: None,
        options: options(&["red", "blue", "answer"]),
        response: ResponseChannel::Message,
        provider_refs: serde_json::Map::new(),
    }
}

fn input_event(request: &str, state: InputState) -> Event {
    Event::InputUpdated {
        input_id: answer_input_id(&request.into()),
        turn_id: Some("turn-2".into()),
        state,
    }
}

/// `c1` answers `req-1` with `blue`; the live worker claims the effect and
/// sends it.
async fn sent_answer(s: &Store, det: &Det) -> Effect {
    let d = s
        .decide(envelope(det, "c1"), Caller::local(), |d| {
            let claim = d.claim_answer(&"req-1".into(), "blue", ResolvedBy::Board)?;
            assert_eq!(claim, AnswerClaim::First);
            d.enqueue(&S1.into(), answer("req-1", "blue"))?;
            Ok(accepted())
        })
        .await
        .unwrap();
    assert_eq!(d.effects.len(), 1);
    let fence = s.register_worker().await.unwrap();
    let e = s.claim_next(fence).await.unwrap().unwrap();
    s.settle(e.effect_id.clone(), fence, Dispatch::Sent)
        .await
        .unwrap();
    e
}

#[tokio::test]
async fn a_message_answer_is_confirmed_when_its_input_starts() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    emit(&s, question("req-1")).await;
    let e = sent_answer(&s, &det).await;
    assert_ne!(state(&s, &e).await, EffectState::Confirmed);

    // Evidence for another request or an ordinary input does not confirm it.
    emit(&s, input_event("req-2", InputState::Started)).await;
    emit(&s, started("01J00000000000000000000000")).await;
    emit(&s, input_event("req-1", InputState::Received)).await;
    assert_ne!(state(&s, &e).await, EffectState::Confirmed);

    emit(&s, input_event("req-1", InputState::Started)).await;
    assert_eq!(state(&s, &e).await, EffectState::Confirmed);
}

#[tokio::test]
async fn an_outcome_unknown_message_answer_is_confirmed_by_a_late_start() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    emit(&s, question("req-1")).await;
    let d = s
        .decide(envelope(&det, "c1"), Caller::local(), |d| {
            d.claim_answer(&"req-1".into(), "blue", ResolvedBy::Board)?;
            d.enqueue(&S1.into(), answer("req-1", "blue"))?;
            Ok(accepted())
        })
        .await
        .unwrap();
    let fence = s.register_worker().await.unwrap();
    let e = s.claim_next(fence).await.unwrap().unwrap();
    assert_eq!(e.effect_id, d.effects[0].effect_id);
    s.settle(e.effect_id.clone(), fence, Dispatch::Unknown("pipe".into()))
        .await
        .unwrap();
    assert_eq!(state(&s, &e).await, EffectState::OutcomeUnknown);
    emit(&s, input_event("req-1", InputState::Started)).await;
    assert_eq!(state(&s, &e).await, EffectState::Confirmed);
}

#[tokio::test]
async fn a_refused_message_answer_input_retires_its_effect() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    emit(&s, question("req-1")).await;
    let e = sent_answer(&s, &det).await;
    emit(&s, input_event("req-1", InputState::Refused)).await;
    let effect = s.effect(e.effect_id.clone()).await.unwrap().unwrap();
    assert_eq!(effect.state, EffectState::RetiredNotDispatched);
    assert_eq!(effect.reason.as_deref(), Some(effects::REASON_REFUSED));
}
