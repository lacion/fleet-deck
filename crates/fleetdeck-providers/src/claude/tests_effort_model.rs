//! The levels of an effort change after a model change (B18 Phase 2,
//! docs/v2/design/effort.md §9.2 item 6): a successful `set_model` clears
//! the applied model, and a `get_settings` read that was written before the
//! change does not put the old model back.

use serde_json::json;

use super::tests::{feed, writes};
use super::tests_effort_set::{
    HAIKU, OPUS, error_answer, finish_turn, model_changes, restored, settings_answer, settings_ids,
    started,
};
use super::*;
use crate::adapter::ProviderError;

/// Writes `set_model(model)` and feeds its success answer.
fn model_set(core: &mut ClaudeCore, model: &str) -> Vec<Output> {
    let w = writes(&core.set_model(model).unwrap());
    let id = w[0]["request_id"].as_str().unwrap().to_owned();
    feed(
        core,
        json!({"type":"control_response","response":{"subtype":"success","request_id":id,
               "response":{}}}),
    )
}

#[test]
fn a_model_change_offers_the_new_levels_before_the_settings_answer() {
    let mut core = started(HAIKU);
    assert_eq!(core.applied_model.as_deref(), Some(HAIKU));
    let out = model_set(&mut core, OPUS);
    assert_eq!(settings_ids(&out), ["fd-settings-2"]);
    assert_eq!(core.applied_model, None);
    // The settings answer has not come: the new model decides the levels.
    assert_eq!(writes(&core.set_effort("max", None).unwrap()).len(), 1);
    assert!(matches!(
        core.set_effort("ultra", None),
        Err(ProviderError::EffortNotOffered(_))
    ));
}

#[test]
fn a_model_change_after_the_asks_stopped_offers_the_new_levels() {
    let mut core = started(HAIKU);
    assert_eq!(settings_ids(&finish_turn(&mut core, 1)), ["fd-settings-2"]);
    feed(&mut core, error_answer("fd-settings-2", "unsupported"));
    assert!(core.settings_stopped);
    let out = model_set(&mut core, OPUS);
    assert!(settings_ids(&out).is_empty(), "{out:?}");
    // No read will come: the levels of the new model stay in force.
    assert_eq!(writes(&core.set_effort("max", None).unwrap()).len(), 1);
    assert_eq!(core.applied_model, None);
}

#[test]
fn a_read_written_before_the_model_change_keeps_the_old_model_out() {
    let mut core = started(HAIKU);
    // A read is open when the model change is answered.
    assert_eq!(settings_ids(&finish_turn(&mut core, 1)), ["fd-settings-2"]);
    let out = model_set(&mut core, OPUS);
    assert!(settings_ids(&out).is_empty(), "{out:?}");
    // The rule holds across a checkpoint.
    let mut core = restored(&core);
    let out = feed(
        &mut core,
        settings_answer("fd-settings-2", HAIKU, json!("medium")),
    );
    assert!(model_changes(&out).is_empty(), "{out:?}");
    assert_eq!(core.applied_model, None);
    assert_eq!(core.model.as_deref(), Some(OPUS));
    assert_eq!(writes(&core.set_effort("max", None).unwrap()).len(), 1);
    // The read that follows the change sets the applied model again.
    assert_eq!(settings_ids(&out), ["fd-settings-3"]);
    let out = feed(
        &mut core,
        settings_answer("fd-settings-3", OPUS, json!("high")),
    );
    assert!(model_changes(&out).is_empty(), "{out:?}");
    assert_eq!(core.applied_model.as_deref(), Some(OPUS));
}

#[test]
fn a_read_after_the_model_change_still_corrects_the_model() {
    let mut core = started(HAIKU);
    model_set(&mut core, OPUS);
    // The CLI applied another model than the one that was set.
    let out = feed(
        &mut core,
        settings_answer("fd-settings-2", HAIKU, json!("medium")),
    );
    assert_eq!(
        model_changes(&out),
        [fleetdeck_proto::Event::ModelChanged {
            model: HAIKU.to_owned()
        }]
    );
    assert_eq!(core.applied_model.as_deref(), Some(HAIKU));
    assert!(matches!(
        core.set_effort("max", None),
        Err(ProviderError::EffortNotOffered(_))
    ));
}
