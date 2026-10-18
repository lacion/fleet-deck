//! The coordinator authority on the station (R13 P6 §5.4, §5.6, §6.3): the
//! hash with the context, the checks right after the admission, the remote
//! check and the live gates right before the effect, and the notification.
//! The deck answer is a fake.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_link::desktop_authority::{action_hash, action_hash_for, issue_coordinator};
use fleetdeck_proto::coordinator_desktop_wire::{
    AuthorityPhase, CoordinatorContext, ProtectedRead,
};
use fleetdeck_proto::desktop::{DesktopAction as A, DesktopInfo, MonitorInfo, Urgency, WallWindow};
use fleetdeck_proto::desktop_authority::AUTHORITY_REFUSED;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::version::{Negotiated, PROTO_MAX, feature};
use fleetdeck_proto::{SessionId, StationId};
use serde_json::json;

use super::authority::{Admitted, Start};
use super::authority_tests::{EPOCH, Harness, NODE, id};
use super::coordinator::{
    DispatchCheck, Extras, Gates, Live, answer_result, check_live, claim_kind, fleet_off, lost,
    map_info, new_window_id, unknown, validate,
};
use super::fake::{Opts, Rig};
use super::tests::{act_err, on};
use super::{Fail, code};

pub(super) const DECK: &str = "ab00000000000000000000000000000000000000000000000000000000000001";

pub(super) fn ctx() -> CoordinatorContext {
    CoordinatorContext {
        grant_id: "grant-1".to_owned(),
        binding_id: "binding-1".to_owned(),
        generation: 3,
        caller_station: StationId::new("01HZZZZZZZZZZZZZZZZZZZZZZA"),
        caller_session: SessionId::new("01HZZZZZZZZZZZZZZZZZZZZZZB"),
        caller_label: "bold-fox".to_owned(),
    }
}

pub(super) fn negotiated(features: &[&str]) -> Negotiated {
    Negotiated {
        version: PROTO_MAX,
        features: features.iter().map(|f| (*f).to_owned()).collect(),
    }
}

fn all_features() -> Negotiated {
    negotiated(&[feature::DESKTOP_MCP, feature::CAPTURE_MCP])
}

/// What the fake deck check saw, and what it answers.
pub(super) struct FakeCheck {
    result: Result<(), Fail>,
    pub seen: Mutex<Vec<(AuthorityPhase, Option<ProtectedRead>)>>,
    hook: Box<dyn Fn() + Send + Sync>,
}

impl FakeCheck {
    pub fn answering(result: Result<(), Fail>) -> Arc<Self> {
        Self::with_hook(result, || {})
    }

    pub fn ok() -> Arc<Self> {
        Self::answering(Ok(()))
    }

    pub fn with_hook(
        result: Result<(), Fail>,
        hook: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            result,
            seen: Mutex::new(Vec::new()),
            hook: Box::new(hook),
        })
    }

    pub fn phases(&self) -> Vec<AuthorityPhase> {
        self.seen.lock().unwrap().iter().map(|(p, _)| *p).collect()
    }
}

#[async_trait]
impl DispatchCheck for FakeCheck {
    async fn check(&self, phase: AuthorityPhase, read: Option<&ProtectedRead>) -> Result<(), Fail> {
        (self.hook)();
        self.seen.lock().unwrap().push((phase, read.cloned()));
        self.result.clone()
    }
}

pub(super) fn open_gates() -> Gates {
    Arc::new(|| Ok(()))
}

pub(super) fn extras() -> Extras {
    Extras {
        deck_fp: DECK.to_owned(),
        tailnet_host: Some("box.tail1234.ts.net".to_owned()),
    }
}

/// A coordinator authority that the deck would issue now for `action`,
/// admitted, with the given checks.
pub(super) fn coordinated(
    h: &Harness,
    action: &A,
    n: u64,
    context: &CoordinatorContext,
    remote: Arc<dyn DispatchCheck>,
    gates: Gates,
    extras: Extras,
) -> Admitted {
    let authority = issue_coordinator(
        action,
        NODE,
        id(n),
        EPOCH.to_owned(),
        h.now(),
        context.clone(),
    );
    h.admit(action, Some(authority))
        .unwrap()
        .coordinated(remote, gates, extras)
}

