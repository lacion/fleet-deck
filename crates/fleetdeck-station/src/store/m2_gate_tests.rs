//! M2 gate correction (DL-070): a not-dispatched answer opens its request
//! again, and a lost session reports its loss before it empties the
//! background list.

use fleetdeck_proto::event::BackgroundTask;

use super::*;

/// Command `id` answers `request` with `option` the way `decide` does: the
/// first claim queues one answer effect.
async fn answer_with(s: &Store, det: &Det, id: &str, request: &str, option: &str) -> Decided {
    let (request, option) = (request.to_owned(), option.to_owned());
    s.decide(envelope(det, id), Caller::local(), move |d| {
        let claim = d.claim_answer(
            &S1.into(),
            &request.as_str().into(),
            &option,
            ResolvedBy::Board,
        )?;
        if claim == AnswerClaim::First {
            d.enqueue(&S1.into(), answer(&request, &option))?;
        }
        Ok(accepted())
    })
    .await
    .unwrap()
}

async fn request(s: &Store, id: &str) -> RuntimeRequest {
    s.runtime_request(S1.into(), id.into())
        .await
        .unwrap()
        .unwrap()
}

/// `c1` answers `req-1`, the live worker claims the effect and gets `how`.
async fn answer_and_settle(s: &Store, det: &Det, how: Dispatch) -> Effect {
    let d = answer_with(s, det, "c1", "req-1", "allow").await;
    assert_eq!(d.effects.len(), 1);
    let fence = s.register_worker().await.unwrap();
    let e = s.claim_next(fence).await.unwrap().unwrap();
    s.settle(e.effect_id.clone(), fence, how).await.unwrap();
    e
}

/// The request still holds the answer of `c1`.
async fn assert_held(s: &Store) {
    let req = request(s, "req-1").await;
    assert_eq!(
        req.receipt.map(|r| r.command_id),
        Some(Some(CommandId::from("c1")))
    );
}

#[tokio::test]
async fn a_not_dispatched_answer_opens_its_request_again_with_its_own_deadline() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::DenyAfter { after_ms: 1_000 }).await;
    emit(&s, approval("req-1", "node-1", &["allow", "deny"])).await;
    let requested_at = request(&s, "req-1").await.requested_at;
    det.advance(Duration::from_millis(400));
    let d = answer_with(&s, &det, "c1", "req-1", "allow").await;
    assert_eq!(
        request(&s, "req-1").await.deny_at,
        None,
        "the claim stops the timer"
    );
    det.advance(Duration::from_millis(300));
    let fence = s.register_worker().await.unwrap();
    let e = s.claim_next(fence).await.unwrap().unwrap();
    assert_eq!(e.effect_id, d.effects[0].effect_id);
    s.settle(
        e.effect_id.clone(),
        fence,
        Dispatch::NotDispatched("stopping".into()),
    )
    .await
    .unwrap();

    // Effect retirement and release are one transaction.
    assert_eq!(state(&s, &e).await, EffectState::RetiredNotDispatched);
    let req = request(&s, "req-1").await;
    assert_eq!(req.state, RequestState::Open);
    assert_eq!(req.receipt, None);
    assert_eq!(
        req.deny_at,
        Some(requested_at + 1_000),
        "the request's own deadline, not settlement time plus the timeout"
    );

    // The original command stays decided and sends nothing again.
    let again = answer_with(&s, &det, "c1", "req-1", "allow").await;
    assert!(again.duplicate);
    assert_eq!(again.receipt.state, ReceiptState::Accepted);
    assert!(again.effects.is_empty());
    assert_eq!(state(&s, &e).await, EffectState::RetiredNotDispatched);
    assert!(s.claim_next(fence).await.unwrap().is_none());

    // A new command answers once; a third finds the answer taken.
    let fresh = answer_with(&s, &det, "c2", "req-1", "deny").await;
    assert_eq!(fresh.effects.len(), 1);
    assert!(
        answer_with(&s, &det, "c3", "req-1", "allow")
            .await
            .effects
            .is_empty()
    );
    let receipt = request(&s, "req-1").await.receipt.unwrap();
    assert_eq!(receipt.command_id, Some("c2".into()));
    assert_eq!(receipt.option_id, "deny");
    let next = s.claim_next(fence).await.unwrap().unwrap();
    assert_eq!(next.effect_id, fresh.effects[0].effect_id);
    assert!(s.claim_next(fence).await.unwrap().is_none());
}

#[tokio::test]
async fn a_released_wait_request_gets_no_deadline() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    emit(&s, approval("req-1", "node-1", &["allow", "deny"])).await;
    answer_and_settle(&s, &det, Dispatch::NotDispatched("stopping".into())).await;
    let req = request(&s, "req-1").await;
    assert_eq!(
        (req.state, req.receipt, req.deny_at),
        (RequestState::Open, None, None)
    );
}

