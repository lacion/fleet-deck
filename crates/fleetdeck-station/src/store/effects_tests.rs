//! Effect outbox, runtime requests and recovery (M2 P2-5).
use std::time::Duration;

use fleetdeck_proto::command::{
    ApprovalDecision, CommandOrigin, CommandResult, RETRY_WINDOW_MS, ReceiptState, SessionCommand,
    StopMode, UserInput,
};
use fleetdeck_proto::event::{
    ApprovalOption, ExpiredReason, InputState, ProviderKind, RequestKind, ResolvedBy,
    ResponseChannel, RunOrigin, SessionState, ToolStatus, TurnStatus,
};
use fleetdeck_proto::{CommandId, Reject, RejectCode};
use fleetdeck_testkit::Det;

use super::*;

const S1: &str = "s1";
const P1: &str = "proc-1";

async fn fresh(det: &Det) -> Store {
    Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap()
}

fn session(policy: OfflineApprovalPolicy) -> NewSession {
    NewSession {
        session_id: S1.into(),
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        cwd: "/w".to_owned(),
        worktree: None,
        branch: None,
        mode: Some("default".to_owned()),
        title: None,
        offline_approval_policy: policy,
    }
}

/// A recovered store with session `s1` and its live process `proc-1`.
async fn running(det: &Det, policy: OfflineApprovalPolicy) -> Store {
    let s = fresh(det).await;
    s.recover(Vec::new()).await.unwrap();
    s.create_session(session(policy)).await.unwrap();
    s.process_started(S1.into(), P1.into(), Some(42))
        .await
        .unwrap();
    s
}

fn envelope(det: &Det, id: &str) -> CommandEnvelope {
    CommandEnvelope {
        authority: None,
        command_id: CommandId::from(id),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Prompt {
            session_id: S1.into(),
            input: UserInput::new(id.to_owned()),
        },
    }
}

fn accepted() -> CommandResult {
    CommandResult {
        session_id: Some(S1.into()),
        ..CommandResult::default()
    }
}

fn prompt(input: &str) -> EffectPayload {
    EffectPayload::Prompt {
        input: InputRecord {
            input_id: input.into(),
            origin: RunOrigin::Board,
            input: UserInput::new("hi".to_owned()),
            shell_context: Vec::new(),
        },
        authority: None,
    }
}

fn answer(request: &str, option: &str) -> EffectPayload {
    EffectPayload::Answer {
        request_id: request.into(),
        decision: ApprovalDecision {
            answered_by: None,
            option_id: option.to_owned(),
            updated_input: None,
            text: None,
        },
        by: ResolvedBy::Board,
        authority: None,
    }
}

/// Decides command `id`, which queues `payload` for `s1`.
async fn queue(s: &Store, det: &Det, id: &str, payload: EffectPayload) -> Effect {
    let d = s
        .decide(envelope(det, id), Caller::local(), move |d| {
            d.enqueue(&S1.into(), payload)?;
            Ok(accepted())
        })
        .await
        .unwrap();
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    d.effects.into_iter().next().unwrap()
}

async fn state(s: &Store, e: &Effect) -> EffectState {
    s.effect(e.effect_id.clone()).await.unwrap().unwrap().state
}

async fn emit(s: &Store, event: Event) -> Vec<EventEnvelope> {
    s.append(S1.into(), vec![event.into()]).await.unwrap()
}

fn options(ids: &[&str]) -> Vec<ApprovalOption> {
    ids.iter()
        .map(|id| ApprovalOption {
            option_id: (*id).to_owned(),
            label: (*id).to_owned(),
            kind: None,
        })
        .collect()
}

fn approval(request: &str, node: &str, opts: &[&str]) -> Event {
    Event::ApprovalRequested {
        request_id: request.into(),
        node_id: Some(node.into()),
        request_kind: RequestKind::Command,
        title: "Bash".to_owned(),
        details: "ls".to_owned(),
        input: None,
        options: options(opts),
        response: ResponseChannel::RpcReply,
        provider_refs: serde_json::Map::new(),
    }
}

