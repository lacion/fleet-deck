//! The effort fold (B18, docs/v2/design/effort.md §4.1).

use crate::capability::Capabilities;
use crate::event::{Event, EventBody, EventEnvelope, ProviderKind, ProviderRefs};
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
