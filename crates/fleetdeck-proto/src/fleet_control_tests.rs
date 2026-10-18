//! ADR-008 wire types: the version gate (§3), the new coordinator ops and
//! queries (§4.1), the authority (§6.1) and the option kinds (§5.1).

use serde_json::{Value, json};

use crate::command::{
    CommandEnvelope, CommandOrigin, ControlSkill, CoordinatorSpec, GrantScope, SessionCommand,
};
use crate::event::{ApprovalOption, OptionKind};
use crate::ids::{CommandId, SessionId};
use crate::mutation::{
    AuthorityAnswer, AuthorityCheck, CommandAuthority, CoordinatorOp, CoordinatorQuery, PolicyNow,
    PolicyNowAnswer, ProjectItemKind, ProjectTarget,
};

/// A `coordinator.v1` start spec, recorded before ADR-008.
const V1_SPEC: &str = r#"{"grant_id":"grant-1","classes":["read","mail"],"idle_ttl_ms":60000,"expires_at":4102444800000,"control_skill":{"version":"1","text":"You coordinate the fleet."}}"#;

fn lineage_spec() -> CoordinatorSpec {
    CoordinatorSpec {
        grant_id: "grant-1".to_owned(),
        classes: vec!["read".to_owned(), "mail".to_owned()],
        idle_ttl_ms: 60_000,
        expires_at: 4_102_444_800_000,
        control_skill: ControlSkill {
            version: "1".to_owned(),
            text: "You coordinate the fleet.".to_owned(),
        },
        scope: GrantScope::Lineage,
        answer_cap: None,
        desktop_cap: None,
        capture_cap: None,
        image_byte_cap: None,
        stations: Vec::new(),
    }
}

#[test]
fn a_lineage_spec_serializes_byte_identically_to_the_recorded_v1_json() {
    assert_eq!(serde_json::to_string(&lineage_spec()).unwrap(), V1_SPEC);
    let decoded: CoordinatorSpec = serde_json::from_str(V1_SPEC).unwrap();
    assert_eq!(decoded, lineage_spec());
    assert_eq!(decoded.scope, GrantScope::Lineage);
    assert_eq!(decoded.answer_cap, None);
}

#[test]
fn a_fleet_spec_round_trips_with_scope_and_answer_cap() {
    let mut spec = lineage_spec();
    spec.classes = vec![
        "inspect".to_owned(),
        "manage".to_owned(),
        "answer".to_owned(),
    ];
    spec.scope = GrantScope::Fleet;
    spec.answer_cap = Some(500);
    let text = serde_json::to_string(&spec).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["scope"], "fleet");
    assert_eq!(value["answer_cap"], 500);
    assert_eq!(
        serde_json::from_str::<CoordinatorSpec>(&text).unwrap(),
        spec
    );
}

#[test]
fn a_spec_still_rejects_unknown_fields_and_unknown_scopes() {
    let mut value: Value = serde_json::from_str(V1_SPEC).unwrap();
    value["surprise"] = json!(1);
    assert!(serde_json::from_value::<CoordinatorSpec>(value).is_err());
    let mut value: Value = serde_json::from_str(V1_SPEC).unwrap();
    value["scope"] = json!("galaxy");
    assert!(serde_json::from_value::<CoordinatorSpec>(value).is_err());
}

fn op_samples() -> Vec<Value> {
    let (st, se) = ("st-1", "01J0000000000000000000000S");
    vec![
        json!({"op": "resume", "station": st, "session_id": se}),
        json!({"op": "rename", "station": st, "session_id": se, "title": "t"}),
        json!({"op": "set_mode", "station": st, "session_id": se, "mode": "plan"}),
        json!({"op": "project_assign", "station": st, "session_id": se,
               "target": {"kind": "project", "id": "p1"}}),
        json!({"op": "project_assign", "station": st, "session_id": se,
               "target": {"kind": "none"}}),
        json!({"op": "project_assign", "station": st, "session_id": se,
               "target": {"kind": "auto"}}),
        json!({"op": "project_start", "project_id": "p1", "prompt": "go",
               "provider_instance": "claude-main", "worktree": true}),
        json!({"op": "project_start", "project_id": "p1", "prompt": "go", "worktree": false}),
        json!({"op": "answer", "station": st, "session_id": se, "request_id": "req-1",
               "option_id": "allow"}),
        json!({"op": "answer", "station": st, "session_id": se, "request_id": "req-1",
               "question_answers": {"color": "red"}}),
        json!({"op": "answer", "station": st, "session_id": se, "request_id": "req-1",
               "text": "yes"}),
    ]
}