fn started(input: &str) -> Event {
    Event::InputUpdated {
        input_id: input.into(),
        turn_id: Some("turn-1".into()),
        state: InputState::Started,
    }
}

fn codes(events: &[EventEnvelope]) -> Vec<String> {
    events
        .iter()
        .map(|e| match &e.event {
            EventBody::Known(Event::Error { code, .. }) => code.clone(),
            EventBody::Known(other) => other.kind_name().to_owned(),
            EventBody::Unknown { kind, .. } => kind.clone(),
        })
        .collect()
}

async fn all_events(s: &Store) -> Vec<EventEnvelope> {
    match s.read_after(0, 10_000).await.unwrap() {
        ReadResult::Events { events, .. } => events,
        ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
    }
}

// --- outbox -------------------------------------------------------------------

#[tokio::test]
async fn a_decision_queues_its_effects_in_its_transaction_for_the_live_process() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    let mut wake = s.subscribe_outbox();
    let e = queue(&s, &det, "c1", prompt("in-1")).await;
    assert_eq!(e.state, EffectState::Queued);
    assert_eq!(e.process_incarnation, Some(P1.into()));
    assert_eq!(e.command_id, Some("c1".into()));
    assert!(wake.has_changed().unwrap());
    wake.mark_unchanged();

    // A rejected decision rolls its effects back.
    let d = s
        .decide(envelope(&det, "c2"), Caller::local(), |d| {
            d.enqueue(&S1.into(), prompt("in-2"))?;
            Err(Reject::new(RejectCode::InvalidState, "no").into())
        })
        .await
        .unwrap();
    assert_eq!(d.receipt.state, ReceiptState::Rejected);
    assert!(d.effects.is_empty());
    assert_eq!(s.session_effects(S1.into()).await.unwrap().len(), 1);
    assert!(!wake.has_changed().unwrap());
}

#[tokio::test]
async fn an_effect_needs_a_live_process_and_a_failed_enqueue_stores_nothing() {
    let det = Det::new();
    let s = fresh(&det).await;
    s.recover(Vec::new()).await.unwrap();
    s.create_session(session(OfflineApprovalPolicy::Wait))
        .await
        .unwrap();
    let err = s
        .decide(envelope(&det, "c1"), Caller::local(), |d| {
            d.enqueue(&S1.into(), prompt("in-1"))?;
            Ok(accepted())
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NoProcess(_)), "{err}");
    assert!(s.receipt("c1".into()).await.unwrap().is_none());
    // A start needs no process.
    let start = EffectPayload::Start {
        spec: fleetdeck_proto::command::SessionSpec {
            effort: None,
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            mode: None,
            model: None,
            title: None,
            coordinator: None,
            launch: None,
            extra_env: None,
        },
        prompt: None,
        workspace: None,
        setup_cmd: None,
        authority: None,
        fleet_git: None,
    };
    let e = queue(&s, &det, "c2", start).await;
    assert_eq!(e.process_incarnation, None);
}

#[tokio::test]
async fn the_store_admits_no_command_before_recovery() {
    let det = Det::new();
    let s = fresh(&det).await;
    assert!(!s.is_recovered());
    let err = s
        .decide(envelope(&det, "c1"), Caller::local(), |_| Ok(accepted()))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotRecovered));
    s.recover(Vec::new()).await.unwrap();
    assert!(s.is_recovered());
}

