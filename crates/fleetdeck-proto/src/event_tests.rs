use super::*;
use crate::capability::Capabilities;
use serde_json::json;

fn refs() -> ProviderRefs {
    let mut refs = ProviderRefs::new();
    refs.insert("session_id".into(), json!("native-1"));
    refs
}

/// One sample of every known kind.
fn samples() -> Vec<Event> {
    let turn = TurnId::from("turn-1");
    let node = NodeId::from("node-1");
    let req = RequestId::from("req-1");
    vec![
        Event::SessionStarted {
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            provider_version: Some("2.1.0".into()),
            model: None,
            cwd: "/w".into(),
            mode: Some("default".into()),
            capabilities: Box::new(Capabilities::minimal()),
            resumed: None,
            provider_refs: refs(),
            title: None,
            branch: None,
            repo: None,
        },
        Event::SessionStateChanged {
            state: SessionState::Running,
            detail: None,
        },
        Event::RunStarted {
            run_id: "run-1".into(),
            input_summary: "hi".into(),
            origin: RunOrigin::Local,
        },
        Event::TurnStarted {
            run_id: "run-1".into(),
            turn_id: turn.clone(),
            provider_refs: refs(),
        },
        Event::MessageDelta {
            turn_id: turn.clone(),
            parent_node_id: None,
            text: "h".into(),
        },
        Event::MessageCompleted {
            turn_id: turn.clone(),
            parent_node_id: Some(node.clone()),
            text: "hi".into(),
        },
        Event::ReasoningDelta {
            turn_id: turn.clone(),
            text: "t".into(),
        },
        Event::ToolStarted {
            node_id: node.clone(),
            parent_node_id: None,
            tool_kind: ToolKind::Shell,
            title: "ls".into(),
            input_summary: "ls".into(),
            files: vec![],
            provider_refs: refs(),
        },
        Event::ToolOutput {
            node_id: node.clone(),
            chunk: "a".into(),
            truncated: false,
        },
        Event::ToolCompleted {
            node_id: node.clone(),
            status: ToolStatus::Ok,
            files: vec!["a.txt".into()],
            exit_code: Some(0),
        },
        Event::FileChanged {
            path: "a.txt".into(),
            change: FileChange::Added,
            node_id: Some(node.clone()),
        },
        Event::ApprovalRequested {
            request_id: req.clone(),
            node_id: Some(node),
            request_kind: RequestKind::Command,
            title: "Run ls".into(),
            details: "ls -la".into(),
            input: Some(json!({"command": "ls"})),
            options: vec![ApprovalOption {
                option_id: "allow".into(),
                label: "Allow".into(),
            }],
            response: ResponseChannel::RpcReply,
            provider_refs: refs(),
        },
        Event::ApprovalResolved {
            request_id: req.clone(),
            option_id: "allow".into(),
            by: ResolvedBy::Board,
        },
        Event::ApprovalExpired {
            request_id: req,
            reason: ExpiredReason::NotResumable,
        },
        Event::PlanProposed {
            plan_id: "plan-1".into(),
            turn_id: None,
            markdown: "# plan".into(),
        },
        Event::UsageUpdated {
            usage: Usage::default(),
            rate_limits: vec![RateLimit {
                scope: "five_hour".into(),
                used_percent: Some(12.5),
                resets_at: Some(1),
            }],
        },
        Event::TurnCompleted {
            turn_id: turn.clone(),
            status: TurnStatus::Done,
            usage: Some(Usage {
                input_tokens: Some(1),
                output_tokens: Some(2),
                cost_usd: None,
                cost_inferred: false,
            }),
        },
        Event::TurnFinalized {
            turn_id: turn.clone(),
            checkpoint: Some(TurnCheckpoint {
                worktree_id: Some("0f".into()),
                scope: "tracked".into(),
                start: CheckpointSide::Available {
                    commit: "abc".into(),
                },
                end: CheckpointSide::Unavailable {
                    reason: "queued_input".into(),
                },
            }),
            diff_summary: None,
        },
        Event::RunCompleted {
            run_id: "run-1".into(),
            status: TurnStatus::Interrupted,
            background_pending: true,
        },
        Event::ModeChanged {
            mode: "plan".into(),
        },
        Event::ModelChanged {
            model: "haiku".into(),
        },
        Event::InterruptResult {
            control_id: Some("e1".into()),
            accepted: true,
            message: None,
        },
        Event::InputUpdated {
            input_id: "in-1".into(),
            turn_id: Some(turn),
            state: InputState::Started,
        },
        Event::BackgroundUpdated {
            tasks: vec![BackgroundTask {
                task_id: "t1".into(),
                task_kind: "local_bash".into(),
            }],
        },
        Event::Error {
            code: "usage_limit".into(),
            message: "limit".into(),
            retry_at: Some(1_700_000_000_000),
        },
        Event::SessionEnded {
            reason: "stopped".into(),
            exit_code: Some(0),
        },
        Event::RevertUpdated {
            revert_id: "rv-1".into(),
            turn_id: "turn-1".into(),
            turns: 2,
            state: crate::event::RevertState::ReconciliationRequired,
            reason: Some("rollback_uncertain".into()),
        },
        Event::SessionRenamed {
            title: Some("docs".into()),
        },
        Event::SessionNamed {
            callsign: "otter-ab12".into(),
            worktree: Some("/r/app--fd-otter-ab12".into()),
        },
        Event::OutputGap {
            from: 10,
            to: 65_546,
            reason: crate::event::OutputGapReason::OutputFrameTooLarge,
        },
        Event::AutoContinueChanged { enabled: true },
        Event::AutoContinueSkipped {
            reason: "newer_owner_work".into(),
        },
        Event::CoordinatorBound {
            binding_id: "b-1".into(),
            grant_id: "g-1".into(),
            generation: 1,
            provider_instance: "claude".into(),
            native_id: "native-1".into(),
        },
        Event::CoordinatorEnded {
            binding_id: "b-1".into(),
            generation: 1,
            reason: "idle".into(),
        },
        Event::SessionTerminal {
            shell_id: Some("tui-1".into()),
            pid: Some(4242),
            reason: Some("handover requested".into()),
        },
        Event::SlashCommands {
            commands: vec!["review".into(), "compact".into()],
        },
        Event::SessionNotice {
            code: "v1_plugin".into(),
            text: "The Fleet Deck v1 plugin is active in this session.".into(),
        },
        Event::SessionModels {
            models: vec![
                crate::event::ModelChoice {
                    value: "default".into(),
                    label: "Default (recommended)".into(),
                    description: Some("Use the default model".into()),
                },
                crate::event::ModelChoice {
                    value: "opus".into(),
                    label: "Opus".into(),
                    description: None,
                },
            ],
        },
        Event::SessionBase {
            git_ref: Some("main".into()),
            oid: "0123456789abcdef0123456789abcdef01234567".into(),
            inferred: false,
        },
    ]
}

