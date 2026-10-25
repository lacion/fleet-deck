//! Adapter checkpoint tests at the sans-IO core (ADR-005 §2.1 and tests
//! 36, 37, 46, 47; DL-108). Each test commits a checkpoint at the point
//! where the station commits it (the claim transaction of an outbound
//! frame, or an output transaction), drops the core as a station crash,
//! restores a new core from the checkpoint and continues with the frames
//! that the shim replays. The station halves (the database transactions
//! and a real shim) are in `crates/fleetdeck-station` and
//! `crates/fleetdeck-cli/tests/shim_checkpoint.rs`.

use fleetdeck_proto::Event;
use fleetdeck_proto::event::ProviderKind;
use serde_json::json;

use super::checkpoint::{CHECKPOINT_READS, CHECKPOINT_VERSION};
use super::tests::{
    T, ans, command_approval, complete, config, emits, feed, input, notify, ready_core, start_turn,
    writes,
};
use super::*;
use crate::checkpoint::{AdapterCheckpoint, CheckpointError, REASON_UNSUPPORTED};

/// A station crash and restart: the core is gone; a new one is restored.
fn crash_and_restore(core: CodexCore) -> CodexCore {
    let checkpoint = core.checkpoint().unwrap();
    let text = checkpoint.to_json().unwrap();
    drop(core);
    let stored = AdapterCheckpoint::from_json(&text).unwrap();
    CodexCore::restore(config("/w"), &stored).unwrap()
}

fn id_number(id: &serde_json::Value) -> u64 {
    id.as_str()
        .and_then(|s| s.strip_prefix("fd-"))
        .and_then(|n| n.parse().ok())
        .unwrap()
}

#[test]
fn a_crash_after_an_outbound_dispatch_before_any_response_recovers_the_correlation_and_never_reuses_an_rpc_id_at_the_adapter()
 {
    let mut core = ready_core();
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    let sent = writes(&out);
    assert_eq!(sent[0]["method"], "turn/start");
    let sent_id = sent[0]["id"].clone();
    // The station commits the state with the frame claim, sends the frame,
    // and crashes before the response.
    let mut core = crash_and_restore(core);
    // The shim replays the response: it finds its correlation.
    let ev = emits(&feed(
        &mut core,
        json!({"id": sent_id, "result": {"turn": {"id": "n1", "status": "inProgress"}}}),
    ));
    assert!(
        ev.iter().any(
            |e| matches!(e, Event::TurnStarted { turn_id, .. } if turn_id.as_str() == "turn-1")
        ),
        "{ev:?}"
    );
    notify(
        &mut core,
        "turn/started",
        json!({"threadId": T, "turn": {"id": "n1"}}),
    );
    // The next request id is above every id that a committed state used.
    let out = core.interrupt(None).unwrap();
    let next = writes(&out);
    assert_eq!(next[0]["method"], "turn/interrupt");
    assert!(id_number(&next[0]["id"]) > id_number(&sent_id), "{next:?}");
}

#[test]
fn a_busy_follow_up_and_a_mode_or_model_change_without_later_stdout_survive_a_restart_at_the_adapter()
 {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    // Three transitions without any provider output.
    assert!(core.prompt(&input("second", "in-2")).unwrap().is_empty());
    assert!(
        core.set_mode("on-request/workspace-write")
            .unwrap()
            .is_empty()
    );
    assert!(core.set_model("gpt-next").unwrap().is_empty());
    let mut core = crash_and_restore(core);
    assert_eq!(core.pending_inputs(), 1, "the queued follow-up survived");
    // Restoring wrote nothing; the follow-up goes out only when the running
    // turn completes, with the new mode and model.
    let out = complete(&mut core, "n1", "completed");
    let w = writes(&out);
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0]["method"], "turn/start");
    assert_eq!(w[0]["params"]["clientUserMessageId"], "in-2");
    assert_eq!(w[0]["params"]["approvalPolicy"], "on-request");
    assert_eq!(w[0]["params"]["model"], "gpt-next");
}