pub(super) fn notify(title: &str, urgency: Urgency) -> A {
    A::Notify {
        title: title.to_owned(),
        body: "Body".to_owned(),
        urgency,
    }
}

fn refused(e: &Fail) -> bool {
    e.code == "forbidden" && e.message == AUTHORITY_REFUSED
}

// ------------------------------------------------------------------ hash

#[test]
fn a_changed_context_field_fails_the_hash_and_an_owner_hash_is_unchanged() {
    let h = Harness::new();
    let action = notify("Hello", Urgency::Normal);
    let good = issue_coordinator(&action, NODE, id(1), EPOCH.to_owned(), h.now(), ctx());
    assert!(h.admit(&action, Some(good.clone())).is_ok());
    let changed: Vec<CoordinatorContext> = vec![
        CoordinatorContext {
            grant_id: "grant-2".into(),
            ..ctx()
        },
        CoordinatorContext {
            binding_id: "binding-2".into(),
            ..ctx()
        },
        CoordinatorContext {
            generation: 4,
            ..ctx()
        },
        CoordinatorContext {
            caller_station: StationId::new("01HZZZZZZZZZZZZZZZZZZZZZZZ"),
            ..ctx()
        },
        CoordinatorContext {
            caller_session: SessionId::new("01HZZZZZZZZZZZZZZZZZZZZZZZ"),
            ..ctx()
        },
        CoordinatorContext {
            caller_label: "other".into(),
            ..ctx()
        },
    ];
    for (n, c) in (2_u64..).zip(changed) {
        let mut a = good.clone();
        a.action_id = id(n);
        a.coordinator = Some(c);
        assert!(refused(&h.admit(&action, Some(a)).unwrap_err()));
    }
    // The context removed: the owner hash is not the coordinator hash.
    let mut a = good;
    a.action_id = id(20);
    a.coordinator = None;
    assert!(refused(&h.admit(&action, Some(a)).unwrap_err()));
    // An owner or wake authority keeps its hash and its admission.
    assert_eq!(
        action_hash(&action, NODE),
        action_hash_for(&action, NODE, None)
    );
    assert_ne!(
        action_hash(&action, NODE),
        action_hash_for(&action, NODE, Some(&ctx()))
    );
    h.admitted(&A::ScreenLock, 30);
}

// -------------------------------------------------- the checks of intake

#[test]
fn every_refusal_after_the_admission_has_the_refused_text() {
    let n = notify("Hello", Urgency::Normal);
    let c = ctx();
    let all = all_features();
    assert!(validate(Some(&c), &n, &all).is_ok());
    for action in [
        A::BrowserOpen {
            url: "http://localhost:3000".into(),
            monitor: None,
            session: None,
        },
        A::PreviewClose {
            window_id: "ab".repeat(16),
        },
        A::ScreenCapture {
            monitor: "DP-1".into(),
        },
    ] {
        assert!(validate(Some(&c), &action, &all).is_ok(), "{action:?}");
    }
    // The link features.
    assert!(refused(
        &validate(Some(&c), &n, &negotiated(&[feature::CAPTURE_MCP])).unwrap_err()
    ));
    let capture = A::ScreenCapture {
        monitor: "DP-1".into(),
    };
    let only_desktop = negotiated(&[feature::DESKTOP_MCP]);
    assert!(validate(Some(&c), &n, &only_desktop).is_ok());
    assert!(refused(
        &validate(Some(&c), &capture, &only_desktop).unwrap_err()
    ));
    // The label: more than 64 characters, a character outside the set, empty.
    for label in [
        "x".repeat(65),
        "a b".to_owned(),
        "caf\u{e9}".to_owned(),
        "a/b".to_owned(),
        String::new(),
    ] {
        let bad = CoordinatorContext {
            caller_label: label,
            ..ctx()
        };
        assert!(refused(&validate(Some(&bad), &n, &all).unwrap_err()));
    }
    let long_ok = CoordinatorContext {
        caller_label: "A_b-9".repeat(12) + "abcd",
        ..ctx()
    };
    assert_eq!(long_ok.caller_label.len(), 64);
    assert!(validate(Some(&long_ok), &n, &all).is_ok());
    // Any other empty field.
    for bad in [
        CoordinatorContext {
            grant_id: String::new(),
            ..ctx()
        },
        CoordinatorContext {
            binding_id: String::new(),
            ..ctx()
        },
        CoordinatorContext {
            caller_station: StationId::new(""),
            ..ctx()
        },
        CoordinatorContext {
            caller_session: SessionId::new(""),
            ..ctx()
        },
    ] {
        assert!(refused(&validate(Some(&bad), &n, &all).unwrap_err()));
    }
    // The kind of the action.
    for action in [
        A::ScreenLock,
        A::ScreenDnd { on: true },
        A::WindowClose {
            address: "0x1".into(),
        },
        A::WallClose {
            display_id: "d".into(),
        },
        A::WorkspaceShow {
            monitor: "DP-1".into(),
            workspace: 1,
        },
    ] {
        assert!(
            refused(&validate(Some(&c), &action, &all).unwrap_err()),
            "{action:?}"
        );
    }
    // A critical notification.
    assert!(refused(
        &validate(Some(&c), &notify("Hello", Urgency::Critical), &all).unwrap_err()
    ));
    // An owner or wake authority has no context: `preview_close` is refused.
    let close = A::PreviewClose {
        window_id: "ab".repeat(16),
    };
    assert!(refused(&validate(None, &close, &all).unwrap_err()));
}

