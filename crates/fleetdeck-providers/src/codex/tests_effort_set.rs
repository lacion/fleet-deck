//! `set_effort` of the Codex core (B18 Phase 2, docs/v2/design/effort.md
//! §9.2 item 7): the pick that waits for a turn, travels with its request and
//! settles at the answer of that request.

use fleetdeck_proto::Event;
use fleetdeck_proto::event::EffortSetOutcome::{self, Accepted, Applied, Superseded, Unverified};
use serde_json::{Value, json};

use super::core::EffortPick;
use super::tests::*;
use super::*;
use crate::adapter::ProviderError;

/// `(control_id, requested, effort, outcome)` of every `effort.set_result`.
type Row = (Option<String>, String, Option<String>, EffortSetOutcome);

fn row(control: &str, requested: &str, effort: Option<&str>, outcome: EffortSetOutcome) -> Row {
    (
        Some(control.to_owned()),
        requested.to_owned(),
        effort.map(str::to_owned),
        outcome,
    )
}

fn results(out: &[Output]) -> Vec<Row> {
    emits(out)
        .into_iter()
        .filter_map(|e| match e {
            Event::EffortSetResult {
                control_id,
                requested,
                effort,
                outcome,
                ..
            } => Some((control_id, requested, effort, outcome)),
            _ => None,
        })
        .collect()
}

fn pick(effort: &str, control: &str) -> EffortPick {
    EffortPick {
        effort: effort.to_owned(),
        control_id: Some(control.to_owned()),
    }
}

fn settings_updated(core: &mut CodexCore, effort: &str) -> Vec<Output> {
    notify(
        core,
        "thread/settings/updated",
        json!({"threadId": T, "threadSettings": {"effort": effort}}),
    )
}

fn accept(core: &mut CodexCore, out: &[Output], native: &str) -> Vec<Output> {
    reply(
        core,
        out,
        json!({"turn": {"id": native, "status": "inProgress"}}),
    )
}

fn refuse(core: &mut CodexCore, out: &[Output]) -> Vec<Output> {
    let id = writes(out)[0]["id"].clone();
    feed(
        core,
        json!({"id": id, "error": {"code": -32000, "message": "no turn"}}),
    )
}

/// Ends the turn `native` of the input `client`.
fn finish(core: &mut CodexCore, client: &str, native: &str) {
    notify(
        core,
        "turn/started",
        json!({"threadId": T, "turn": {"id": native}}),
    );
    user_item(core, client);
    complete(core, native, "completed");
}

fn sent_effort(out: &[Output]) -> Value {
    writes(out)[0]["params"]["effort"].clone()
}

fn model_entry(id: &str, levels: Option<&[&str]>) -> Value {
    let mut m = json!({"id": id, "model": id, "displayName": id.to_uppercase()});
    if let Some(levels) = levels {
        m["supportedReasoningEfforts"] = levels
            .iter()
            .map(|l| json!({"reasoningEffort": l, "description": "d"}))
            .collect();
        m["defaultReasoningEffort"] = json!("medium");
    }
    m
}

/// A core whose thread model is `m`, with the `model/list` answer `data`.
fn core_with_models(data: &[Value]) -> (CodexCore, Vec<Output>) {
    let mut cfg = config("/w");
    cfg.request_models = true;
    let mut core = CodexCore::new(cfg);
    let out = core.initialize().unwrap();
    reply(
        &mut core,
        &out,
        json!({"userAgent": "fleetdeck/0.160.0 (Linux)"}),
    );
    let out = core.open_thread(&SessionSpec::default()).unwrap();
    let after = reply(&mut core, &out, json!({"thread": {"id": T}, "model": "m"}));
    let done = reply(&mut core, &after, json!({"data": data, "nextCursor": null}));
    (core, done)
}