#[test]
fn every_kind_has_a_sample_and_round_trips() {
    let samples = samples();
    let kinds: Vec<&str> = samples.iter().map(Event::kind_name).collect();
    assert_eq!(kinds, Event::KINDS.to_vec());
    for event in samples {
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["kind"], event.kind_name());
        let body: EventBody = serde_json::from_value(value).unwrap();
        assert_eq!(body, EventBody::Known(event));
    }
}

#[test]
fn an_unknown_kind_is_kept_verbatim() {
    let raw = json!({"kind": "future.thing", "x": 1});
    let body: EventBody = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(body.kind_name(), "future.thing");
    assert!(body.known().is_none());
    assert_eq!(serde_json::to_value(&body).unwrap(), raw);
}

#[test]
fn a_known_kind_is_strict_about_required_fields() {
    let err = serde_json::from_value::<EventBody>(json!({"kind": "turn.completed"})).unwrap_err();
    assert!(
        err.to_string().contains("invalid `turn.completed` event"),
        "{err}"
    );
    let err = serde_json::from_value::<EventBody>(json!({"x": 1})).unwrap_err();
    assert!(err.to_string().contains("kind"), "{err}");
}

#[test]
fn extra_fields_are_ignored_and_optional_fields_may_be_missing() {
    let raw = json!({
        "kind": "turn.completed", "turn_id": "turn-1", "status": "done",
        "new_field_from_a_newer_peer": true
    });
    let body: EventBody = serde_json::from_value(raw).unwrap();
    assert_eq!(
        body,
        EventBody::Known(Event::TurnCompleted {
            turn_id: "turn-1".into(),
            status: TurnStatus::Done,
            usage: None,
        })
    );
}

