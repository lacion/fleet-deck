//! M2 P2-8 crash tests (PLAN §6.4, §11, §13 Phase 2): one `fleetdeck
//! station serve` process aborts at each effect boundary; a new station
//! process recovers from station.db, and the same command is sent again.
//!
//! | crash | the command after the restart | the effect | sent to the provider |
//! |---|---|---|---|
//! | before the decision commit | decided anew: rejected, the session is lost | none | never |
//! | after the decision commit | its stored receipt | `retired_not_dispatched` | never |
//! | after the dispatch claim | its stored receipt | `outcome_unknown` | never |
//! | after the send | its stored receipt | `outcome_unknown` | once |
//! | before the confirmation is stored | its stored receipt | `outcome_unknown` | once |
//!
//! In each case the provider process of the crashed station is gone, the
//! station never replays a command into a new process, and the session is
//! `lost`. The worker restart and the provider restart (as separate
//! faults, inside one station process) are in
//! `crates/fleetdeck-station/src/manager/fault_tests.rs`.
//!
//! The test build enables the `fault-injection` feature of
//! fleetdeck-station (a dev-dependency feature), so the `fleetdeck`
//! binary under test honors `FLEETDECK_FAULT`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use common::{Station, effects_of, open_store, stored_events};
use fleetdeck_proto::command::{ReceiptState, RejectCode, SessionCommand, UserInput};
use fleetdeck_proto::event::{Event, ExpiredReason, InputState, SessionState};
use fleetdeck_proto::session::Activity;
use fleetdeck_proto::{CommandId, Receipt};
use fleetdeck_station::fault::FaultPoint;
use fleetdeck_station::local::{LocalRequest, LocalResponse};
use fleetdeck_station::store::{EffectState, RequestState};

/// SIGABRT: `std::process::abort`.
const SIGABRT: i32 = 6;
const SECOND: &str = "fault-test-second-prompt";

fn prompt(session: &str, text: &str) -> LocalRequest {
    LocalRequest::Command {
        command_id: Some(CommandId::from(SECOND.to_owned())),
        command: SessionCommand::Prompt {
            session_id: session.to_owned().into(),
            input: UserInput {
                text: text.to_owned(),
            },
        },
    }
}

fn receipt(response: Option<LocalResponse>) -> Receipt {
    match response {
        Some(LocalResponse::Receipt { receipt }) => receipt,
        other => panic!("no receipt: {other:?}"),
    }
}

fn now_ms() -> i64 {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    i64::try_from(ms).unwrap()
}

/// The result of one crash at `point`.
struct Crash {
    /// The receipt of the same command after the restart.
    receipt: Receipt,
    /// The restart time; a stored receipt is older.
    restarted_at: i64,
    /// The effects that the command queued.
    effects: Vec<fleetdeck_station::store::Effect>,
    /// How often fake-claude received the second prompt.
    sent: usize,
}

/// Runs one session, crashes the station at `point` while it handles a
/// second prompt, restarts the station, and sends that prompt again.
async fn crash_at(point: FaultPoint) -> Crash {
    let label = match point {
        FaultPoint::BeforeCommit | FaultPoint::AfterCommit => "session.prompt",
        _ => "prompt",
    };
    let mut station = Station::new();
    station.spawn(Some(&format!("{}@{label}", point.name())));
    let session = station.run("hello one");
    station.wait_for("the first turn", &session, |s| {
        s.state == SessionState::Running
            && s.activity == Activity::Idle
            && s.usage.input_tokens.is_some()
    });

    // The crash: the answer may or may not come back before it.
    let _ = station.call(&prompt(&session, "hello two")).await;
    assert_eq!(station.wait_crash(), SIGABRT, "{} must crash", point.name());
    station.wait_providers_gone();

    let restarted_at = now_ms();
    station.spawn(None);
    let lost = station.wait_for("the lost session", &session, |_| true);
    assert_eq!(lost.state, SessionState::Lost);
    assert!(lost.current_turn.is_none() && lost.current_run.is_none());

    let receipt = receipt(station.call(&prompt(&session, "hello two")).await);
    station.stop();

    let log = station.stdin();
    assert_eq!(log.pids.len(), 1, "no new provider process");
    assert_eq!(log.count("hello one"), 1);
    let store = open_store(&station.db()).await;
    let effects = effects_of(&store, &session, SECOND).await;
    // Only the first prompt ever started: no evidence for the second one
    // was stored (before the crash) or made (after it).
    let started = stored_events(&store)
        .await
        .into_iter()
        .filter(|(s, e)| {
            *s == session
                && matches!(
                    e,
                    Event::InputUpdated {
                        state: InputState::Started,
                        ..
                    }
                )
        })
        .count();
    assert_eq!(started, 1, "only the first prompt started");
    Crash {
        receipt,
        restarted_at,
        effects,
        sent: log.count("hello two"),
    }
}

