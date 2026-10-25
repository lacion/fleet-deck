//! `skills/list` and `session.slash_commands` of the Codex core (B11).

use fleetdeck_proto::Event;
use serde_json::{Value, json};

use super::tests::*;
use super::*;

/// A core with `request_models` on and an open thread; returns the core and
/// the outputs of the thread reply.
fn skills_core() -> (CodexCore, Vec<Output>) {
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

fn skill(name: &str, enabled: bool) -> Value {
    json!({"name": name, "description": "", "enabled": enabled, "scope": "user", "path": "/s/SKILL.md"})
}

/// The `skills/list` write of `out`.
fn skills_write(out: &[Output]) -> Value {
    writes(out)
        .into_iter()
        .find(|w| w["method"] == "skills/list")
        .expect("a skills/list write")
}

fn slash(out: &[Output]) -> Vec<(Vec<String>, Vec<String>)> {
    emits(out)
        .into_iter()
        .filter_map(|e| match e {
            Event::SlashCommands { commands, skills } => Some((commands, skills)),
            _ => None,
        })
        .collect()
}

fn answer(core: &mut CodexCore, after: &[Output], result: Value) -> Vec<Output> {
    let id = skills_write(after)["id"].clone();
    feed(core, json!({"id": id, "result": result}))
}

#[test]
fn the_thread_open_sends_skills_list_with_the_session_folder() {
    let (_core, after) = skills_core();
    let w = writes(&after);
    assert_eq!(w.len(), 2);
    assert_eq!(w[0]["method"], "model/list");
    assert_eq!(w[1]["method"], "skills/list");
    assert_eq!(w[1]["params"], json!({"cwds": ["/w"]}));
}

#[test]
fn a_skills_answer_emits_the_enabled_sorted_names_as_skills() {
    let (mut core, after) = skills_core();
    let out = answer(
        &mut core,
        &after,
        json!({"data": [
            {"cwd": "/w", "skills": [skill("zeta", true), skill("off", false), skill("alpha", true), skill("a b", true)], "errors": []},
            {"cwd": "/x", "skills": [skill("alpha", true), skill("mid", true)], "errors": []},
        ]}),
    );
    assert_eq!(
        slash(&out),
        vec![(
            vec![],
            vec!["alpha".to_owned(), "mid".to_owned(), "zeta".to_owned()]
        )]
    );
}

#[test]
fn an_empty_skills_answer_emits_an_empty_list_once() {
    let (mut core, after) = skills_core();
    let out = answer(&mut core, &after, json!({"data": []}));
    assert_eq!(slash(&out), vec![(vec![], vec![])]);
    let changed = notify(&mut core, "skills/changed", json!({}));
    let again = answer(&mut core, &changed, json!({"data": []}));
    assert!(slash(&again).is_empty());
}

#[test]
fn the_same_skills_again_emit_nothing() {
    let (mut core, after) = skills_core();
    let result = json!({"data": [{"cwd": "/w", "skills": [skill("b", true), skill("a", true)], "errors": []}]});
    assert_eq!(slash(&answer(&mut core, &after, result.clone())).len(), 1);
    let changed = notify(&mut core, "skills/changed", json!({}));
    assert!(slash(&answer(&mut core, &changed, result)).is_empty());
    // A new skill emits again.
    let changed = notify(&mut core, "skills/changed", json!({}));
    let more = json!({"data": [{"cwd": "/w", "skills": [skill("a", true), skill("b", true), skill("c", true)], "errors": []}]});
    assert_eq!(slash(&answer(&mut core, &changed, more)).len(), 1);
}

#[test]
fn a_failed_skills_list_emits_no_event_and_no_error() {
    let (mut core, after) = skills_core();
    let id = skills_write(&after)["id"].clone();
    let out = feed(
        &mut core,
        json!({"id": id, "error": {"code": -32601, "message": "method not found"}}),
    );
    assert!(emits(&out).is_empty(), "{out:?}");
}

#[test]
fn skills_changed_sends_skills_list_again_after_the_open() {
    let (mut core, _after) = skills_core();
    let out = notify(&mut core, "skills/changed", json!({}));
    let w = writes(&out);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0]["method"], "skills/list");
    assert_eq!(w[0]["params"], json!({"cwds": ["/w"]}));
}

#[test]
fn skills_changed_before_the_open_sends_nothing() {
    let mut cfg = config("/w");
    cfg.request_models = true;
    let mut core = CodexCore::new(cfg);
    let out = notify(&mut core, "skills/changed", json!({}));
    assert!(out.is_empty());
}

#[test]
fn without_request_models_no_skills_list_is_sent() {
    let mut core = ready_core();
    assert!(core.host_requests.is_empty());
    let out = notify(&mut core, "skills/changed", json!({}));
    assert!(out.is_empty());
}