#[test]
fn the_claimed_kinds_are_the_three_mutations() {
    assert_eq!(claim_kind(&notify("a", Urgency::Low)), Some("notify"));
    assert_eq!(
        claim_kind(&A::BrowserOpen {
            url: "http://localhost".into(),
            monitor: None,
            session: None
        }),
        Some("preview_open")
    );
    assert_eq!(
        claim_kind(&A::PreviewClose {
            window_id: "x".into()
        }),
        Some("preview_close")
    );
    assert_eq!(
        claim_kind(&A::ScreenCapture {
            monitor: "DP-1".into()
        }),
        None
    );
}

// ------------------------------------------------------- the deck answer

#[test]
fn only_a_clear_valid_answer_passes_and_the_rest_fails_closed() {
    let ok = |v: serde_json::Value| Outcome::Ok(v);
    assert_eq!(
        answer_result(&ok(json!({"valid": true, "reason": null}))),
        Ok(())
    );
    let lost_text = "Desktop authority was withdrawn before this operation could continue.";
    let unknown_text =
        "Desktop authority could not be checked. No further desktop work was started.";
    let lost_answer = answer_result(&ok(json!({"valid": false, "reason": "authority_lost"})));
    assert_eq!(lost_answer, Err(lost()));
    let e = lost_answer.unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("authority_lost", lost_text));
    let unknown_cases = [
        ok(json!({"valid": false, "reason": "authority_unknown"})),
        ok(json!({"valid": false, "reason": null})),
        ok(json!({"valid": false})),
        ok(json!({"valid": "yes", "reason": null})),
        ok(json!({"valid": true, "reason": null, "extra": 1})),
        ok(json!("valid")),
        ok(json!(null)),
        Outcome::Err(WireError::new("internal", "a wire error")),
    ];
    for outcome in &unknown_cases {
        let e = answer_result(outcome).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            ("authority_unknown", unknown_text),
            "{outcome:?}"
        );
    }
    assert_eq!(lost().code, code::AUTHORITY_LOST);
    assert_eq!(unknown().code, code::AUTHORITY_UNKNOWN);
}

// ----------------------------------------------- the check before the effect

async fn act_notify(
    rig: &Rig,
    h: &Harness,
    n: u64,
    remote: Arc<dyn DispatchCheck>,
    gates: Gates,
) -> Result<fleetdeck_proto::desktop::DesktopActResult, Fail> {
    let action = notify("Hello", Urgency::Normal);
    let admitted = coordinated(h, &action, n, &ctx(), remote, gates, extras());
    rig.desktop.act(&on, action, admitted).await
}

