//! The session manager against `fake-codex app-server` scenario `sim` (M2
//! P5-2): a Codex session goes through the same decide → commit → effect →
//! adapter path as a Claude session, and its effects confirm from the
//! Codex events.

use super::*;

/// Waits until `count` events match `pred`.
async fn wait_count(st: &Station, what: &str, count: usize, pred: impl Fn(&Event) -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let events = st.events().await;
        let n = events
            .iter()
            .filter(|e| matches!(&e.event, EventBody::Known(k) if pred(k)))
            .count();
        if n >= count {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{n} of {count} {what}: {:#?}",
            kinds(&events)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn turn_done(e: &Event) -> bool {
    matches!(
        e,
        Event::TurnCompleted {
            status: TurnStatus::Done,
            ..
        }
    )
}

pub(in crate::manager) fn codex_spec(cwd: &str) -> SessionSpec {
    SessionSpec {
        provider_instance: "codex".into(),
        cwd: cwd.to_owned(),
        mode: None,
        model: None,
        title: None,
        coordinator: None,
        launch: None,
        extra_env: None,
    }
}

#[tokio::test]
async fn a_codex_session_runs_approvals_steer_interrupt_and_background_work() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let s = st.start_with(codex_spec(&ws), Some("ask touch x")).await;
    let request = st.wait_for("approval", approval).await;
    st.accepted(answer(&s, &request, "accept")).await;
    wait_count(&st, "done turns", 1, turn_done).await;

    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("wait for a steer"),
    })
    .await;
    wait_count(
        &st,
        "streams",
        1,
        |e| matches!(e, Event::MessageDelta { text, .. } if text == "waiting"),
    )
    .await;
    st.accepted(SessionCommand::Steer {
        session_id: s.clone(),
        input: text("stop now"),
    })
    .await;
    wait_count(
        &st,
        "steered answer",
        1,
        |e| matches!(e, Event::MessageCompleted { text, .. } if text == "steered: stop now"),
    )
    .await;
    wait_count(&st, "done turns", 2, turn_done).await;

    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("wait for the interrupt"),
    })
    .await;
    wait_count(
        &st,
        "streams",
        2,
        |e| matches!(e, Event::MessageDelta { text, .. } if text == "waiting"),
    )
    .await;
    st.accepted(SessionCommand::Interrupt {
        session_id: s.clone(),
    })
    .await;
    wait_count(&st, "interrupted turn", 1, |e| {
        matches!(
            e,
            Event::TurnCompleted {
                status: TurnStatus::Interrupted,
                ..
            }
        )
    })
    .await;

    // Background work: the driver polls until the terminal finishes.
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("bg sleep 1"),
    })
    .await;
    wait_count(&st, "pending background", 1, |e| {
        matches!(
            e,
            Event::RunCompleted {
                background_pending: true,
                ..
            }
        )
    })
    .await;
    wait_count(
        &st,
        "background listed",
        1,
        |e| matches!(e, Event::BackgroundUpdated { tasks } if !tasks.is_empty()),
    )
    .await;
    wait_count(
        &st,
        "background cleared",
        1,
        |e| matches!(e, Event::BackgroundUpdated { tasks } if tasks.is_empty()),
    )
    .await;

    st.accepted(SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Graceful,
    })
    .await;
    st.wait_for("session ended", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    let states = st.settled(&s).await;
    assert_eq!(
        states,
        vec![
            ("start", EffectState::Confirmed),
            ("answer", EffectState::Confirmed),
            ("prompt", EffectState::Confirmed),
            ("steer", EffectState::Confirmed),
            ("prompt", EffectState::Confirmed),
            ("interrupt", EffectState::Confirmed),
            ("prompt", EffectState::Confirmed),
            ("stop", EffectState::Confirmed),
        ]
    );
    assert_eq!(st.state(&s).await, SessionState::Exited);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_codex_start_with_an_unknown_mode_spawns_nothing() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let mut spec = codex_spec(&ws);
    spec.mode = Some("bypassPermissions".to_owned());
    let s = st.start_with(spec, Some("hello")).await;
    // The adapter refuses the mode before it spawns a process: the start
    // retires as not dispatched and the session ends, never running in a
    // mode that the owner did not choose.
    let states = st.settled(&s).await;
    assert_eq!(states, vec![("start", EffectState::RetiredNotDispatched)]);
    st.wait_for("start failure", |e| match e {
        Event::Error { code, message, .. } if code == "start_failed" => {
            message.contains("bypassPermissions").then_some(())
        }
        _ => None,
    })
    .await;
    assert_eq!(st.state(&s).await, SessionState::Exited);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_codex_question_takes_its_answers_and_an_approval_refuses_an_input_edit() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let s = st
        .start_with(codex_spec(&ws), Some("question Which color?"))
        .await;
    let request = st.wait_for("question", approval).await;
    // The answers of a question travel in `updated_input`; for Codex that is
    // an answer, not an input edit, which Codex does not support (DL-068).
    let mut command = answer(&s, &request, "answer");
    if let SessionCommand::Answer { decision, .. } = &mut command {
        decision.updated_input = Some(serde_json::json!({
            "answers": {"choice": {"answers": ["Blue"]}}
        }));
    }
    st.accepted(command).await;
    wait_count(
        &st,
        "answered turn",
        1,
        |e| matches!(e, Event::MessageCompleted { text, .. } if text == "answered: Blue"),
    )
    .await;
    wait_count(&st, "done turns", 1, turn_done).await;

    // A command approval with an edited input stays an input edit.
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("ask touch x"),
    })
    .await;
    let approvals = |e: &Event| {
        matches!(
            e,
            Event::ApprovalRequested {
                request_kind: fleetdeck_proto::event::RequestKind::Command,
                ..
            }
        )
    };
    wait_count(&st, "command approval", 1, approvals).await;
    let command_request = st
        .events()
        .await
        .iter()
        .find_map(|e| match &e.event {
            EventBody::Known(k) if approvals(k) => approval(k),
            _ => None,
        })
        .unwrap();
    let mut edit = answer(&s, &command_request, "accept");
    if let SessionCommand::Answer { decision, .. } = &mut edit {
        decision.updated_input = Some(serde_json::json!({"command": "rm -rf x"}));
    }
    assert_eq!(st.rejected(edit).await, RejectCode::Unsupported);
    st.accepted(answer(&s, &command_request, "accept")).await;
    wait_count(&st, "done turns", 2, turn_done).await;

    st.accepted(SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Graceful,
    })
    .await;
    let states = st.settled(&s).await;
    assert_eq!(
        states,
        vec![
            ("start", EffectState::Confirmed),
            ("answer", EffectState::Confirmed),
            ("prompt", EffectState::Confirmed),
            ("answer", EffectState::Confirmed),
            ("stop", EffectState::Confirmed),
        ]
    );
    st.manager.shutdown(StopMode::Kill).await;
}

