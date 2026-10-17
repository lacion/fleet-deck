// Tests of the radar and land wire types (design §10, §11; ADR-009 §4).
use super::*;
use serde_json::json;

const STATES: [(&str, LandState); 10] = [
    ("queued", LandState::Queued),
    ("running", LandState::Running),
    ("landed", LandState::Landed),
    ("conflict", LandState::Conflict),
    ("check_failed", LandState::CheckFailed),
    ("check_mutated", LandState::CheckMutated),
    ("remote_moved", LandState::RemoteMoved),
    ("failed", LandState::Failed),
    ("unresolved", LandState::Unresolved),
    ("cancelled", LandState::Cancelled),
];

const PHASES: [(&str, LandPhase); 12] = [
    ("admitted", LandPhase::Admitted),
    ("captured", LandPhase::Captured),
    ("worktree_ready", LandPhase::WorktreeReady),
    ("rebased", LandPhase::Rebased),
    ("checking", LandPhase::Checking),
    ("checked", LandPhase::Checked),
    ("branch_push_intent", LandPhase::BranchPushIntent),
    ("branch_pushed", LandPhase::BranchPushed),
    ("pr_intent", LandPhase::PrIntent),
    ("default_push_intent", LandPhase::DefaultPushIntent),
    ("lost", LandPhase::Lost),
    ("done", LandPhase::Done),
];

#[test]
fn land_states_and_phases_have_their_wire_names_and_an_unknown_fallback() {
    for (name, want) in STATES {
        let got: LandState = serde_json::from_value(json!(name)).unwrap();
        assert_eq!(got, want, "{name}");
        assert_eq!(serde_json::to_value(want).unwrap(), json!(name));
    }
    for (name, want) in PHASES {
        let got: LandPhase = serde_json::from_value(json!(name)).unwrap();
        assert_eq!(got, want, "{name}");
        assert_eq!(serde_json::to_value(want).unwrap(), json!(name));
    }
    let state: LandState = serde_json::from_value(json!("paused")).unwrap();
    let phase: LandPhase = serde_json::from_value(json!("teleporting")).unwrap();
    assert_eq!((state, phase), (LandState::Unknown, LandPhase::Unknown));
}

#[test]
fn only_the_ending_states_are_terminal_and_unknown_never_is() {
    let terminal: Vec<LandState> = STATES
        .iter()
        .map(|(_, s)| *s)
        .filter(|s| s.is_terminal())
        .collect();
    assert_eq!(
        terminal,
        [
            LandState::Landed,
            LandState::Conflict,
            LandState::CheckFailed,
            LandState::CheckMutated,
            LandState::RemoteMoved,
            LandState::Failed,
            LandState::Unresolved,
            LandState::Cancelled,
        ]
    );
    assert!(!LandState::Queued.is_terminal() && !LandState::Running.is_terminal());
    assert!(!LandState::Unknown.is_terminal());
}

fn full_plan() -> serde_json::Value {
    json!({
        "project_id": "p1", "settings_revision": 3, "home_generation": 1,
        "mode": "pr", "fetch_remote": "origin",
        "remote": {"repo": "github.com/o/r", "dest": "https://github.com:443/o/r"},
        "default_branch": "main",
        "branch_ref": "fd/o-r/ada", "publish_ref": "fd/o-r/ada",
        "branch_target": {"target": "origin", "dest": "https://github.com:443/o/r"},
        "forge": {"kind": "github", "host": "github.com", "repo": "o/r"},
        "check_cmd": "make check", "check_env": {"A": "1"},
        "pr_title": "t", "pr_body": "b"
    })
}

#[test]
fn a_land_plan_round_trips_in_full_and_with_its_options_absent() {
    let plan: LandPlan = serde_json::from_value(full_plan()).unwrap();
    assert_eq!(plan.mode, LandMode::Pr);
    assert_eq!(plan.default_branch.as_deref(), Some("main"));
    assert!(plan.forge.is_some());
    assert_eq!(serde_json::to_value(&plan).unwrap(), full_plan());

    let mut bare = full_plan();
    for key in ["default_branch", "forge", "check_cmd"] {
        bare.as_object_mut().unwrap().remove(key);
    }
    let plan: LandPlan = serde_json::from_value(bare.clone()).unwrap();
    assert!(plan.default_branch.is_none() && plan.forge.is_none() && plan.check_cmd.is_none());
    assert_eq!(serde_json::to_value(&plan).unwrap(), bare);
}

#[test]
fn a_land_plan_with_a_newer_mode_decodes_and_a_missing_field_is_refused() {
    let mut value = full_plan();
    value["mode"] = json!("squash");
    let plan: LandPlan = serde_json::from_value(value).unwrap();
    assert_eq!(plan.mode, LandMode::Unknown);
    let mut value = full_plan();
    value.as_object_mut().unwrap().remove("publish_ref");
    assert!(serde_json::from_value::<LandPlan>(value).is_err());
    // An extra field of a newer deck is ignored (additive wire, O5).
    let mut value = full_plan();
    value["future"] = json!(1);
    assert!(serde_json::from_value::<LandPlan>(value).is_ok());
}