#[tokio::test]
async fn a_notification_runs_after_the_execute_check_with_the_fixed_argv() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let check = FakeCheck::ok();
    let result = act_notify(&rig, &h, 1, check.clone(), open_gates())
        .await
        .unwrap();
    assert_eq!((result.address, result.window_id), (None, None));
    assert!(result.capture.is_none());
    assert_eq!(check.phases(), [AuthorityPhase::Execute]);
    assert_eq!(check.seen.lock().unwrap()[0].1, None);
    let calls = rig.calls_of("notify-send");
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].args,
        [
            "--app-name=Fleet Deck",
            "--urgency=normal",
            "--",
            "Hello",
            "Body"
        ]
    );
}

#[tokio::test]
async fn a_withdrawn_or_unknown_authority_starts_nothing() {
    for (fail, wire) in [(lost(), "authority_lost"), (unknown(), "authority_unknown")] {
        let rig = Rig::new(Opts::default());
        let h = Harness::new();
        let e = act_notify(&rig, &h, 1, FakeCheck::answering(Err(fail)), open_gates())
            .await
            .unwrap_err();
        assert_eq!(act_err(e).0, wire);
        assert!(rig.calls().is_empty());
    }
}

#[tokio::test]
async fn a_gate_turned_off_during_the_remote_check_is_refused_after_the_await() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let fleet = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&fleet);
    let check = FakeCheck::with_hook(Ok(()), move || flag.store(false, Ordering::SeqCst));
    let read = Arc::clone(&fleet);
    let gates: Gates = Arc::new(move || {
        if read.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(fleet_off())
        }
    });
    let e = act_notify(&rig, &h, 1, check.clone(), gates)
        .await
        .unwrap_err();
    assert_eq!(
        act_err(e),
        ("forbidden", "Fleet control is off on this station.".into())
    );
    assert_eq!(check.phases(), [AuthorityPhase::Execute]);
    assert!(rig.calls().is_empty());
}

#[tokio::test]
async fn the_check_runs_after_the_preparation_and_right_before_the_spawn() {
    let rig = Rig::new(Opts::default());
    let h = Harness::new();
    let dir = rig.dir.path().to_owned();
    let lines = move |name: &str| {
        std::fs::read_to_string(dir.join(name))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("== "))
            .count()
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let check = FakeCheck::with_hook(Ok(()), move || {
        log.lock()
            .unwrap()
            .push((lines("reads.log"), lines("calls.log")));
    });
    act_notify(&rig, &h, 1, check, open_gates()).await.unwrap();
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    // The desktop was read (the preparation) and nothing had changed yet.
    assert!(seen[0].0 > 0, "{seen:?}");
    assert_eq!(seen[0].1, 0, "{seen:?}");
    // The program of the effect ran after the check.
    assert_eq!(rig.calls_of("notify-send").len(), 1);
}

#[tokio::test]
async fn start_checks_every_phase_and_begin_only_once() {
    let h = Harness::new();
    let action = A::ScreenCapture {
        monitor: "DP-1".into(),
    };
    let check = FakeCheck::ok();
    let admitted = coordinated(
        &h,
        &action,
        1,
        &ctx(),
        check.clone(),
        open_gates(),
        extras(),
    );
    let start = Start::new(admitted, &action);
    let env = ChildEnv::empty();
    start.begin(&env).await.unwrap();
    start.begin(&env).await.unwrap();
    assert_eq!(check.phases(), [AuthorityPhase::Execute]);
    start
        .check_phase(AuthorityPhase::Import, &env)
        .await
        .unwrap();
    start
        .check_phase(AuthorityPhase::Import, &env)
        .await
        .unwrap();
    assert_eq!(
        check.phases(),
        [
            AuthorityPhase::Execute,
            AuthorityPhase::Import,
            AuthorityPhase::Import
        ]
    );
    // The local gates are read after each remote answer.
    let fleet = Arc::new(AtomicBool::new(true));
    let read = Arc::clone(&fleet);
    let gates: Gates = Arc::new(move || {
        if read.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(fleet_off())
        }
    });
    let admitted = coordinated(&h, &action, 2, &ctx(), FakeCheck::ok(), gates, extras());
    let start = Start::new(admitted, &action);
    start.begin(&env).await.unwrap();
    fleet.store(false, Ordering::SeqCst);
    let e = start
        .check_phase(AuthorityPhase::Import, &env)
        .await
        .unwrap_err();
    assert_eq!(e, fleet_off());
    // An owner authorization has no remote check: both pass.
    let owner = Start::new(h.admitted(&A::ScreenLock, 3), &A::ScreenLock);
    owner
        .check_phase(AuthorityPhase::Import, &env)
        .await
        .unwrap();
    assert!(owner.coordinator().is_none());
}

