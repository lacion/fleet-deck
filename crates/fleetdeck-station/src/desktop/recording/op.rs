//! The recording operation of the station (R13 P5b task S1, design
//! `omarchy-p5b.md` §3.2, §5.3, §6.1, §7, §10): `desktop.act` with
//! `screen_record`, from the slot to the answer.
//!
//! The steps, in this order, all inside the `seconds + 30` operation
//! deadline:
//!
//! 1. the seconds, the startup recovery, the one capture slot (`busy`);
//! 2. the gates and the admission, 5 s in total: a fresh context, the
//!    monitor, its mode, the lock and the recorder tool;
//! 3. the free space, then the operation row, the quota and the
//!    reservation;
//! 4. the staging file, then the `start` lease;
//! 5. the stop check, the gates and the start check of the authority,
//!    immediately before the spawn;
//! 6. the spawn of the owned recorder with the retained descriptor, and the
//!    watchdog and the lease renewals;
//! 7. the end of the recorder, then [`Run::finalize`] (validation, the
//!    `import` lease, the import and the commit), or the discard.
//!
//! The operation runs in a task of its own: dropping the waiter never
//! abandons the row, the recorder or the cleanup.

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_proto::artifacts::ArtifactPublished;
use fleetdeck_proto::capture::{CaptureResult, code, text};
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction, DesktopInfo, MonitorInfo};
use fleetdeck_proto::recording::{
    LeasePhase, PEAK_RESERVATION, RECORDING_FILE, StopReason, valid_dimensions, valid_seconds,
};
use rustix::fs::{CWD, Mode, OFlags};
use tokio::task::JoinHandle;

use super::guard::LeaseGuard;
use super::latch::Latch;
use super::lease::{Lease, Verdict};
use super::probe::{self, Snapshot};
use super::supervisor::Watch;
use super::{Holder, RecordRequest, check_gates, fail_for, fail_for_coordinator};
use crate::desktop::capture::{Staging, cleanup_failed};
use crate::desktop::run::resolve;
use crate::desktop::{Ctx, Desktop, Fail, args, authority, code as desktop_code, deadline, quiet};
use crate::media::recorder::{Deadlines, spawn_with};
use crate::store::Store;
use crate::store::capture_ops::{Begin, CaptureOp, Lookup, RecordingBegin, StopIntent};

/// The text of `failed` when the start-up check of the captures failed.
const NOT_RECOVERED: &str = "The station has not finished its start-up check of captures.";
/// The text of `failed` for a failure of the station store.
const STORE_FAILED: &str = "The station could not record the recording.";
/// The text of `failed` for an operation task that ended abnormally.
const TASK_FAILED: &str = "The recording did not finish.";
/// The text of `not_found` for a deleted recording.
const DELETED: &str = "The recording was deleted.";

/// A failure of the steps after the row exists. `cleanup` is false when the
/// outcome is not known: the files and the `running` row then stay for the
/// startup recovery. `reason` is the stop reason that the row stores.
pub(super) struct Stage {
    pub fail: Fail,
    pub cleanup: bool,
    pub reason: StopReason,
    /// `Some(ceiling)` when `fail` is the terminal code of `reason` (an
    /// automatic stop); a coordinator recording then replaces it with the
    /// coordinator code ([`Stage::for_coordinator`]).
    pub derived: Option<bool>,
}

impl Stage {
    pub(super) fn new(fail: Fail, reason: StopReason) -> Self {
        Self {
            fail,
            cleanup: true,
            reason,
            derived: None,
        }
    }

    /// An automatic stop: the owner terminal code of the reason.
    pub(super) fn discard(reason: StopReason, ceiling: bool) -> Self {
        Self {
            derived: Some(ceiling),
            ..Self::new(fail_for(reason, ceiling), reason)
        }
    }

    /// The operation deadline before the recorder: an owner keeps `fail`; a
    /// coordinator gets the coordinator code of `deadline`.
    pub(super) fn deadline(fail: Fail) -> Self {
        Self {
            derived: Some(false),
            ..Self::new(fail, StopReason::Deadline)
        }
    }

