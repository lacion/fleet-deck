//! PRE-PR fixes of ADR-008 on the deck (section 17), over a fake station
//! link: the authority check answers again after the `coordinator.policy_now`
//! wait, it applies the real launch mode to the grant ceiling, and a
//! coordinator answers a question with its answers or text, never an option.
//! The round 2 fixes: a launching command without a launch mode is refused,
//! and `needs_you` shows a question without option ids.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::fleet::*;
use common::*;
use fleetdeck_deck::store::{GrantClass, NewGrant};
use fleetdeck_proto::command::{GrantScope, SessionCommand};
use fleetdeck_proto::mutation::{AuthorityCheck, CoordinatorBinding, CoordinatorOp};
use fleetdeck_proto::{Receipt, RejectCode};
use serde_json::{Value, json};

/// PRE-PR fix 2: the deck checks the authority again after the
/// `coordinator.policy_now` wait, so a change during the wait makes the
/// check invalid.
#[tokio::test]
async fn a_change_while_the_deck_waits_for_the_caller_policy_makes_the_check_invalid() {
    let mut r = rig().await;
    let b = bound(&r.f, "coord-1", &[GrantClass::Drive], true).await;
    let receipt = call(&r.f, &b, "k1", stop(&r.f, "w1")).await;
    assert!(receipt.reject.is_none(), "{receipt:?}");
    let (id, _) = wire(&mut r).await;
    let check = AuthorityCheck {
        grant_id: b.grant_id.clone(),
        generation: 1,
        caller_station: r.f.station.clone(),
        caller_session: "coord-1".into(),
        target_session: "w1".into(),
        command_id: id,
        launch: None,
    };
    let live = json!({"fleet_control": true, "bound": true});
    let store = r.f.store.clone();
    // No change: valid.
    let got = authority_check_held(&mut r, &check, live.clone(), async {}).await;
    assert_eq!(got, json!({"valid": true}));
    // The target becomes a coordinator during the wait.
    let (st, station) = (store.clone(), r.f.station.clone());
    let become_coordinator = async move {
        let mut g = NewGrant::new(station, "w1".into());
        g.classes = [GrantClass::Drive].into();
        let grant_id = st.create_grant(g).await.unwrap().grant_id;
        let binding = CoordinatorBinding {
            binding_id: "b-w1".to_owned(),
            grant_id,
            generation: 1,
            provider_instance: "claude".into(),
            native_id: "n-w1".to_owned(),
        };
        assert!(st.bind_grant(binding).await.unwrap());
    };
    let got = authority_check_held(&mut r, &check, live.clone(), become_coordinator).await;
    assert_eq!(got["reason"], "target_excluded", "{got}");
    // The deck switch goes off during the wait.
    let st = store.clone();
    let off = async move { st.set_fleet_control(false) };
    let got = authority_check_held(&mut r, &check, live.clone(), off).await;
    assert_eq!(got["reason"], "fleet_control_off", "{got}");
    store.set_fleet_control(true);
    // The grant is revoked during the wait.
    let (st, grant) = (store.clone(), b.grant_id.clone());
    let revoke = async move {
        assert!(st.revoke_grant(grant, "owner".to_owned()).await.unwrap());
    };
    let got = authority_check_held(&mut r, &check, live, revoke).await;
    assert_eq!(got["reason"], "grant_revoked", "{got}");
}

