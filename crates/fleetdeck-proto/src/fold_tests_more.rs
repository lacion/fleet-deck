// The newer fold tests (split from `fold_tests.rs` for the module size rule).
use super::*;
use crate::session::BatchRef;

#[test]
fn session_base_sets_the_base_of_the_summary() {
    let mut f = Feed::new();
    f.push(started());
    assert_eq!(f.summary.base, None);
    let s = f.push(Event::SessionBase {
        git_ref: Some("main".to_owned()),
        oid: "abc123".to_owned(),
        inferred: true,
    });
    let base = s.base.clone().unwrap();
    assert_eq!(base.git_ref.as_deref(), Some("main"));
    assert_eq!(base.oid, "abc123");
    assert!(base.inferred);
    // A replay of the same sequence changes nothing.
    let before = s.clone();
    f.seq -= 1;
    assert_eq!(
        *f.push(Event::SessionBase {
            git_ref: None,
            oid: "other".to_owned(),
            inferred: false,
        }),
        before
    );
}

#[test]
fn session_batch_sets_the_batch_of_the_summary() {
    let mut f = Feed::new();
    f.push(started());
    assert_eq!(f.summary.batch, None);
    let batch = BatchRef {
        batch_id: "b1".to_owned(),
        index: 2,
        size: 4,
    };
    let s = f.push(Event::SessionBatch {
        batch: batch.clone(),
    });
    assert_eq!(s.batch, Some(batch));
}

#[test]
fn a_batch_reference_is_valid_only_inside_its_limits() {
    let ok = |id: &str, index, size| {
        BatchRef {
            batch_id: id.to_owned(),
            index,
            size,
        }
        .problem()
    };
    assert_eq!(ok("01J-ab_Z9", 1, 1), None);
    assert_eq!(ok("b", 100, 100), None);
    assert!(ok("", 1, 2).is_some());
    assert!(ok("a b", 1, 2).is_some());
    assert!(ok(&"a".repeat(65), 1, 2).is_some());
    assert!(ok("b", 0, 2).is_some());
    assert!(ok("b", 3, 2).is_some());
    assert!(ok("b", 1, 101).is_some());
}

fn terminal(shell_id: Option<&str>, pid: Option<u32>, reason: Option<&str>) -> Event {
    Event::SessionTerminal {
        shell_id: shell_id.map(str::to_owned),
        pid,
        reason: reason.map(str::to_owned),
    }
}

#[test]
fn a_terminal_hold_is_set_updated_and_cleared_by_session_terminal() {
    let mut f = Feed::new();
    f.push(started());
    // (a) The first event sets the hold; `since` is the envelope time.
    let s = f.push(terminal(Some("tui-1"), None, Some("handover requested")));
    let first = s.terminal.clone().expect("hold");
    assert_eq!(first.shell_id, "tui-1");
    assert_eq!(first.pid, None);
    assert_eq!(first.since, s.updated_at);
    assert_eq!(first.reason.as_deref(), Some("handover requested"));
    // (b) The same shell with a pid keeps `since` and takes pid and reason.
    let s = f.push(terminal(Some("tui-1"), Some(7), Some("running")));
    let hold = s.terminal.clone().expect("hold");
    assert_eq!(hold.since, first.since);
    assert_ne!(s.updated_at, first.since);
    assert_eq!(hold.pid, Some(7));
    assert_eq!(hold.reason.as_deref(), Some("running"));
    // (c) The same shell without a pid keeps the known pid.
    let s = f.push(terminal(Some("tui-1"), None, Some("resized")));
    let hold = s.terminal.clone().expect("hold");
    assert_eq!(hold.pid, Some(7));
    assert_eq!(hold.since, first.since);
    assert_eq!(hold.reason.as_deref(), Some("resized"));
    // (d) No shell id ends the hold.
    let s = f.push(terminal(None, None, Some("tui exited")));
    assert_eq!(s.terminal, None);
}

#[test]
fn a_summary_decodes_without_terminal_and_encodes_none_without_the_key() {
    // (e)
    let mut v = serde_json::to_value(SessionSummary::new(SessionId::from("s1"))).unwrap();
    let object = v.as_object_mut().unwrap();
    assert!(!object.contains_key("terminal"), "None is not serialized");
    object.remove("terminal");
    let s: SessionSummary = serde_json::from_value(v).unwrap();
    assert_eq!(s.terminal, None);
}

#[test]
fn an_inferred_cost_part_makes_the_total_inferred() {
    let mut f = Feed::new();
    f.push(started());
    let usage = |cost, inferred| Event::UsageUpdated {
        usage: Usage {
            input_tokens: Some(1),
            output_tokens: Some(1),
            cost_usd: cost,
            cost_inferred: inferred,
            ..Usage::default()
        },
        rate_limits: vec![],
    };
    let s = f.push(usage(Some(0.5), false));
    assert!(!s.usage.cost_inferred);
    // An inferred flag without a cost is not a part.
    let s = f.push(usage(None, true));
    assert!(!s.usage.cost_inferred);
    let s = f.push(usage(Some(0.25), true));
    assert!(s.usage.cost_inferred);
    assert_eq!(s.usage.cost_usd, Some(0.75));
}

