//! ADR-008 §5.1: the semantic `OptionKind` of every Codex decision shape.

use fleetdeck_proto::Event;
use fleetdeck_proto::event::{ApprovalOption, OptionKind};
use serde_json::{Value, json};

use super::requests::option_kind;
use super::tests::{T, emits, feed, ready_core, start_turn};

fn options_of(decisions: Option<Value>) -> Vec<ApprovalOption> {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let mut params = json!({
        "threadId": T, "turnId": "n1", "itemId": "call_1", "startedAtMs": 1,
        "command": "touch x", "cwd": "/w"
    });
    if let Some(d) = decisions {
        params["availableDecisions"] = d;
    }
    let ev = emits(&feed(
        &mut core,
        json!({"id": 0, "method": "item/commandExecution/requestApproval", "params": params}),
    ));
    let Some(Event::ApprovalRequested { options, .. }) = ev.into_iter().next() else {
        panic!("no approval request");
    };
    options
}

fn kinds(options: &[ApprovalOption]) -> Vec<(&str, Option<OptionKind>)> {
    options
        .iter()
        .map(|o| (o.option_id.as_str(), o.kind))
        .collect()
}

#[test]
fn the_default_decisions_are_classified() {
    let options = options_of(None);
    assert_eq!(
        kinds(&options),
        [
            ("accept", Some(OptionKind::AllowOnce)),
            ("acceptForSession", Some(OptionKind::AllowSession)),
            ("decline", Some(OptionKind::Deny)),
            ("cancel", Some(OptionKind::Deny)),
        ]
    );
}

#[test]
fn object_valued_amendments_are_amend_and_other_objects_are_other() {
    let options = options_of(Some(json!([
        "accept",
        {"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["touch", "x"]}},
        {"applyNetworkPolicyAmendment": {"network_policy_amendment": {"host": "example.org"}}},
        {"somethingNew": {"amendment": ["x"]}},
        "somethingElse",
        "decline"
    ])));
    assert_eq!(
        kinds(&options),
        [
            ("accept", Some(OptionKind::AllowOnce)),
            ("acceptWithExecpolicyAmendment", Some(OptionKind::Amend)),
            ("applyNetworkPolicyAmendment", Some(OptionKind::Amend)),
            ("somethingNew", Some(OptionKind::Other)),
            ("somethingElse", Some(OptionKind::Other)),
            ("decline", Some(OptionKind::Deny)),
        ]
    );
}

#[test]
fn the_classifier_covers_every_native_decision_id() {
    for (id, kind) in [
        ("accept", OptionKind::AllowOnce),
        ("acceptForSession", OptionKind::AllowSession),
        ("acceptWithExecpolicyAmendment", OptionKind::Amend),
        ("applyNetworkPolicyAmendment", OptionKind::Amend),
        ("decline", OptionKind::Deny),
        ("cancel", OptionKind::Deny),
        ("answer", OptionKind::Other),
        ("anything", OptionKind::Other),
    ] {
        assert_eq!(option_kind(id), kind, "{id}");
    }
}

#[test]
fn question_options_have_no_kind() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let ev = emits(&feed(
        &mut core,
        json!({"id": 1, "method": "item/tool/requestUserInput", "params": {
            "threadId": T, "turnId": "n1", "itemId": "q1",
            "questions": [{"id": "color", "header": "Color", "question": "Which color?",
                           "options": [{"label": "Blue"}]}]}}),
    ));
    let Some(Event::ApprovalRequested { options, .. }) = ev
        .into_iter()
        .find(|e| matches!(e, Event::ApprovalRequested { .. }))
    else {
        panic!("no question request");
    };
    assert!(!options.is_empty());
    assert!(options.iter().all(|o| o.kind.is_none()), "{options:?}");
}

#[test]
fn async_question_options_have_no_kind() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let ev = emits(&feed(
        &mut core,
        json!({"method": "item/completed", "params": {"threadId": T,
            "item": {"type": "agentMessage", "id": "m1", "text": "Pick one", "delivery": "async",
                     "questions": [{"title": "Which color?", "options": ["red", "blue"]}]}}}),
    ));
    let Some(Event::ApprovalRequested { options, .. }) = ev
        .into_iter()
        .find(|e| matches!(e, Event::ApprovalRequested { .. }))
    else {
        panic!("no question request");
    };
    assert!(options.iter().all(|o| o.kind.is_none()), "{options:?}");
}
