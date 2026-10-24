//! The effort fold (B18, docs/v2/design/effort.md §4.1).

use crate::capability::Capabilities;
use crate::event::{
    EffortSetOutcome, Event, EventBody, EventEnvelope, ModelChoice, ProviderKind, ProviderRefs,
};
use crate::ids::SessionId;
use crate::session::SessionSummary;
use serde_json::json;

struct Feed {
    summary: SessionSummary,
    seq: u64,
}

impl Feed {
    fn new() -> Self {
        Self {
            summary: SessionSummary::new(SessionId::from("s1")),
            seq: 0,
        }
    }

    fn push(&mut self, event: Event) -> &SessionSummary {
        self.seq += 1;
        let envelope = EventEnvelope {
            station_seq: self.seq + 100,
            session_id: SessionId::from("s1"),
            session_seq: self.seq,
            at: 1_000 + i64::try_from(self.seq).unwrap(),
            proto_version: 1,
            raw_ref: None,
            event: EventBody::Known(event),
        };
        self.summary.apply(&envelope);
        &self.summary
    }
}

fn started() -> Event {
    Event::SessionStarted {
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        provider_version: Some("2.1.0".to_owned()),
        model: Some("haiku".to_owned()),
        cwd: "/w".to_owned(),
        mode: Some("default".to_owned()),
        capabilities: Box::new(Capabilities::minimal()),
        resumed: None,
        provider_refs: ProviderRefs::new(),
        title: None,
        branch: None,
        repo: None,
    }
}

fn effort(text: Option<&str>) -> Event {
    Event::EffortChanged {
        effort: text.map(str::to_owned),
    }
}

#[test]
fn effort_changed_sets_and_clears_the_effort() {
    let mut f = Feed::new();
    f.push(started());
    assert_eq!(f.push(effort(Some("high"))).effort.as_deref(), Some("high"));
    assert_eq!(f.push(effort(Some("low"))).effort.as_deref(), Some("low"));
    assert_eq!(f.push(effort(None)).effort, None);
}

#[test]
fn session_started_clears_the_effort() {
    let mut f = Feed::new();
    f.push(started());
    assert_eq!(f.push(effort(Some("high"))).effort.as_deref(), Some("high"));
    assert_eq!(f.push(started()).effort, None);
}

#[test]
fn a_summary_without_effort_decodes_and_serializes_without_it() {
    let f = Feed::new();
    let mut v = serde_json::to_value(&f.summary).unwrap();
    assert!(!v.as_object().unwrap().contains_key("effort"));
    v.as_object_mut().unwrap().remove("effort");
    let back: SessionSummary = serde_json::from_value(v).unwrap();
    assert_eq!(back.effort, None);

    let mut with = Feed::new();
    with.push(effort(Some("max")));
    let v = serde_json::to_value(&with.summary).unwrap();
    assert_eq!(v["effort"], json!("max"));
}

#[test]
fn effort_changed_with_null_round_trips() {
    let body: EventBody =
        serde_json::from_value(json!({"kind": "effort.changed", "effort": null})).unwrap();
    assert!(matches!(
        body.known(),
        Some(Event::EffortChanged { effort: None })
    ));
    let v = serde_json::to_value(Event::EffortChanged { effort: None }).unwrap();
    assert_eq!(v, json!({"kind": "effort.changed", "effort": null}));
}

fn set_result(requested: &str, effort: Option<&str>, outcome: EffortSetOutcome) -> Event {
    Event::EffortSetResult {
        control_id: Some("eff-1".to_owned()),
        requested: requested.to_owned(),
        effort: effort.map(str::to_owned),
        outcome,
        message: None,
    }
}

#[test]
fn effort_chosen_sets_the_choice() {
    let mut f = Feed::new();
    f.push(started());
    let s = f.push(Event::EffortChosen {
        effort: "high".to_owned(),
    });
    assert_eq!(s.effort_choice.as_deref(), Some("high"));
    assert_eq!(s.effort, None);
}

