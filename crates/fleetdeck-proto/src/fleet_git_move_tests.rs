// Tests of the session move wire types (design §12, ADR-009 §4, ADR-010).
use serde_json::json;

use super::*;
use crate::command::SessionCommand;
use crate::event::{Event, EventBody};
use crate::mutation::CoordinatorOp;
use crate::version::feature;

const S: &str = "01J0000000000000000000000S";
const T: &str = "01J0000000000000000000000T";

const IN_STATES: [(&str, MoveInState); 11] = [
    ("staged", MoveInState::Staged),
    ("imported", MoveInState::Imported),
    ("launching", MoveInState::Launching),
    ("launch_refused", MoveInState::LaunchRefused),
    ("resumed", MoveInState::Resumed),
    ("released", MoveInState::Released),
    ("confirmed", MoveInState::Confirmed),
    ("resume_failed", MoveInState::ResumeFailed),
    ("failed", MoveInState::Failed),
    ("abandoned", MoveInState::Abandoned),
    ("discarded", MoveInState::Discarded),
];

fn publication() -> MovePublication {
    MovePublication {
        local_branch: "fd/alpha/ada".into(),
        dest: "https://github.com:443/o/r".into(),
        ref_name: "fd/alpha/ada".into(),
        oid: "a".repeat(40),
    }
}

fn move_in_json() -> serde_json::Value {
    json!({"op": "move_in", "move_id": "mv-1", "kind": "branch_only",
        "provider_instance": "claude", "title": "docs", "callsign_hint": "ada",
        "from": {"station_id": "alpha", "session_id": S, "publication": publication()},
        "fleet_git": {"kind": "worktree", "project_id": "p1", "project_name": "app",
            "settings": {}, "settings_revision": 1, "home_generation": 1,
            "home_path": "/w/app",
            "remote": {"repo": "github.com/o/r", "dest": "https://github.com:443/o/r"},
            "slug": "bravo", "clone_url": "https://github.com/o/r.git"}})
}

#[test]
fn the_move_commands_round_trip_with_their_wire_names_and_methods() {
    for (value, method, session) in [
        (
            json!({"op": "move_out", "session_id": S, "move_id": "mv-1", "to_station": "bravo"}),
            "session.move_out",
            Some(S),
        ),
        (
            json!({"op": "move_finish", "session_id": S, "move_id": "mv-1",
                "to_station": "bravo", "to_session": T}),
            "session.move_finish",
            Some(S),
        ),
        (
            json!({"op": "move_undo", "session_id": S, "move_id": "mv-1"}),
            "session.move_undo",
            Some(S),
        ),
        (move_in_json(), "session.move_in", None),
    ] {
        let command: SessionCommand = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(command.method(), method);
        assert_eq!(command.session_id().map(|id| id.as_str()), session);
        let back = serde_json::to_value(&command).unwrap();
        assert_eq!(back["op"], value["op"]);
        // The wire form decodes to the same command (the `settings` of a
        // `move_in` is written in full).
        assert_eq!(
            serde_json::from_value::<SessionCommand>(back.clone()).unwrap(),
            command
        );
        if method != "session.move_in" {
            assert_eq!(back, value);
        }
    }
}

#[test]
fn a_move_in_without_title_and_callsign_hint_omits_both() {
    let mut value = move_in_json();
    value.as_object_mut().unwrap().remove("title");
    value.as_object_mut().unwrap().remove("callsign_hint");
    let command: SessionCommand = serde_json::from_value(value.clone()).unwrap();
    let SessionCommand::MoveIn {
        title,
        callsign_hint,
        kind,
        ..
    } = &command
    else {
        panic!("not a move_in");
    };
    assert!(title.is_none() && callsign_hint.is_none());
    assert_eq!(*kind, MoveKind::BranchOnly);
    let back = serde_json::to_value(&command).unwrap();
    assert!(back.get("title").is_none() && back.get("callsign_hint").is_none());
    assert_eq!(
        serde_json::from_value::<SessionCommand>(back).unwrap(),
        command
    );
}

#[test]
fn a_move_in_with_a_newer_kind_decodes_as_unknown() {
    let mut value = move_in_json();
    value["kind"] = json!("teleport");
    let command: SessionCommand = serde_json::from_value(value).unwrap();
    assert!(matches!(
        command,
        SessionCommand::MoveIn {
            kind: MoveKind::Unknown,
            ..
        }
    ));
}

