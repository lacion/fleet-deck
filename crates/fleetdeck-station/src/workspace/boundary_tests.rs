//! The boundary rules of PLAN §6.6 (DL-078) on event sequences of both
//! providers: native queued turns, steers, background writers, delayed
//! finalization, provider-initiated turns and process end.

use fleetdeck_proto::event::{BackgroundTask, InputState, ProviderRefs, TurnStatus};
use fleetdeck_proto::{Event, InputId, TurnId};

use super::boundary::*;

fn started(turn: &str) -> Event {
    Event::TurnStarted {
        run_id: "run-1".into(),
        turn_id: turn.into(),
        provider_refs: ProviderRefs::new(),
    }
}

fn completed(turn: &str) -> Event {
    Event::TurnCompleted {
        turn_id: turn.into(),
        status: TurnStatus::Done,
        usage: None,
    }
}

fn input(id: &str, turn: Option<&str>, state: InputState) -> Event {
    Event::InputUpdated {
        input_id: id.into(),
        turn_id: turn.map(TurnId::from),
        state,
    }
}

fn background(n: usize) -> Event {
    Event::BackgroundUpdated {
        tasks: (0..n)
            .map(|i| BackgroundTask {
                task_id: format!("b{i}"),
                task_kind: "local_bash".into(),
            })
            .collect(),
    }
}

fn avail(c: &str) -> Side {
    Side::Available(c.into())
}

fn unavailable(r: &str) -> Side {
    Side::Unavailable(r.into())
}

/// Starts a quiet prompt with a captured start checkpoint.
fn quiet_prompt(b: &mut Boundary, id: &str, commit: &str) {
    assert_eq!(b.begin_input(InputId::from(id)), StartPlan::Capture);
    b.record_start(&InputId::from(id), avail(commit));
}

#[test]
fn a_quiet_turn_has_both_checkpoints_and_the_capture_waits_for_the_fence() {
    let mut b = Boundary::new();
    quiet_prompt(&mut b, "in-1", "c0");
    assert_eq!(
        b.on_event(&started("turn-1")),
        vec![Next::TurnStarted {
            turn: "turn-1".into(),
            start: avail("c0"),
        }]
    );
    b.on_event(&input("in-1", Some("turn-1"), InputState::Started));
    assert_eq!(
        b.on_event(&completed("turn-1")),
        vec![Next::CaptureEnd {
            turn: "turn-1".into()
        }]
    );
    assert!(b.is_due(&"turn-1".into()));
    assert_eq!(b.take_due(), Some("turn-1".into()));
    assert_eq!(b.take_due(), None);
    assert_eq!(b.finalized(&"turn-1".into()), Some(avail("c0")));
    // A repeated `turn.completed` decides nothing again.
    assert!(b.on_event(&completed("turn-1")).is_empty());
}

/// PLAN required test: delayed finalization with T2 writes. The dispatch of
/// the next input holds the fence and runs the due capture first, so the
/// T1 completion is captured before the T2 input is written.
#[test]
fn delayed_finalization_is_captured_before_the_next_input_is_written() {
    let mut b = Boundary::new();
    quiet_prompt(&mut b, "in-1", "c0");
    b.on_event(&started("turn-1"));
    b.on_event(&completed("turn-1"));
    // The capture task has not run yet; the next prompt arrives.
    assert_eq!(
        b.take_due(),
        Some("turn-1".into()),
        "dispatch captures T1 first"
    );
    assert_eq!(b.begin_input(InputId::from("in-2")), StartPlan::Capture);
    // The late capture task finds nothing due.
    assert!(!b.is_due(&"turn-1".into()));
}

/// Claude queues a follow-up natively: the queued input makes the running
/// turn's completion and its own start unavailable.
#[test]
fn a_natively_queued_input_makes_both_boundaries_unavailable() {
    let mut b = Boundary::new();
    quiet_prompt(&mut b, "in-1", "c0");
    b.on_event(&started("turn-1"));
    b.on_event(&input("in-1", Some("turn-1"), InputState::Started));
    assert_eq!(
        b.begin_input(InputId::from("in-2")),
        StartPlan::Unavailable(REASON_TURN_RUNNING)
    );
    b.on_event(&input("in-2", None, InputState::Received));
    assert_eq!(
        b.on_event(&completed("turn-1")),
        vec![Next::Finalize {
            turn: "turn-1".into(),
            end: unavailable(REASON_QUEUED_INPUT),
        }]
    );
    assert_eq!(b.take_due(), None);
    assert_eq!(
        b.on_event(&started("turn-2")),
        vec![Next::TurnStarted {
            turn: "turn-2".into(),
            start: unavailable(REASON_TURN_RUNNING),
        }]
    );
}