#[tokio::test]
async fn the_claim_is_committed_before_dispatch_and_only_evidence_confirms() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    let e1 = queue(&s, &det, "c1", prompt("in-1")).await;
    let e2 = queue(&s, &det, "c2", prompt("in-2")).await;
    let fence = s.register_worker().await.unwrap();
    let claimed = s.claim_next(fence).await.unwrap().unwrap();
    assert_eq!(claimed.effect_id, e1.effect_id, "oldest first");
    let stored = s.effect(e1.effect_id.clone()).await.unwrap().unwrap();
    assert_eq!(stored.state, EffectState::Claimed);
    assert_eq!(stored.fence_token, Some(fence));
    assert_eq!(stored.process_incarnation, Some(P1.into()));

    // A pipe write is not a confirmation.
    let after = s
        .settle(e1.effect_id.clone(), fence, Dispatch::Sent)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.state, EffectState::Claimed);
    // Evidence for another input confirms nothing.
    emit(&s, started("in-2")).await;
    assert_eq!(state(&s, &e1).await, EffectState::Claimed);
    assert_eq!(
        state(&s, &e2).await,
        EffectState::Queued,
        "queued: no evidence"
    );
    emit(&s, started("in-1")).await;
    assert_eq!(state(&s, &e1).await, EffectState::Confirmed);
}

#[tokio::test]
async fn a_replaced_worker_is_fenced_and_its_claims_become_outcome_unknown() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    let e = queue(&s, &det, "c1", prompt("in-1")).await;
    let old = s.register_worker().await.unwrap();
    s.claim_next(old).await.unwrap().unwrap();
    let head = s.head();

    let new = s.register_worker().await.unwrap();
    assert!(new > old);
    let stored = s.effect(e.effect_id.clone()).await.unwrap().unwrap();
    assert_eq!(stored.state, EffectState::OutcomeUnknown);
    assert_eq!(
        stored.reason.as_deref(),
        Some(effects::REASON_WORKER_RESTART)
    );
    let told = s.read_after(head, 10).await.unwrap();
    let ReadResult::Events { events, .. } = told else {
        panic!("events expected")
    };
    assert_eq!(codes(&events), ["delivery_unknown"]);

    // The old worker can neither settle nor claim.
    let err = s
        .settle(e.effect_id.clone(), old, Dispatch::Confirmed)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Fenced { .. }), "{err}");
    assert!(matches!(
        s.claim_next(old).await.unwrap_err(),
        StoreError::Fenced { .. }
    ));
    // The new worker never sends it again ...
    assert!(s.claim_next(new).await.unwrap().is_none());
    // ... and late provider evidence still settles it.
    emit(&s, started("in-1")).await;
    assert_eq!(state(&s, &e).await, EffectState::Confirmed);
}

#[tokio::test]
async fn dispatch_results_settle_the_claim() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    let fence = s.register_worker().await.unwrap();
    let cases = [
        (Dispatch::Confirmed, EffectState::Confirmed, None),
        (
            Dispatch::NotDispatched("unsupported".to_owned()),
            EffectState::RetiredNotDispatched,
            Some("input.updated"),
        ),
        (
            Dispatch::Unknown("broken pipe".to_owned()),
            EffectState::OutcomeUnknown,
            Some("delivery_unknown"),
        ),
    ];
    for (n, (dispatch, want, told)) in cases.into_iter().enumerate() {
        let e = queue(&s, &det, &format!("c{n}"), prompt(&format!("in-{n}"))).await;
        s.claim_next(fence).await.unwrap().unwrap();
        let head = s.head();
        let settled = s
            .settle(e.effect_id.clone(), fence, dispatch)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(settled.state, want);
        let ReadResult::Events { events, .. } = s.read_after(head, 10).await.unwrap() else {
            panic!("events expected")
        };
        assert_eq!(codes(&events).first().map(String::as_str), told);
    }
}

#[tokio::test]
async fn a_refused_input_retires_its_prompt_as_not_dispatched() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    let e = queue(&s, &det, "c1", prompt("in-1")).await;
    let fence = s.register_worker().await.unwrap();
    s.claim_next(fence).await.unwrap();
    emit(
        &s,
        Event::InputUpdated {
            input_id: "in-1".into(),
            turn_id: None,
            state: InputState::Refused,
        },
    )
    .await;
    let stored = s.effect(e.effect_id).await.unwrap().unwrap();
    assert_eq!(stored.state, EffectState::RetiredNotDispatched);
    assert_eq!(stored.reason.as_deref(), Some(effects::REASON_REFUSED));
}