/// PRE-PR fix 4: the real launch mode is checked against the grant ceiling
/// and the bypass rule (ADR-008 section 5.2).
#[tokio::test]
async fn the_authority_check_applies_the_ceiling_and_the_bypass_rule_to_the_launch_mode() {
    use fleetdeck_proto::event::ProviderKind;
    use fleetdeck_proto::mutation::LaunchMode;
    let mut r = rig().await;
    let station = r.f.station.clone();
    let ceiling = |top: &'static str| {
        move |g: &mut NewGrant| {
            g.scope = GrantScope::Fleet;
            g.max_modes.insert("claude".to_owned(), top.to_owned());
        }
    };
    let classes = [GrantClass::Drive];
    let b = bound_edit(&r.f, &station, "coord-1", &classes, ceiling("acceptEdits")).await;
    let receipt = call(&r.f, &b, "k1", stop(&r.f, "w1")).await;
    assert!(receipt.reject.is_none(), "{receipt:?}");
    let (id, _) = wire_accepted(&mut r).await;
    let check =
        |grant: &CoordinatorBinding, launch: Option<(ProviderKind, Option<&str>)>| AuthorityCheck {
            grant_id: grant.grant_id.clone(),
            generation: 1,
            caller_station: station.clone(),
            caller_session: "coord-1".into(),
            target_session: "w1".into(),
            command_id: id.clone(),
            launch: launch.map(|(provider, mode)| LaunchMode {
                provider,
                mode: mode.map(str::to_owned),
            }),
        };
    let live = Some(json!({"fleet_control": true, "bound": true}));
    // No launch, the provider default and a mode within the ceiling: valid.
    for launch in [
        None,
        Some((ProviderKind::Claude, None)),
        Some((ProviderKind::Claude, Some("plan"))),
        Some((ProviderKind::Claude, Some("acceptEdits"))),
    ] {
        let got = authority_check(&mut r, &check(&b, launch), live.clone()).await;
        assert_eq!(got, json!({"valid": true}), "{launch:?}");
    }
    // Above the ceiling, a bypass mode, a family without a ceiling and an
    // unknown family: no `policy_now` round trip is needed.
    for launch in [
        (ProviderKind::Claude, Some("auto")),
        (ProviderKind::Claude, Some("bypassPermissions")),
        (ProviderKind::Codex, Some("untrusted/read-only")),
        (ProviderKind::Codex, None),
        (ProviderKind::Unknown, None),
    ] {
        let got = authority_check(&mut r, &check(&b, Some(launch)), None).await;
        assert_eq!(
            got,
            json!({"valid": false, "reason": "mode_not_allowed"}),
            "{launch:?}"
        );
    }
    // A bypass mode stays refused when the ceiling itself is that mode.
    let wide = bound_edit(
        &r.f,
        &station,
        "coord-1",
        &classes,
        ceiling("bypassPermissions"),
    )
    .await;
    let receipt = call(&r.f, &wide, "k2", stop(&r.f, "w1")).await;
    assert!(receipt.reject.is_none(), "{receipt:?}");
    let (id2, _) = wire_accepted(&mut r).await;
    let mut c = check(
        &wide,
        Some((ProviderKind::Claude, Some("bypassPermissions"))),
    );
    c.command_id = id2;
    let got = authority_check(&mut r, &c, None).await;
    assert_eq!(got["reason"], "mode_not_allowed", "{got}");
    // Under that wide ceiling a lower mode passes.
    c.launch = Some(LaunchMode {
        provider: ProviderKind::Claude,
        mode: Some("auto".to_owned()),
    });
    let got = authority_check(&mut r, &c, live).await;
    assert_eq!(got, json!({"valid": true}));
}