#[test]
fn only_accepted_sets_the_choice() {
    let mut f = Feed::new();
    f.push(started());
    for outcome in [
        EffortSetOutcome::Applied,
        EffortSetOutcome::Adjusted,
        EffortSetOutcome::Unverified,
        EffortSetOutcome::Refused,
        EffortSetOutcome::Superseded,
    ] {
        let s = f.push(set_result("low", Some("low"), outcome));
        assert_eq!(s.effort_choice, None, "{outcome:?}");
    }
    let s = f.push(set_result("low", None, EffortSetOutcome::Accepted));
    assert_eq!(s.effort_choice.as_deref(), Some("low"));
}

#[test]
fn a_late_verification_does_not_put_back_an_older_level() {
    let mut f = Feed::new();
    f.push(started());
    f.push(set_result("low", None, EffortSetOutcome::Accepted));
    f.push(set_result("max", None, EffortSetOutcome::Accepted));
    let s = f.push(set_result("low", Some("high"), EffortSetOutcome::Adjusted));
    assert_eq!(s.effort_choice.as_deref(), Some("max"));
}

#[test]
fn session_started_keeps_the_choice_and_clears_the_effort() {
    let mut f = Feed::new();
    f.push(started());
    f.push(effort(Some("high")));
    f.push(set_result("low", None, EffortSetOutcome::Accepted));
    let s = f.push(started());
    assert_eq!(s.effort_choice.as_deref(), Some("low"));
    assert_eq!(s.effort, None);
}

#[test]
fn effort_set_result_does_not_change_the_observed_effort() {
    let mut f = Feed::new();
    f.push(started());
    f.push(effort(Some("high")));
    for outcome in [
        EffortSetOutcome::Accepted,
        EffortSetOutcome::Applied,
        EffortSetOutcome::Adjusted,
        EffortSetOutcome::Unverified,
        EffortSetOutcome::Refused,
        EffortSetOutcome::Superseded,
    ] {
        let s = f.push(set_result("low", Some("low"), outcome));
        assert_eq!(s.effort.as_deref(), Some("high"), "{outcome:?}");
    }
}

#[test]
fn the_new_events_round_trip_and_a_model_choice_without_the_new_keys_decodes() {
    let body: EventBody = serde_json::from_value(json!({
        "kind": "effort.set_result", "control_id": null, "requested": "low",
        "effort": null, "outcome": "accepted"
    }))
    .unwrap();
    let event = body.known().unwrap().clone();
    assert!(matches!(
        &event,
        Event::EffortSetResult {
            message: None,
            outcome: EffortSetOutcome::Accepted,
            ..
        }
    ));
    let v = serde_json::to_value(&event).unwrap();
    assert!(!v.as_object().unwrap().contains_key("message"));
    assert_eq!(v["outcome"], json!("accepted"));

    let chosen = serde_json::to_value(Event::EffortChosen {
        effort: "high".to_owned(),
    })
    .unwrap();
    assert_eq!(chosen, json!({"kind": "effort.chosen", "effort": "high"}));

    let choice: ModelChoice =
        serde_json::from_value(json!({"value": "opus", "label": "Opus"})).unwrap();
    assert_eq!(choice.resolved, None);
    assert_eq!(choice.efforts, None);
    assert_eq!(choice.default_effort, None);
    let v = serde_json::to_value(&choice).unwrap();
    for key in ["resolved", "efforts", "default_effort"] {
        assert!(!v.as_object().unwrap().contains_key(key), "{key}");
    }
}

#[test]
fn a_summary_without_effort_choice_decodes_and_serializes_without_it() {
    let f = Feed::new();
    let mut v = serde_json::to_value(&f.summary).unwrap();
    assert!(!v.as_object().unwrap().contains_key("effort_choice"));
    v.as_object_mut().unwrap().remove("effort_choice");
    let back: SessionSummary = serde_json::from_value(v).unwrap();
    assert_eq!(back.effort_choice, None);
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
