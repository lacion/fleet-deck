//! The input protocol on the fake desktop (R13 P7 §7.3): the order of the
//! steps, the fixed argv, the text on stdin only, the pointer point, the
//! durable claim and the replay.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_link::desktop_authority::action_hash_for;
use fleetdeck_proto::coordinator_desktop_wire::AuthorityPhase;
use fleetdeck_proto::desktop::DesktopAction as A;
use fleetdeck_testkit::Det;

use super::claim::StoreClaim;
use super::rig::{Fx, Probe, scripted_clock};
use super::{Claimed, Env};
use crate::desktop::authority_tests::NODE;
use crate::desktop::coordinator_tests::{DECK, ctx};
use crate::desktop::tests::on;
use crate::store::{Store, StoreConfig};

fn strs(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

#[tokio::test]
async fn a_key_press_runs_the_twenty_steps_in_order() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.run(&fx.key("Ctrl+Shift+a"), &probe).await.unwrap();
    // The claim comes first, then the check, then the disclosure, the second
    // check, the focus, the third check, the final active-window read, and
    // the one `wtype` run. Nothing else runs between the last check and the
    // spawn but the final read; the last line is the final focus observation.
    assert_eq!(
        fx.timeline(),
        [
            "claim",
            "check",
            "notify-send",
            "check",
            &fx.focus_line(),
            "check",
            "hyprctl -j activewindow",
            "wtype",
            "hyprctl -j activewindow",
        ]
    );
    // Three explicit execute checks.
    assert_eq!(
        *probe.check.phases.lock().unwrap(),
        [AuthorityPhase::Execute; 3]
    );
    assert_eq!(
        fx.calls_of("wtype")[0].args,
        strs(&[
            "-M", "ctrl", "-M", "shift", "-k", "a", "-m", "shift", "-m", "ctrl"
        ])
    );
    assert_eq!(
        probe.claim.calls.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn the_disclosure_is_the_fixed_text_without_the_input() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.run(&fx.key("Return"), &probe).await.unwrap();
    let notify = fx.calls_of("notify-send");
    assert_eq!(notify.len(), 1);
    assert_eq!(
        notify[0].args,
        [
            "--app-name=Fleet Deck",
            "--urgency=normal",
            "--",
            "Fleet Deck",
            "Fleet Deck is sending keyboard input for bold-fox to an agent preview."
        ]
    );
    fx.clear_logs();
    fx.run(&fx.pointer(1, 1), &probe).await.unwrap();
    assert_eq!(
        fx.calls_of("notify-send")[0].args[4],
        "Fleet Deck is moving the pointer for bold-fox in an agent preview."
    );
}

#[tokio::test]
async fn text_travels_only_on_stdin_and_in_no_argv_env_or_log() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    let text = "-rf --help SENTINEL $(x) 'q' \"d\" caf\u{e9} \u{65e5}\u{672c}\u{8a9e}  ";
    fx.run(&fx.text(text), &probe).await.unwrap();
    // The argv is a dash and nothing else; stdin holds every byte.
    assert_eq!(fx.calls_of("wtype")[0].args, strs(&["-"]));
    assert_eq!(fx.read("wtype.stdin").as_bytes(), text.as_bytes());
    // The explicit UTF-8 locale, and the text in no environment variable.
    assert_eq!(fx.read("wtype.locale").trim(), "C.UTF-8");
    assert!(!fx.read("wtype.env").contains("SENTINEL"));
    assert!(fx.read("wtype.env").contains("LC_ALL=C.UTF-8"));
    // The text is in no argv of any program.
    for log in ["calls.log", "reads.log"] {
        assert!(!fx.read(log).contains("SENTINEL"), "{log}");
    }
}

#[tokio::test]
async fn the_text_is_not_trimmed_changed_or_given_a_return() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    for text in ["  x  ", "e\u{301}", "a b", &"z".repeat(2000)] {
        fx.run(&fx.text(text), &probe).await.unwrap();
        assert_eq!(fx.read("wtype.stdin"), text);
    }
    // One `wtype` run per call; a Return is only a `key_press`.
    assert_eq!(fx.calls_of("wtype").len(), 4);
    assert!(fx.calls_of("wtype").iter().all(|c| c.args == strs(&["-"])));
}

#[tokio::test]
async fn the_pointer_moves_to_the_global_point_after_focus_and_a_final_check() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    // The preview is at (100, 100): the point (10, 20) is (110, 120).
    fx.run(&fx.pointer(10, 20), &probe).await.unwrap();
    assert_eq!(
        fx.timeline(),
        [
            "claim",
            "check",
            "notify-send",
            "check",
            &fx.focus_line(),
            "check",
            "hyprctl -j activewindow",
            "hyprctl dispatch movecursor 110 120",
            "hyprctl -j activewindow",
        ]
    );
    // The pointer needs no `wtype`.
    assert!(fx.calls_of("wtype").is_empty());
    // The corners of the preview (800 x 600).
    fx.run(&fx.pointer(0, 0), &probe).await.unwrap();
    fx.run(&fx.pointer(799, 599), &probe).await.unwrap();
    let moves: Vec<String> = fx
        .calls_of("hyprctl")
        .into_iter()
        .filter(|c| c.args.iter().any(|a| a == "movecursor"))
        .map(|c| c.args.join(" "))
        .collect();
    assert_eq!(
        moves,
        [
            "dispatch movecursor 110 120",
            "dispatch movecursor 100 100",
            "dispatch movecursor 899 699"
        ]
    );
}