// --- answer admission before the claim (M2 gate correction, DL-070) ----------------

/// Submits `command` under the fixed command id `id`.
async fn submit_as(st: &Station, id: &str, command: SessionCommand) -> crate::store::Decided {
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from(id),
        target_incarnation: None,
        retry_until: i64::try_from(st.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Local,
        command,
    };
    st.manager.submit(envelope, Caller::local()).await.unwrap()
}

/// `command` (an answer) with `updated_input` and `text`.
fn with(
    mut command: SessionCommand,
    input: Option<serde_json::Value>,
    message: Option<&str>,
) -> SessionCommand {
    if let SessionCommand::Answer { decision, .. } = &mut command {
        decision.updated_input = input;
        decision.text = message.map(str::to_owned);
    }
    command
}

fn answered_blue(e: &Event) -> bool {
    matches!(e, Event::MessageCompleted { text, .. } if text == "answered: Blue")
}

/// The request is open and unanswered, with its first deadline.
async fn assert_unclaimed(st: &Station, request: &RequestId, deny_at: Option<i64>) {
    let req = st
        .manager
        .store()
        .runtime_request(request.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(req.state, crate::store::RequestState::Open);
    assert_eq!(req.receipt, None);
    assert_eq!(req.deny_at, deny_at);
}

#[tokio::test]
async fn a_malformed_first_answer_is_refused_and_a_corrected_one_is_delivered_once() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let s = st
        .start_with(codex_spec(&ws), Some("question Which color?"))
        .await;
    let request = st.wait_for("question", approval).await;
    let deny_at = st
        .manager
        .store()
        .runtime_request(request.clone())
        .await
        .unwrap()
        .unwrap()
        .deny_at;
    let malformed = [
        None,
        Some(serde_json::json!({"answers": {}})),
        Some(serde_json::json!({"answers": {"colour": {"answers": ["Blue"]}}})),
        Some(serde_json::json!({"answers": {"choice": {"answers": [7]}}})),
    ];
    for (n, input) in malformed.into_iter().enumerate() {
        let id = format!("bad-{n}");
        let command = with(answer(&s, &request, "answer"), input, None);
        let d = submit_as(&st, &id, command.clone()).await;
        assert_eq!(d.receipt.state, ReceiptState::Rejected, "{id}");
        assert_eq!(
            d.receipt.reject.as_ref().map(|r| r.code),
            Some(RejectCode::InvalidCommand),
            "{id}"
        );
        assert!(d.effects.is_empty(), "{id}");
        assert_unclaimed(&st, &request, deny_at).await;
        // The same command id gets its stored rejection again.
        let again = submit_as(&st, &id, command).await;
        assert!(again.duplicate);
        assert_eq!(again.receipt, d.receipt);
    }
    assert!(
        st.effect_states(&s)
            .await
            .iter()
            .all(|(k, _)| *k != "answer"),
        "a refused answer queues no effect"
    );

    // A new command id with the answers takes the still open question.
    let blue = Some(serde_json::json!({"answers": {"choice": {"answers": ["Blue"]}}}));
    let good = with(answer(&s, &request, "answer"), blue, None);
    let d = submit_as(&st, "good", good.clone()).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    assert_eq!(d.effects.len(), 1);
    wait_count(&st, "answered turn", 1, answered_blue).await;
    wait_count(&st, "done turns", 1, turn_done).await;
    // The accepted command repeats its receipt. A later answer, even a
    // malformed one, gets the first outcome (L3) without a payload check,
    // and sends nothing.
    let dup = submit_as(&st, "good", good).await;
    assert!(dup.duplicate && dup.effects.is_empty());
    let late = st.accepted(answer(&s, &request, "answer")).await;
    assert!(late.effects.is_empty());

    st.accepted(SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Graceful,
    })
    .await;
    let states = st.settled(&s).await;
    assert_eq!(
        states,
        vec![
            ("start", EffectState::Confirmed),
            ("answer", EffectState::Confirmed),
            ("stop", EffectState::Confirmed),
        ]
    );
    let delivered = st
        .events()
        .await
        .iter()
        .filter(|e| matches!(&e.event, EventBody::Known(k) if answered_blue(k)))
        .count();
    assert_eq!(delivered, 1, "the answer reached fake Codex once");
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_empty_free_answer_to_a_message_question_is_refused_before_the_claim() {
    use fleetdeck_proto::event::{ApprovalOption, RequestKind, ResponseChannel};

    let mut st = Station::new(4).await;
    let store = st.manager.store().clone();
    // A Codex message-channel question (an async `agentMessage` question),
    // which `fake-codex sim` does not ask: the store gets its events. No
    // adapter process exists, so the worker retires each answer unsent.
    let s = SessionId::from("codex-message");
    store
        .create_session(crate::store::NewSession {
            session_id: s.clone(),
            provider: ProviderKind::Codex,
            provider_instance: "codex".into(),
            cwd: st.workspace(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    store
        .process_started(s.clone(), "proc-message".into(), None)
        .await
        .unwrap();
    let question = |id: &str| Event::ApprovalRequested {
        request_id: id.into(),
        node_id: None,
        request_kind: RequestKind::Question,
        title: "Question".to_owned(),
        details: "Red or Blue?".to_owned(),
        input: Some(serde_json::json!({"title": "Question", "options": ["Red"]})),
        options: ["Red", "answer"]
            .iter()
            .map(|o| ApprovalOption {
                option_id: (*o).to_owned(),
                label: (*o).to_owned(),
                kind: None,
            })
            .collect(),
        response: ResponseChannel::Message,
        provider_refs: serde_json::Map::new(),
    };
    store
        .append(
            s.clone(),
            vec![question("q-1").into(), question("q-2").into()],
        )
        .await
        .unwrap();
    let (q1, q2) = (RequestId::from("q-1"), RequestId::from("q-2"));

    for message in [None, Some("")] {
        let command = with(answer(&s, &q1, "answer"), None, message);
        assert_eq!(st.rejected(command).await, RejectCode::InvalidCommand);
        assert_unclaimed(&st, &q1, None).await;
    }
    let d = st
        .accepted(with(answer(&s, &q1, "answer"), None, Some("Blue")))
        .await;
    assert_eq!(d.effects.len(), 1);
    // An offered option needs no text; an unknown one keeps its refusal.
    assert_eq!(
        st.rejected(answer(&s, &q2, "Green")).await,
        RejectCode::InvalidCommand
    );
    let d = st.accepted(answer(&s, &q2, "Red")).await;
    assert_eq!(d.effects.len(), 1);

    // The worker proves that no answer left the station: both requests
    // open again for a new command (DL-070 decision 13).
    let states = st.settled(&s).await;
    assert_eq!(
        states,
        vec![
            ("answer", EffectState::RetiredNotDispatched),
            ("answer", EffectState::RetiredNotDispatched),
        ]
    );
    assert_unclaimed(&st, &q1, None).await;
    assert_unclaimed(&st, &q2, None).await;
    st.manager.shutdown(StopMode::Kill).await;
}
