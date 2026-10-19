//! `desktop.act` with `screen_capture` and `desktop.capture_status` (R13 P5a
//! task D2, design `omarchy-p5a.md` §2.2, §2.4). The work is in
//! [`crate::desktop::capture`] and [`crate::artifacts::capture_import`]; this
//! file reads the station policy and the link identity and calls it.
//!
//! R13 P5b task S2 (design `omarchy-p5b.md` §4.2, §14): the status of a
//! recording answers a [`RecordingCaptureStatus`], with the progress while it
//! runs and the stop reason when it is terminal, on a link with
//! `desktop_recording.v1`; a link without `artifacts_video.v1` gets the legacy
//! projection of a published artifact.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_link::desktop_authority::action_hash_for;
use fleetdeck_proto::artifacts::ArtifactPublished;
use fleetdeck_proto::capture::{CaptureStatus, CaptureStatusQuery, code, text, valid_capture_id};
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::desktop::DesktopAction;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::recording::{
    RecordingCaptureStatus, RecordingPhase, RecordingProgress, project_legacy,
};
use fleetdeck_proto::version::feature;

use super::StationLink;
use super::ok;
use crate::artifacts::ArtifactService;
use crate::artifacts::capture_import::SyncFs;
use crate::desktop::capture::{ADMISSION, Allowed, Launch, Params, Request};
use crate::desktop::{self, Fail, authority};
use crate::store::StoreError;
use crate::store::capture_ops::{CaptureState, Lookup, RecordingRow};

/// The deadline of `desktop.capture_status` (design §7.1).
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
/// The text of `failed` when the status lookup passes its deadline.
const STATUS_TIMED_OUT: &str = "The screenshot status did not finish in time.";
/// The text of `unsupported` for a station without an artifact directory.
pub(super) const NO_ARTIFACTS: &str = "this station has no artifact directory";

fn failed(e: Fail) -> Outcome {
    Outcome::Err(WireError::new(e.code, e.message))
}

fn service(ctx: &StationLink) -> Result<&ArtifactService, Fail> {
    ctx.manager
        .artifacts()
        .ok_or_else(|| Fail::new(desktop::code::UNSUPPORTED, NO_ARTIFACTS))
}

/// The executable of the `shim-exec` launch: the one of the adapter, else the
/// running executable.
fn shim_exe(desktop: &desktop::Desktop) -> std::io::Result<std::path::PathBuf> {
    desktop
        .shim_exe()
        .map_or_else(std::env::current_exe, |p| Ok(p.to_path_buf()))
}

/// `desktop.act` with `screen_capture`: `capture_id` is the `action_id` that
/// `authority::admit` consumed. `coordinator` is the context of a coordinator
/// capture (R13 P6); the action hash covers it.
pub(super) async fn capture(
    ctx: &StationLink,
    control: &Control,
    admitted: authority::Admitted,
    capture_id: String,
    monitor: String,
    coordinator: Option<CoordinatorContext>,
) -> Outcome {
    if !valid_capture_id(&capture_id) {
        return failed(Fail::new(
            desktop::code::INVALID_PARAMS,
            "The capture ID is not valid.",
        ));
    }
    let service = match service(ctx) {
        Ok(s) => s.clone(),
        Err(e) => return failed(e),
    };
    let exe = match shim_exe(&desktop::service()) {
        Ok(exe) => exe,
        Err(e) => {
            tracing::warn!(kind = ?e.kind(), "cannot find the station executable");
            return failed(Fail::new(code::CAPTURE_FAILED, text::CAPTURE_FAILED));
        }
    };
    let station_node = ctx.node.identity.fingerprint().to_string();
    let action = DesktopAction::ScreenCapture {
        monitor: monitor.clone(),
    };
    let manager = Arc::clone(&ctx.manager);
    let allowed = Arc::new(move || {
        let policy = manager.policy();
        Allowed {
            desktop: policy.allow_desktop(),
            capture: policy.allow_desktop_capture(),
            monitors: policy.capture_monitors().to_vec(),
            quota_bytes: policy.capture_quota_bytes(),
            free_reserve_bytes: policy.capture_free_reserve_bytes(),
        }
    });
    // The live station name, as `station.info` reports it.
    let name = match ctx.manager.policy().name() {
        "" => ctx.name.clone(),
        live => live.to_owned(),
    };
    let request = Request {
        service,
        allowed,
        admitted,
        deck_fp: control.params().peer.to_string(),
        capture_id,
        action_hash: action_hash_for(&action, &station_node, coordinator.as_ref()),
        monitor,
        station_id: name.clone(),
        station_name: name,
        params: Params {
            launch: Launch::ShimExec { exe },
            fs: Arc::new(SyncFs),
            admission: ADMISSION,
        },
    };
    match desktop::service().screen_capture(request).await {
        Ok(mut result) => {
            super::intake_desktop::stamp(ctx, &mut result.info);
            super::intake_desktop::stamp_previews(control, &mut result.info);
            ok(result)
        }
        Err(e) => failed(e),
    }
}

