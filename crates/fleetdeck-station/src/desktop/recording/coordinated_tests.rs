//! The coordinator recording (R13 P5b-2 §13.3, §7.5; P6 §4.6, §6.3): the fixed
//! disclosure before the recorder, the remote checks, the refusal before the
//! import, the coordinator terminal codes and the provenance. The deck
//! answers are fakes; the desktop, `wf-recorder` and `notify-send` are the
//! fakes of the owner recording.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fleetdeck_proto::artifacts::ArtifactKind;
use fleetdeck_proto::capture::{CaptureCaller, CaptureKind, code};
use fleetdeck_proto::coordinator_desktop_wire::{
    AuthorityPhase, CoordinatorContext, ProtectedRead,
};
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction};
use fleetdeck_proto::recording::{LeasePhase, RecordingMeta, StopReason};

use super::op::{Stage, static_code};
use super::rig_tests::{ID, Reply, T, err};
use super::{fail_for, fail_for_coordinator};
use crate::artifacts::capture_import::tests::FP;
use crate::desktop::Fail;
use crate::desktop::authority::Admitted;
use crate::desktop::coordinator::{DispatchCheck, Gates, fleet_off, lost, unknown};
use crate::desktop::coordinator_tests::{FakeCheck, ctx, extras, open_gates};
use crate::store::capture_ops::CaptureState;

const NODE: &str = "aa11";
const DISCLOSURE: &str = "Fleet Deck is recording DP-1 for bold-fox for up to 1 seconds.";

/// Answers `execute` and `import` apart.
struct PhaseCheck {
    execute: Result<(), Fail>,
    import: Result<(), Fail>,
    seen: Mutex<Vec<AuthorityPhase>>,
}