    /// The stage of a coordinator recording: an automatic stop gets the
    /// coordinator terminal code of its reason (§7.5); every other failure
    /// keeps its own code.
    pub(super) fn for_coordinator(self) -> Self {
        match self.derived {
            Some(ceiling) => Self {
                fail: fail_for_coordinator(self.reason, ceiling),
                ..self
            },
            None => self,
        }
    }

    /// A stop before the recorder started.
    pub(super) fn cancelled(reason: StopReason) -> Self {
        Self::new(
            Fail::new(code::RECORDING_CANCELLED, text::RECORDING_CANCELLED),
            reason,
        )
    }

    /// The outcome is not known: nothing is cleaned.
    pub(super) fn unknown(fail: Fail, reason: StopReason) -> Self {
        Self {
            fail,
            cleanup: false,
            reason,
            derived: None,
        }
    }
}

pub(super) fn timeout_fail() -> Fail {
    Fail::new(desktop_code::FAILED, deadline::ACT_TIMEOUT)
}

pub(super) fn internal(e: impl std::fmt::Display) -> Fail {
    tracing::warn!(error = %e, "the recording store failed");
    Fail::new(desktop_code::FAILED, STORE_FAILED)
}

pub(super) fn join_failed(e: &tokio::task::JoinError) -> Fail {
    tracing::warn!(error = %e, "a recording task ended abnormally");
    Fail::new(desktop_code::FAILED, TASK_FAILED)
}

pub(super) fn recording_failed() -> Fail {
    Fail::new(code::RECORDING_FAILED, text::RECORDING_FAILED)
}

fn not_found() -> Fail {
    let existing = args::monitor("", &[]).err();
    Fail::new(
        desktop_code::NOT_FOUND,
        existing.map_or_else(String::new, |e| e.message),
    )
}

/// The wire code of a stored code (every code that the operation stores).
pub(super) fn static_code(stored: Option<&str>) -> &'static str {
    const KNOWN: [&str; 22] = [
        code::BUSY,
        code::DISK_FULL,
        code::CAPTURE_QUOTA,
        code::SCREEN_LOCKED,
        code::CAPTURE_ID_CONFLICT,
        code::RECORDING_DIMENSIONS,
        code::RECORDING_TOO_LARGE,
        code::RECORDING_FAILED,
        code::RECORDING_INVALID,
        code::RECORDING_PUBLISH_FAILED,
        code::RECORDING_CANCELLED,
        code::RECORDING_INTERRUPTED,
        desktop_code::UNSUPPORTED,
        desktop_code::FORBIDDEN,
        desktop_code::INVALID_PARAMS,
        desktop_code::NOT_FOUND,
        desktop_code::CONFLICT,
        desktop_code::FAILED,
        desktop_code::AUTHORITY_LOST,
        desktop_code::AUTHORITY_UNKNOWN,
        "desktop_not_found",
        // The coordinator terminal code of the deadline (§7.5).
        "desktop_read_timeout",
    ];
    match stored.and_then(|s| KNOWN.iter().find(|k| **k == s)) {
        Some(known) => known,
        None if stored == Some("desktop_timeout") => "desktop_timeout",
        None => {
            tracing::warn!("a stored recording code is not known");
            desktop_code::FAILED
        }
    }
}

fn monitor_of<'a>(info: &'a DesktopInfo, name: &str) -> Result<&'a MonitorInfo, Fail> {
    info.monitors
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(not_found)
}

fn locked_fail() -> Fail {
    Fail::new(code::SCREEN_LOCKED, text::SCREEN_LOCKED)
}

/// Opens `<data>/captures` without following a link; makes it (0700) when it
/// is missing.
fn open_captures(path: &Path) -> Result<OwnedFd, Fail> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match rustix::fs::openat(CWD, path, flags, Mode::empty()) {
        Ok(fd) => Ok(fd),
        Err(e) if e == rustix::io::Errno::NOENT => {
            match rustix::fs::mkdirat(CWD, path, Mode::from_raw_mode(0o700)) {
                Ok(()) => {}
                Err(e) if e == rustix::io::Errno::EXIST => {}
                Err(_) => return Err(recording_failed()),
            }
            rustix::fs::openat(CWD, path, flags, Mode::empty()).map_err(|_| recording_failed())
        }
        Err(_) => Err(recording_failed()),
    }
}