/// PRE-PR fix 3 (deck part): a coordinator never answers a question with an
/// option, also not with a standing allow; it answers with its answers or
/// its text.
#[tokio::test]
async fn a_coordinator_answers_a_question_with_answers_or_text_and_never_with_an_option() {
    use fleetdeck_proto::event::{ApprovalOption, RequestKind};
    let mut r = rig().await;
    let option = |id: &str| ApprovalOption {
        option_id: id.to_owned(),
        label: id.to_owned(),
        kind: None,
    };
    let question = |id: &str, options: Vec<ApprovalOption>| {
        let mut q = request(id, 1_000, None);
        q.request_kind = RequestKind::Question;
        q.options = options;
        q
    };
    let mut sessions = ["coord-1", "w1", "w2"].map(card).to_vec();
    sessions[1].open_requests = vec![
        // A Claude question: `allow`, `allow_always` and `deny`, no `answer`.
        question(
            "q-open",
            vec![option("allow"), option("allow_always"), option("deny")],
        ),
        question("q-none", vec![]),
        question("q-answer", vec![option("always"), option("answer")]),
    ];
    let (station, inc) = (r.f.station.clone(), r.f.inc.clone());
    r.f.store
        .apply_snapshot(station.clone(), inc, 0, sessions)
        .await
        .unwrap();
    let b = bound(&r.f, "coord-1", &[GrantClass::Answer], true).await;
    let answer = |request: &str, option: Option<&str>, qa: Option<Value>, text: Option<&str>| {
        CoordinatorOp::Answer {
            station: station.clone(),
            session_id: "w1".into(),
            request_id: request.into(),
            option_id: option.map(str::to_owned),
            question_answers: qa,
            text: text.map(str::to_owned),
        }
    };
    let code = |r: &Receipt| r.reject.as_ref().map(|x| x.code);
    // An option, also `allow_always` and the `answer` one: refused.
    for (i, (request, option)) in [
        ("q-open", "allow"),
        ("q-open", "allow_always"),
        ("q-answer", "answer"),
    ]
    .into_iter()
    .enumerate()
    {
        let op = answer(request, Some(option), Some(json!({"k": "v"})), None);
        let got = call(&r.f, &b, &format!("o{i}"), op).await;
        assert_eq!(
            code(&got),
            Some(RejectCode::Forbidden),
            "{request}/{option}"
        );
    }
    // Nothing was sent so far.
    no_request(&mut r.station, Duration::from_millis(200)).await;
    // `question_answers` (and `text`) are accepted as the literal `answer`.
    let cases = [
        ("a1", answer("q-none", None, Some(json!({"k": "v"})), None)),
        ("a2", answer("q-answer", None, None, Some("because"))),
        ("a3", answer("q-open", None, Some(json!({"k": "v"})), None)),
    ];
    for (id, op) in cases {
        let got = call(&r.f, &b, id, op).await;
        assert!(got.reject.is_none(), "{id}: {got:?}");
        let (_, params) = wire_accepted(&mut r).await;
        let command: SessionCommand = serde_json::from_value(params["command"].clone()).unwrap();
        let SessionCommand::Answer { decision, .. } = command else {
            panic!("{params}");
        };
        assert_eq!(decision.option_id, "answer", "{id}");
    }
}

/// RE-REVIEW round 2 (deck decision 1): a station that sends no launch mode
/// for a command that launches a provider is older than the deck; the check
/// fails closed with `launch_mode_missing`. A follow-up needs no launch mode.
#[tokio::test]
async fn a_launching_command_without_a_launch_mode_is_refused_as_version_skew() {
    use fleetdeck_proto::command::{SessionSpec, UserInput};
    use fleetdeck_proto::event::ProviderKind;
    use fleetdeck_proto::mutation::{CoordinatorDelivery, CoordinatorTarget, LaunchMode};
    let mut r = rig().await;
    let station = r.f.station.clone();
    r.f.store.set_station_info(&station, claude_info());
    let mut sessions = ["coord-1", "w1", "w2"].map(card).to_vec();
    sessions[1].provider_instance = Some("claude".into());
    sessions[1].mode = Some("plan".to_owned());
    let inc = r.f.inc.clone();
    r.f.store
        .apply_snapshot(station.clone(), inc, 0, sessions)
        .await
        .unwrap();
    let classes = [
        GrantClass::Drive,
        GrantClass::Manage,
        GrantClass::Spawn,
        GrantClass::Mail,
    ];
    let b = bound_edit(&r.f, &station, "coord-1", &classes, |g| {
        g.scope = GrantScope::Fleet;
        g.max_modes
            .insert("claude".to_owned(), "acceptEdits".to_owned());
    })
    .await;
    let ops = [
        (
            "resume",
            CoordinatorOp::Resume {
                station: station.clone(),
                session_id: "w1".into(),
            },
        ),
        (
            "set_mode",
            CoordinatorOp::SetMode {
                station: station.clone(),
                session_id: "w1".into(),
                mode: "plan".to_owned(),
            },
        ),
        (
            "start",
            CoordinatorOp::Spawn {
                station: station.clone(),
                spec: SessionSpec {
                    effort: None,
                    provider_instance: "claude".into(),
                    cwd: "/w".to_owned(),
                    mode: Some("plan".to_owned()),
                    model: None,
                    title: None,
                    coordinator: None,
                    launch: None,
                    extra_env: None,
                },
                prompt: Some(UserInput::new("go")),
            },
        ),
    ];
    let live = Some(json!({"fleet_control": true, "bound": true}));
    let within = LaunchMode {
        provider: ProviderKind::Claude,
        mode: Some("plan".to_owned()),
    };
    for (i, (name, op)) in ops.into_iter().enumerate() {
        let receipt = call(&r.f, &b, &format!("l{i}"), op).await;
        assert!(receipt.reject.is_none(), "{name}: {receipt:?}");
        let (id, _) = wire_accepted(&mut r).await;
        let mut check = AuthorityCheck {
            grant_id: b.grant_id.clone(),
            generation: 1,
            caller_station: station.clone(),
            caller_session: "coord-1".into(),
            target_session: "w1".into(),
            command_id: id,
            launch: None,
        };
        // No launch mode: no `policy_now` round trip is needed.
        let got = authority_check(&mut r, &check, None).await;
        assert_eq!(
            got,
            json!({"valid": false, "reason": "launch_mode_missing"}),
            "{name}"
        );
        // A launch mode within the ceiling: valid.
        check.launch = Some(within.clone());
        let got = authority_check(&mut r, &check, live.clone()).await;
        assert_eq!(got, json!({"valid": true}), "{name}");
    }
    // A follow-up needs no launch mode.
    let mail = CoordinatorOp::Send {
        to: CoordinatorTarget {
            station: station.clone(),
            session_id: "w1".into(),
        },
        text: "hello".to_owned(),
        delivery: CoordinatorDelivery::FollowUp,
    };
    let receipt = call(&r.f, &b, "l-mail", mail).await;
    assert!(receipt.reject.is_none(), "{receipt:?}");
    let (id, _) = wire_accepted(&mut r).await;
    let check = AuthorityCheck {
        grant_id: b.grant_id.clone(),
        generation: 1,
        caller_station: station.clone(),
        caller_session: "coord-1".into(),
        target_session: "w1".into(),
        command_id: id,
        launch: None,
    };
    let got = authority_check(&mut r, &check, live).await;
    assert_eq!(got, json!({"valid": true}));
}