#[tokio::test]
async fn sent_unknown_and_confirmed_answers_keep_their_claim() {
    for how in [
        Dispatch::Sent,
        Dispatch::Unknown("write failed".into()),
        Dispatch::Confirmed,
    ] {
        let det = Det::new();
        let s = running(&det, OfflineApprovalPolicy::DenyAfter { after_ms: 1_000 }).await;
        emit(&s, approval("req-1", "node-1", &["allow", "deny"])).await;
        answer_and_settle(&s, &det, how).await;
        assert_held(&s).await;
        assert_eq!(request(&s, "req-1").await.deny_at, None);
    }
}

#[tokio::test]
async fn a_fenced_out_worker_releases_nothing() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    emit(&s, approval("req-1", "node-1", &["allow", "deny"])).await;
    answer_with(&s, &det, "c1", "req-1", "allow").await;
    let old = s.register_worker().await.unwrap();
    let e = s.claim_next(old).await.unwrap().unwrap();
    // A worker restart makes the claim unknown; the old worker is fenced.
    s.register_worker().await.unwrap();
    let late = s
        .settle(
            e.effect_id.clone(),
            old,
            Dispatch::NotDispatched("stopping".into()),
        )
        .await;
    assert!(matches!(late, Err(StoreError::Fenced { .. })));
    assert_eq!(state(&s, &e).await, EffectState::OutcomeUnknown);
    assert_held(&s).await;
}

#[tokio::test]
async fn a_closed_request_stays_closed_after_a_not_dispatched_answer() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    emit(&s, approval("req-1", "node-1", &["allow", "deny"])).await;
    answer_with(&s, &det, "c1", "req-1", "allow").await;
    let fence = s.register_worker().await.unwrap();
    let e = s.claim_next(fence).await.unwrap().unwrap();
    emit(
        &s,
        Event::ApprovalExpired {
            request_id: "req-1".into(),
            reason: ExpiredReason::TurnEnded,
        },
    )
    .await;
    s.settle(
        e.effect_id.clone(),
        fence,
        Dispatch::NotDispatched("stopping".into()),
    )
    .await
    .unwrap();
    let req = request(&s, "req-1").await;
    assert_eq!(req.state, RequestState::Expired);
    assert!(req.receipt.is_some());
}

#[tokio::test]
async fn an_effect_that_does_not_own_the_receipt_releases_nothing() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    emit(&s, approval("req-1", "node-1", &["allow", "deny"])).await;
    answer_with(&s, &det, "c1", "req-1", "allow").await;
    // Another command's answer effect (no claim), and one of c1's option
    // that the receipt does not hold.
    queue(&s, &det, "c2", answer("req-1", "allow")).await;
    let fence = s.register_worker().await.unwrap();
    let first = s.claim_next(fence).await.unwrap().unwrap();
    s.settle(first.effect_id, fence, Dispatch::Sent)
        .await
        .unwrap();
    let other = s.claim_next(fence).await.unwrap().unwrap();
    assert_eq!(other.command_id, Some("c2".into()));
    s.settle(
        other.effect_id,
        fence,
        Dispatch::NotDispatched("stopping".into()),
    )
    .await
    .unwrap();
    assert_held(&s).await;

    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    emit(&s, approval("req-1", "node-1", &["allow", "deny"])).await;
    s.decide(envelope(&det, "c1"), Caller::local(), |d| {
        d.claim_answer(&S1.into(), &"req-1".into(), "allow", ResolvedBy::Board)?;
        d.enqueue(&S1.into(), answer("req-1", "deny"))?;
        Ok(accepted())
    })
    .await
    .unwrap();
    let fence = s.register_worker().await.unwrap();
    let e = s.claim_next(fence).await.unwrap().unwrap();
    s.settle(
        e.effect_id,
        fence,
        Dispatch::NotDispatched("stopping".into()),
    )
    .await
    .unwrap();
    assert_held(&s).await;
}

#[tokio::test]
async fn a_lost_process_expires_the_answered_request_without_a_release() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    emit(&s, approval("req-1", "node-1", &["allow", "deny"])).await;
    let d = answer_with(&s, &det, "c1", "req-1", "allow").await;
    s.process_ended(P1.into(), "exit 1".to_owned())
        .await
        .unwrap();
    assert_eq!(
        state(&s, &d.effects[0]).await,
        EffectState::RetiredNotDispatched
    );
    let req = request(&s, "req-1").await;
    assert_eq!(req.state, RequestState::Expired);
    assert!(req.receipt.is_some());
}