/// The time left until `deadline`.
pub(super) fn left(deadline: Instant) -> Result<Duration, Fail> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(timeout_fail)
}

/// Runs `fut` for at most the time left until `deadline`.
pub(super) async fn within<T>(
    deadline: Instant,
    fut: impl std::future::Future<Output = Result<T, Fail>>,
) -> Result<T, Fail> {
    let limit = left(deadline)?;
    tokio::time::timeout(limit, fut)
        .await
        .map_err(|_| timeout_fail())?
}

/// The tasks of a running recording; they end when the guard is dropped.
pub(super) struct Tasks {
    pub watch: JoinHandle<()>,
    pub renewals: JoinHandle<()>,
}

impl Drop for Tasks {
    fn drop(&mut self) {
        self.watch.abort();
        self.renewals.abort();
    }
}

/// What the steps after the row share.
pub(super) struct Run<'a> {
    pub desktop: &'a Arc<Desktop>,
    pub req: &'a RecordRequest,
    pub ctx: &'a Ctx,
    pub store: &'a Store,
    pub latch: &'a Arc<Latch>,
    pub lease: &'a Arc<Lease>,
    pub base: &'a Snapshot,
    pub deadline: Instant,
    /// Station Unix ms of the intake.
    pub at: i64,
    pub artifact_id: &'a str,
}

impl Desktop {
    /// `desktop.act` with `screen_record` (see the module text). The work
    /// runs in a task of its own, so a dropped caller does not abandon it.
    ///
    /// # Errors
    /// The codes of the module text, the terminal codes of §7.5, and
    /// `failed`.
    pub(crate) async fn screen_record(
        self: &Arc<Self>,
        req: RecordRequest,
    ) -> Result<DesktopActResult, Fail> {
        let desktop = Arc::clone(self);
        let task = tokio::spawn(async move { desktop.record_op(req).await });
        let result = match task.await {
            Ok(result) => result,
            Err(e) => Err(join_failed(&e)),
        };
        match &result {
            Ok(_) => tracing::info!(action = "screen_record", result = "ok", "desktop action"),
            Err(e) => tracing::info!(action = "screen_record", result = e.code, "desktop action"),
        }
        result
    }