#[test]
fn a_reattached_codex_session_answers_an_approval_opened_before_the_crash_with_the_restored_checkpoint_at_the_adapter()
 {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let Event::ApprovalRequested { request_id, .. } =
        command_approval(&mut core, json!(7), "call_1")
    else {
        panic!("no approval")
    };
    // The station commits the request line's events with the checkpoint and
    // acknowledges it; then it crashes.
    let mut core = crash_and_restore(core);
    let out = core.answer(&ans(&request_id, "accept")).unwrap();
    let w = writes(&out);
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0]["id"], json!(7), "the native request id was restored");
    assert_eq!(w[0]["result"]["decision"], "accept");
    for frame in &w {
        let method = frame["method"].as_str().unwrap_or_default();
        assert!(
            method != "initialize" && method != "thread/start" && method != "thread/resume",
            "{frame}"
        );
    }
    // A repeated answer is still a duplicate after the restore.
    assert!(core.answer(&ans(&request_id, "accept")).unwrap().is_empty());
}

#[test]
fn an_unsupported_adapter_checkpoint_version_makes_the_session_not_resumable_without_a_fresh_adapter_at_the_adapter()
 {
    let core = ready_core();
    let good = core.checkpoint().unwrap();
    assert_eq!(good.version, CHECKPOINT_VERSION);
    assert!(CHECKPOINT_READS.contains(&CHECKPOINT_VERSION));
    let newer = AdapterCheckpoint {
        version: CHECKPOINT_VERSION + 1,
        ..good.clone()
    };
    let err = CodexCore::restore(config("/w"), &newer).unwrap_err();
    assert_eq!(
        err,
        CheckpointError::Unsupported {
            provider: ProviderKind::Codex,
            version: CHECKPOINT_VERSION + 1
        }
    );
    assert_eq!(err.reason(), REASON_UNSUPPORTED);
    let other = AdapterCheckpoint {
        provider: ProviderKind::Claude,
        ..good.clone()
    };
    assert_eq!(
        CodexCore::restore(config("/w"), &other)
            .unwrap_err()
            .reason(),
        REASON_UNSUPPORTED
    );
    let corrupt = AdapterCheckpoint {
        state: json!({"thread_id": 3}),
        ..good
    };
    assert!(matches!(
        CodexCore::restore(config("/w"), &corrupt),
        Err(CheckpointError::Corrupt(_))
    ));
}

#[test]
fn a_restored_core_has_the_same_checkpoint_and_keeps_a_held_interrupt() {
    let mut core = ready_core();
    let out = core.prompt(&input("hi", "in-1")).unwrap();
    assert!(!writes(&out).is_empty());
    // An interrupt before the native turn id is held (`Some(None)`).
    assert!(core.interrupt(None).unwrap().is_empty());
    let before = core.checkpoint().unwrap();
    let core = crash_and_restore(core);
    assert_eq!(core.checkpoint().unwrap(), before);
    assert_eq!(core.interrupt_pending, Some(None));
    assert_eq!(core.thread().unwrap(), T);
}

#[test]
fn turns_ended_counts_the_ended_turns_and_starts_at_zero_after_a_restore() {
    let mut core = ready_core();
    assert_eq!(core.turns_ended(), 0);
    start_turn(&mut core, "in-1", "t1");
    assert_eq!(core.turns_ended(), 0, "a running turn is not ended");
    complete(&mut core, "t1", "completed");
    assert_eq!(core.turns_ended(), 1);
    start_turn(&mut core, "in-2", "t2");
    complete(&mut core, "t2", "completed");
    assert_eq!(core.turns_ended(), 2);
    // The counter is not in the checkpoint (DL-024, DL-199).
    let core = crash_and_restore(core);
    assert_eq!(core.turns_ended(), 0);
}