#[test]
fn an_unknown_tool_kind_or_provider_degrades_instead_of_failing() {
    assert_eq!(
        serde_json::from_value::<ToolKind>(json!("notebook")).unwrap(),
        ToolKind::Other
    );
    assert_eq!(
        serde_json::from_value::<ProviderKind>(json!("pi")).unwrap(),
        ProviderKind::Unknown
    );
}

#[test]
fn the_envelope_carries_the_sequence_numbers() {
    let env = EventEnvelope {
        station_seq: 7,
        session_id: "s1".into(),
        session_seq: 3,
        at: 1,
        proto_version: 1,
        raw_ref: None,
        event: Event::ModeChanged {
            mode: "plan".into(),
        }
        .into(),
    };
    let value = serde_json::to_value(&env).unwrap();
    assert_eq!(value["event"]["kind"], "mode.changed");
    assert_eq!(value["station_seq"], 7);
    let back: EventEnvelope = serde_json::from_value(value).unwrap();
    assert_eq!(back, env);
}

mod decode_props {
    use super::*;
    use crate::link::ControlFrame;
    use proptest::prelude::*;

    fn kind() -> impl Strategy<Value = String> {
        prop_oneof![
            proptest::sample::select(Event::KINDS.to_vec()).prop_map(str::to_owned),
            "[a-z]{1,8}\\.[a-z]{1,8}",
        ]
    }

    fn json_leaf() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            "[a-z0-9_.-]{0,12}".prop_map(Value::from),
        ]
    }

    proptest! {
        /// Decoding never panics; a known kind decodes as known or fails, an
        /// unknown kind always decodes as `Unknown` and re-encodes verbatim.
        #[test]
        fn event_decode_is_total(
            kind in kind(),
            fields in proptest::collection::btree_map("[a-z_]{1,14}", json_leaf(), 0..8),
        ) {
            let mut map = serde_json::Map::new();
            for (k, v) in fields {
                map.insert(k, v);
            }
            map.insert("kind".into(), Value::from(kind.clone()));
            let raw = Value::Object(map);
            match serde_json::from_value::<EventBody>(raw.clone()) {
                Ok(EventBody::Unknown { kind: k, .. }) => {
                    prop_assert!(!Event::KINDS.contains(&k.as_str()));
                    let back = serde_json::to_value(EventBody::Unknown {
                        kind: k,
                        payload: raw.as_object().cloned().unwrap_or_default(),
                    }).unwrap();
                    prop_assert_eq!(back, raw);
                }
                Ok(EventBody::Known(event)) => prop_assert_eq!(event.kind_name(), kind.as_str()),
                Err(_) => prop_assert!(Event::KINDS.contains(&kind.as_str())),
            }
        }

        /// Arbitrary text never panics the control frame decoder.
        #[test]
        fn control_frame_decode_never_panics(text in "\\PC{0,200}") {
            let _ = serde_json::from_str::<ControlFrame>(&text);
        }
    }
}

#[test]
fn a_model_choice_without_a_description_decodes() {
    let body: EventBody = serde_json::from_value(
        json!({"kind": "session.models", "models": [{"value": "opus", "label": "Opus"}]}),
    )
    .unwrap();
    let Some(Event::SessionModels { models }) = body.known() else {
        panic!("expected session.models");
    };
    assert_eq!(models[0].description, None);
}

#[test]
fn a_session_base_without_a_ref_or_the_inferred_flag_decodes() {
    let body: EventBody = serde_json::from_value(
        json!({"kind": "session.base", "oid": "0123456789abcdef0123456789abcdef01234567"}),
    )
    .unwrap();
    let Some(Event::SessionBase {
        git_ref, inferred, ..
    }) = body.known()
    else {
        panic!("expected session.base");
    };
    assert_eq!(*git_ref, None);
    assert!(!*inferred);
}
