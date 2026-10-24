//! `thread/rollback` in the core (DL-087, review sa-56b1636a): no prompt,
//! follow-up or steer enters the thread while a rollback has no answer; a
//! late answer (confirmation or refusal) opens the thread again.

use serde_json::json;

use super::tests::*;
use super::*;
use crate::adapter::ProviderError;

/// A core with one finished turn and one `thread/rollback` on the wire.
fn rolling_back() -> (CodexCore, Vec<Output>) {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    complete(&mut core, "n1", "completed");
    core.check_rollback(1).unwrap();
    let out = core.rollback(1).unwrap();
    assert_eq!(writes(&out)[0]["method"], "thread/rollback");
    assert_eq!(writes(&out)[0]["params"]["numTurns"], 1);
    (core, out)
}

#[test]
fn a_prompt_or_a_follow_up_is_refused_while_a_rollback_has_no_answer() {
    let (mut core, _) = rolling_back();
    let err = core.prompt(&input("again", "in-2")).unwrap_err();
    assert!(matches!(err, ProviderError::Refused(_)), "{err:?}");
    // A second rollback is refused too.
    assert!(matches!(
        core.check_rollback(1),
        Err(ProviderError::Refused(_))
    ));
    // The refused input id stays free: nothing was tracked or written.
    assert!(core.is_idle());
}

#[test]
fn a_late_confirmation_or_refusal_opens_the_thread_again() {
    let (mut core, out) = rolling_back();
    reply(&mut core, &out, json!({"thread": {"id": T}}));
    assert!(core.rollback_answered());
    let out = core.prompt(&input("again", "in-2")).unwrap();
    assert_eq!(writes(&out)[0]["method"], "turn/start");

    let (mut core, out) = rolling_back();
    let id = writes(&out)[0]["id"].clone();
    feed(
        &mut core,
        json!({"id": id, "error": {"code": -32600, "message": "no"}}),
    );
    assert!(core.rollback_answered());
    assert!(core.prompt(&input("again", "in-2")).is_ok());
}