impl PhaseCheck {
    fn new(execute: Result<(), Fail>, import: Result<(), Fail>) -> Arc<Self> {
        Arc::new(Self {
            execute,
            import,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn phases(&self) -> Vec<AuthorityPhase> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl DispatchCheck for PhaseCheck {
    async fn check(
        &self,
        phase: AuthorityPhase,
        _read: Option<&ProtectedRead>,
    ) -> Result<(), Fail> {
        self.seen.lock().unwrap().push(phase);
        match phase {
            AuthorityPhase::Import => self.import.clone(),
            _ => self.execute.clone(),
        }
    }
}

/// The count of the lines that start a program in a log of the fakes.
fn programs(dir: &Path, name: &str) -> usize {
    std::fs::read_to_string(dir.join(name))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with("== "))
        .count()
}

fn coordinated(
    t: &T,
    seconds: u32,
    remote: Arc<dyn DispatchCheck>,
    gates: Gates,
) -> super::RecordRequest {
    let action = DesktopAction::ScreenRecord {
        monitor: "DP-1".to_owned(),
        seconds,
    };
    let mut req = t.request(ID, "DP-1", seconds);
    req.admitted = Admitted::open_coordinated(&action, NODE, ctx(), remote, gates, extras());
    req.coordinator = Some(ctx());
    req
}

async fn run(
    t: &T,
    seconds: u32,
    remote: Arc<dyn DispatchCheck>,
    gates: Gates,
) -> Result<DesktopActResult, Fail> {
    t.desktop()
        .screen_record(coordinated(t, seconds, remote, gates))
        .await
}

async fn column(t: &T) -> Option<String> {
    t.store()
        .db()
        .call(|conn| {
            Ok(conn.query_row(
                "SELECT coordinator FROM capture_ops WHERE capture_id = ?1",
                [ID],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn the_disclosure_is_exact_and_comes_before_the_check_and_the_recorder() {
    let t = T::new().await;
    let dir = t.dir().to_owned();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let check = FakeCheck::with_hook(Ok(()), move || {
        let runs = std::fs::read_to_string(dir.join("recorder.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| *l == "--")
            .count();
        log.lock()
            .unwrap()
            .push((programs(&dir, "calls.log"), runs));
    });
    let done = run(&t, 1, check.clone(), open_gates()).await.unwrap();
    assert!(done.capture.is_some());
    assert_eq!(
        check.phases(),
        [AuthorityPhase::Execute, AuthorityPhase::Import]
    );
    // At the execute check the disclosure was sent and no recorder ran.
    assert_eq!(seen.lock().unwrap()[0], (1, 0));
    let notes = t.fake.calls_of("notify-send");
    assert_eq!(notes.len(), 1);
    assert_eq!(
        notes[0].args,
        [
            "--app-name=Fleet Deck",
            "--urgency=normal",
            "--",
            "Fleet Deck",
            DISCLOSURE
        ]
    );
    assert_eq!(t.recorder_runs(), 1);
}

#[test]
fn the_disclosure_text_names_the_monitor_the_label_and_the_seconds() {
    assert_eq!(
        super::coordinated::disclosure("DP-1", 1, &ctx()),
        DISCLOSURE
    );
    assert_eq!(
        super::coordinated::disclosure("HDMI-A-1", 120, &ctx()),
        "Fleet Deck is recording HDMI-A-1 for bold-fox for up to 120 seconds."
    );
}

#[tokio::test]
async fn a_failed_disclosure_spawns_nothing_and_fails_the_recording() {
    let t = T::new().await;
    t.fake.fail("notify-send");
    let check = FakeCheck::ok();
    let (c, m) = err(run(&t, 1, check.clone(), open_gates()).await);
    assert_eq!(
        (c, m.as_str()),
        ("failed", "`notify-send` failed with exit code 2")
    );
    assert_eq!(t.recorder_runs(), 0);
    // The authority check never ran: the disclosure comes first.
    assert!(check.phases().is_empty());
    t.assert_discarded(ID, "failed", StopReason::RecorderExited)
        .await;
}

#[tokio::test]
async fn a_refusal_of_the_execute_check_spawns_nothing() {
    for (fail, want) in [(lost(), "authority_lost"), (unknown(), "authority_unknown")] {
        let t = T::new().await;
        let check = PhaseCheck::new(Err(fail), Ok(()));
        let (c, _) = err(run(&t, 1, check.clone(), open_gates()).await);
        assert_eq!(c, want);
        assert_eq!(check.phases(), [AuthorityPhase::Execute]);
        assert_eq!(t.recorder_runs(), 0);
        t.assert_discarded(ID, want, StopReason::LeaseDenied).await;
    }
}

#[tokio::test]
async fn a_refusal_before_the_import_discards_with_coordinator_authority_lost() {
    for fail in [lost(), unknown()] {
        let t = T::new().await;
        let check = PhaseCheck::new(Ok(()), Err(fail));
        let (c, _) = err(run(&t, 1, check.clone(), open_gates()).await);
        assert_eq!(c, "authority_lost");
        assert_eq!(
            check.phases(),
            [AuthorityPhase::Execute, AuthorityPhase::Import]
        );
        // The recording ran, then was discarded: no artifact, no file.
        assert_eq!(t.recorder_runs(), 1);
        t.assert_discarded(ID, "authority_lost", StopReason::CoordinatorAuthorityLost)
            .await;
        // The same capture ID again repeats the stored failure; no recorder.
        let again = err(run(&t, 1, FakeCheck::ok(), open_gates()).await);
        assert_eq!(again.0, "authority_lost");
        assert_eq!(t.recorder_runs(), 1);
    }
}

#[tokio::test]
async fn a_gate_that_is_off_spawns_nothing_and_sends_no_disclosure() {
    let t = T::new().await;
    let gates: Gates = Arc::new(|| Err(fleet_off()));
    let (c, m) = err(run(&t, 1, FakeCheck::ok(), gates).await);
    assert_eq!(
        (c, m.as_str()),
        ("forbidden", "Fleet control is off on this station.")
    );
    assert!(t.fake.calls().is_empty());
    assert_eq!(t.recorder_runs(), 0);
    // Refused at the first read of the gates: no row exists.
    assert!(t.row(ID).await.is_none());

    // A gate that turns off after the first read stops before the disclosure.
    let t = T::new().await;
    let first = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&first);
    let gates: Gates = Arc::new(move || {
        if flag.swap(false, Ordering::SeqCst) {
            Ok(())
        } else {
            Err(fleet_off())
        }
    });
    let (c, _) = err(run(&t, 1, FakeCheck::ok(), gates).await);
    assert_eq!(c, "forbidden");
    assert!(t.fake.calls().is_empty());
    assert_eq!(t.recorder_runs(), 0);
}

#[tokio::test]
async fn the_provenance_the_meta_and_the_context_are_stored() {
    let t = T::new().await;
    let done = run(&t, 1, FakeCheck::ok(), open_gates()).await.unwrap();
    let p = done.capture.unwrap().published;
    assert_eq!(p.artifact.kind, ArtifactKind::Video);
    assert_eq!(
        p.artifact.author,
        "coordinator:01HZZZZZZZZZZZZZ/01HZZZZZZZZZZZZZZZZZZZZZZB"
    );
    let c = p.version.capture.clone().unwrap();
    assert_eq!(c.kind, CaptureKind::Recording);
    assert_eq!(c.caller, CaptureCaller::Coordinator);
    assert_eq!(
        (
            c.caller_station.as_deref(),
            c.caller_session.as_deref(),
            c.caller_grant_id.as_deref()
        ),
        (
            Some("01HZZZZZZZZZZZZZZZZZZZZZZA"),
            Some("01HZZZZZZZZZZZZZZZZZZZZZZB"),
            Some("grant-1")
        )
    );
    assert_eq!((c.capture_id.as_str(), c.monitor.as_str()), (ID, "DP-1"));
    assert_eq!(
        c.recording,
        Some(RecordingMeta {
            requested_seconds: 1,
            duration_ms: 1000,
            width: 3440,
            height: 1440,
            stop_reason: StopReason::Duration,
        })
    );
    // The private operation record keeps the context with the generation.
    let stored: CoordinatorContext = serde_json::from_str(&column(&t).await.unwrap()).unwrap();
    assert_eq!(stored, ctx());
    assert_eq!(stored.generation, 3);
    let row = t.row(ID).await.unwrap();
    assert_eq!(row.op.state, CaptureState::Committed);
    assert_eq!(row.stop_reason, Some(StopReason::Duration));
}

#[tokio::test]
async fn the_owner_recording_is_unchanged() {
    let t = T::new().await;
    let done = t.run(ID, "DP-1", 1).await.unwrap();
    let p = done.capture.unwrap().published;
    assert_eq!(p.artifact.author, "owner");
    let c = p.version.capture.unwrap();
    assert_eq!(c.caller, CaptureCaller::Owner);
    assert_eq!(
        (c.caller_station, c.caller_session, c.caller_grant_id),
        (None, None, None)
    );
    assert_eq!(column(&t).await, None);
    assert!(t.fake.calls_of("notify-send").is_empty());
    assert_eq!(t.recorder_runs(), 1);
}

#[tokio::test]
async fn a_coordinator_recording_ends_with_the_coordinator_terminal_code() {
    for (reason, owner, coordinator) in [
        (
            StopReason::DeckPolicy,
            "recording_interrupted",
            "authority_lost",
        ),
        (
            StopReason::CoordinatorAuthorityLost,
            "recording_interrupted",
            "authority_lost",
        ),
        (
            StopReason::ArtifactDeleted,
            "recording_interrupted",
            "authority_lost",
        ),
        (
            StopReason::Deadline,
            "desktop_timeout",
            "desktop_read_timeout",
        ),
        // `owner_login_ended` has no coordinator code: the owner code stays.
        (
            StopReason::OwnerLoginEnded,
            "recording_interrupted",
            "recording_interrupted",
        ),
    ] {
        let owner_run = T::new().await;
        owner_run
            .peer
            .set_phase(LeasePhase::Start, Reply::Discard(reason));
        let (c, _) = err(owner_run.run(ID, "DP-1", 5).await);
        assert_eq!(c, owner, "{reason:?}");
        owner_run.assert_discarded(ID, owner, reason).await;

        let t = T::new().await;
        t.peer.set_phase(LeasePhase::Start, Reply::Discard(reason));
        let (c, m) = err(run(&t, 5, FakeCheck::ok(), open_gates()).await);
        assert_eq!(c, coordinator, "{reason:?}");
        assert_eq!(m, super::code_text(coordinator), "{reason:?}");
        t.assert_discarded(ID, coordinator, reason).await;
        assert_eq!(t.recorder_runs(), 0);
        // A repeat answers the stored code, also `desktop_read_timeout`.
        assert_eq!(static_code(Some(coordinator)), coordinator);
    }
}

#[tokio::test]
async fn a_start_lease_without_an_answer_is_authority_unknown_for_a_coordinator() {
    let t = T::new().await;
    t.peer.set_phase(LeasePhase::Start, Reply::Transport);
    let (c, _) = err(run(&t, 5, FakeCheck::ok(), open_gates()).await);
    assert_eq!(c, "authority_unknown");
    t.assert_discarded(ID, "authority_unknown", StopReason::LeaseDenied)
        .await;
}

#[test]
fn the_stage_gets_the_coordinator_code_only_for_a_derived_stop() {
    let derived = Stage::discard(StopReason::PolicyOff, false);
    assert_eq!(derived.fail.code, "recording_interrupted");
    assert_eq!(derived.for_coordinator().fail.code, "authority_lost");
    // The ceiling splits the output limit for both.
    for (ceiling, want) in [(true, "recording_too_large"), (false, "recording_failed")] {
        let s = Stage::discard(StopReason::OutputLimit, ceiling).for_coordinator();
        assert_eq!(s.fail.code, want);
    }
    // A failure with its own code keeps it.
    let own = Stage::new(Fail::new("forbidden", "x"), StopReason::PolicyOff);
    assert_eq!(own.for_coordinator().fail.code, "forbidden");
    // The deadline before the recorder: the owner keeps its failure.
    let before = Stage::deadline(Fail::new("failed", "t"));
    assert_eq!(before.fail.code, "failed");
    assert_eq!(before.for_coordinator().fail.code, "desktop_read_timeout");
    // The cancel and the unknown outcome keep their codes.
    assert_eq!(
        Stage::cancelled(StopReason::OwnerStop)
            .for_coordinator()
            .fail
            .code,
        code::RECORDING_CANCELLED
    );
    // `fail_for_coordinator` equals `fail_for` where the table has no code.
    let reason = StopReason::OwnerLoginEnded;
    assert_eq!(
        fail_for_coordinator(reason, false).code,
        fail_for(reason, false).code
    );
}

#[tokio::test]
async fn the_row_of_a_refused_coordinator_recording_keeps_its_context() {
    let t = T::new().await;
    let check = PhaseCheck::new(Err(lost()), Ok(()));
    let _ = run(&t, 1, check, open_gates()).await;
    let stored: CoordinatorContext = serde_json::from_str(&column(&t).await.unwrap()).unwrap();
    assert_eq!(stored, ctx());
    assert_eq!(
        t.store()
            .recording_get(FP, ID)
            .await
            .unwrap()
            .unwrap()
            .op
            .state,
        CaptureState::Failed
    );
}