#[tokio::test]
async fn a_lost_session_reports_the_loss_before_it_empties_the_background_list() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("station.db");
    let det = Det::new();
    {
        let s = Store::open(&path, det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        s.recover(Vec::new()).await.unwrap();
        s.create_session(session(OfflineApprovalPolicy::Wait))
            .await
            .unwrap();
        s.process_started(S1.into(), P1.into(), None).await.unwrap();
        emit(
            &s,
            Event::RunStarted {
                author: None,
                input_id: None,
                run_id: "run-1".into(),
                input_summary: "hi".to_owned(),
                origin: RunOrigin::Board,
                input_text: None,
            },
        )
        .await;
        emit(
            &s,
            Event::TurnStarted {
                run_id: "run-1".into(),
                turn_id: "turn-1".into(),
                provider_refs: serde_json::Map::new(),
            },
        )
        .await;
        emit(
            &s,
            Event::BackgroundUpdated {
                tasks: vec![BackgroundTask {
                    task_id: "task-1".to_owned(),
                    task_kind: "local_agent".to_owned(),
                }],
            },
        )
        .await;
    }
    let s = Store::open(&path, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let head = s.head();
    s.recover(Vec::new()).await.unwrap();
    let ReadResult::Events { events, .. } = s.read_after(head, 100).await.unwrap() else {
        panic!("events expected")
    };
    assert_eq!(
        codes(&events),
        [
            "turn.completed",
            "run.completed",
            "session.state",
            "background.updated",
        ]
    );
    let summary = s.summary(S1.into()).await.unwrap().unwrap();
    assert_eq!(summary.state, SessionState::Lost);
    assert!(summary.background_tasks.is_empty());
    assert_eq!(summary.current_run, None);
}

#[tokio::test]
async fn claim_answer_reports_unknown_closed_and_invalid_answers() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    emit(&s, approval("req-1", "node-1", &["allow", "deny"])).await;
    emit(
        &s,
        Event::ApprovalExpired {
            request_id: "req-1".into(),
            reason: ExpiredReason::TurnEnded,
        },
    )
    .await;
    emit(&s, approval("req-2", "node-2", &["allow", "deny"])).await;
    let mut seen = Vec::new();
    for (n, (req, option)) in [("req-9", "allow"), ("req-1", "allow"), ("req-2", "maybe")]
        .into_iter()
        .enumerate()
    {
        let (tx, rx) = std::sync::mpsc::channel();
        s.decide(
            envelope(&det, &format!("c{n}")),
            Caller::local(),
            move |d| {
                tx.send(d.claim_answer(&S1.into(), &req.into(), option, ResolvedBy::Board)?)
                    .unwrap();
                Ok(accepted())
            },
        )
        .await
        .unwrap();
        seen.push(rx.recv().unwrap());
    }
    assert_eq!(
        seen,
        [
            AnswerClaim::UnknownRequest,
            AnswerClaim::Closed(RequestState::Expired),
            AnswerClaim::UnknownOption,
        ]
    );
}

// --- request key (DL-402 follow-up) ------------------------------------------------

/// A second session `s2` of the same store.
async fn second_session(s: &Store) {
    let mut new = session(OfflineApprovalPolicy::Wait);
    new.session_id = "s2".into();
    s.create_session(new).await.unwrap();
}

#[tokio::test]
async fn a_resolved_event_closes_only_the_request_of_its_session() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    second_session(&s).await;
    emit(&s, approval("req-1", "node-1", &["allow", "deny"])).await;
    s.append(
        "s2".into(),
        vec![approval("req-1", "node-1", &["allow", "deny"]).into()],
    )
    .await
    .unwrap();
    let get = |session: &str| s.runtime_request(session.into(), "req-1".into());
    assert_eq!(get(S1).await.unwrap().unwrap().state, RequestState::Open);
    assert_eq!(get("s2").await.unwrap().unwrap().state, RequestState::Open);

    emit(
        &s,
        Event::ApprovalResolved {
            request_id: "req-1".into(),
            option_id: "allow".to_owned(),
            by: ResolvedBy::Board,
            answered_by: None,
        },
    )
    .await;
    assert_eq!(
        get(S1).await.unwrap().unwrap().state,
        RequestState::Resolved
    );
    assert_eq!(get("s2").await.unwrap().unwrap().state, RequestState::Open);
    let all = s.runtime_requests_with_id("req-1".into()).await.unwrap();
    assert_eq!(all.len(), 2);
}

fn picked(session: &str, state: RequestState) -> RuntimeRequest {
    RuntimeRequest {
        request_id: "req-3".into(),
        session_id: session.into(),
        process_incarnation: None,
        node_id: None,
        kind: RequestKind::Command,
        options: Vec::new(),
        response: ResponseChannel::RpcReply,
        state,
        receipt: None,
        deny_at: None,
        requested_at: 1,
        time_proven: true,
    }
}

#[test]
fn pick_local_request_cases() {
    use crate::store::requests::{LocalPick, pick_local_request};
    let (open, closed) = (RequestState::Open, RequestState::Resolved);
    assert_eq!(pick_local_request(&[]), Err(LocalPick::None));
    let one = [picked("a", closed)];
    assert_eq!(pick_local_request(&one).unwrap().session_id.as_str(), "a");
    let mixed = [picked("a", closed), picked("b", open)];
    assert_eq!(pick_local_request(&mixed).unwrap().session_id.as_str(), "b");
    let both_open = [picked("a", open), picked("b", open)];
    assert_eq!(pick_local_request(&both_open), Err(LocalPick::Ambiguous));
    let both_closed = [picked("a", closed), picked("b", RequestState::Expired)];
    assert_eq!(pick_local_request(&both_closed), Err(LocalPick::Ambiguous));
}