    #[allow(clippy::too_many_lines, reason = "the numbered steps of one operation")]
    async fn record_op(self: Arc<Self>, req: RecordRequest) -> Result<DesktopActResult, Fail> {
        let deadline =
            Instant::now() + Duration::from_secs(u64::from(req.seconds)) + req.params.timing.slack;
        let at = quiet::now_ms();
        // 1
        if !valid_seconds(u64::from(req.seconds)) {
            return Err(Fail::new(
                desktop_code::INVALID_PARAMS,
                "`seconds` must be from 1 to 120",
            ));
        }
        within(deadline, async {
            req.service.ensure_recovered().await.map_err(|e| {
                tracing::warn!(error = %e, "the capture recovery is not done");
                Fail::new(desktop_code::FAILED, NOT_RECOVERED)
            })
        })
        .await?;
        let _slot = self.capture.try_take_for(Holder::Recording {
            deck_fp: req.deck_fp.clone(),
            capture_id: req.capture_id.clone(),
        })?;
        // 2
        req.admitted.local_gates()?;
        check_gates(&(req.allowed)(), &req.monitor)?;
        let allowed = Arc::clone(&req.allowed);
        let gate = move || allowed().desktop;
        let admission = req.params.admission;
        let (ctx, info, base, program) = within(deadline, async {
            tokio::time::timeout(admission, async {
                let ctx = self.context().await?;
                let info = self.fresh_info(&ctx, &gate, true).await?;
                let mode = monitor_of(&info, &req.monitor)?;
                if info.locked != Some(false) {
                    return Err(locked_fail());
                }
                if !valid_dimensions(mode.width, mode.height) {
                    return Err(Fail::new(
                        code::RECORDING_DIMENSIONS,
                        text::RECORDING_DIMENSIONS,
                    ));
                }
                let program = resolve(&ctx.env, "wf-recorder")
                    .ok_or_else(|| Fail::new(desktop_code::UNSUPPORTED, text::NO_RECORDER))?;
                let base = probe::snapshot_in(&ctx, &req.monitor).await?;
                if base.locked != Some(false) {
                    return Err(locked_fail());
                }
                Ok::<_, Fail>((ctx, info, base, program))
            })
            .await
            .map_err(|_| timeout_fail())?
        })
        .await?;
        // 3
        let captures = req.service.captures_dir();
        let dir = open_captures(&captures)?;
        (req.params.room)(&dir, PEAK_RESERVATION)?;
        drop(dir);
        let store = req.service.store().clone();
        let artifact_id = store.capture_artifact_id();
        let begin = store
            .recording_begin(RecordingBegin {
                deck_fp: &req.deck_fp,
                capture_id: &req.capture_id,
                action_hash: &req.action_hash,
                monitor: &req.monitor,
                artifact_id: &artifact_id,
                seconds: req.seconds,
                link_epoch: &req.link_epoch,
                coordinator: req.coordinator.as_ref(),
                now: store.now_ms(),
            })
            .await
            .map_err(internal)?;
        match begin {
            Begin::Conflict => {
                return Err(Fail::new(
                    code::CAPTURE_ID_CONFLICT,
                    text::CAPTURE_ID_CONFLICT,
                ));
            }
            Begin::Quota => return Err(Fail::new(code::CAPTURE_QUOTA, text::CAPTURE_QUOTA)),
            Begin::Existing(row) => {
                return self
                    .record_answer_existing(&req, &ctx, &gate, info, row, deadline)
                    .await;
            }
            Begin::Started => {}
        }
        // The registry entry, with the memory latch that the launch guard
        // reads, exists before the last store read of the stop (step 5): a
        // `capture_stop` either finds the entry or wrote its store latch
        // before that read.
        let latch = Latch::new(store.clone(), &req.deck_fp, &req.capture_id);
        #[cfg(test)]
        self.recordings.hooks.wait_before_insert().await;
        let _live = self.recordings.insert(&latch);
        #[cfg(test)]
        if let Some(gate) = self.recordings.hooks.phase() {
            latch.hold_phase_write(gate);
        }
        let lease = Lease::new(
            Arc::clone(&req.link),
            &req.capture_id,
            &req.action_hash,
            Arc::clone(&req.params.clocks),
            req.params.timing,
        );
        let run = Run {
            desktop: &self,
            req: &req,
            ctx: &ctx,
            store: &store,
            latch: &latch,
            lease: &lease,
            base: &base,
            deadline,
            at,
            artifact_id: &artifact_id,
        };
        match run.stages(&program).await {
            Ok(published) => Ok(self
                .record_answer(&req, &ctx, &gate, info, published, deadline)
                .await),
            Err(stage) => {
                // A coordinator recording ends with the coordinator codes
                // of §7.5; the row stores the same code.
                let stage = if req.coordinator.is_some() {
                    stage.for_coordinator()
                } else {
                    stage
                };
                if stage.cleanup {
                    run.fail(&stage).await;
                }
                Err(stage.fail)
            }
        }
    }

    /// The answer: the fresh info, or the last valid one with `info_fresh:
    /// false` when the refresh fails (design §4.1).
    async fn record_answer(
        &self,
        req: &RecordRequest,
        ctx: &Ctx,
        gate: &(dyn Fn() -> bool + Send + Sync),
        last: DesktopInfo,
        published: ArtifactPublished,
        deadline: Instant,
    ) -> DesktopActResult {
        let refreshed = within(deadline, self.fresh_info(ctx, gate, true)).await;
        let (info, info_fresh) = match refreshed {
            Ok(info) => (info, None),
            Err(e) => {
                tracing::warn!(
                    code = e.code,
                    "the desktop info refresh after a recording failed"
                );
                (last, Some(false))
            }
        };
        DesktopActResult {
            address: None,
            window_id: None,
            info,
            capture: Some(CaptureResult {
                capture_id: req.capture_id.clone(),
                published,
            }),
            info_fresh,
        }
    }