/// The `CaptureStatus` of a lookup (design §2.4).
pub(super) fn status_of(capture_id: &str, lookup: Lookup) -> CaptureStatus {
    let capture_id = capture_id.to_owned();
    match lookup {
        Lookup::Unknown => CaptureStatus::Unknown { capture_id },
        Lookup::Running(_) => CaptureStatus::Pending { capture_id },
        Lookup::Deleted(_) => CaptureStatus::Deleted { capture_id },
        Lookup::Failed(op) => CaptureStatus::Failed {
            capture_id,
            error: op.code.unwrap_or_else(|| desktop::code::FAILED.to_owned()),
            message: op.message.unwrap_or_default(),
        },
        Lookup::Committed {
            artifact, version, ..
        } => CaptureStatus::Committed {
            capture_id,
            published: ArtifactPublished {
                artifact,
                version,
                created: true,
            },
        },
    }
}

/// The status answer of one capture (design §4.2, §14). `row` is the recording
/// row of a recording, `None` for a screenshot or an unknown ID.
///
/// - `recording_aware` (the link negotiated `desktop_recording.v1`): a
///   running recording adds `recording` (the progress, with `elapsed_ms` from
///   the recorder start at `now_ms`); a terminal one adds `stop_reason`.
/// - `video_aware` (the link negotiated `artifacts_video.v1`): without it a
///   published artifact in the answer gets [`project_legacy`].
pub(super) fn status_answer(
    mut status: CaptureStatus,
    row: Option<&RecordingRow>,
    now_ms: i64,
    recording_aware: bool,
    video_aware: bool,
) -> RecordingCaptureStatus {
    if !video_aware && let CaptureStatus::Committed { published, .. } = &mut status {
        project_legacy(published);
    }
    let (recording, stop_reason) = match row {
        Some(row) if recording_aware && row.op.state == CaptureState::Running => (
            Some(RecordingProgress {
                phase: row.phase.unwrap_or(RecordingPhase::Preparing),
                requested_seconds: row.requested_seconds,
                started_at: row.recorder_started_at,
                elapsed_ms: row
                    .recorder_started_at
                    .map(|at| u64::try_from(now_ms.saturating_sub(at)).unwrap_or(0)),
            }),
            None,
        ),
        Some(row) if recording_aware => (None, row.stop_reason),
        _ => (None, None),
    };
    RecordingCaptureStatus {
        status,
        recording,
        stop_reason,
    }
}

