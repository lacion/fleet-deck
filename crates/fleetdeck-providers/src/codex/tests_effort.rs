//! The reasoning effort of the Codex thread (B18, docs/v2/design/effort.md
//! §4.2 and §4.3.1).

use fleetdeck_proto::Event;
use serde_json::{Value, json};

use super::tests::*;
use super::*;

/// A core with the handshake done and the thread not yet open; returns the
/// core and the writes of `open_thread`.
fn open_core(request_models: bool, spec: &SessionSpec) -> (CodexCore, Vec<Output>) {
    let mut cfg = config("/w");
    cfg.request_models = request_models;
    let mut core = CodexCore::new(cfg);
    let out = core.initialize().unwrap();
    reply(
        &mut core,
        &out,
        json!({"userAgent": "fleetdeck/0.160.0 (Linux)"}),
    );
    let out = core.open_thread(spec).unwrap();
    (core, out)
}

/// The outputs of a thread answer, without the `skills/list` write (B11).
fn answered(answer: Value, request_models: bool) -> (CodexCore, Vec<Output>) {
    let (mut core, out) = open_core(request_models, &SessionSpec::default());
    let mut after = reply(&mut core, &out, answer);
    after.retain(|o| !matches!(o, Output::Write(w) if w["method"] == "skills/list"));
    (core, after)
}

fn efforts(out: &[Output]) -> Vec<Option<String>> {
    emits(out)
        .into_iter()
        .filter_map(|e| match e {
            Event::EffortChanged { effort } => Some(effort),
            _ => None,
        })
        .collect()
}

fn settings(core: &mut CodexCore, thread: &str, effort: &str) -> Vec<Output> {
    notify(
        core,
        "thread/settings/updated",
        json!({"threadId": thread, "threadSettings": {"effort": effort}}),
    )
}

#[test]
fn a_thread_start_answer_reports_its_effort() {
    let (_core, after) = answered(
        json!({"thread": {"id": T}, "model": "m", "reasoningEffort": "high"}),
        true,
    );
    assert!(matches!(
        &after[0],
        Output::Emit(Event::SessionStarted { .. })
    ));
    // `session.started`, the identity, `effort.changed`, then `model/list`.
    assert!(matches!(&after[1], Output::NativeIdentity { .. }));
    assert!(matches!(
        &after[2],
        Output::Emit(Event::EffortChanged { effort }) if effort.as_deref() == Some("high")
    ));
    let w = writes(&after);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0]["method"], "model/list");
    assert!(matches!(after.last(), Some(Output::Write(_))));
}

#[test]
fn a_null_effort_in_the_answer_emits_nothing() {
    let (_core, after) = answered(
        json!({"thread": {"id": T}, "model": "m", "reasoningEffort": null}),
        false,
    );
    assert!(efforts(&after).is_empty());
}

#[test]
fn the_thread_object_is_the_fallback() {
    let (_core, after) = answered(
        json!({"thread": {"id": T, "reasoningEffort": "low"}, "model": "m"}),
        false,
    );
    assert_eq!(efforts(&after), vec![Some("low".to_owned())]);
}

#[test]
fn settings_updated_of_the_own_thread_emits_a_change_once() {
    let mut core = ready_core();
    assert_eq!(
        efforts(&settings(&mut core, T, "low")),
        vec![Some("low".to_owned())]
    );
    assert!(efforts(&settings(&mut core, T, "low")).is_empty());
    assert_eq!(
        efforts(&settings(&mut core, T, "high")),
        vec![Some("high".to_owned())]
    );
}

#[test]
fn settings_updated_of_a_sub_agent_thread_is_ignored() {
    let mut core = ready_core();
    let out = settings(&mut core, "child-thread", "low");
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(core.unknown_frames, 0);
}

#[test]
fn an_invalid_effort_is_not_passed_on() {
    let mut core = ready_core();
    assert!(settings(&mut core, T, "bogus value").is_empty());
    let (_core, after) = answered(
        json!({"thread": {"id": T}, "model": "m", "reasoningEffort": "bogus value"}),
        false,
    );
    assert!(efforts(&after).is_empty());
}

#[test]
fn resume_and_fork_answers_report_the_effort() {
    for (spec, name) in [
        (
            SessionSpec {
                resume: Some("t0".into()),
                ..SessionSpec::default()
            },
            "resume",
        ),
        (
            SessionSpec {
                resume: Some("t0".into()),
                fork: true,
                ..SessionSpec::default()
            },
            "fork",
        ),
    ] {
        let (mut core, out) = open_core(false, &spec);
        let after = reply(
            &mut core,
            &out,
            json!({"thread": {"id": T}, "model": "m", "reasoningEffort": "xhigh"}),
        );
        assert_eq!(efforts(&after), vec![Some("xhigh".to_owned())], "{name}");
    }
}

#[test]
fn a_pre_effort_codex_checkpoint_restores() {
    let core = ready_core();
    let mut checkpoint = core.checkpoint().unwrap();
    // The state of a build without the field.
    let removed = checkpoint
        .state
        .as_object_mut()
        .unwrap()
        .remove("thread_effort");
    assert!(removed.is_some());
    let mut core = CodexCore::restore(config("/w"), &checkpoint).unwrap();
    assert_eq!(core.thread_effort, None);
    assert_eq!(
        efforts(&settings(&mut core, T, "low")),
        vec![Some("low".to_owned())]
    );
}

#[test]
fn the_codex_checkpoint_keeps_the_thread_effort() {
    let mut core = ready_core();
    assert_eq!(efforts(&settings(&mut core, T, "high")).len(), 1);
    let checkpoint = core.checkpoint().unwrap();
    assert_eq!(checkpoint.state["thread_effort"], json!("high"));
    let mut core = CodexCore::restore(config("/w"), &checkpoint).unwrap();
    assert!(efforts(&settings(&mut core, T, "high")).is_empty());
}