#[test]
fn move_in_state_decodes_all_values_and_a_newer_value_as_unknown() {
    for (wire, state) in IN_STATES {
        assert_eq!(
            serde_json::from_value::<MoveInState>(json!(wire)).unwrap(),
            state
        );
        assert_eq!(serde_json::to_value(state).unwrap(), json!(wire));
        // The status state is the union of the target and the source states.
        let status: MoveStatusState = serde_json::from_value(json!(wire)).unwrap();
        assert_eq!(serde_json::to_value(status).unwrap(), json!(wire));
    }
    for wire in ["holding", "finished", "aborted", "undone"] {
        let status: MoveStatusState = serde_json::from_value(json!(wire)).unwrap();
        assert_ne!(status, MoveStatusState::Unknown, "{wire}");
        assert_eq!(serde_json::to_value(status).unwrap(), json!(wire));
    }
    assert_eq!(
        serde_json::from_value::<MoveInState>(json!("teleporting")).unwrap(),
        MoveInState::Unknown
    );
    assert_eq!(
        serde_json::from_value::<MoveStatusState>(json!("teleporting")).unwrap(),
        MoveStatusState::Unknown
    );
    assert_eq!(
        serde_json::from_value::<MoveReleaseState>(json!("teleporting")).unwrap(),
        MoveReleaseState::Unknown
    );
    assert_eq!(
        serde_json::from_value::<MoveDiscardState>(json!("teleporting")).unwrap(),
        MoveDiscardState::Unknown
    );
    assert_eq!(
        serde_json::from_value::<MoveDiscardEffect>(json!("teleporting")).unwrap(),
        MoveDiscardEffect::Unknown
    );
}

#[test]
fn a_discard_answer_is_an_ack_only_for_a_final_state_with_a_known_effect() {
    let done: MoveDiscardAnswer =
        serde_json::from_value(json!({"state": "discarded", "effect": "settled"})).unwrap();
    assert_eq!(done.state, MoveDiscardState::Discarded);
    assert_eq!(done.effect, Some(MoveDiscardEffect::Settled));
    assert!(done.is_ack());
    let settling: MoveDiscardAnswer = serde_json::from_value(json!({"state": "settling"})).unwrap();
    assert_eq!(settling.effect, None);
    assert!(!settling.is_ack());
    assert_eq!(
        serde_json::to_value(&settling).unwrap(),
        json!({"state": "settling"})
    );
    for (state, effect) in [
        ("abandoned", "none"),
        ("resumed", "none"),
        ("discarded", "cancelled"),
    ] {
        let a: MoveDiscardAnswer =
            serde_json::from_value(json!({"state": state, "effect": effect})).unwrap();
        assert!(a.is_ack(), "{state} {effect}");
    }
    for value in [
        json!({"state": "discarded"}),
        json!({"state": "discarded", "effect": "later"}),
        json!({"state": "newer", "effect": "settled"}),
        json!({"state": "settling", "effect": "settled"}),
    ] {
        let a: MoveDiscardAnswer = serde_json::from_value(value.clone()).unwrap();
        assert!(!a.is_ack(), "{value}");
    }
}

#[test]
fn an_export_answer_for_a_branch_only_move_omits_the_manifest_and_the_token() {
    let answer = MoveExportAnswer {
        publication: publication(),
        manifest: None,
        stream_token: None,
    };
    let value = serde_json::to_value(&answer).unwrap();
    assert_eq!(value, json!({"publication": publication()}));
    assert_eq!(
        serde_json::from_value::<MoveExportAnswer>(value).unwrap(),
        answer
    );
}

#[test]
fn a_transcript_manifest_round_trips_inside_the_export_and_prepare_shapes() {
    let manifest = TranscriptManifest {
        provider: crate::event::ProviderKind::Claude,
        native_session_id: "cli-1".into(),
        files: vec![ManifestFile {
            key: "main".into(),
            rel: "cli-1.jsonl".into(),
            size: 10,
            sha256: "f".repeat(64),
        }],
        total: 10,
    };
    let answer = MoveExportAnswer {
        publication: publication(),
        manifest: Some(manifest.clone()),
        stream_token: Some("tok".into()),
    };
    let value = serde_json::to_value(&answer).unwrap();
    assert_eq!(value["manifest"]["files"][0]["rel"], "cli-1.jsonl");
    assert_eq!(
        serde_json::from_value::<MoveExportAnswer>(value).unwrap(),
        answer
    );
    let prepare = MovePrepareParams {
        move_id: "mv-1".into(),
        manifest,
        source: MoveSourceRef {
            station_id: StationId::from("alpha"),
            session_id: SessionId::from(S),
        },
    };
    let value = serde_json::to_value(&prepare).unwrap();
    assert_eq!(value["source"]["station_id"], "alpha");
    assert_eq!(
        serde_json::from_value::<MovePrepareParams>(value).unwrap(),
        prepare
    );
}