fn query_samples() -> Vec<Value> {
    let se = "01J0000000000000000000000S";
    vec![
        json!({"query": "session_tail", "session_id": se, "max_items": 50}),
        json!({"query": "session_diff", "session_id": se}),
        json!({"query": "needs_you"}),
        json!({"query": "projects"}),
        json!({"query": "project_items", "project_id": "p1", "kind": "issues"}),
        json!({"query": "project_items", "project_id": "p1", "kind": "prs"}),
        json!({"query": "artifacts"}),
        json!({"query": "artifacts", "session_id": se}),
        json!({"query": "artifact", "artifact_id": "a1"}),
        json!({"query": "artifact", "artifact_id": "a1", "version": 2}),
        json!({"query": "file_read", "session_id": se, "path": "src/lib.rs"}),
        json!({"query": "file_search", "session_id": se, "pattern": "todo", "mode": "content"}),
    ]
}

#[test]
fn every_new_op_round_trips_and_rejects_unknown_fields() {
    for sample in op_samples() {
        let op: CoordinatorOp = serde_json::from_value(sample.clone()).unwrap();
        assert_eq!(serde_json::to_value(&op).unwrap(), sample, "{sample}");
        let mut extra = sample.clone();
        extra["surprise"] = json!(1);
        assert!(
            serde_json::from_value::<CoordinatorOp>(extra).is_err(),
            "{sample}"
        );
    }
}

#[test]
fn every_new_query_round_trips_and_rejects_unknown_fields() {
    for sample in query_samples() {
        let query: CoordinatorQuery = serde_json::from_value(sample.clone()).unwrap();
        assert_eq!(serde_json::to_value(&query).unwrap(), sample, "{sample}");
        // serde ignores `deny_unknown_fields` for a unit variant of an
        // internally tagged enum (as for `list_sessions`), so only variants
        // with fields are checked.
        if sample.as_object().is_some_and(|m| m.len() == 1) {
            continue;
        }
        let mut extra = sample.clone();
        extra["surprise"] = json!(1);
        assert!(
            serde_json::from_value::<CoordinatorQuery>(extra).is_err(),
            "{sample}"
        );
    }
}

#[test]
fn the_new_op_and_query_payloads_decode_to_the_expected_variants() {
    let op: CoordinatorOp = serde_json::from_value(op_samples()[3].clone()).unwrap();
    let CoordinatorOp::ProjectAssign { target, .. } = op else {
        panic!("not a project_assign");
    };
    assert_eq!(target, ProjectTarget::Project { id: "p1".into() });
    let query: CoordinatorQuery = serde_json::from_value(query_samples()[5].clone()).unwrap();
    assert_eq!(
        query,
        CoordinatorQuery::ProjectItems {
            project_id: "p1".into(),
            kind: ProjectItemKind::Prs
        }
    );
}

#[test]
fn a_project_target_rejects_a_bad_shape() {
    for bad in [
        json!({"kind": "project"}),
        json!({"kind": "project", "id": "p1", "surprise": 1}),
        json!({"kind": "elsewhere"}),
    ] {
        assert!(
            serde_json::from_value::<ProjectTarget>(bad.clone()).is_err(),
            "{bad}"
        );
    }
}

fn envelope(authority: Option<CommandAuthority>) -> CommandEnvelope {
    CommandEnvelope {
        command_id: CommandId::new("01J0000000000000000000000C"),
        target_incarnation: None,
        retry_until: 10,
        origin: CommandOrigin::Coordinator,
        authority,
        command: SessionCommand::Interrupt {
            session_id: SessionId::new("01J0000000000000000000000S"),
        },
    }
}

