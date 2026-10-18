//! The end of a recording (R13 P5b task S1, design `omarchy-p5b.md` §7.2,
//! §7.3, §7.6, §9.3, §9.4): what the exit of the recorder means, the checks
//! of the finished file on the retained descriptor, the `import` lease, the
//! import and the commit. Any discard on this way is latched first, so the
//! first discard reason wins and the commit refuses it. The lease guard of
//! the operation watches the expiry until the commit returns (§6.4).

use std::sync::Arc;

use fleetdeck_proto::artifacts::{ArtifactPublished, MAX_TITLE_CHARS};
use fleetdeck_proto::capture::{CaptureKind, CaptureProvenance, code, text};
use fleetdeck_proto::recording::{LeasePhase, RecordingMeta, StopReason};

use super::guard::{Armed, LeaseGuard};
use super::lease::Verdict;
use super::op::{Run, Stage, Tasks, join_failed, left};
use super::{check_gates, coordinated, probe};
use crate::artifacts::capture_import::{CaptureFileKind, import_capture};
use crate::desktop::Fail;
use crate::desktop::authority::Start;
use crate::desktop::capture::{Staging, finish_staging};
use crate::media::mp4::{self, Mp4Info, Mp4Limits};
use crate::media::recorder::{Exit, StopCause};
use crate::store::artifacts::CommitError;
use crate::store::capture_ops::{Lookup, NewCaptureArtifact, StopIntent};

/// What the checks of the finished file found.
struct Checked {
    len: u64,
    sha256: String,
    info: Mp4Info,
}

fn invalid() -> Fail {
    Fail::new(code::RECORDING_INVALID, text::RECORDING_INVALID)
}

/// Design §9.3 steps 1 to 9 on the retained descriptor: identity and size,
/// the MP4 structure, the dimensions of the admitted mode, the SHA-256 and
/// the identity again.
fn check_file(staging: &mut Staging, seconds: u32, want: (u32, u32)) -> Result<Checked, Fail> {
    let len = staging.file().metadata().map_err(|_| invalid())?.len();
    staging.recheck(len).map_err(|_| invalid())?;
    let info =
        mp4::validate(staging.file(), len, &Mp4Limits::for_seconds(seconds)).map_err(|e| {
            tracing::warn!(error = ?e, "the recording file is not valid");
            invalid()
        })?;
    if (info.width, info.height) != want || info.duration_ms > u64::from(seconds) * 1000 + 1000 {
        return Err(invalid());
    }
    let sha256 = staging.sha256().map_err(|_| invalid())?;
    staging.recheck(len).map_err(|_| invalid())?;
    Ok(Checked { len, sha256, info })
}