#[tokio::test]
async fn confirmations_follow_the_evidence_table() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    let set_mode = queue(
        &s,
        &det,
        "c1",
        EffectPayload::SetMode {
            mode: "plan".to_owned(),
            authority: None,
        },
    )
    .await;
    let interrupt = queue(&s, &det, "c2", EffectPayload::Interrupt { authority: None }).await;
    let stop = queue(
        &s,
        &det,
        "c3",
        EffectPayload::Stop {
            how: StopMode::Graceful,
            authority: None,
        },
    )
    .await;
    let fence = s.register_worker().await.unwrap();
    while let Some(e) = s.claim_next(fence).await.unwrap() {
        s.settle(e.effect_id, fence, Dispatch::Sent).await.unwrap();
    }
    emit(
        &s,
        Event::ModeChanged {
            mode: "acceptEdits".to_owned(),
        },
    )
    .await;
    assert_eq!(state(&s, &set_mode).await, EffectState::Claimed);
    emit(
        &s,
        Event::ModeChanged {
            mode: "plan".to_owned(),
        },
    )
    .await;
    assert_eq!(state(&s, &set_mode).await, EffectState::Confirmed);
    assert_eq!(state(&s, &interrupt).await, EffectState::Claimed);
    emit(&s, interrupted(&interrupt, true)).await;
    assert_eq!(state(&s, &interrupt).await, EffectState::Confirmed);
    assert_eq!(state(&s, &stop).await, EffectState::Claimed);
    emit(
        &s,
        Event::SessionEnded {
            reason: "stopped".to_owned(),
            exit_code: Some(0),
        },
    )
    .await;
    assert_eq!(state(&s, &stop).await, EffectState::Confirmed);
}

// --- runtime requests -----------------------------------------------------------

