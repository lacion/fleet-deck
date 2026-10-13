//! `model/list` and `session.models` of the Codex core (DL-151).

use fleetdeck_proto::Event;
use fleetdeck_proto::event::ModelChoice;
use serde_json::{Value, json};

use super::tests::*;
use super::*;

/// A core with `request_models` on and an open thread; returns the core and
/// the writes of the thread reply.
fn models_core() -> (CodexCore, Vec<Output>) {
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
    (core, after)
}

fn model(id: &str) -> Value {
    json!({"id": id, "model": id, "displayName": id.to_uppercase(), "description": "d", "hidden": false})
}

#[test]
fn model_list_is_sent_once_after_the_thread_opens() {
    let (_core, after) = models_core();
    assert!(matches!(
        &after[0],
        Output::Emit(Event::SessionStarted { .. })
    ));
    let w = writes(&after);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0]["method"], "model/list");
    assert_eq!(w[0]["params"], json!({}));
    // The default config (replays) asks for no models.
    let core = ready_core();
    assert!(core.host_requests.is_empty());
}

#[test]
fn model_list_pages_are_joined_into_one_event_without_hidden_models() {
    let (mut core, after) = models_core();
    let mut hidden = model("secret");
    hidden["hidden"] = json!(true);
    let page2 = reply(
        &mut core,
        &after,
        json!({"data": [model("a"), hidden], "nextCursor": "c1"}),
    );
    assert!(emits(&page2).is_empty());
    let w = writes(&page2);
    assert_eq!(w[0]["method"], "model/list");
    assert_eq!(w[0]["params"], json!({"cursor": "c1"}));
    let done = reply(
        &mut core,
        &page2,
        json!({"data": [{"id": "only-id"}, {"model": "b", "displayName": null}], "nextCursor": null}),
    );
    let choice = |value: &str, label: &str, description: Option<&str>| ModelChoice {
        value: value.to_owned(),
        label: label.to_owned(),
        description: description.map(str::to_owned),
    };
    assert_eq!(
        emits(&done),
        [Event::SessionModels {
            models: vec![
                choice("a", "A", Some("d")),
                choice("only-id", "only-id", None),
                choice("b", "b", None),
            ]
        }]
    );
    assert!(writes(&done).is_empty());
}

#[test]
fn model_list_follows_at_most_four_more_pages() {
    let (mut core, mut out) = models_core();
    for n in 0..4 {
        out = reply(
            &mut core,
            &out,
            json!({"data": [model(&format!("m{n}"))], "nextCursor": format!("c{n}")}),
        );
        assert_eq!(writes(&out).len(), 1, "page {n}");
    }
    // The fifth page ends the list although it names a cursor.
    let done = reply(
        &mut core,
        &out,
        json!({"data": [model("m4")], "nextCursor": "c4"}),
    );
    assert!(writes(&done).is_empty());
    let [Event::SessionModels { models }] = &emits(&done)[..] else {
        panic!("expected one session.models");
    };
    assert_eq!(models.len(), 5);
}

#[test]
fn a_failed_model_list_and_an_empty_one_emit_nothing() {
    let (mut core, after) = models_core();
    let id = writes(&after)[0]["id"].clone();
    let failed = feed(
        &mut core,
        json!({"id": id, "error": {"code": -32601, "message": "no such method"}}),
    );
    assert!(failed.is_empty());
    let (mut core, after) = models_core();
    let empty = reply(&mut core, &after, json!({"data": [], "nextCursor": null}));
    assert!(empty.is_empty());
}