/// `desktop.capture_status`: the durable state of one capture of the
/// calling deck. It checks no gate: it only reads the record.
pub(super) async fn status(
    ctx: &StationLink,
    control: &Control,
    params: serde_json::Value,
) -> Outcome {
    let query: CaptureStatusQuery = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => {
            return failed(Fail::new(
                desktop::code::INVALID_PARAMS,
                format!("params: {e}"),
            ));
        }
    };
    if !valid_capture_id(&query.capture_id) {
        return failed(Fail::new(
            desktop::code::INVALID_PARAMS,
            "The capture ID is not valid.",
        ));
    }
    let service = match service(ctx) {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    let deck_fp = control.params().peer.to_string();
    let store = service.store();
    let read = async {
        let lookup = store.capture_lookup(&deck_fp, &query.capture_id).await?;
        let row = match lookup {
            Lookup::Unknown => None,
            _ => store.recording_get(&deck_fp, &query.capture_id).await?,
        };
        Ok::<_, StoreError>((lookup, row))
    };
    match tokio::time::timeout(STATUS_TIMEOUT, read).await {
        Ok(Ok((lookup, row))) => {
            let negotiated = &control.params().negotiated;
            ok(status_answer(
                status_of(&query.capture_id, lookup),
                row.as_ref(),
                store.now_ms(),
                negotiated.has(feature::DESKTOP_RECORDING),
                negotiated.has(feature::ARTIFACTS_VIDEO),
            ))
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "the capture status cannot be read");
            Outcome::Err(WireError::new("internal", e.to_string()))
        }
        Err(_) => failed(Fail::new(desktop::code::FAILED, STATUS_TIMED_OUT)),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use fleetdeck_core::ChildEnv;
    use fleetdeck_proto::artifacts::{Artifact, ArtifactKind, ArtifactVersion};
    use fleetdeck_proto::capture::{CaptureCaller, CaptureKind, CaptureProvenance};
    use fleetdeck_proto::recording::{RecordingMeta, StopReason};

    use super::*;
    use crate::desktop::capture::{Launch, capture_png};
    use crate::desktop::fake::{Grim, grim};
    use crate::store::capture_ops::{CaptureOp, StopIntent};

    const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    fn row(state: CaptureState, phase: RecordingPhase, started: Option<i64>) -> RecordingRow {
        RecordingRow {
            op: CaptureOp {
                deck_fp: "fp".to_owned(),
                capture_id: ID.to_owned(),
                action_hash: "h".to_owned(),
                kind: CaptureKind::Recording,
                state,
                monitor: "DP-1".to_owned(),
                artifact_id: "ART".to_owned(),
                sha256: None,
                code: None,
                message: None,
                reserved_bytes: 0,
                started_at: 1,
                updated_at: 1,
            },
            requested_seconds: 7,
            link_epoch: Some("epoch".to_owned()),
            phase: Some(phase),
            stop_intent: StopIntent::None,
            stop_reason: None,
            recorder_started_at: started,
            ended_at: None,
            duration_ms: None,
            width: None,
            height: None,
        }
    }

    fn published(kind: CaptureKind) -> ArtifactPublished {
        let recording = kind == CaptureKind::Recording;
        ArtifactPublished {
            artifact: Artifact {
                artifact_id: "ART".to_owned(),
                station_id: "s".to_owned(),
                session_id: None,
                title: "t".to_owned(),
                kind: if recording {
                    ArtifactKind::Video
                } else {
                    ArtifactKind::Image
                },
                latest_version: 1,
                created_at: 1,
                updated_at: 1,
                author: "owner".to_owned(),
            },
            version: ArtifactVersion {
                artifact_id: "ART".to_owned(),
                version: 1,
                size: 3,
                sha256: "ab".repeat(32),
                files: vec!["capture".to_owned()],
                created_at: 1,
                capture: Some(CaptureProvenance {
                    capture_id: ID.to_owned(),
                    station_id: "s".to_owned(),
                    monitor: "DP-1".to_owned(),
                    kind,
                    caller: CaptureCaller::Owner,
                    caller_station: None,
                    caller_session: None,
                    caller_grant_id: None,
                    at: 1,
                    recording: recording.then_some(RecordingMeta {
                        requested_seconds: 7,
                        duration_ms: 6_000,
                        width: 2,
                        height: 2,
                        stop_reason: StopReason::Duration,
                    }),
                }),
            },
            created: true,
        }
    }

    fn committed(kind: CaptureKind) -> CaptureStatus {
        CaptureStatus::Committed {
            capture_id: ID.to_owned(),
            published: published(kind),
        }
    }

    #[test]
    fn a_running_recording_answers_its_progress_and_no_stop_reason() {
        let r = row(
            CaptureState::Running,
            RecordingPhase::Recording,
            Some(1_000),
        );
        let pending = CaptureStatus::Pending {
            capture_id: ID.to_owned(),
        };
        let answer = status_answer(pending.clone(), Some(&r), 4_500, true, true);
        assert_eq!(answer.status, pending);
        assert_eq!(answer.stop_reason, None);
        assert_eq!(
            answer.recording,
            Some(RecordingProgress {
                phase: RecordingPhase::Recording,
                requested_seconds: 7,
                started_at: Some(1_000),
                elapsed_ms: Some(3_500),
            })
        );
        // Before the recorder started there is no elapsed time.
        let r = row(CaptureState::Running, RecordingPhase::Preparing, None);
        let answer = status_answer(pending, Some(&r), 4_500, true, true);
        let progress = answer.recording.unwrap();
        assert_eq!(progress.phase, RecordingPhase::Preparing);
        assert_eq!((progress.started_at, progress.elapsed_ms), (None, None));
    }

    #[test]
    fn a_terminal_recording_answers_its_stop_reason_and_no_progress() {
        let mut r = row(CaptureState::Failed, RecordingPhase::Stopping, Some(1));
        r.stop_reason = Some(StopReason::RecordingInterrupted);
        let failed = CaptureStatus::Failed {
            capture_id: ID.to_owned(),
            error: "recording_interrupted".to_owned(),
            message: "m".to_owned(),
        };
        let answer = status_answer(failed, Some(&r), 9, true, true);
        assert_eq!(answer.recording, None);
        assert_eq!(answer.stop_reason, Some(StopReason::RecordingInterrupted));
        // A committed one too.
        r.op.state = CaptureState::Committed;
        r.stop_reason = Some(StopReason::OwnerStop);
        let answer = status_answer(committed(CaptureKind::Recording), Some(&r), 9, true, true);
        assert_eq!(answer.stop_reason, Some(StopReason::OwnerStop));
    }

    #[test]
    fn a_link_without_desktop_recording_gets_no_extra_field() {
        let r = row(CaptureState::Running, RecordingPhase::Recording, Some(1));
        let pending = CaptureStatus::Pending {
            capture_id: ID.to_owned(),
        };
        let answer = status_answer(pending.clone(), Some(&r), 9, false, true);
        assert_eq!((&answer.recording, &answer.stop_reason), (&None, &None));
        assert_eq!(
            serde_json::to_value(&answer).unwrap(),
            serde_json::to_value(&pending).unwrap()
        );
    }

    #[test]
    fn the_artifact_of_a_recording_is_projected_only_for_a_link_without_video() {
        let r = row(CaptureState::Committed, RecordingPhase::Finalizing, Some(1));
        let video = status_answer(committed(CaptureKind::Recording), Some(&r), 9, true, true);
        let CaptureStatus::Committed { published, .. } = video.status else {
            panic!("committed");
        };
        assert_eq!(published.artifact.kind, ArtifactKind::Video);
        assert!(published.version.capture.is_some());

        let legacy = status_answer(committed(CaptureKind::Recording), Some(&r), 9, true, false);
        let CaptureStatus::Committed { published, .. } = legacy.status else {
            panic!("committed");
        };
        assert_eq!(published.artifact.kind, ArtifactKind::Other);
        assert!(published.version.capture.is_none());
    }

    #[test]
    fn a_screenshot_answer_is_the_same_json_with_or_without_the_features() {
        let status = committed(CaptureKind::Screenshot);
        let want = serde_json::to_value(&status).unwrap();
        for (recording, video) in [(true, true), (true, false), (false, true), (false, false)] {
            let answer = status_answer(status.clone(), None, 9, recording, video);
            assert_eq!(serde_json::to_value(&answer).unwrap(), want);
        }
    }

    #[test]
    fn the_shim_exe_of_the_adapter_is_the_exe_of_the_launch_else_the_running_one() {
        let plain = desktop::Desktop::with_env(ChildEnv::empty());
        assert_eq!(shim_exe(&plain).unwrap(), std::env::current_exe().unwrap());

        // A script that records its argv, drops `--parent <pid>` and the `--`,
        // and runs the rest, as `shim-exec` does.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("shim.sh");
        let log = dir.path().join("argv.log");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do echo \"$a\"; done > {}\n\
                 [ \"$1\" = shim-exec ] && [ \"$2\" = --parent ] || exit 3\n\
                 shift 3; [ \"$1\" = -- ] || exit 4; shift; exec \"$@\"\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let set = desktop::Desktop::with_env(ChildEnv::empty()).with_shim_exe(script.clone());
        let exe = shim_exe(&set).unwrap();
        assert_eq!(exe, script);

        let env = grim(dir.path(), &Grim::Bytes(b"png"));
        let mut out = tempfile::tempfile().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let got = capture_png(
            &env,
            &Launch::ShimExec { exe },
            "DP-1",
            &mut out,
            1024,
            deadline,
            &|_| Ok(()),
        );
        assert_eq!(got.unwrap(), 3);
        let argv = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = argv.lines().collect();
        assert_eq!(&lines[..2], ["shim-exec", "--parent"]);
        assert_eq!(lines[3], "--");
        assert!(lines[4].ends_with("/bin/grim"), "{argv}");
        assert_eq!(
            &lines[5..],
            ["-t", "png", "-s", "1", "-l", "6", "-o", "DP-1", "-"]
        );
    }
}