/// The command was accepted before the crash: the same id gets the stored
/// receipt, and no second effect.
fn assert_stored_receipt(crash: &Crash) {
    assert_eq!(
        crash.receipt.state,
        ReceiptState::Accepted,
        "{:?}",
        crash.receipt
    );
    assert!(
        crash.receipt.at < crash.restarted_at,
        "the receipt comes from before the crash"
    );
    assert_eq!(crash.effects.len(), 1, "{:?}", crash.effects);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_before_the_decision_commit_leaves_no_trace_of_the_command() {
    let crash = crash_at(FaultPoint::BeforeCommit).await;
    // Nothing of the decision survived: the command is decided anew, and
    // the session is lost now.
    assert_eq!(crash.receipt.state, ReceiptState::Rejected);
    assert!(crash.receipt.at >= crash.restarted_at);
    let reject = crash.receipt.reject.unwrap();
    assert_eq!(reject.code, RejectCode::InvalidState, "{}", reject.message);
    assert!(crash.effects.is_empty());
    assert_eq!(crash.sent, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_after_the_decision_commit_retires_the_queued_effect() {
    let crash = crash_at(FaultPoint::AfterCommit).await;
    assert_stored_receipt(&crash);
    let effect = &crash.effects[0];
    assert_eq!(
        effect.state,
        EffectState::RetiredNotDispatched,
        "{effect:?}"
    );
    assert_eq!(crash.sent, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_after_the_dispatch_claim_makes_the_outcome_unknown_and_sends_nothing() {
    let crash = crash_at(FaultPoint::AfterClaim).await;
    assert_stored_receipt(&crash);
    assert_eq!(crash.effects[0].state, EffectState::OutcomeUnknown);
    assert_eq!(crash.sent, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_after_the_send_makes_the_outcome_unknown_and_never_resends() {
    let crash = crash_at(FaultPoint::AfterSend).await;
    assert_stored_receipt(&crash);
    assert_eq!(crash.effects[0].state, EffectState::OutcomeUnknown);
    assert_eq!(crash.sent, 1, "sent once, never again");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_before_the_confirmation_is_stored_makes_the_outcome_unknown() {
    let crash = crash_at(FaultPoint::BeforeConfirm).await;
    assert_stored_receipt(&crash);
    assert_eq!(crash.effects[0].state, EffectState::OutcomeUnknown);
    assert_eq!(crash.sent, 1, "sent once, never again");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_station_kill_expires_the_open_approval_and_rejects_its_stale_reply() {
    let mut station = Station::start();
    let session = station.run("ask ls");
    let asking = station.wait_for("the approval", &session, |s| !s.open_requests.is_empty());
    let request = asking.open_requests[0].request_id.as_str().to_owned();

    station.signal("KILL");
    assert_eq!(station.wait_crash(), 9);
    station.wait_providers_gone();
    station.spawn(None);

    let lost = station.wait_for("the lost session", &session, |_| true);
    assert_eq!(lost.state, SessionState::Lost);
    assert!(lost.open_requests.is_empty());
    assert_eq!(lost.activity, Activity::Ended);
    // The owner's late answer is a stale reply: rejected, and sent nowhere.
    let err = station.fails(&["station", "approve", &request]);
    assert!(err.contains("rejected (request_closed)"), "{err}");
    // A duplicate replay of the same answer gets the same rejection.
    let again = station.fails(&["station", "approve", &request]);
    assert!(again.contains("rejected (request_closed)"), "{again}");
    // A new session gets a new provider process with none of the old input.
    let next = station.run("hello again");
    station.wait_for("the new turn", &next, |s| {
        s.activity == Activity::Idle && s.usage.input_tokens.is_some()
    });
    station.stop();

    let log = station.stdin();
    assert_eq!(log.pids.len(), 2);
    assert_eq!(
        log.count("control_response"),
        0,
        "no answer reached a provider"
    );
    assert_eq!(log.count("ask ls"), 1, "nothing was replayed");
    let store = open_store(&station.db()).await;
    let open = store
        .runtime_request(request.clone().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(open.state, RequestState::Expired);
    assert!(stored_events(&store).await.iter().any(|(_, e)| matches!(
        e,
        Event::ApprovalExpired { request_id, reason: ExpiredReason::NotResumable }
            if request_id.as_str() == request
    )));
}