#[test]
fn a_pick_travels_with_its_turn_and_a_newer_choice_waits_for_the_next_turn() {
    let mut core = ready_core();
    assert!(
        core.set_effort("low", Some("e1".into()))
            .unwrap()
            .is_empty()
    );
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    assert_eq!(sent_effort(&out), json!("low"));
    assert_eq!(core.pending_effort, None);
    // A choice made while the request is open supersedes nothing.
    assert!(
        core.set_effort("high", Some("e2".into()))
            .unwrap()
            .is_empty()
    );
    // The `thread/settings/updated` of this turn comes before the answer.
    settings_updated(&mut core, "low");
    let after = accept(&mut core, &out, "n1");
    assert_eq!(
        results(&after),
        vec![
            row("e1", "low", None, Accepted),
            row("e1", "low", Some("low"), Applied),
        ]
    );
    assert_eq!(core.pending_effort, Some(pick("high", "e2")));
    assert_eq!(core.spec.effort.as_deref(), Some("low"));
    finish(&mut core, "in-1", "n1");
    let next = core.prompt(&input("again", "in-2")).unwrap();
    assert_eq!(sent_effort(&next), json!("high"));
    assert_eq!(core.pending_effort, None);
}

#[test]
fn a_set_effort_writes_nothing_and_a_second_one_supersedes_the_first() {
    let mut core = ready_core();
    assert!(
        core.set_effort("low", Some("e1".into()))
            .unwrap()
            .is_empty()
    );
    let out = core.set_effort("high", Some("e2".into())).unwrap();
    assert!(writes(&out).is_empty());
    assert_eq!(results(&out), vec![row("e1", "low", None, Superseded)]);
    assert_eq!(core.pending_effort, Some(pick("high", "e2")));
}

#[test]
fn an_accepted_turn_without_a_report_of_the_level_is_unverified() {
    let mut core = ready_core();
    core.set_effort("low", Some("e1".into())).unwrap();
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    let after = accept(&mut core, &out, "n1");
    assert_eq!(
        results(&after),
        vec![
            row("e1", "low", None, Accepted),
            row("e1", "low", None, Unverified),
        ]
    );
}

#[test]
fn another_reported_level_is_unverified_not_applied() {
    let mut core = ready_core();
    settings_updated(&mut core, "high");
    core.set_effort("low", Some("e1".into())).unwrap();
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    let after = accept(&mut core, &out, "n1");
    assert_eq!(
        results(&after),
        vec![
            row("e1", "low", None, Accepted),
            row("e1", "low", None, Unverified),
        ]
    );
}

#[test]
fn a_refused_turn_puts_its_pick_back_when_nothing_newer_waits() {
    let mut core = ready_core();
    core.set_effort("low", Some("e1".into())).unwrap();
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    let after = refuse(&mut core, &out);
    assert!(results(&after).is_empty(), "no Accepted for a refused turn");
    assert_eq!(core.pending_effort, Some(pick("low", "e1")));
    assert_eq!(core.spec.effort, None);
    let next = core.prompt(&input("again", "in-2")).unwrap();
    assert_eq!(sent_effort(&next), json!("low"));
}

#[test]
fn a_refused_turn_supersedes_its_pick_when_a_newer_choice_waits() {
    let mut core = ready_core();
    core.set_effort("low", Some("e1".into())).unwrap();
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    core.set_effort("high", Some("e2".into())).unwrap();
    let after = refuse(&mut core, &out);
    assert_eq!(results(&after), vec![row("e1", "low", None, Superseded)]);
    assert_eq!(core.pending_effort, Some(pick("high", "e2")));
}

#[test]
fn a_refused_turn_without_a_pick_emits_no_effort_result() {
    let mut core = ready_core();
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    assert!(results(&refuse(&mut core, &out)).is_empty());
}

#[test]
fn a_turn_without_a_pick_sends_the_session_effort_and_emits_no_result() {
    let mut core = ready_core_with(&SessionSpec {
        effort: Some("low".into()),
        ..SessionSpec::default()
    });
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    assert_eq!(sent_effort(&out), json!("low"));
    assert!(results(&accept(&mut core, &out, "n1")).is_empty());
}

