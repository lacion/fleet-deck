//! `session.models` of the Claude core (DL-151).

use fleetdeck_proto::Event;
use fleetdeck_proto::event::ModelChoice;
use serde_json::{Value, json};

use super::tests::{config, emits, feed, init, kinds, writes};
use super::*;

/// Sends `initialize` and returns the id of its request.
fn initialize_id(core: &mut ClaudeCore) -> String {
    writes(&core.initialize().unwrap())[0]["request_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn initialize_answer(id: &str, models: &Value) -> Value {
    json!({"type":"control_response","response":{"subtype":"success","request_id":id,
           "response":{"current_permission_mode":"default","models":models}}})
}

fn model_choice(value: &str, label: &str, description: Option<&str>) -> ModelChoice {
    ModelChoice {
        value: value.to_owned(),
        label: label.to_owned(),
        description: description.map(str::to_owned),
    }
}

#[test]
fn an_initialize_answer_before_the_first_init_emits_the_models_after_the_session_start() {
    let mut core = ClaudeCore::new(config(false));
    let id = initialize_id(&mut core);
    let models = json!([
        {"value":"default","displayName":"Default (recommended)","resolvedModel":"claude-opus-5-5",
         "description":"Use the default model"},
        {"value":"opus","displayName":"Opus","resolvedModel":"claude-opus-5-5"},
        {"displayName":"No value"},
        {"value":"haiku"}]);
    // The answer is released at once and holds the models back.
    assert!(emits(&feed(&mut core, initialize_answer(&id, &models))).is_empty());
    let frame = json!({"type":"system","subtype":"init","session_id":"s-1",
        "slash_commands":["compact"],
        "plugins":[{"name":"fleetdeck","path":"/p","source":"fleetdeck@fleetdeck"}]});
    let events = emits(&feed(&mut core, frame.clone()));
    assert!(matches!(events[0], Event::SessionStarted { .. }));
    assert!(matches!(events[1], Event::SessionNotice { .. }));
    assert_eq!(
        events[2],
        Event::SessionModels {
            models: vec![
                model_choice(
                    "default",
                    "Default (recommended)",
                    Some("Use the default model")
                ),
                model_choice("opus", "Opus", None),
                model_choice("haiku", "haiku", None),
            ]
        }
    );
    assert!(matches!(events[3], Event::SlashCommands { .. }));
    assert_eq!(events.len(), 4);
    // Claude sends `init` again for each turn: the models are not repeated.
    assert!(emits(&feed(&mut core, frame)).is_empty());
}

#[test]
fn an_initialize_answer_after_the_first_init_emits_the_models_at_once() {
    let mut core = ClaudeCore::new(config(false));
    init(&mut core);
    let id = initialize_id(&mut core);
    let models = json!([{"value":"sonnet","displayName":"Sonnet","description":"Fast"}]);
    let events = emits(&feed(&mut core, initialize_answer(&id, &models)));
    assert_eq!(
        events,
        [Event::SessionModels {
            models: vec![model_choice("sonnet", "Sonnet", Some("Fast"))]
        }]
    );
}

#[test]
fn an_initialize_answer_without_models_emits_no_models_event() {
    for models in [json!([]), json!([{"displayName":"No value"}]), json!("x")] {
        let mut core = ClaudeCore::new(config(false));
        let id = initialize_id(&mut core);
        assert!(emits(&feed(&mut core, initialize_answer(&id, &models))).is_empty());
        let events = init(&mut core);
        assert_eq!(kinds(&events), ["session.started"]);
        let mut core = ClaudeCore::new(config(false));
        init(&mut core);
        let id = initialize_id(&mut core);
        assert!(emits(&feed(&mut core, initialize_answer(&id, &models))).is_empty());
    }
}
