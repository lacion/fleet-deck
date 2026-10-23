//! `session.slash_commands` of the Claude core (B11, DL-410).

use fleetdeck_proto::Event;
use serde_json::{Value, json};

use super::commands::{MAX_COMMAND_NAME, MAX_COMMANDS};
use super::tests::{config, emits, feed};
use super::*;

fn initialize_id(core: &mut ClaudeCore) -> String {
    super::tests::writes(&core.initialize().unwrap())[0]["request_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn initialize_answer(id: &str, response: Value) -> Value {
    let mut response = response.as_object().unwrap().clone();
    response.insert("current_permission_mode".to_owned(), json!("default"));
    json!({"type":"control_response","response":{"subtype":"success","request_id":id,
           "response":response}})
}

fn names(names: &[&str]) -> Value {
    Value::Array(names.iter().map(|n| json!({ "name": n })).collect())
}

fn list(commands: &[&str], skills: &[&str]) -> Event {
    Event::SlashCommands {
        commands: commands.iter().map(|s| (*s).to_owned()).collect(),
        skills: skills.iter().map(|s| (*s).to_owned()).collect(),
    }
}

fn root_init(commands: &Value, skills: &Value, terminal: &Value) -> Value {
    json!({"type":"system","subtype":"init","session_id":"s-1",
           "slash_commands":commands,"skills":skills,"terminal_slash_commands":terminal})
}

#[test]
fn an_initialize_answer_with_commands_emits_the_sorted_names_before_the_session_start() {
    let mut core = ClaudeCore::new(config(false));
    let id = initialize_id(&mut core);
    let commands = names(&["review", "/compact", "a b", ""]);
    let events = emits(&feed(
        &mut core,
        initialize_answer(&id, json!({ "commands": commands })),
    ));
    assert_eq!(events, [list(&["compact", "review"], &[])]);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::SessionStarted { .. }))
    );
}

#[test]
fn an_initialize_answer_without_commands_emits_no_list() {
    for response in [json!({}), json!({"commands":"x"})] {
        let mut core = ClaudeCore::new(config(false));
        let id = initialize_id(&mut core);
        assert!(emits(&feed(&mut core, initialize_answer(&id, response))).is_empty());
    }
}

#[test]
fn commands_changed_before_the_first_init_is_released_at_once() {
    let mut core = ClaudeCore::new(config(false));
    let out = feed(
        &mut core,
        json!({"type":"system","subtype":"commands_changed","commands":names(&["b","a"])}),
    );
    assert_eq!(emits(&out), [list(&["a", "b"], &[])]);
    assert!(!core.started);
}

#[test]
fn the_first_init_drops_the_terminal_commands_and_adds_the_skills() {
    let mut core = ClaudeCore::new(config(false));
    let id = initialize_id(&mut core);
    let first = emits(&feed(
        &mut core,
        initialize_answer(
            &id,
            json!({"commands": names(&["doctor", "compact", "review"])}),
        ),
    ));
    assert_eq!(first, [list(&["compact", "doctor", "review"], &[])]);
    let frame = root_init(
        &json!(["compact", "/doctor", "review"]),
        &json!(["review", 7, "deploy"]),
        &json!(["doctor"]),
    );
    let events = emits(&feed(&mut core, frame));
    assert!(matches!(events[0], Event::SessionStarted { .. }));
    assert_eq!(
        events[1],
        list(&["compact", "review"], &["deploy", "review"])
    );
    assert_eq!(events.len(), 2);
}

#[test]
fn the_same_list_again_emits_nothing() {
    let mut core = ClaudeCore::new(config(false));
    let frame = root_init(&json!(["b", "a"]), &json!(["s"]), &json!([]));
    assert_eq!(emits(&feed(&mut core, frame.clone())).len(), 2);
    assert!(emits(&feed(&mut core, frame)).is_empty());
    let changed =
        json!({"type":"system","subtype":"commands_changed","commands":names(&["a","b"])});
    assert!(emits(&feed(&mut core, changed)).is_empty());
}

#[test]
fn a_child_init_does_not_change_the_list() {
    let mut core = ClaudeCore::new(config(false));
    feed(&mut core, root_init(&json!(["a"]), &json!([]), &json!([])));
    let mut child = root_init(&json!(["x", "y"]), &json!(["z"]), &json!([]));
    child["parent_tool_use_id"] = json!("tu-1");
    assert!(emits(&feed(&mut core, child)).is_empty());
}

#[test]
fn an_empty_first_list_is_emitted_once() {
    let mut core = ClaudeCore::new(config(false));
    let frame = root_init(&json!([]), &json!([]), &json!([]));
    let events = emits(&feed(&mut core, frame.clone()));
    assert_eq!(events.len(), 2);
    assert_eq!(events[1], list(&[], &[]));
    assert!(emits(&feed(&mut core, frame)).is_empty());
}

#[test]
fn names_over_the_limits_are_dropped() {
    let mut core = ClaudeCore::new(config(false));
    let long = "x".repeat(MAX_COMMAND_NAME + 1);
    let fits = "y".repeat(MAX_COMMAND_NAME);
    let many: Vec<String> = (0..MAX_COMMANDS + 100)
        .map(|i| format!("c{i:04}"))
        .collect();
    let mut all = many.clone();
    all.push(long);
    all.push(fits);
    let events = emits(&feed(
        &mut core,
        root_init(&json!(all), &json!([]), &json!([])),
    ));
    let Event::SlashCommands { commands, .. } = &events[1] else {
        panic!("{events:?}")
    };
    assert_eq!(commands.len(), MAX_COMMANDS);
    assert!(commands.iter().all(|c| c.len() <= MAX_COMMAND_NAME));
    let mut sorted = commands.clone();
    sorted.sort();
    assert_eq!(&sorted, commands);
    // The 129-byte name is dropped, the 128-byte name sorts after the `c` names.
    let mut core = ClaudeCore::new(config(false));
    let names_in = json!([
        "x".repeat(MAX_COMMAND_NAME + 1),
        "y".repeat(MAX_COMMAND_NAME)
    ]);
    let events = emits(&feed(
        &mut core,
        root_init(&names_in, &json!([]), &json!([])),
    ));
    assert_eq!(
        events[1],
        list(&["y".repeat(MAX_COMMAND_NAME).as_str()], &[])
    );
}

#[test]
fn the_command_state_survives_a_checkpoint() {
    let mut core = ClaudeCore::new(config(false));
    let frame = root_init(&json!(["a", "b"]), &json!(["s"]), &json!(["b"]));
    assert_eq!(emits(&feed(&mut core, frame.clone())).len(), 2);
    let stored = core.checkpoint().unwrap();
    let mut core = ClaudeCore::restore(config(false), &stored).unwrap();
    assert!(emits(&feed(&mut core, frame)).is_empty());
    // A checkpoint written before B11 has no `commands`.
    let mut old = stored.clone();
    old.state
        .as_object_mut()
        .unwrap()
        .remove("commands")
        .unwrap();
    let mut core = ClaudeCore::restore(config(false), &old).unwrap();
    let again = root_init(&json!(["a"]), &json!([]), &json!([]));
    assert_eq!(emits(&feed(&mut core, again)), [list(&["a"], &[])]);
}