fn status(state: &str) -> serde_json::Value {
    json!({"state": state, "phase": "checking", "reconciling": false, "attempt": 1,
        "files": ["a.rs"], "source_oid": "a".repeat(40)})
}

#[test]
fn the_land_status_answer_tells_fenced_unknown_and_a_status_apart() {
    let fenced: LandStatusAnswer = serde_json::from_value(json!({"state": "fenced"})).unwrap();
    assert_eq!(fenced, LandStatusAnswer::Fenced);
    assert_eq!(
        serde_json::to_value(&fenced).unwrap(),
        json!({"state": "fenced"})
    );
    let unknown: LandStatusAnswer = serde_json::from_value(json!({"state": "unknown"})).unwrap();
    assert_eq!(unknown, LandStatusAnswer::Unknown);
    assert_eq!(
        serde_json::to_value(&unknown).unwrap(),
        json!({"state": "unknown"})
    );
    let known: LandStatusAnswer = serde_json::from_value(status("running")).unwrap();
    let LandStatusAnswer::Known(st) = &known else {
        panic!("a status is Known: {known:?}");
    };
    assert_eq!(
        (st.state, st.phase, st.attempt),
        (LandState::Running, LandPhase::Checking, 1)
    );
    assert_eq!(st.files, ["a.rs"]);
    assert_eq!(serde_json::to_value(&known).unwrap(), status("running"));
}

#[test]
fn a_status_with_a_newer_state_and_a_phase_is_known_not_unknown() {
    let got: LandStatusAnswer = serde_json::from_value(status("paused")).unwrap();
    let LandStatusAnswer::Known(st) = got else {
        panic!("a newer state with a phase is Known");
    };
    assert_eq!(st.state, LandState::Unknown);
    // A `fenced` word with a phase is not the marker either.
    let got: LandStatusAnswer = serde_json::from_value(status("fenced")).unwrap();
    assert!(matches!(got, LandStatusAnswer::Known(_)));
    assert!(serde_json::from_value::<LandStatusAnswer>(json!({"nope": 1})).is_err());
}

#[test]
fn the_land_abandon_answer_decodes_busy_and_fenced_with_each_write() {
    let busy: LandAbandonAnswer = serde_json::from_value(json!({"state": "busy"})).unwrap();
    assert_eq!(busy, LandAbandonAnswer::Busy);
    assert_eq!(
        serde_json::to_value(&busy).unwrap(),
        json!({"state": "busy"})
    );
    for (name, write) in [
        ("none", LandWrite::None),
        ("applied", LandWrite::Applied),
        ("rejected", LandWrite::Rejected),
        ("unknown", LandWrite::Unknown),
    ] {
        let value = json!({"state": "fenced", "write": name, "run_state": "unresolved"});
        let got: LandAbandonAnswer = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            got,
            LandAbandonAnswer::Fenced {
                write,
                run_state: LandState::Unresolved
            }
        );
        assert_eq!(serde_json::to_value(&got).unwrap(), value);
    }
    // A state or a write of a newer station fails to decode (the deck keeps
    // the row `requested` and sends again).
    assert!(serde_json::from_value::<LandAbandonAnswer>(json!({"state": "gone"})).is_err());
    let bad = json!({"state": "fenced", "write": "maybe", "run_state": "failed"});
    assert!(serde_json::from_value::<LandAbandonAnswer>(bad).is_err());
}

#[test]
fn the_radar_types_round_trip_with_a_pair_against_the_default_branch() {
    let params = RadarParams {
        project_id: "p1".into(),
        branches: vec![RadarBranch {
            key: "s1".into(),
            ref_name: "fd/o-r/ada".into(),
        }],
    };
    assert_eq!(
        serde_json::to_value(&params).unwrap(),
        json!({"project_id": "p1", "branches": [{"key": "s1", "ref_name": "fd/o-r/ada"}]})
    );
    let result = json!({
        "checked_at": 1_791_000_000_000_i64, "base_oid": "b".repeat(40),
        "pairs": [
            {"a": "s1", "b": RADAR_DEFAULT_KEY, "conflict": true,
             "files": ["a.rs"], "files_truncated": false},
            {"a": "s1", "b": "s2", "conflict": false, "files": [], "files_truncated": false}
        ],
        "skipped": 2
    });
    let got: RadarResult = serde_json::from_value(result.clone()).unwrap();
    assert_eq!(got.pairs[0].b, "@default");
    assert!(got.pairs[0].conflict && got.code.is_none());
    assert_eq!(serde_json::to_value(&got).unwrap(), result);
    // A failed fetch has no base oid and carries a code and a text.
    let failed: RadarResult = serde_json::from_value(json!({
        "checked_at": 1, "pairs": [], "skipped": 0,
        "code": "fetch_failed", "text": "t"
    }))
    .unwrap();
    assert!(failed.base_oid.is_none());
    assert_eq!(failed.code.as_deref(), Some(code::FETCH_FAILED));
    assert_eq!(RADAR_MAX_BRANCHES, 50);
}

#[test]
fn the_land_codes_have_their_wire_strings() {
    assert_eq!(code::RADAR_BUSY, "radar_busy");
    assert_eq!(code::ABANDONED, "abandoned");
    assert_eq!(code::STATION_REMOVED, "station_removed");
}
