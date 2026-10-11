//! M2 P2-7 exit test (PLAN §13 Phase 2): a Claude session (fake-claude)
//! runs, asks for approval, is answered and interrupted from the
//! station-local CLI. The station and every CLI call are separate
//! `fleetdeck` processes; the station talks to fake-claude through a real
//! process and the Unix socket.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use common::{Station, fleetdeck, open_store, stored_events};
use fleetdeck_proto::event::Event;
use fleetdeck_proto::session::Activity;
use fleetdeck_testkit::{FakeAgent, TempNode};

#[tokio::test(flavor = "multi_thread")]
async fn a_claude_session_runs_asks_for_approval_is_answered_and_interrupted_from_the_cli() {
    let mut station = Station::start();
    assert_eq!(station.ok(&["station", "sessions"]).trim(), "no sessions");

    // 1. A session runs and asks for approval.
    let first = station.run("ask ls");
    let asking = station.wait_for("the approval", &first, |s| !s.open_requests.is_empty());
    assert_eq!(asking.activity, Activity::NeedsYou);
    let request = asking.open_requests[0].request_id.as_str().to_owned();
    let text = station.ok(&["station", "sessions"]);
    assert!(
        text.contains(&first) && text.contains("needs_you"),
        "{text}"
    );
    assert!(text.contains(&format!("request {request}")), "{text}");

    // 2. The CLI answers it; the tool runs and the turn ends.
    let answered = station.ok(&["station", "approve", &request]);
    assert!(answered.contains("answered"), "{answered}");
    station.wait_for("the answered turn to end", &first, |s| {
        s.open_requests.is_empty() && s.current_turn.is_none() && s.activity == Activity::Idle
    });
    // A second answer to the same request sends nothing (L3).
    station.ok(&["station", "approve", &request]);

    // 3. A second session runs a long turn; the CLI interrupts it.
    let second = station.run("wait for the interrupt");
    station.wait_for("the long turn", &second, |s| {
        s.activity == Activity::Working
    });
    let sent = station.ok(&["station", "interrupt", &second]);
    assert!(sent.contains("interrupt sent"), "{sent}");
    station.wait_for("the interrupted turn to end", &second, |s| {
        s.current_turn.is_none() && s.activity == Activity::Idle
    });

    // Rejections and errors exit 1 with a reason.
    let err = station.fails(&["station", "interrupt", &second]);
    assert!(err.contains("rejected (invalid_state)"), "{err}");
    let err = station.fails(&["station", "approve", "req-nope"]);
    assert!(err.contains("no request `req-nope`"), "{err}");
    let outside = station.node.root().to_str().unwrap().to_owned();
    let provider = FakeAgent::Claude.instance_id();
    let err = station.fails(&[
        "station",
        "run",
        "--provider",
        provider,
        "--cwd",
        &outside,
        "hi",
    ]);
    assert!(err.contains("rejected (forbidden)"), "{err}");

    // 4. SIGTERM stops both sessions; the socket goes away.
    station.stop();
    let err = station.fails(&["station", "sessions"]);
    assert!(err.contains("no station answers"), "{err}");

    // The station's log holds the evidence of each step.
    let store = open_store(&station.db()).await;
    let events: Vec<Event> = stored_events(&store)
        .await
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    let resolved = events
        .iter()
        .filter(|e| matches!(e, Event::ApprovalResolved { request_id, .. } if request_id.as_str() == request))
        .count();
    assert_eq!(resolved, 1, "one answer reached the provider");
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::ApprovalResolved { by, option_id, .. }
                if *by == fleetdeck_proto::event::ResolvedBy::Local && option_id == "allow"
        )),
        "the CLI answered as `local`"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::InterruptResult {
                accepted: true,
                control_id: Some(_),
                ..
            }
        )),
        "the provider confirmed the interrupt"
    );
    let ended = events
        .iter()
        .filter(|e| matches!(e, Event::SessionEnded { .. }))
        .count();
    assert_eq!(ended, 2, "both sessions ended at the shutdown");
}

#[test]
fn the_cli_reports_a_missing_station() {
    let node = TempNode::new("idle").unwrap();
    let out = fleetdeck(&node)
        .args(["station", "sessions"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("no station answers"), "{err}");
    assert!(err.contains("fleetdeck station serve"), "{err}");
}