#[test]
fn the_pick_goes_into_the_collaboration_mode_too() {
    let mut core = ready_core();
    core.set_collaboration("plan").unwrap();
    core.set_effort("high", Some("e1".into())).unwrap();
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    let params = &writes(&out)[0]["params"];
    assert_eq!(params["effort"], json!("high"));
    assert_eq!(
        params["collaborationMode"]["settings"]["reasoning_effort"],
        json!("high")
    );
}

#[test]
fn model_list_keeps_the_levels_and_the_default_of_each_model() {
    let data = [
        model_entry("m", Some(&["low", "bad value", "high"])),
        model_entry("n", None),
        model_entry("p", Some(&[])),
    ];
    let (core, done) = core_with_models(&data);
    let [Event::SessionModels { models }] = &emits(&done)[..] else {
        panic!("expected one session.models");
    };
    assert_eq!(
        models[0].efforts,
        Some(vec!["low".to_owned(), "high".to_owned()])
    );
    assert_eq!(models[0].default_effort.as_deref(), Some("medium"));
    assert_eq!(models[1].efforts, None);
    assert_eq!(models[2].efforts, Some(Vec::new()));
    assert_eq!(core.model_choices, *models);
}

#[test]
fn a_level_outside_the_levels_of_the_current_model_is_not_offered() {
    let data = [
        model_entry("m", Some(&["low", "high"])),
        model_entry("n", None),
        model_entry("p", Some(&["max"])),
        model_entry("q", Some(&[])),
    ];
    let (mut core, _) = core_with_models(&data);
    let err = core.set_effort("max", Some("e1".into())).unwrap_err();
    assert!(matches!(&err, ProviderError::EffortNotOffered(e) if e == "max"));
    assert_eq!(core.pending_effort, None, "nothing waits");
    assert!(
        core.set_effort("low", Some("e1".into()))
            .unwrap()
            .is_empty()
    );
    // The pending model is the current one: `p` offers `max` only.
    core.set_model("p").unwrap();
    assert!(matches!(
        core.set_effort("low", None),
        Err(ProviderError::EffortNotOffered(_))
    ));
    assert!(core.set_effort("max", Some("e2".into())).is_ok());
    // A model without an advertised list takes any level.
    core.set_model("n").unwrap();
    assert!(core.set_effort("ultra", None).is_ok());
    // A model without an effort setting offers none.
    core.set_model("q").unwrap();
    assert!(matches!(
        core.set_effort("low", None),
        Err(ProviderError::EffortNotOffered(_))
    ));
}

#[test]
fn a_level_is_not_checked_without_a_model_list() {
    let mut core = ready_core();
    assert!(core.set_effort("anything", None).unwrap().is_empty());
}

#[test]
fn set_effort_after_the_end_is_refused() {
    let mut core = ready_core();
    core.on_exit(Some(0));
    assert!(matches!(
        core.set_effort("low", None),
        Err(ProviderError::SessionEnded)
    ));
}

#[test]
fn the_thread_requests_carry_the_session_effort_in_config() {
    for (resume, fork, method) in [
        (None, false, "thread/start"),
        (Some("t0"), false, "thread/resume"),
        (Some("t0"), true, "thread/fork"),
    ] {
        let mut core = CodexCore::new(config("/w"));
        let out = core.initialize().unwrap();
        reply(
            &mut core,
            &out,
            json!({"userAgent": "fleetdeck/0.160.0 (Linux)"}),
        );
        let spec = SessionSpec {
            effort: Some("xhigh".into()),
            resume: resume.map(str::to_owned),
            fork,
            ..SessionSpec::default()
        };
        let w = writes(&core.open_thread(&spec).unwrap());
        assert_eq!(w[0]["method"], method);
        assert_eq!(
            w[0]["params"]["config"],
            json!({"model_reasoning_effort": "xhigh"}),
            "{method}"
        );
        // Without an effort the request has no `config`.
        let mut core = CodexCore::new(config("/w"));
        let out = core.initialize().unwrap();
        reply(
            &mut core,
            &out,
            json!({"userAgent": "fleetdeck/0.160.0 (Linux)"}),
        );
        let spec = SessionSpec {
            resume: resume.map(str::to_owned),
            fork,
            ..SessionSpec::default()
        };
        let w = writes(&core.open_thread(&spec).unwrap());
        assert!(w[0]["params"].get("config").is_none(), "{method}");
    }
}