    /// A repeated request: the answer comes from the row, nothing runs.
    async fn record_answer_existing(
        &self,
        req: &RecordRequest,
        ctx: &Ctx,
        gate: &(dyn Fn() -> bool + Send + Sync),
        info: DesktopInfo,
        row: CaptureOp,
        deadline: Instant,
    ) -> Result<DesktopActResult, Fail> {
        tracing::debug!(state = ?row.state, "a repeated recording request");
        let lookup = req
            .service
            .store()
            .capture_lookup(&req.deck_fp, &req.capture_id)
            .await
            .map_err(internal)?;
        match lookup {
            Lookup::Unknown => Err(Fail::new(desktop_code::FAILED, STORE_FAILED)),
            Lookup::Running(_) => Err(Fail::new(code::BUSY, text::BUSY)),
            Lookup::Failed(op) => Err(Fail::new(
                static_code(op.code.as_deref()),
                op.message.unwrap_or_default(),
            )),
            Lookup::Deleted(_) => Err(Fail::new(desktop_code::NOT_FOUND, DELETED)),
            Lookup::Committed {
                artifact, version, ..
            } => {
                let published = ArtifactPublished {
                    artifact,
                    version,
                    created: true,
                };
                Ok(self
                    .record_answer(req, ctx, gate, info, published, deadline)
                    .await)
            }
        }
    }
}