#[tokio::test]
async fn the_result_has_no_handle_no_address_and_no_capture() {
    let fx = Fx::new().await;
    let result = fx.desktop().input_result(&on).await.unwrap();
    assert_eq!(result.address, None);
    assert_eq!(result.window_id, None);
    assert!(result.capture.is_none() && result.info_fresh.is_none());
}

#[tokio::test]
async fn the_ages_at_their_exact_limit_pass() {
    // The clock reads: the authority answer (0), the focus snapshot (400),
    // the final checks (500): ages 500 ms and 100 ms.
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    probe.clock = Some(scripted_clock(&[0, 400, 500]));
    fx.run(&fx.key("a"), &probe).await.unwrap();
    assert_eq!(fx.calls_of("wtype").len(), 1);
}

#[tokio::test]
async fn a_replayed_done_operation_runs_nothing() {
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    probe.claim = fx.claim(Claimed::Replay);
    fx.run(&fx.text("again"), &probe).await.unwrap();
    // Only the claim happened: no notification, focus or input.
    assert_eq!(fx.timeline(), ["claim"]);
}

async fn store() -> Store {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
}

const OP: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

fn hash(action: &A) -> String {
    action_hash_for(action, NODE, Some(&ctx()))
}

async fn claimed_run(
    fx: &Fx,
    store: &Store,
    action: &A,
    context: &fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext,
) -> Result<(), crate::desktop::Fail> {
    let probe = Probe::new(fx);
    let h = hash(action);
    let kind = crate::desktop::coordinator::claim_kind(action).unwrap();
    let claim = StoreClaim::new(store, DECK, OP, &h, kind, context);
    let result = fx.run_with(action, &probe, &claim).await;
    claim
        .finish(result.as_ref().err().map_or(Ok(()), |e| Err(e.code)))
        .await;
    result
}

#[tokio::test]
async fn a_claimed_operation_runs_once_and_a_replay_or_a_changed_payload_does_not() {
    let fx = Fx::new().await;
    let store = store().await;
    let c = ctx();
    let action = fx.text("first");
    claimed_run(&fx, &store, &action, &c).await.unwrap();
    assert_eq!(fx.calls_of("wtype").len(), 1);
    let row = store.desktop_op_get(DECK, OP).await.unwrap().unwrap();
    assert_eq!(
        (row.state.as_str(), row.kind.as_str()),
        ("done", "input_type_text")
    );
    // The same operation again: the stored result, no second input.
    claimed_run(&fx, &store, &action, &c).await.unwrap();
    assert_eq!(fx.calls_of("wtype").len(), 1);
    assert_eq!(fx.calls_of("notify-send").len(), 1);
    // The same operation with another payload: refused, nothing runs.
    let other = fx.text("second");
    let e = claimed_run(&fx, &store, &other, &c).await.unwrap_err();
    assert_eq!(e.code, "authority_unknown");
    assert_eq!(fx.calls_of("wtype").len(), 1);
    assert_eq!(fx.calls_of("notify-send").len(), 1);
}

#[tokio::test]
async fn a_claim_that_a_crash_left_open_is_never_resumed() {
    let fx = Fx::new().await;
    let store = store().await;
    let c = ctx();
    let action = fx.key("a");
    // A run that claimed and then stopped (the station restarted): the row
    // stays `claimed`.
    let kind = crate::desktop::coordinator::claim_kind(&action).unwrap();
    store
        .desktop_op_claim(DECK, OP, &hash(&action), kind, &c, 1)
        .await
        .unwrap();
    let e = claimed_run(&fx, &store, &action, &c).await.unwrap_err();
    assert_eq!(e.code, "authority_unknown");
    assert!(fx.calls_of("wtype").is_empty());
    assert!(fx.calls_of("notify-send").is_empty());
    let row = store.desktop_op_get(DECK, OP).await.unwrap().unwrap();
    assert_eq!(row.state, "claimed");
}

#[tokio::test]
async fn an_env_without_a_hook_uses_the_system_clock_and_the_fixed_limits() {
    let env = Env::new(
        std::sync::Arc::new(super::rig::all_on),
        std::sync::Arc::new(|| true),
        Vec::new(),
    );
    assert_eq!(env.wtype_limit, std::time::Duration::from_secs(15));
    assert_eq!(env.handler_limit, std::time::Duration::from_secs(30));
    assert_eq!(super::POINTER_LIMIT, std::time::Duration::from_secs(1));
    assert_eq!(super::ACTIVE_LIMIT, std::time::Duration::from_millis(250));
    assert_eq!(
        super::AUTHORITY_MAX_AGE,
        std::time::Duration::from_millis(500)
    );
    assert_eq!(super::FOCUS_MAX_AGE, std::time::Duration::from_millis(100));
    assert_eq!(super::POLL_EVERY, std::time::Duration::from_millis(100));
}