// ------------------------------------------------------------ live gates

#[test]
fn the_live_gates_have_their_codes_and_texts() {
    let on = Live {
        fleet_control: true,
        desktop: true,
        capture: true,
        monitors: Vec::new(),
    };
    assert_eq!(check_live(&on, false, None), Ok(()));
    assert_eq!(check_live(&on, true, Some("DP-1")), Ok(()));
    let off = |f: fn(&mut Live)| {
        let mut l = on.clone();
        f(&mut l);
        l
    };
    // Fleet control is first, for an action and for a capture.
    let fleet = off(|l| l.fleet_control = false);
    for capture in [false, true] {
        assert_eq!(check_live(&fleet, capture, Some("DP-1")), Err(fleet_off()));
    }
    let desktop = off(|l| l.desktop = false);
    assert_eq!(check_live(&desktop, false, None), Err(Fail::forbidden()));
    let no_capture = off(|l| l.capture = false);
    assert_eq!(check_live(&no_capture, false, None), Ok(()));
    let e = check_live(&no_capture, true, Some("DP-1")).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("forbidden", "Desktop screenshots are off on this station.")
    );
    assert!(check_live(&no_capture, true, None).is_err());
    let listed = off(|l| l.monitors = vec!["HDMI-A-1".to_owned()]);
    assert_eq!(check_live(&listed, true, Some("HDMI-A-1")), Ok(()));
    let e = check_live(&listed, true, Some("DP-1")).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("forbidden", "This monitor is not allowed for screenshots.")
    );
    // The monitor rule does not apply to a read of a status.
    assert_eq!(check_live(&listed, true, None), Ok(()));
}

#[test]
fn a_window_handle_is_32_lowercase_hex_characters_and_differs() {
    let a = new_window_id().unwrap();
    let b = new_window_id().unwrap();
    for id in [&a, &b] {
        assert_eq!(id.len(), 32);
        assert!(
            id.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        );
    }
    assert_ne!(a, b);
}

fn pair(f: &Fail) -> (&'static str, &str) {
    (f.code, f.message.as_str())
}

fn live(monitors: &[&str]) -> Live {
    Live {
        fleet_control: true,
        desktop: true,
        capture: true,
        monitors: monitors.iter().map(|m| (*m).to_owned()).collect(),
    }
}

// -------------------------------------------------- the info of a read

fn monitors_of(base: &DesktopInfo, n: usize, name: impl Fn(usize) -> String) -> Vec<MonitorInfo> {
    (0..n)
        .map(|i| MonitorInfo {
            name: name(i),
            ..base.monitors[0].clone()
        })
        .collect()
}

fn wall(id: &str, monitor: Option<&str>) -> WallWindow {
    WallWindow {
        display_id: id.to_owned(),
        monitor: monitor.map(str::to_owned),
        address: format!("0x{id}"),
    }
}

#[tokio::test]
async fn the_info_follows_the_capture_rule_the_tools_and_the_walls() {
    let rig = Rig::new(Opts::default());
    let mut info = rig.desktop.info(&on).await.unwrap();
    info.tools.grim = true;
    // A wall without a monitor is left out.
    info.walls = vec![wall("d1", Some("DP-1")), wall("d2", None)];
    let mapped = map_info(&info, &live(&["HDMI-A-1"])).unwrap();
    let allowed: Vec<(&str, bool)> = mapped
        .monitors
        .iter()
        .map(|m| (m.name.as_str(), m.capture_allowed))
        .collect();
    assert_eq!(allowed, [("DP-1", false), ("HDMI-A-1", true)]);
    assert!(mapped.capabilities.screenshot);
    let walls: Vec<(&str, &str)> = mapped
        .walls
        .iter()
        .map(|w| (w.display_id.as_str(), w.monitor.as_str()))
        .collect();
    assert_eq!(walls, [("d1", "DP-1")]);
    // Each capability needs its setting.
    let caps = |info: &DesktopInfo, live: &Live| {
        let c = map_info(info, live).unwrap().capabilities;
        (c.notify, c.preview, c.screenshot)
    };
    assert_eq!(
        caps(
            &info,
            &Live {
                desktop: false,
                ..live(&[])
            }
        ),
        (false, false, true)
    );
    assert_eq!(
        caps(
            &info,
            &Live {
                capture: false,
                ..live(&[])
            }
        ),
        (true, true, false)
    );
    // A missing tool turns its capability off.
    info.tools.chromium = false;
    info.tools.notify_send = false;
    assert_eq!(caps(&info, &live(&[])), (false, false, true));
}