#[test]
fn an_envelope_without_authority_serializes_as_before_and_decodes() {
    let text = serde_json::to_string(&envelope(None)).unwrap();
    assert!(!text.contains("authority"), "{text}");
    let old = r#"{"command_id":"01J0000000000000000000000C","target_incarnation":null,"retry_until":10,"origin":"coordinator","command":{"op":"interrupt","session_id":"01J0000000000000000000000S"}}"#;
    assert_eq!(text, old);
    assert_eq!(
        serde_json::from_str::<CommandEnvelope>(old).unwrap(),
        envelope(None)
    );
}

#[test]
fn an_envelope_with_authority_round_trips_and_the_authority_rejects_unknown_fields() {
    let authority = CommandAuthority {
        grant_id: "grant-1".to_owned(),
        caller_station: "st-1".into(),
        caller_session: SessionId::new("01J0000000000000000000000K"),
        generation: 3,
    };
    let text = serde_json::to_string(&envelope(Some(authority.clone()))).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["authority"]["generation"], 3);
    assert_eq!(
        serde_json::from_str::<CommandEnvelope>(&text)
            .unwrap()
            .authority,
        Some(authority)
    );
    let mut bad = value;
    bad["authority"]["surprise"] = json!(1);
    assert!(serde_json::from_value::<CommandEnvelope>(bad).is_err());
}

#[test]
fn the_authority_check_and_policy_types_round_trip_and_reject_unknown_fields() {
    let check = json!({"grant_id": "g", "generation": 2, "caller_station": "st-1",
        "caller_session": "01J0000000000000000000000K",
        "target_session": "01J0000000000000000000000S",
        "command_id": "01J0000000000000000000000C"});
    let parsed: AuthorityCheck = serde_json::from_value(check.clone()).unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), check);
    let mut extra = check;
    extra["surprise"] = json!(1);
    assert!(serde_json::from_value::<AuthorityCheck>(extra).is_err());

    let valid: AuthorityAnswer = serde_json::from_value(json!({"valid": true})).unwrap();
    assert_eq!(
        valid,
        AuthorityAnswer {
            valid: true,
            reason: None
        }
    );
    assert_eq!(
        serde_json::to_value(&valid).unwrap(),
        json!({"valid": true})
    );
    let invalid = AuthorityAnswer {
        valid: false,
        reason: Some("revoked".into()),
    };
    let text = serde_json::to_string(&invalid).unwrap();
    assert_eq!(
        serde_json::from_str::<AuthorityAnswer>(&text).unwrap(),
        invalid
    );
    assert!(serde_json::from_value::<AuthorityAnswer>(json!({"valid": true, "x": 1})).is_err());

    let policy = json!({"caller_session": "01J0000000000000000000000K"});
    let parsed: PolicyNow = serde_json::from_value(policy.clone()).unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), policy);
    assert!(serde_json::from_value::<PolicyNow>(json!({"caller_session": "k", "x": 1})).is_err());
    let answer = PolicyNowAnswer {
        fleet_control: true,
        bound: false,
        binding_matches: None,
    };
    let text = serde_json::to_string(&answer).unwrap();
    assert_eq!(
        serde_json::from_str::<PolicyNowAnswer>(&text).unwrap(),
        answer
    );
    assert!(
        serde_json::from_value::<PolicyNowAnswer>(
            json!({"fleet_control": true, "bound": true, "x": 1})
        )
        .is_err()
    );
}

#[test]
fn an_approval_option_without_kind_decodes_and_serializes_without_it() {
    let old = r#"{"option_id":"allow","label":"Allow"}"#;
    let option: ApprovalOption = serde_json::from_str(old).unwrap();
    assert_eq!(option.kind, None);
    assert_eq!(serde_json::to_string(&option).unwrap(), old);
}

#[test]
fn every_option_kind_has_its_snake_case_name() {
    for (kind, name) in [
        (OptionKind::AllowOnce, "allow_once"),
        (OptionKind::AllowSession, "allow_session"),
        (OptionKind::AllowAlways, "allow_always"),
        (OptionKind::Deny, "deny"),
        (OptionKind::Amend, "amend"),
        (OptionKind::Other, "other"),
    ] {
        let option = ApprovalOption {
            option_id: "x".into(),
            label: "X".into(),
            kind: Some(kind),
        };
        let value = serde_json::to_value(&option).unwrap();
        assert_eq!(value["kind"], name);
        assert_eq!(
            serde_json::from_value::<ApprovalOption>(value).unwrap(),
            option
        );
    }
}