#[tokio::test]
async fn answers_are_idempotent_per_request_and_confirmed_by_the_tool_node() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    emit(&s, approval("req-1", "node-1", &["allow", "deny"])).await;
    let req = s
        .runtime_request(S1.into(), "req-1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(req.state, RequestState::Open);
    assert_eq!(req.process_incarnation, Some(P1.into()));
    assert_eq!(req.deny_at, None, "wait policy");

    let mut claims = Vec::new();
    for (id, option) in [("c1", "allow"), ("c2", "deny"), ("c3", "maybe")] {
        let d = s
            .decide(envelope(&det, id), Caller::local(), move |d| {
                let claim =
                    d.claim_answer(&S1.into(), &"req-1".into(), option, ResolvedBy::Board)?;
                if claim == AnswerClaim::First {
                    d.enqueue(&S1.into(), answer("req-1", option))?;
                }
                Ok(accepted())
            })
            .await
            .unwrap();
        claims.push(d.effects.len());
    }
    assert_eq!(claims, [1, 0, 0], "one answer effect for three answers");
    let req = s
        .runtime_request(S1.into(), "req-1".into())
        .await
        .unwrap()
        .unwrap();
    let receipt = req.receipt.unwrap();
    assert_eq!(receipt.command_id, Some("c1".into()));
    assert_eq!(receipt.option_id, "allow");

    let fence = s.register_worker().await.unwrap();
    let e = s.claim_next(fence).await.unwrap().unwrap();
    s.settle(e.effect_id.clone(), fence, Dispatch::Sent)
        .await
        .unwrap();
    emit(
        &s,
        Event::ApprovalResolved {
            answered_by: None,
            request_id: "req-1".into(),
            option_id: "allow".to_owned(),
            by: ResolvedBy::Board,
        },
    )
    .await;
    emit(
        &s,
        Event::ToolCompleted {
            node_id: "node-1".into(),
            status: ToolStatus::Ok,
            files: Vec::new(),
            exit_code: Some(0),
            provider_refs: serde_json::Map::new(),
        },
    )
    .await;
    assert_eq!(state(&s, &e).await, EffectState::Confirmed);
    let req = s
        .runtime_request(S1.into(), "req-1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(req.state, RequestState::Resolved);
    assert_eq!(
        req.receipt.unwrap().command_id,
        Some("c1".into()),
        "first receipt stays"
    );
}

#[tokio::test]
async fn deny_after_denies_overdue_requests_with_the_provider_deny_option() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::DenyAfter { after_ms: 1_000 }).await;
    emit(
        &s,
        approval("req-1", "node-1", &["allow", "allow_always", "deny"]),
    )
    .await;
    emit(
        &s,
        approval("req-2", "node-2", &["accept", "accept_for_session"]),
    )
    .await;
    let req = s
        .runtime_request(S1.into(), "req-1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(req.deny_at, Some(req.requested_at + 1_000));

    det.advance(Duration::from_millis(999));
    assert!(s.deny_overdue().await.unwrap().is_empty());
    det.advance(Duration::from_millis(1));
    let head = s.head();
    let sent = s.deny_overdue().await.unwrap();
    assert_eq!(sent.len(), 1);
    let EffectPayload::Answer {
        request_id,
        decision,
        by,
        ..
    } = &sent[0].payload
    else {
        panic!("answer expected")
    };
    assert_eq!(request_id.as_str(), "req-1");
    assert_eq!(decision.option_id, "deny");
    assert_eq!(*by, ResolvedBy::Timeout);
    assert_eq!(sent[0].command_id, None);
    let receipt = s
        .runtime_request(S1.into(), "req-1".into())
        .await
        .unwrap()
        .unwrap()
        .receipt
        .unwrap();
    assert_eq!(receipt.by, ResolvedBy::Timeout);

    // req-2 has no deny option: it stays open (fail closed), the owner is told.
    let req2 = s
        .runtime_request(S1.into(), "req-2".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(req2.state, RequestState::Open);
    assert_eq!(req2.deny_at, None);
    let ReadResult::Events { events, .. } = s.read_after(head, 10).await.unwrap() else {
        panic!("events expected")
    };
    assert_eq!(codes(&events), ["offline_deny_impossible"]);
    // Nothing is denied twice.
    det.advance(Duration::from_secs(60));
    assert!(s.deny_overdue().await.unwrap().is_empty());
}

// --- process loss and recovery ----------------------------------------------------

#[tokio::test]
async fn a_lost_process_retires_queued_effects_and_expires_its_requests() {
    let det = Det::new();
    let s = running(&det, OfflineApprovalPolicy::Wait).await;
    let claimed = queue(&s, &det, "c1", prompt("in-1")).await;
    let queued = queue(&s, &det, "c2", prompt("in-2")).await;
    let interrupt = queue(&s, &det, "c3", EffectPayload::Interrupt { authority: None }).await;
    let fence = s.register_worker().await.unwrap();
    s.claim_next(fence).await.unwrap().unwrap();
    emit(&s, approval("req-1", "node-1", &["allow", "deny"])).await;

    let told = s
        .process_ended(P1.into(), "exit 1".to_owned())
        .await
        .unwrap();
    assert_eq!(state(&s, &claimed).await, EffectState::OutcomeUnknown);
    assert_eq!(state(&s, &queued).await, EffectState::RetiredNotDispatched);
    assert_eq!(
        state(&s, &interrupt).await,
        EffectState::RetiredNotDispatched
    );
    assert_eq!(
        codes(&told),
        ["delivery_unknown", "input.updated", "approval.expired"]
    );
    let EventBody::Known(Event::ApprovalExpired { reason, .. }) = &told[2].event else {
        panic!("approval.expired expected")
    };
    assert_eq!(*reason, ExpiredReason::ProcessLost);
    assert_eq!(s.live_process(S1.into()).await.unwrap(), None);
    // New commands find no process.
    let err = s
        .decide(envelope(&det, "c4"), Caller::local(), |d| {
            d.enqueue(&S1.into(), EffectPayload::Interrupt { authority: None })?;
            Ok(accepted())
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NoProcess(_)));
}

/// A station with one queued and one claimed effect, an open request and a
/// running turn, closed without any cleanup (a crash).
async fn crashed_station(det: &Det, path: &std::path::Path) -> (Effect, Effect) {
    let s = Store::open(path, det.clock(), det.ids(), StoreConfig::default())
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
    emit(&s, approval("req-1", "node-1", &["allow", "deny"])).await;
    let claimed = queue(&s, det, "c1", prompt("in-1")).await;
    let queued = queue(&s, det, "c2", prompt("in-2")).await;
    let fence = s.register_worker().await.unwrap();
    s.claim_next(fence).await.unwrap().unwrap();
    (claimed, queued)
}

#[tokio::test]
async fn start_recovery_settles_everything_of_dead_processes_before_admission() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("station.db");
    let det = Det::new();
    let (claimed, queued) = crashed_station(&det, &path).await;

    let s = Store::open(&path, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    assert!(matches!(
        s.decide(envelope(&det, "c9"), Caller::local(), |_| Ok(accepted()))
            .await
            .unwrap_err(),
        StoreError::NotRecovered
    ));
    let head = s.head();
    let report = s.recover(Vec::new()).await.unwrap();
    assert_eq!(
        report,
        RecoveryReport {
            dead_processes: 1,
            retired: 1,
            unknown: 1,
            expired_requests: 1,
            lost_sessions: 1,
        }
    );
    assert_eq!(state(&s, &claimed).await, EffectState::OutcomeUnknown);
    assert_eq!(state(&s, &queued).await, EffectState::RetiredNotDispatched);
    let req = s
        .runtime_request(S1.into(), "req-1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(req.state, RequestState::Expired);
    let ReadResult::Events { events, .. } = s.read_after(head, 100).await.unwrap() else {
        panic!("events expected")
    };
    assert_eq!(
        codes(&events),
        [
            "input.updated",
            "delivery_unknown",
            "approval.expired",
            "turn.completed",
            "run.completed",
            "session.state",
        ]
    );
    let EventBody::Known(Event::ApprovalExpired { reason, .. }) = &events[2].event else {
        panic!("approval.expired expected")
    };
    assert_eq!(*reason, ExpiredReason::NotResumable);
    let summary = s.summary(S1.into()).await.unwrap().unwrap();
    assert_eq!(summary.state, SessionState::Lost);
    assert_eq!(summary.current_run, None);
    assert!(summary.open_requests.is_empty());
    // Nothing is sent again, and commands are admitted now.
    let fence = s.register_worker().await.unwrap();
    assert!(s.claim_next(fence).await.unwrap().is_none());
    s.decide(envelope(&det, "c9"), Caller::local(), |_| Ok(accepted()))
        .await
        .unwrap();
    // A second recovery finds nothing to do.
    let again = Store::open(&path, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    assert_eq!(
        again.recover(Vec::new()).await.unwrap(),
        RecoveryReport::default()
    );
    assert_eq!(all_events(&again).await.len(), all_events(&s).await.len());
}

#[tokio::test]
async fn recovery_keeps_the_queue_and_requests_of_a_surviving_process() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("station.db");
    let det = Det::new();
    let (claimed, queued) = crashed_station(&det, &path).await;

    let s = Store::open(&path, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let report = s.recover(vec![P1.into()]).await.unwrap();
    assert_eq!(report.dead_processes, 0);
    assert_eq!(report.lost_sessions, 0);
    assert_eq!(report.retired, 0);
    assert_eq!(report.unknown, 1, "a claim of the old station is unknown");
    assert_eq!(state(&s, &claimed).await, EffectState::OutcomeUnknown);
    assert_eq!(state(&s, &queued).await, EffectState::Queued);
    let req = s
        .runtime_request(S1.into(), "req-1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(req.state, RequestState::Open);
    assert_eq!(
        s.summary(S1.into()).await.unwrap().unwrap().state,
        SessionState::Starting
    );
}

// --- interrupt evidence (ADR-001 §3.1) ---------------------------------------------

#[path = "effects_interrupt_tests.rs"]
mod interrupt;

#[path = "m2_gate_tests.rs"]
mod m2_gate;

#[path = "answer_input_tests.rs"]
mod answer_input;
use interrupt::interrupted;
#[path = "request_time_tests.rs"]
mod request_time;
