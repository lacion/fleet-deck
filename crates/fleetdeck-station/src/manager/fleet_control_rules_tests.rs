//! The rest of the station side of fleet control (ADR-008 §5, §6.2): no
//! implicit continuation (test 11a), the bypass rule of resume and set mode
//! (test 14), the answer rules (test 12) and `coordinator.policy_now`.

use super::*;

/// Starts a session, waits until it is idle, and hibernates it.
async fn hibernated(st: &mut Station) -> SessionId {
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    st.wait_turns(&s, 1).await;
    st.det.advance(Duration::from_millis(30 * 60_000 + 1));
    let store = st.manager.store().clone();
    let policy = st.manager.policy().clone();
    let swept = hibernate::sweep(&store, &policy, &Bindings::default(), 30)
        .await
        .unwrap();
    assert_eq!(swept, 1);
    st.wait_for("session ended", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    st.settled(&s).await;
    s
}

// Test 11a: no implicit continuation by a coordinator.
#[tokio::test]
async fn a_coordinator_input_never_resumes_a_hibernated_session_but_a_board_input_does() {
    let mut st = Station::new(4).await;
    let s = hibernated(&mut st).await;
    st.switch(true);
    let input = |kind: &str| match kind {
        "prompt" => SessionCommand::Prompt {
            session_id: s.clone(),
            input: text("wake"),
        },
        "steer" => SessionCommand::Steer {
            session_id: s.clone(),
            input: text("wake"),
        },
        _ => follow_up(&s, "wake"),
    };
    for kind in ["prompt", "steer", "follow_up"] {
        for (origin, authority) in [
            (CommandOrigin::Coordinator, Some(authority("coord-1"))),
            // A coordinator input without authority (an ADR-006 coordinator).
            (CommandOrigin::Coordinator, None),
            // Coordinator mail that the deck routed as owner mail.
            (CommandOrigin::Mail, Some(authority("coord-1"))),
            (CommandOrigin::Board, Some(authority("coord-1"))),
        ] {
            let d = st.send_as(origin, authority, input(kind)).await;
            let (code, message) = st.refused(&d).await;
            assert_eq!(code, RejectCode::InvalidState, "{kind}");
            assert_eq!(message, "the session has ended", "{kind}");
        }
    }
    assert_eq!(st.state(&s).await, SessionState::Exited);
    assert!(
        st.effects(&s)
            .await
            .iter()
            .all(|e| !matches!(e.payload, EffectPayload::Resume { .. })),
        "nothing was resumed"
    );
    // Owner mail without authority, and a board input, still resume it (DL-180).
    let d = st
        .send_as(CommandOrigin::Mail, None, follow_up(&s, "owner mail"))
        .await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    assert!(matches!(d.effects[0].payload, EffectPayload::Resume { .. }));
    assert_eq!(st.state(&s).await, SessionState::Starting);
    st.manager.shutdown(StopMode::Kill).await;

    let mut st = Station::new(4).await;
    let s = hibernated(&mut st).await;
    let d = st
        .send_as(CommandOrigin::Board, None, follow_up(&s, "board"))
        .await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    assert!(matches!(d.effects[0].payload, EffectPayload::Resume { .. }));
    st.manager.shutdown(StopMode::Kill).await;
}

pub(super) fn row(
    id: &str,
    provider: ProviderKind,
    instance: &str,
    mode: Option<&str>,
    cwd: &str,
) -> NewSession {
    NewSession {
        session_id: id.into(),
        provider,
        provider_instance: instance.into(),
        cwd: cwd.to_owned(),
        worktree: None,
        branch: None,
        mode: mode.map(str::to_owned),
        title: None,
        offline_approval_policy: OfflineApprovalPolicy::Wait,
    }
}

// Test 14 (station part): a coordinator resume or mode change into a bypass
// mode is refused; the ceiling is the deck's check.
#[tokio::test]
async fn a_coordinator_cannot_resume_or_set_a_session_into_a_bypass_mode() {
    let mut st = Station::new(12).await;
    st.switch(true);
    let ws = st.workspace();
    let store = st.manager.store().clone();
    for (id, provider, instance, mode) in [
        (
            "ended-claude",
            ProviderKind::Claude,
            "claude",
            "bypassPermissions",
        ),
        (
            "ended-codex",
            ProviderKind::Codex,
            "codex",
            "never/danger-full-access",
        ),
    ] {
        store
            .create_session(row(id, provider, instance, Some(mode), &ws))
            .await
            .unwrap();
        let d = st
            .coord(SessionCommand::Resume {
                session_id: id.into(),
            })
            .await;
        let (code, message) = st.refused(&d).await;
        assert_eq!(code, RejectCode::Forbidden, "{id}");
        assert_eq!(message, "a coordinator cannot use a bypass mode", "{id}");
        // The owner's own resume is not this rule's business.
        let d = st
            .send(SessionCommand::Resume {
                session_id: id.into(),
            })
            .await;
        assert_ne!(
            d.receipt.reject.map(|r| r.message),
            Some("a coordinator cannot use a bypass mode".to_owned()),
            "{id}"
        );
    }
    // A mode that is not a bypass mode passes this rule: the refusal comes
    // from the ordinary resume admission, not from `forbidden`.
    store
        .create_session(row(
            "ended-safe",
            ProviderKind::Codex,
            "codex",
            Some("untrusted/read-only"),
            &ws,
        ))
        .await
        .unwrap();
    let d = st
        .coord(SessionCommand::Resume {
            session_id: "ended-safe".into(),
        })
        .await;
    assert_ne!(d.receipt.reject.unwrap().code, RejectCode::Forbidden);

    // `set_mode` on a live session.
    let deck = FakeDeck::new(Reply::Valid);
    st.link_deck(&deck);
    let claude = st.start(&ws, Some("hello")).await;
    st.wait_turns(&claude, 1).await;
    let codex_spec = {
        let mut spec = spec(&ws, "codex");
        spec.mode = None;
        spec
    };
    let codex = st.start_with(codex_spec, Some("hello")).await;
    st.wait_turns(&codex, 1).await;
    for (session, mode) in [
        (&claude, "bypassPermissions"),
        (&codex, "never/danger-full-access"),
    ] {
        let d = st
            .coord(SessionCommand::SetMode {
                session_id: session.clone(),
                mode: mode.to_owned(),
            })
            .await;
        let (code, message) = st.refused(&d).await;
        assert_eq!(code, RejectCode::Forbidden, "{mode}");
        assert_eq!(message, "a coordinator cannot use a bypass mode", "{mode}");
    }
    let d = st
        .coord(SessionCommand::SetMode {
            session_id: claude.clone(),
            mode: "plan".to_owned(),
        })
        .await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    assert!(matches!(
        d.effects[0].payload,
        EffectPayload::SetMode {
            authority: Some(_),
            ..
        }
    ));
    st.manager.shutdown(StopMode::Kill).await;
}

// Test 12: the first answer wins, with a clear result for a coordinator.
#[tokio::test]
async fn a_coordinator_answer_after_a_board_answer_is_already_answered_and_a_board_repeat_is_not() {
    let mut st = Station::new(4).await;
    st.switch(true);
    let deck = FakeDeck::new(Reply::Valid);
    st.link_deck(&deck);
    let ws = st.workspace();
    let s = st.start(&ws, Some("ask ls")).await;
    let request = st.wait_for("approval", approval).await;
    let d = st
        .send_as(CommandOrigin::Board, None, answer(&s, &request, "allow"))
        .await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    assert_eq!(d.effects.len(), 1);

    // A coordinator is told who won.
    let d = st.coord(answer(&s, &request, "allow")).await;
    let (code, message) = st.refused(&d).await;
    assert_eq!(code, RejectCode::AlreadyAnswered);
    assert_eq!(message, "already answered by board");
    // A board answer after a board answer keeps today's result: accepted,
    // nothing sent.
    let d = st
        .send_as(CommandOrigin::Board, None, answer(&s, &request, "allow"))
        .await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    assert!(d.effects.is_empty());
    st.wait_turns(&s, 1).await;
    assert!(
        deck.asked().is_empty(),
        "the board answer needs no authority"
    );

    // The other way round: the coordinator is first, the board repeats.
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("ask ls"),
    })
    .await;
    let second = st
        .wait_for("second approval", |e| approval(e).filter(|r| *r != request))
        .await;
    let d = st.coord(answer(&s, &second, "allow")).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    assert!(matches!(
        &d.effects[0].payload,
        EffectPayload::Answer {
            by: ResolvedBy::Coordinator,
            authority: Some(_),
            ..
        }
    ));
    let d = st.coord(answer(&s, &second, "allow")).await;
    assert_eq!(
        st.refused(&d).await,
        (
            RejectCode::AlreadyAnswered,
            "already answered by coordinator".to_owned()
        )
    );
    let d = st
        .send_as(CommandOrigin::Board, None, answer(&s, &second, "deny"))
        .await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    assert!(d.effects.is_empty());
    st.wait_turns(&s, 2).await;
    st.manager.shutdown(StopMode::Kill).await;
}