#[test]
fn the_codex_capabilities_offer_set_effort() {
    assert!(capabilities(false).set_effort);
    assert!(capabilities(true).set_effort);
}

#[test]
fn a_turn_start_without_the_effort_key_decodes() {
    let mut core = ready_core();
    core.set_effort("low", Some("e1".into())).unwrap();
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    let mut checkpoint = core.checkpoint().unwrap();
    let requests = checkpoint.state["host_requests"].as_object_mut().unwrap();
    let mut removed = 0;
    for request in requests.values_mut() {
        if let Some(turn_start) = request.get_mut("TurnStart").and_then(Value::as_object_mut) {
            assert!(turn_start.remove("effort").is_some());
            removed += 1;
        }
    }
    assert_eq!(removed, 1);
    // A checkpoint of a build without the new fields.
    let state = checkpoint.state.as_object_mut().unwrap();
    assert!(state.remove("pending_effort").is_some());
    assert!(state.remove("model_choices").is_some());
    let mut core = CodexCore::restore(config("/w"), &checkpoint).unwrap();
    assert_eq!(core.pending_effort, None);
    assert!(core.model_choices.is_empty());
    let sent_id = writes(&out)[0]["id"].clone();
    let after = feed(
        &mut core,
        json!({"id": sent_id, "result": {"turn": {"id": "n1", "status": "inProgress"}}}),
    );
    assert!(results(&after).is_empty());
}

#[test]
fn a_turn_start_without_a_pick_has_no_effort_key() {
    let mut core = ready_core();
    core.prompt(&input("hi", "in-1")).unwrap();
    let checkpoint = core.checkpoint().unwrap();
    let requests = checkpoint.state["host_requests"].as_object().unwrap();
    let turn_start = requests
        .values()
        .find_map(|r| r.get("TurnStart"))
        .expect("an open turn/start");
    assert!(turn_start.get("effort").is_none(), "{turn_start}");
}

#[test]
fn a_pending_pick_survives_a_restore() {
    let mut core = ready_core();
    core.set_effort("low", Some("e1".into())).unwrap();
    let checkpoint = core.checkpoint().unwrap();
    assert_eq!(
        checkpoint.state["pending_effort"],
        json!({"effort": "low", "control_id": "e1"})
    );
    let mut core = CodexCore::restore(config("/w"), &checkpoint).unwrap();
    assert_eq!(core.pending_effort, Some(pick("low", "e1")));
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    assert_eq!(sent_effort(&out), json!("low"));
}

#[test]
fn a_pick_in_an_open_request_survives_a_restore_and_settles_at_the_answer() {
    let mut core = ready_core();
    core.set_effort("low", Some("e1".into())).unwrap();
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    let checkpoint = core.checkpoint().unwrap();
    let mut core = CodexCore::restore(config("/w"), &checkpoint).unwrap();
    let sent_id = writes(&out)[0]["id"].clone();
    let after = feed(
        &mut core,
        json!({"id": sent_id, "result": {"turn": {"id": "n1", "status": "inProgress"}}}),
    );
    assert_eq!(
        results(&after),
        vec![
            row("e1", "low", None, Accepted),
            row("e1", "low", None, Unverified),
        ]
    );
}