impl Run<'_> {
    /// What the exit means (design §7.2, §7.3, §7.5): the save reason, or
    /// the discard reason and whether the size reached the ceiling.
    fn judge_exit(&self, exit: &Exit) -> Result<StopReason, (StopReason, bool)> {
        if let Some(reason) = self.latch.first_discard() {
            return Err((reason, self.latch.ceiling()));
        }
        let by_signal = |xfsz: bool| {
            if xfsz {
                (StopReason::OutputLimit, true)
            } else {
                (StopReason::RecorderExited, false)
            }
        };
        match exit.cause {
            Some(StopCause::LeaseExpired | StopCause::Suspended) => {
                Err((StopReason::LeaseExpired, false))
            }
            Some(StopCause::Deadline) => Err((StopReason::Deadline, false)),
            Some(StopCause::Discard) | None => Err(by_signal(exit.xfsz)),
            Some(cause @ (StopCause::Duration | StopCause::OwnerStop)) => {
                let ended = exit.status.is_some_and(|s| s.success());
                if exit.escalated || !ended || exit.xfsz {
                    return Err(by_signal(exit.xfsz));
                }
                Ok(if cause == StopCause::Duration {
                    StopReason::Duration
                } else {
                    StopReason::OwnerStop
                })
            }
        }
    }

    /// A discard after the recorder ended: the reason goes to the latch and
    /// the row; the failure carries its owner terminal code.
    async fn discard_late(&self, reason: StopReason, ceiling: bool) -> Stage {
        if ceiling {
            self.latch.mark_ceiling();
        }
        let held = self.latch.stop(false, reason).await;
        let reason = held
            .filter(|(intent, _)| *intent == StopIntent::Discard)
            .map_or(reason, |(_, held)| held);
        Stage::discard(reason, self.latch.ceiling())
    }

    /// The operation deadline stops new work: a discard with `deadline`.
    async fn in_time(&self) -> Result<(), Stage> {
        if left(self.deadline).is_err() {
            return Err(self.discard_late(StopReason::Deadline, false).await);
        }
        Ok(())
    }

    /// The stage of a discard that was latched meanwhile.
    async fn check_latch(&self) -> Result<(), Stage> {
        match self.latch.first_discard() {
            Some(reason) => Err(self.discard_late(reason, self.latch.ceiling()).await),
            None => Ok(()),
        }
    }

    /// The live local gates, the lock and the monitor, read again (§7.6).
    async fn recheck_local(&self) -> Result<(), Stage> {
        let req = self.req;
        req.admitted
            .local_gates()
            .map_err(|f| Stage::new(f, StopReason::PolicyOff))?;
        check_gates(&(req.allowed)(), &req.monitor)
            .map_err(|f| Stage::new(f, StopReason::PolicyOff))?;
        let now = probe::snapshot(self.desktop, &req.monitor, req.params.timing.probe).await;
        match probe::verdict(self.base, &now) {
            Some(reason) => Err(self.discard_late(reason, false).await),
            None => Ok(()),
        }
    }

    /// Step 7 (see `op.rs`): from the end of the recorder to the commit.
    pub(super) async fn finalize(
        &self,
        mut staging: Staging,
        exit: Exit,
        tasks: Tasks,
        armed: Armed,
        start: &Start,
    ) -> Result<ArtifactPublished, Stage> {
        let req = self.req;
        let save = match self.judge_exit(&exit) {
            Ok(save) => save,
            Err((reason, ceiling)) => return Err(self.discard_late(reason, ceiling).await),
        };
        // The save is latched; a discard that the row already holds wins, and
        // the save reason of the row is the one that is stored.
        let save = match self.latch.stop(true, save).await {
            Some((StopIntent::Discard, reason)) => {
                return Err(self.discard_late(reason, false).await);
            }
            Some((_, held)) => held,
            None => save,
        };
        self.latch.mark_finalizing().await;
        let failed = |fail: Fail| Stage::new(fail, save);
        self.in_time().await?;
        // §9.3: the checks of the finished file, off the runtime threads.
        let (seconds, want) = (req.seconds, self.base.recorded_size());
        let (checked, back) = tokio::task::spawn_blocking(move || {
            let result = check_file(&mut staging, seconds, want);
            (result, staging)
        })
        .await
        .map_err(|e| failed(join_failed(&e)))?;
        staging = back;
        let checked = checked.map_err(failed)?;
        self.check_latch().await?;
        // §7.6: the fresh import lease (it waits for a renewal in flight),
        // then the local gates.
        self.in_time().await?;
        let answered = self.lease.request(LeasePhase::Import).await;
        match answered.verdict {
            Some(Verdict::Continue | Verdict::Save) => {
                // The import lease moves the expiry of the operation: the
                // recorder is gone, so only the guard holds it now.
                armed.guard().renew(self.lease.until(answered.started));
            }
            Some(Verdict::Discard(reason)) => return Err(self.discard_late(reason, false).await),
            None => return Err(self.discard_late(StopReason::LeaseDenied, false).await),
        }
        self.check_latch().await?;
        // A coordinator recording also asks for fresh authority in the
        // `import` phase (P5b §13.3, P6 §6.3); a refusal discards.
        if coordinated::before_import(req.coordinator.as_ref(), start, &self.ctx.env)
            .await
            .is_err()
        {
            return Err(self
                .discard_late(StopReason::CoordinatorAuthorityLost, false)
                .await);
        }
        self.recheck_local().await?;
        self.in_time().await?;
        // The import from the retained descriptor.
        let (root, id, fs, len, room) = (
            req.service.root().to_path_buf(),
            self.artifact_id.to_owned(),
            Arc::clone(&req.params.fs),
            checked.len,
            Arc::clone(&req.params.room),
        );
        let (imported, back) = tokio::task::spawn_blocking(move || {
            let result = import_capture(
                &root,
                &id,
                CaptureFileKind::Mp4,
                staging.file(),
                len,
                &*fs,
                &*room,
            );
            (result, staging)
        })
        .await
        .map_err(|e| failed(join_failed(&e)))?;
        imported.map_err(failed)?;
        drop(back);
        self.check_latch().await?;
        let published = self.commit(save, &checked, armed.guard()).await?;
        drop(armed);
        drop(tasks);
        // The staging goes durably; its bytes stay reserved until then.
        finish_staging(
            &req.service,
            &req.deck_fp,
            &req.capture_id,
            Arc::clone(&req.params.fs),
            true,
        )
        .await;
        Ok(published)
    }

    /// The commit of the artifact and the operation (design §5.3).
    async fn commit(
        &self,
        save: StopReason,
        checked: &Checked,
        lease: &LeaseGuard,
    ) -> Result<ArtifactPublished, Stage> {
        let req = self.req;
        let title: String = format!(
            "Desktop recording \u{b7} {}/{}",
            req.station_name, req.monitor
        )
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect();
        let (caller, caller_station, caller_session, caller_grant_id) =
            coordinated::caller(req.coordinator.as_ref());
        let new = NewCaptureArtifact {
            artifact_id: self.artifact_id.to_owned(),
            title,
            size: checked.len,
            sha256: checked.sha256.clone(),
            provenance: CaptureProvenance {
                capture_id: req.capture_id.clone(),
                station_id: req.station_id.clone(),
                monitor: req.monitor.clone(),
                kind: CaptureKind::Recording,
                caller,
                caller_station,
                caller_session,
                caller_grant_id,
                at: self.at,
                recording: Some(RecordingMeta {
                    requested_seconds: req.seconds,
                    duration_ms: checked.info.duration_ms,
                    width: checked.info.width,
                    height: checked.info.height,
                    stop_reason: save,
                }),
            },
        };
        let publish_failed = || {
            Fail::new(
                code::RECORDING_PUBLISH_FAILED,
                text::RECORDING_PUBLISH_FAILED,
            )
        };
        // The last check before the transaction, with no await between: a
        // lease that ran out (or a suspend) discards instead of committing.
        if !lease.valid() {
            return Err(self.discard_late(StopReason::LeaseExpired, false).await);
        }
        match self
            .store
            .recording_commit(&req.deck_fp, &req.capture_id, new, checked.len)
            .await
        {
            Ok(c) => Ok(ArtifactPublished {
                artifact: c.artifact,
                version: c.version,
                created: c.created,
            }),
            Err(CommitError::UnknownArtifact(_)) => {
                Err(self.discard_late(StopReason::ArtifactDeleted, false).await)
            }
            Err(e) => {
                tracing::warn!(error = %e, "the recording commit failed");
                // The commit may have happened: read the row again.
                match self
                    .store
                    .capture_lookup(&req.deck_fp, &req.capture_id)
                    .await
                {
                    Ok(Lookup::Committed {
                        artifact, version, ..
                    }) => Ok(ArtifactPublished {
                        artifact,
                        version,
                        created: true,
                    }),
                    Ok(_) => match self.latch.first_discard() {
                        // A discard latched before the commit refuses it.
                        Some(reason) => Err(Stage::discard(reason, self.latch.ceiling())),
                        None => Err(Stage::new(publish_failed(), save)),
                    },
                    Err(e) => {
                        tracing::warn!(error = %e, "the recording row cannot be read");
                        Err(Stage::unknown(publish_failed(), save))
                    }
                }
            }
        }
    }
}