#[tokio::test]
async fn an_info_over_a_limit_is_too_large_and_never_cut() {
    let rig = Rig::new(Opts::default());
    let base = rig.desktop.info(&on).await.unwrap();
    let expect = (
        "desktop_result_too_large",
        "The desktop information exceeds the tool response limit.",
    );
    let check = |info: &DesktopInfo| map_info(info, &live(&[]));
    // 65 monitors, then 64.
    let mut info = base.clone();
    info.monitors = monitors_of(&base, 65, |i| format!("M-{i}"));
    assert_eq!(pair(&check(&info).unwrap_err()), expect);
    info.monitors.truncate(64);
    assert!(check(&info).is_ok());
    // 65 walls, then 64.
    let mut info = base.clone();
    info.walls = (0..65)
        .map(|i| wall(&format!("d{i}"), Some("DP-1")))
        .collect();
    assert_eq!(pair(&check(&info).unwrap_err()), expect);
    info.walls.truncate(64);
    assert!(check(&info).is_ok());
    // More than 16 KiB serialized.
    let mut info = base.clone();
    info.monitors = monitors_of(&base, 64, |i| format!("{i:02}{}", "m".repeat(300)));
    assert_eq!(pair(&check(&info).unwrap_err()), expect);
}

// ------------------------------------------------ the deadlines bound the start

#[tokio::test]
async fn the_import_check_does_not_apply_the_start_deadlines() {
    let env = ChildEnv::empty();
    let action = A::ScreenCapture {
        monitor: "DP-1".into(),
    };
    // 11 s after receipt on both clocks: the wall deadline and START_LIMIT
    // are past.
    let late = |h: &Harness| {
        h.wall.fetch_add(11_000, Ordering::SeqCst);
        h.advance_mono(11_000);
    };
    let h = Harness::new();
    let admitted = coordinated(
        &h,
        &action,
        1,
        &ctx(),
        FakeCheck::ok(),
        open_gates(),
        extras(),
    );
    let start = Start::new(admitted, &action);
    start.begin(&env).await.unwrap();
    late(&h);
    start
        .check_phase(AuthorityPhase::Import, &env)
        .await
        .unwrap();
    // The execute sequence keeps its deadlines.
    let e = start
        .check_phase(AuthorityPhase::Execute, &env)
        .await
        .unwrap_err();
    assert_eq!(act_err(e).0, "failed");
    // A withdrawn authority still fails the import check, with its code.
    let h = Harness::new();
    let lost_check = FakeCheck::answering(Err(lost()));
    let admitted = coordinated(&h, &action, 2, &ctx(), lost_check, open_gates(), extras());
    let start = Start::new(admitted, &action);
    late(&h);
    let e = start
        .check_phase(AuthorityPhase::Import, &env)
        .await
        .unwrap_err();
    assert_eq!(act_err(e).0, "authority_lost");
    // The gates and the connection are still checked for the import.
    let h = Harness::new();
    let off: Gates = Arc::new(|| Err(fleet_off()));
    let admitted = coordinated(&h, &action, 3, &ctx(), FakeCheck::ok(), off, extras());
    let start = Start::new(admitted, &action);
    late(&h);
    let e = start
        .check_phase(AuthorityPhase::Import, &env)
        .await
        .unwrap_err();
    assert_eq!(e, fleet_off());
    let h = Harness::new();
    let admitted = coordinated(
        &h,
        &action,
        4,
        &ctx(),
        FakeCheck::ok(),
        open_gates(),
        extras(),
    );
    let start = Start::new(admitted, &action);
    h.current.store(false, Ordering::SeqCst);
    let e = start
        .check_phase(AuthorityPhase::Import, &env)
        .await
        .unwrap_err();
    assert!(refused(&e));
}