/// RE-REVIEW round 2 (deck decision 2): a question has no `options` field
/// (its option ids come from the worker); its labels are lines of the fenced
/// input. An approval keeps its options.
#[tokio::test]
async fn needs_you_lists_the_labels_of_a_question_in_its_input_and_never_its_option_ids() {
    use fleetdeck_proto::event::{ApprovalOption, RequestKind};
    use fleetdeck_proto::mutation::CoordinatorQuery;
    let w = world().await;
    let f = &w.r.f;
    let mut sessions = ["coord-1", "w1"].map(card).to_vec();
    let mut q = request("rq", 1_000, None);
    q.request_kind = RequestKind::Question;
    q.options = vec![
        ApprovalOption {
            option_id: "allow_always".to_owned(),
            label: format!("Yes {HOSTILE}"),
            kind: None,
        },
        ApprovalOption {
            option_id: "no".to_owned(),
            label: "No thanks".to_owned(),
            kind: None,
        },
    ];
    let approval = request("ra", 2_000, None);
    sessions[1].open_requests = vec![q, approval];
    let (station, inc) = (f.station.clone(), f.inc.clone());
    f.store
        .apply_snapshot(station, inc, 0, sessions)
        .await
        .unwrap();
    let b = bound(f, "coord-1", &CLASSES, true).await;
    let got = ask(f, &b, CoordinatorQuery::NeedsYou).await.unwrap();
    let list = got["requests"].as_array().unwrap();
    let find = |id: &str| list.iter().find(|r| r["request_id"] == id).unwrap();
    let question = find("rq");
    assert_eq!(question["request_kind"], "question");
    assert!(question.get("options").is_none(), "{question}");
    assert!(!question.to_string().contains("allow_always"), "{question}");
    one_fence(&question["input"]);
    let input = question["input"].as_str().unwrap();
    assert!(
        input.contains("\n- Yes ") && input.contains("\n- No thanks"),
        "{input}"
    );
    // The hostile label stays inert: its closing tag is broken.
    assert!(input.contains("<\u{200B}/untrusted>"), "{input}");
    // An approval keeps its options.
    let approval = find("ra");
    let options = approval["options"].as_array().unwrap();
    let ids: Vec<&Value> = options.iter().map(|o| &o["option_id"]).collect();
    assert_eq!(ids, [&json!("once"), &json!("no")]);
}