// ADR-008 §5.3: the answer of a coordinator to a structured Codex question
// answers every asked question; a board answer keeps today's rule.
#[tokio::test]
async fn a_coordinator_answers_every_question_of_a_codex_question_request() {
    use fleetdeck_proto::event::{ApprovalOption, RequestKind, ResponseChannel};
    let mut st = Station::new(4).await;
    st.switch(true);
    let store = st.manager.store().clone();
    let ws = st.workspace();
    let questions = serde_json::json!({"questions": [{"id": "a"}, {"id": "b"}]});
    for (id, provider, instance) in [("codex-q", ProviderKind::Codex, "codex")] {
        store
            .create_session(row(id, provider, instance, None, &ws))
            .await
            .unwrap();
        store
            .process_started(id.into(), format!("proc-{id}").into(), None)
            .await
            .unwrap();
        let ask = |request: &str| Event::ApprovalRequested {
            request_id: request.into(),
            node_id: None,
            request_kind: RequestKind::Question,
            title: "Questions".to_owned(),
            details: "two".to_owned(),
            input: Some(questions.clone()),
            options: vec![ApprovalOption {
                option_id: "answer".to_owned(),
                label: "answer".to_owned(),
                kind: None,
            }],
            response: ResponseChannel::RpcReply,
            provider_refs: serde_json::Map::new(),
        };
        store
            .append(
                id.into(),
                vec![
                    ask(&format!("{id}-1")).into(),
                    ask(&format!("{id}-2")).into(),
                    ask(&format!("{id}-3")).into(),
                ],
            )
            .await
            .unwrap();
    }
    let reply = |session: &str, request: &str, ids: &[&str]| {
        let answers: serde_json::Map<String, serde_json::Value> = ids
            .iter()
            .map(|i| ((*i).to_owned(), serde_json::json!({"answers": ["x"]})))
            .collect();
        SessionCommand::Answer {
            session_id: session.into(),
            request_id: request.into(),
            decision: ApprovalDecision {
                option_id: "answer".to_owned(),
                updated_input: Some(serde_json::json!({ "answers": answers })),
                text: None,
                answered_by: None,
            },
        }
    };
    // Codex: a coordinator that leaves a question out is refused before the
    // claim, so the request stays open for a complete answer.
    let d = st.coord(reply("codex-q", "codex-q-1", &["a"])).await;
    let (code, message) = st.refused(&d).await;
    assert_eq!(code, RejectCode::InvalidCommand);
    assert_eq!(message, "every question needs an answer");
    // An answer for a question that was not asked keeps the provider rule.
    let d = st.coord(reply("codex-q", "codex-q-1", &["a", "zz"])).await;
    assert_eq!(st.refused(&d).await.0, RejectCode::InvalidCommand);
    let d = st.coord(reply("codex-q", "codex-q-1", &["a", "b"])).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    // A board answer keeps today's rule: a partial answer is accepted.
    let d = st
        .send_as(
            CommandOrigin::Board,
            None,
            reply("codex-q", "codex-q-2", &["a"]),
        )
        .await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    st.manager.shutdown(StopMode::Kill).await;
}