#[test]
fn artifacts_count_new_artifacts_once_and_the_focus_follows_the_last_event() {
    use crate::artifacts::ArtifactKind;
    let published = |id: &str, version: u32| Event::ArtifactPublished {
        artifact_id: id.to_owned(),
        version,
        title: "Design".to_owned(),
        artifact_kind: ArtifactKind::Html,
    };
    let mut f = Feed::new();
    f.push(started());
    assert_eq!(
        (f.summary.artifacts_count, f.summary.focus.clone()),
        (0, None)
    );
    f.push(published("a", 1));
    f.push(published("a", 2));
    let s = f.push(published("b", 1));
    assert_eq!(s.artifacts_count, 2);
    // A suggestion changes no count.
    let s = f.push(Event::ArtifactSuggested {
        path: "out/a.html".to_owned(),
    });
    assert_eq!(s.artifacts_count, 2);
    let s = f.push(Event::SessionFocus {
        text: "fixing the login test".to_owned(),
    });
    assert_eq!(s.focus.as_deref(), Some("fixing the login test"));
    let s = f.push(Event::SessionFocus {
        text: String::new(),
    });
    assert_eq!(s.focus, None);
}

#[test]
fn hibernation_is_kept_through_the_stop_and_cleared_by_the_next_start() {
    let mut f = Feed::new();
    f.push(started());
    assert!(!f.summary.hibernated);
    f.push(Event::SessionHibernated { after_min: 30 });
    assert!(f.summary.hibernated);
    let s = f.push(Event::SessionEnded {
        reason: "stopped".to_owned(),
        exit_code: Some(0),
    });
    assert!(s.hibernated);
    assert_eq!(s.activity, Activity::Ended);
    let s = f.push(started());
    assert!(!s.hibernated);
    assert_eq!(s.activity, Activity::Idle);
}

#[test]
fn a_generated_title_is_marked_until_the_owner_renames_the_session() {
    let mut f = Feed::new();
    f.push(started());
    assert!(!f.summary.title_generated);
    // The station renames first, then marks the title as generated.
    f.push(Event::SessionRenamed {
        title: Some("Fix the login redirect".to_owned()),
    });
    let s = f.push(Event::SessionTitleGenerated {
        title: "Fix the login redirect".to_owned(),
    });
    assert_eq!(s.title.as_deref(), Some("Fix the login redirect"));
    assert!(s.title_generated);
    // An owner rename removes the mark, also with the same text.
    let s = f.push(Event::SessionRenamed {
        title: Some("Fix the login redirect".to_owned()),
    });
    assert!(!s.title_generated);
    // A marker for a title that is not the current one marks nothing.
    let s = f.push(Event::SessionTitleGenerated {
        title: "Another title".to_owned(),
    });
    assert!(!s.title_generated);
    f.push(Event::SessionRenamed { title: None });
    assert!(!f.summary.title_generated);
}

#[test]
fn a_digest_sets_the_summary_digest_and_the_newest_wins() {
    let mut f = Feed::new();
    f.push(started());
    assert_eq!(f.summary.digest, None);
    f.push(Event::SessionDigest {
        text: "- one".to_owned(),
    });
    let s = f.push(Event::SessionDigest {
        text: "- two".to_owned(),
    });
    assert_eq!(s.digest.as_deref(), Some("- two"));
}

#[test]
fn a_summary_decodes_without_the_digest_fields() {
    let mut v = serde_json::to_value(SessionSummary::new(SessionId::from("s1"))).unwrap();
    let object = v.as_object_mut().unwrap();
    assert!(!object.contains_key("digest"), "None is not serialized");
    object.remove("title_generated");
    let s: SessionSummary = serde_json::from_value(v).unwrap();
    assert!(!s.title_generated);
    assert_eq!(s.digest, None);
}

fn land_event(land_id: &str, state: crate::fleet_git::LandState, attempt: u32) -> Event {
    Event::SessionLand(Box::new(crate::fleet_git::LandInfo {
        land_id: land_id.to_owned(),
        attempt,
        state,
        phase: crate::fleet_git::LandPhase::Checking,
        reconciling: false,
        code: None,
        text: None,
        files: vec!["a.rs".to_owned()],
        source_oid: Some("a".repeat(40)),
        base_oid: None,
        rebased_oid: None,
        branch_published_oid: None,
        default_published_oid: None,
        pr_url: Some("https://example.test/pr/1".to_owned()),
    }))
}