#[test]
fn a_second_input_before_the_first_turn_starts_is_queued() {
    let mut b = Boundary::new();
    quiet_prompt(&mut b, "in-1", "c0");
    assert_eq!(
        b.begin_input(InputId::from("in-2")),
        StartPlan::Unavailable(REASON_QUEUED_INPUT)
    );
    let next = b.on_event(&started("turn-1"));
    assert_eq!(
        next,
        vec![Next::TurnStarted {
            turn: "turn-1".into(),
            start: avail("c0"),
        }]
    );
}

/// A steer joins the running turn: it does not block the completion.
#[test]
fn a_steer_that_joins_the_turn_does_not_block_the_completion() {
    let mut b = Boundary::new();
    quiet_prompt(&mut b, "in-1", "c0");
    b.on_event(&started("turn-1"));
    assert_eq!(
        b.begin_input(InputId::from("steer-1")),
        StartPlan::Unavailable(REASON_TURN_RUNNING)
    );
    b.on_event(&input("steer-1", Some("turn-1"), InputState::Started));
    assert_eq!(
        b.on_event(&completed("turn-1")),
        vec![Next::CaptureEnd {
            turn: "turn-1".into()
        }]
    );
}

/// PLAN required test: background writes after the parent turn.
#[test]
fn background_work_makes_the_completion_and_the_next_start_unavailable() {
    let mut b = Boundary::new();
    quiet_prompt(&mut b, "in-1", "c0");
    b.on_event(&started("turn-1"));
    b.on_event(&background(1));
    assert_eq!(
        b.on_event(&completed("turn-1")),
        vec![Next::Finalize {
            turn: "turn-1".into(),
            end: unavailable(REASON_BACKGROUND_WRITERS),
        }]
    );
    assert_eq!(
        b.begin_input(InputId::from("in-2")),
        StartPlan::Unavailable(REASON_BACKGROUND_WRITERS)
    );
    b.input_not_sent(&InputId::from("in-2"));
    b.on_event(&background(0));
    assert_eq!(b.begin_input(InputId::from("in-3")), StartPlan::Capture);
}

#[test]
fn a_turn_without_a_station_input_has_no_start_checkpoint() {
    let mut b = Boundary::new();
    assert_eq!(
        b.on_event(&started("turn-1")),
        vec![Next::TurnStarted {
            turn: "turn-1".into(),
            start: unavailable(REASON_PROVIDER_INITIATED),
        }]
    );
}

#[test]
fn an_input_that_was_not_sent_or_was_discarded_leaves_the_queue() {
    let mut b = Boundary::new();
    quiet_prompt(&mut b, "in-1", "c0");
    b.input_not_sent(&InputId::from("in-1"));
    quiet_prompt(&mut b, "in-2", "c1");
    b.on_event(&input("in-2", None, InputState::Discarded));
    assert_eq!(b.begin_input(InputId::from("in-3")), StartPlan::Capture);
}

#[test]
fn the_end_of_the_process_finalizes_a_running_turn_without_a_completion_checkpoint() {
    let mut b = Boundary::new();
    quiet_prompt(&mut b, "in-1", "c0");
    b.on_event(&started("turn-1"));
    let next = b.on_event(&Event::SessionEnded {
        reason: "process_exit".into(),
        exit_code: None,
    });
    assert_eq!(
        next,
        vec![Next::Finalize {
            turn: "turn-1".into(),
            end: unavailable(REASON_SESSION_ENDED),
        }]
    );
    assert_eq!(b.start_of(&"turn-1".into()), Some(&avail("c0")));
    // A turn whose completion capture is already due is not finalized twice.
    let mut b = Boundary::new();
    quiet_prompt(&mut b, "in-1", "c0");
    b.on_event(&started("turn-1"));
    b.on_event(&completed("turn-1"));
    let next = b.on_event(&Event::SessionEnded {
        reason: "process_exit".into(),
        exit_code: None,
    });
    assert!(next.is_empty());
    assert!(b.is_due(&"turn-1".into()));
}