// ADR-008 §6.2: `coordinator.policy_now`.
#[test]
fn policy_now_answers_the_live_setting_and_whether_the_caller_is_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let policy = |on: bool| {
        Policy::from_config(&test_config(
            tmp.path(),
            &format!("allow_fleet_control = {on}\n"),
        ))
    };
    let bindings = Bindings::default();
    let ask = |session: &str, p: &Policy| {
        policy_now_answer(
            p,
            &bindings,
            &PolicyNow {
                caller_session: session.into(),
                binding: None,
            },
        )
    };
    bindings.insert(
        "d1".to_owned(),
        coordinator_binding(
            &"coord-1".into(),
            &[ToolClass::Inspect],
            BindingState::Active,
        ),
    );
    bindings.insert(
        "d2".to_owned(),
        coordinator_binding(
            &"pending-1".into(),
            &[ToolClass::Inspect],
            BindingState::Pending,
        ),
    );
    let mut fleet_only =
        coordinator_binding(&"tools-1".into(), &[ToolClass::Own], BindingState::Active);
    fleet_only.grant_id = String::new();
    bindings.insert("d3".to_owned(), fleet_only);
    for (on, session, bound) in [
        (true, "coord-1", true),
        (false, "coord-1", true),
        (true, "unknown", false),
        (false, "unknown", false),
        (true, "pending-1", false),
        (true, "tools-1", false),
    ] {
        let answer = ask(session, &policy(on));
        assert_eq!(answer.fleet_control, on, "{session}");
        assert_eq!(answer.bound, bound, "{session}");
    }
    // The setting is read live: a revoked binding is not bound any more.
    bindings.revoke_session(&"coord-1".into());
    assert!(!ask("coord-1", &policy(true)).bound);
}

// ADR-008 §3: the live value reaches the intake gate with each publish.
#[tokio::test]
async fn a_publish_updates_the_live_setting_that_the_intake_reads() {
    let st = Station::new(4).await;
    let bindings = st.manager.coordinator_bindings().clone();
    assert!(!bindings.fleet_control() && !st.manager.policy().allow_fleet_control());
    let tmp = tempfile::tempdir().unwrap();
    for on in [true, false, true] {
        let config = test_config(tmp.path(), &format!("allow_fleet_control = {on}\n"));
        let prepared = st.manager.prepare(&config).unwrap();
        st.manager.publish(prepared);
        assert_eq!(bindings.fleet_control(), on);
        assert_eq!(st.manager.policy().allow_fleet_control(), on);
    }
    st.manager.shutdown(StopMode::Kill).await;
}
