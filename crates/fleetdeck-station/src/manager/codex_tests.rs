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

fn codex_spec(cwd: &str) -> SessionSpec {
    SessionSpec {
        provider_instance: "codex".into(),
        cwd: cwd.to_owned(),
        mode: None,
        model: None,
        title: None,
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