#[test]
fn a_status_answer_decodes_a_source_state_and_a_target_state() {
    let source: MoveStatusAnswer = serde_json::from_value(json!({"state": "finished"})).unwrap();
    assert_eq!(source.state, MoveStatusState::Finished);
    assert!(source.session_id.is_none() && source.code.is_none());
    let target: MoveStatusAnswer = serde_json::from_value(json!({
        "state": "resume_failed", "session_id": T,
        "code": "move_resume_failed", "text": "No conversation found"}))
    .unwrap();
    assert_eq!(target.state, MoveStatusState::ResumeFailed);
    assert_eq!(target.session_id.as_ref().map(|s| s.as_str()), Some(T));
    assert_eq!(
        serde_json::to_value(&source).unwrap(),
        json!({"state": "finished"})
    );
}

#[test]
fn the_small_answers_have_their_wire_shape() {
    assert_eq!(serde_json::to_value(MoveAbortAnswer {}).unwrap(), json!({}));
    assert_eq!(
        serde_json::to_value(MoveReleaseAnswer {
            state: MoveReleaseState::Confirmed
        })
        .unwrap(),
        json!({"state": "confirmed"})
    );
    assert_eq!(
        serde_json::to_value(MoveFailAnswer {
            state: MoveInState::Failed
        })
        .unwrap(),
        json!({"state": "failed"})
    );
    assert_eq!(
        serde_json::to_value(MoveIdParams {
            move_id: "mv-1".into()
        })
        .unwrap(),
        json!({"move_id": "mv-1"})
    );
    assert_eq!(MOVE_MAX_BYTES, 64 * 1024 * 1024);
    assert_eq!(MOVE_MAX_FILES, 256);
    assert_eq!(MOVE_EXPORT_PURPOSE, "move-export:");
    assert_eq!(MOVE_IMPORT_PURPOSE, "move-import:");
}

#[test]
fn the_three_move_events_round_trip_and_are_known_kinds() {
    for kind in ["session.move_in", "session.moved", "session.move_undone"] {
        assert!(Event::KINDS.contains(&kind), "{kind}");
    }
    let info = MoveInInfo {
        move_id: "mv-1".into(),
        state: MoveInState::ResumeFailed,
        code: Some("move_resume_failed".into()),
        text: None,
    };
    let events = [
        (
            Event::SessionMoveIn(info),
            json!({"kind": "session.move_in", "move_id": "mv-1", "state": "resume_failed",
                "code": "move_resume_failed"}),
        ),
        (
            Event::SessionMoved {
                move_id: "mv-1".into(),
                to_station: StationId::from("bravo"),
                to_session: SessionId::from(T),
            },
            json!({"kind": "session.moved", "move_id": "mv-1", "to_station": "bravo",
                "to_session": T}),
        ),
        (
            Event::SessionMoveUndone {
                move_id: "mv-1".into(),
            },
            json!({"kind": "session.move_undone", "move_id": "mv-1"}),
        ),
    ];
    for (event, value) in events {
        assert_eq!(serde_json::to_value(&event).unwrap(), value);
        assert_eq!(event.kind_name(), value["kind"].as_str().unwrap());
        let body: EventBody = serde_json::from_value(value).unwrap();
        assert_eq!(body, EventBody::Known(event));
    }
}

#[test]
fn a_move_in_event_with_a_newer_state_still_decodes_as_known() {
    let body: EventBody = serde_json::from_value(json!({
        "kind": "session.move_in", "move_id": "mv-1", "state": "teleporting"}))
    .unwrap();
    let EventBody::Known(Event::SessionMoveIn(info)) = body else {
        panic!("not a known move_in event");
    };
    assert_eq!(info.state, MoveInState::Unknown);
}

#[test]
fn the_coordinator_move_op_round_trips_with_its_wire_name() {
    let value = json!({"op": "move", "station": "alpha", "session_id": S, "to_station": "bravo"});
    let op: CoordinatorOp = serde_json::from_value(value.clone()).unwrap();
    assert!(matches!(op, CoordinatorOp::Move { .. }));
    assert_eq!(serde_json::to_value(&op).unwrap(), value);
}

#[test]
fn the_move_feature_is_offered_and_its_name_is_exact() {
    assert_eq!(feature::FLEET_GIT_MOVE, "fleet_git.move.v1");
    assert!(feature::ALL.contains(&feature::FLEET_GIT_MOVE));
    assert!(!feature::REQUIRED.contains(&feature::FLEET_GIT_MOVE));
}
