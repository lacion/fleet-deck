//! ADR-008 §5.1: the semantic `OptionKind` of every Claude option.

use fleetdeck_proto::Event;
use fleetdeck_proto::event::OptionKind;
use serde_json::{Value, json};

use super::requests::option_kind;
use super::tests::{emits, feed, running_core};

fn options_of(tool: &str, suggestions: Option<Value>) -> Vec<(String, Option<OptionKind>)> {
    let mut core = running_core();
    let mut request = json!({
        "subtype": "can_use_tool", "tool_name": tool, "input": {"command": "ls"}
    });
    if let Some(s) = suggestions {
        request["permission_suggestions"] = s;
    }
    let ev = emits(&feed(
        &mut core,
        json!({"type": "control_request", "request_id": "n1", "request": request}),
    ));
    let Some(Event::ApprovalRequested { options, .. }) = ev.into_iter().next() else {
        panic!("no approval request");
    };
    options.into_iter().map(|o| (o.option_id, o.kind)).collect()
}

#[test]
fn allow_and_deny_are_classified_without_rules() {
    assert_eq!(
        options_of("Bash", None),
        [
            ("allow".to_owned(), Some(OptionKind::AllowOnce)),
            ("deny".to_owned(), Some(OptionKind::Deny)),
        ]
    );
}

#[test]
fn an_option_that_carries_permission_rules_is_allow_always() {
    let rules = json!([
        {"type": "addRules", "behavior": "allow", "destination": "localSettings",
         "rules": [{"toolName": "Bash"}]}
    ]);
    assert_eq!(
        options_of("Bash", Some(rules)),
        [
            ("allow".to_owned(), Some(OptionKind::AllowOnce)),
            ("allow_always".to_owned(), Some(OptionKind::AllowAlways)),
            ("deny".to_owned(), Some(OptionKind::Deny)),
        ]
    );
}

#[test]
fn a_question_request_has_no_option_kinds() {
    let options = options_of("AskUserQuestion", None);
    assert!(!options.is_empty());
    assert!(options.iter().all(|(_, k)| k.is_none()), "{options:?}");
}

#[test]
fn the_classifier_fails_closed_for_an_unknown_id() {
    assert_eq!(option_kind("allow"), OptionKind::AllowOnce);
    assert_eq!(option_kind("allow_always"), OptionKind::AllowAlways);
    assert_eq!(option_kind("deny"), OptionKind::Deny);
    assert_eq!(option_kind("allow_forever"), OptionKind::Other);
    assert_eq!(option_kind(""), OptionKind::Other);
}