impl Run<'_> {
    /// Steps 4 to 7.
    async fn stages(&self, program: &Path) -> Result<ArtifactPublished, Stage> {
        let req = self.req;
        let failed = |fail: Fail| Stage::new(fail, StopReason::RecorderExited);
        // 4
        let captures = req.service.captures_dir();
        let mut staging =
            Staging::create(&captures, &req.capture_id, RECORDING_FILE).map_err(|e| {
                tracing::warn!(code = e.code, "cannot make the recording staging file");
                failed(recording_failed())
            })?;
        let watch_file = staging.file().try_clone().map_err(|e| {
            tracing::warn!(kind = ?e.kind(), "cannot duplicate the recording file");
            failed(recording_failed())
        })?;
        let watch_dir = staging.dir().try_clone().map_err(|e| {
            tracing::warn!(kind = ?e.kind(), "cannot duplicate the recording folder");
            failed(recording_failed())
        })?;
        let first = self.lease.request(LeasePhase::Start).await;
        let until = match first.verdict {
            Some(Verdict::Continue) => self.lease.until(first.started),
            Some(Verdict::Save) => return Err(Stage::cancelled(StopReason::OwnerStop)),
            Some(Verdict::Discard(reason)) => return Err(Stage::discard(reason, false)),
            None => return Err(Stage::discard(StopReason::LeaseDenied, false)),
        };
        // From the first lease to the commit the operation keeps its own
        // expiry; it outlives the recorder thread (§6.4).
        let armed = LeaseGuard::new(
            until,
            Arc::clone(&req.params.clocks),
            Arc::clone(self.latch),
        )
        .arm();
        // 5
        self.check_not_stopped().await?;
        left(self.deadline).map_err(Stage::deadline)?;
        req.admitted
            .local_gates()
            .map_err(|f| Stage::new(f, StopReason::PolicyOff))?;
        check_gates(&(req.allowed)(), &req.monitor)
            .map_err(|f| Stage::new(f, StopReason::PolicyOff))?;
        let action = DesktopAction::ScreenRecord {
            monitor: req.monitor.clone(),
            seconds: req.seconds,
        };
        let start = Arc::new(authority::Start::new(req.admitted.clone(), &action));
        // A coordinator recording shows its fixed disclosure first (P5b
        // §13.3); a failure refuses the recording and nothing is spawned.
        within(self.deadline, self.disclose())
            .await
            .map_err(failed)?;
        within(self.deadline, start.begin(&self.ctx.env))
            .await
            .map_err(|f| Stage::new(f, StopReason::LeaseDenied))?;
        // 6: the last checks run on the thread of the recorder, right before
        // the child starts (the stop may arrive while the launch is queued).
        let (guard, refusal) = self.launch_guard(Arc::clone(&start), Arc::clone(armed.guard()));
        let deadlines = Deadlines {
            duration: Duration::from_secs(u64::from(req.seconds)),
            total: self.deadline.saturating_duration_since(Instant::now()),
            lease_until: until,
            grace: req.params.timing.grace,
        };
        let (launch, env, monitor, clocks) = (
            req.params.launch.clone(),
            self.ctx.env.clone(),
            req.monitor.clone(),
            Arc::clone(&req.params.clocks),
        );
        let (program, seconds) = (PathBuf::from(program), req.seconds);
        let (spawned, back) = tokio::task::spawn_blocking(move || {
            let result = spawn_with(
                &launch,
                &env,
                &program,
                &monitor,
                seconds,
                staging.file(),
                deadlines,
                clocks,
                guard,
            );
            (result, staging)
        })
        .await
        .map_err(|e| failed(join_failed(&e)))?;
        staging = back;
        let recorder = spawned.map_err(|e| {
            tracing::warn!(code = e.code, "cannot start the recorder");
            failed(recording_failed())
        })?;
        // A guard that refused started no process: the stage says why.
        if let Some(stage) = refusal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            return Err(stage);
        }
        let ctl = recorder.ctl();
        self.latch.attach(ctl.clone());
        // The supervision starts before any bookkeeping await (§7.4): a
        // store that does not answer never delays a trigger.
        let tasks = Tasks {
            watch: Watch {
                desktop: Arc::clone(self.desktop),
                latch: Arc::clone(self.latch),
                allowed: Arc::clone(&req.allowed),
                monitor: req.monitor.clone(),
                base: self.base.clone(),
                file: watch_file,
                dir: watch_dir,
                room: Arc::clone(&req.params.room),
                link: Arc::clone(&req.link),
                timing: req.params.timing,
                seconds: req.seconds,
                started: Instant::now(),
            }
            .spawn(),
            renewals: self.lease.spawn_renewals(
                ctl,
                Arc::clone(self.latch),
                Arc::clone(armed.guard()),
            ),
        };
        // 7: the phase write runs beside the wait for the recorder; its
        // failure is logged and stops nothing.
        let ((), exit) = tokio::join!(
            self.latch.mark_recording(self.store.now_ms()),
            recorder.exited()
        );
        self.finalize(staging, exit, tasks, armed, &start).await
    }

    /// A stop that was asked for before the recorder started: the operation
    /// ends without a recorder.
    async fn check_not_stopped(&self) -> Result<(), Stage> {
        if let Some(reason) = self.latch.first_discard() {
            return Err(Stage::cancelled(reason));
        }
        let row = self
            .store
            .recording_get(&self.req.deck_fp, &self.req.capture_id)
            .await
            .map_err(|e| Stage::new(internal(e), StopReason::RecorderExited))?;
        match row {
            Some(row) if row.stop_intent != StopIntent::None => Err(Stage::cancelled(
                row.stop_reason.unwrap_or(StopReason::OwnerStop),
            )),
            _ => Ok(()),
        }
    }

    /// A failed operation: the row becomes `failed` with the code, the text
    /// and the reason, then the staging and the uncommitted destination go
    /// durably and the reservation is freed.
    async fn fail(&self, stage: &Stage) {
        let req = self.req;
        if let Err(e) = self
            .store
            .recording_fail(
                &req.deck_fp,
                &req.capture_id,
                stage.fail.code,
                &stage.fail.message,
                stage.reason,
            )
            .await
        {
            tracing::warn!(error = %e, "cannot record the failed recording");
            return;
        }
        cleanup_failed(
            &req.service,
            &req.deck_fp,
            &req.capture_id,
            self.artifact_id,
            &stage.fail,
            Arc::clone(&req.params.fs),
        )
        .await;
    }
}