#[test]
fn session_land_sets_and_replaces_the_land_of_the_summary() {
    use crate::fleet_git::LandState;
    let mut f = Feed::new();
    f.push(started());
    assert!(f.summary.land.is_none());
    let s = f.push(land_event("land-1", LandState::Running, 1));
    let land = s.land.clone().unwrap();
    assert_eq!((land.land_id.as_str(), land.attempt), ("land-1", 1));
    assert_eq!(land.state, LandState::Running);
    assert_eq!(land.files, ["a.rs"]);
    assert_eq!(land.pr_url.as_deref(), Some("https://example.test/pr/1"));
    // A later event replaces it, also one of another land id.
    let s = f.push(land_event("land-2", LandState::Landed, 2));
    let land = s.land.clone().unwrap();
    assert_eq!((land.land_id.as_str(), land.attempt), ("land-2", 2));
    assert_eq!(land.state, LandState::Landed);
    // An empty `land` is not serialized, and its absence decodes.
    let v = serde_json::to_value(&f.summary).unwrap();
    assert!(v.as_object().unwrap().contains_key("land"));
    let bare = serde_json::to_value(SessionSummary::new(SessionId::from("s2"))).unwrap();
    assert!(!bare.as_object().unwrap().contains_key("land"));
    let back: SessionSummary = serde_json::from_value(bare).unwrap();
    assert!(back.land.is_none());
}

#[test]
fn a_session_land_with_a_newer_state_still_folds_as_known() {
    use crate::fleet_git::LandState;
    let mut f = Feed::new();
    f.push(started());
    let body: EventBody = serde_json::from_value(serde_json::json!({
        "kind": "session.land", "land_id": "land-1", "attempt": 1,
        "state": "paused", "phase": "teleporting", "reconciling": false
    }))
    .unwrap();
    assert!(matches!(body, EventBody::Known(_)));
    f.seq += 1;
    let s = f.push_body(body);
    let land = s.land.clone().unwrap();
    assert_eq!(land.state, LandState::Unknown);
    assert_eq!(land.phase, crate::fleet_git::LandPhase::Unknown);
    assert!(land.files.is_empty());
}

#[test]
fn session_moved_sets_moved_to_and_leaves_the_state_alone() {
    use crate::fleet_git::MovedTo;
    use crate::ids::StationId;
    let mut f = Feed::new();
    f.push(started());
    let state = f.summary.state;
    assert!(f.summary.moved_to.is_none());
    let s = f.push(Event::SessionMoved {
        move_id: "mv-1".to_owned(),
        to_station: StationId::from("bravo"),
        to_session: SessionId::from("s2"),
    });
    assert_eq!(
        s.moved_to,
        Some(MovedTo {
            move_id: "mv-1".to_owned(),
            station_id: StationId::from("bravo"),
            session_id: SessionId::from("s2"),
        })
    );
    assert_eq!(s.state, state);
}

#[test]
fn session_move_undone_clears_moved_to_only_for_the_same_move() {
    use crate::ids::StationId;
    let mut f = Feed::new();
    f.push(started());
    f.push(Event::SessionMoved {
        move_id: "mv-1".to_owned(),
        to_station: StationId::from("bravo"),
        to_session: SessionId::from("s2"),
    });
    let s = f.push(Event::SessionMoveUndone {
        move_id: "mv-other".to_owned(),
    });
    assert!(s.moved_to.is_some());
    let s = f.push(Event::SessionMoveUndone {
        move_id: "mv-1".to_owned(),
    });
    assert!(s.moved_to.is_none());
}

#[test]
fn session_move_in_sets_and_replaces_the_move_state_of_the_summary() {
    use crate::fleet_git::{MoveInInfo, MoveInState};
    let info = |state| MoveInInfo {
        move_id: "mv-1".to_owned(),
        state,
        code: None,
        text: None,
    };
    let mut f = Feed::new();
    f.push(started());
    assert!(f.summary.move_in.is_none());
    let s = f.push(Event::SessionMoveIn(info(MoveInState::Resumed)));
    assert_eq!(s.move_in, Some(info(MoveInState::Resumed)));
    let s = f.push(Event::SessionMoveIn(info(MoveInState::Confirmed)));
    assert_eq!(s.move_in, Some(info(MoveInState::Confirmed)));
}

#[test]
fn a_summary_without_the_move_fields_serializes_and_decodes_as_before() {
    let bare = serde_json::to_value(SessionSummary::new(SessionId::from("s2"))).unwrap();
    let object = bare.as_object().unwrap();
    assert!(!object.contains_key("moved_to"));
    assert!(!object.contains_key("move_in"));
    let back: SessionSummary = serde_json::from_value(bare).unwrap();
    assert!(back.moved_to.is_none() && back.move_in.is_none());
}
