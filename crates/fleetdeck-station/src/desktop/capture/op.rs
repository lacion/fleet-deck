//! The screenshot operation of the station (R13 P5a task D2, design
//! `omarchy-p5a.md` §2.2, §4, §5, §6, §7.1): `desktop.act` with
//! `screen_capture`, from the slot to the answer.
//!
//! The steps, in this order, all inside the 30 s operation deadline:
//!
//! 1. wait for the startup recovery;
//! 2. take the one capture slot (`busy`);
//! 3. read the gates live (`forbidden`);
//! 4. admission, 5 s in total: a fresh desktop context and info, the monitor,
//!    its size and the lock (`not_found`, `capture_dimensions`,
//!    `screen_locked`);
//! 5. the free space (`disk_full`);
//! 6. the operation row (`capture_id_conflict`, a repeated request, a
//!    `capture_quota`);
//! 7. the staging file;
//! 8. the gates and the start check of the authority, right before `grim`;
//! 9. `grim`, with the byte cap, the deadline and a free-space check per
//!    write;
//! 10. the PNG check, a fresh lock and monitor check, the hash, the recheck;
//! 11. the import into `<artifacts>/<artifact_id>/1/`;
//! 12. the commit of the artifact and the operation, then the staging goes;
//! 13. the answer.
//!
//! The operation runs in a task of its own: dropping the waiter never
//! abandons the row. The deadline stops new work between the steps; a sync
//! or a database commit that has started may finish after it.

use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetdeck_proto::artifacts::{ArtifactPublished, MAX_TITLE_CHARS};
use fleetdeck_proto::capture::{CaptureKind, CaptureProvenance, CaptureResult, code, text};
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction, DesktopInfo, MonitorInfo};
use rustix::fs::{CWD, Mode, OFlags};

use super::{
    Launch, MAX_PNG_BYTES, Staging, capture_png, check_monitor, check_png, cleanup_failed,
    coordinator, finish_staging, room,
};
use crate::artifacts::ArtifactService;
use crate::artifacts::capture_import::{DurableFs, import_png};
use crate::desktop::{Ctx, Desktop, Fail, args, authority, code as desktop_code, deadline, quiet};
use crate::store::capture_ops::{Begin, CAPTURE_FILE, CaptureOp, Lookup, NewCaptureArtifact};

/// The time limit of the admission (step 4).
pub const ADMISSION: Duration = Duration::from_secs(5);

/// The text of `forbidden` when screenshots are off.
pub(super) const OFF: &str = "Desktop screenshots are off on this station.";
/// The text of `forbidden` for a monitor outside the allowlist.
const MONITOR_NOT_ALLOWED: &str = "This monitor is not allowed for screenshots.";
/// The text of `not_found` for a deleted screenshot.
const DELETED: &str = "The screenshot was deleted.";
/// The text of `failed` when the start-up check of the captures failed.
const NOT_RECOVERED: &str = "The station has not finished its start-up check of screenshots.";
/// The text of `failed` for a failure of the station store.
const STORE_FAILED: &str = "The station could not record the screenshot.";
/// The text of `failed` for an operation task that ended abnormally.
const TASK_FAILED: &str = "The screenshot did not finish.";

/// The live gates of the station policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowed {
    /// `allow_desktop`.
    pub desktop: bool,
    /// `allow_desktop_capture`.
    pub capture: bool,
    /// `capture_monitors`; empty means every monitor.
    pub monitors: Vec<String>,
}

/// What a test replaces: the launch of `grim`, the syncs and the admission
/// limit.
pub struct Params {
    pub launch: Launch,
    pub fs: Arc<dyn DurableFs>,
    pub admission: Duration,
}

/// One screenshot request.
pub struct Request {
    pub service: ArtifactService,
    /// The live gates; it is read at step 3 and right before `grim`.
    pub allowed: Arc<dyn Fn() -> Allowed + Send + Sync>,
    pub admitted: authority::Admitted,
    /// The authenticated deck fingerprint (64 hex).
    pub deck_fp: String,
    pub capture_id: String,
    pub action_hash: String,
    pub monitor: String,
    /// The station's own id (the deck replaces it with its stable id).
    pub station_id: String,
    pub station_name: String,
    pub params: Params,
}

/// A failure of the steps after the row exists. `cleanup` is false when the
/// outcome is not known: the files and the `running` row then stay for the
/// startup recovery.
struct Stage {
    fail: Fail,
    cleanup: bool,
}

impl From<Fail> for Stage {
    fn from(fail: Fail) -> Self {
        Self {
            fail,
            cleanup: true,
        }
    }
}

fn timeout_fail() -> Fail {
    Fail::new(desktop_code::FAILED, deadline::ACT_TIMEOUT)
}

fn internal(e: impl std::fmt::Display) -> Fail {
    tracing::warn!(error = %e, "the capture store failed");
    Fail::new(desktop_code::FAILED, STORE_FAILED)
}

fn join_failed(e: &tokio::task::JoinError) -> Fail {
    tracing::warn!(error = %e, "a screenshot task ended abnormally");
    Fail::new(desktop_code::FAILED, TASK_FAILED)
}

fn capture_failed() -> Fail {
    Fail::new(code::CAPTURE_FAILED, text::CAPTURE_FAILED)
}

/// The failure for a monitor that does not exist: the existing text of the
/// desktop arguments with the code `not_found`.
fn not_found() -> Fail {
    let existing = args::monitor("", &[]).err();
    Fail::new(
        desktop_code::NOT_FOUND,
        existing.map_or_else(String::new, |e| e.message),
    )
}

/// The wire code of a stored code (every code that the operation stores).
fn static_code(stored: Option<&str>) -> &'static str {
    const KNOWN: [&str; 18] = [
        code::BUSY,
        code::DISK_FULL,
        code::CAPTURE_QUOTA,
        code::CAPTURE_TOO_LARGE,
        code::CAPTURE_DIMENSIONS,
        code::CAPTURE_FAILED,
        code::CAPTURE_PUBLISH_FAILED,
        code::SCREEN_LOCKED,
        code::CAPTURE_ID_CONFLICT,
        code::CAPTURE_INTERRUPTED,
        desktop_code::UNSUPPORTED,
        desktop_code::FORBIDDEN,
        desktop_code::INVALID_PARAMS,
        desktop_code::NOT_FOUND,
        desktop_code::CONFLICT,
        desktop_code::FAILED,
        desktop_code::AUTHORITY_LOST,
        desktop_code::AUTHORITY_UNKNOWN,
    ];
    match stored.and_then(|s| KNOWN.iter().find(|k| **k == s)) {
        Some(known) => known,
        None => {
            tracing::warn!("a stored capture code is not known");
            desktop_code::FAILED
        }
    }
}

pub(super) fn check_gates(allowed: &Allowed, monitor: &str) -> Result<(), Fail> {
    if !(allowed.desktop && allowed.capture) {
        return Err(Fail::new(desktop_code::FORBIDDEN, OFF));
    }
    if !allowed.monitors.is_empty() && !allowed.monitors.iter().any(|m| m == monitor) {
        return Err(Fail::new(desktop_code::FORBIDDEN, MONITOR_NOT_ALLOWED));
    }
    Ok(())
}

fn monitor_of<'a>(info: &'a DesktopInfo, name: &str) -> Result<&'a MonitorInfo, Fail> {
    info.monitors
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(not_found)
}

fn check_unlocked(info: &DesktopInfo) -> Result<(), Fail> {
    if info.locked == Some(false) {
        Ok(())
    } else {
        Err(Fail::new(code::SCREEN_LOCKED, text::SCREEN_LOCKED))
    }
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
                Err(_) => return Err(capture_failed()),
            }
            rustix::fs::openat(CWD, path, flags, Mode::empty()).map_err(|_| capture_failed())
        }
        Err(_) => Err(capture_failed()),
    }
}

impl Desktop {
    /// `desktop.act` with `screen_capture` (see the module text). The work
    /// runs in a task of its own, so a dropped caller does not abandon it.
    ///
    /// # Errors
    /// The codes of the module text, the codes of `capture_png` and
    /// `check_png`, `capture_publish_failed`, and `failed`.
    pub(crate) async fn screen_capture(
        self: &Arc<Self>,
        req: Request,
    ) -> Result<DesktopActResult, Fail> {
        let desktop = Arc::clone(self);
        let task = tokio::spawn(async move { desktop.capture_op(req).await });
        let result = match task.await {
            Ok(result) => result,
            Err(e) => Err(join_failed(&e)),
        };
        match &result {
            Ok(_) => tracing::info!(action = "screen_capture", result = "ok", "desktop action"),
            Err(e) => tracing::info!(action = "screen_capture", result = e.code, "desktop action"),
        }
        result
    }

    async fn capture_op(&self, req: Request) -> Result<DesktopActResult, Fail> {
        let deadline = Instant::now() + self.limits().short;
        // 1
        within(deadline, async {
            req.service.ensure_recovered().await.map_err(|e| {
                tracing::warn!(error = %e, "the capture recovery is not done");
                Fail::new(desktop_code::FAILED, NOT_RECOVERED)
            })
        })
        .await?;
        // 2
        let _slot = self.capture.try_take()?;
        // 3
        req.admitted.local_gates()?;
        check_gates(&(req.allowed)(), &req.monitor)?;
        let allowed = Arc::clone(&req.allowed);
        let gate = move || allowed().desktop;
        // 4
        let at = quiet::now_ms();
        let admission = req.params.admission;
        let (ctx, info) = within(deadline, async {
            tokio::time::timeout(admission, async {
                let ctx = self.context().await?;
                let info = self.fresh_info(&ctx, &gate, true).await?;
                check_monitor(monitor_of(&info, &req.monitor)?)?;
                check_unlocked(&info)?;
                Ok::<_, Fail>((ctx, info))
            })
            .await
            .map_err(|_| timeout_fail())?
        })
        .await?;
        // 5
        let captures = req.service.captures_dir();
        let dir = open_captures(&captures)?;
        room(&dir, MAX_PNG_BYTES)?;
        drop(dir);
        // 6
        let store = req.service.store();
        let artifact_id = store.capture_artifact_id();
        let begin = store
            .capture_begin_with(
                &req.deck_fp,
                &req.capture_id,
                &req.action_hash,
                &req.monitor,
                &artifact_id,
                req.admitted.coordinator(),
                store.now_ms(),
            )
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
                    .answer_existing(&req, &ctx, &gate, info, row, deadline)
                    .await;
            }
            Begin::Started => {}
        }
        let mut last = info;
        match self
            .stages(&req, &ctx, &gate, &mut last, at, deadline, &artifact_id)
            .await
        {
            Ok(published) => Ok(self
                .answer(&req, &ctx, &gate, last, published, deadline)
                .await),
            Err(stage) => {
                if stage.cleanup {
                    cleanup_failed(
                        &req.service,
                        &req.deck_fp,
                        &req.capture_id,
                        &artifact_id,
                        &stage.fail,
                        Arc::clone(&req.params.fs),
                    )
                    .await;
                }
                Err(stage.fail)
            }
        }
    }

    /// Steps 7 to 12.
    #[allow(clippy::too_many_arguments, reason = "one stage of one operation")]
    async fn stages(
        &self,
        req: &Request,
        ctx: &Ctx,
        gate: &(dyn Fn() -> bool + Send + Sync),
        last: &mut DesktopInfo,
        at: i64,
        deadline: Instant,
        artifact_id: &str,
    ) -> Result<ArtifactPublished, Stage> {
        // 7
        let captures = req.service.captures_dir();
        let mut staging = Staging::create(&captures, &req.capture_id, CAPTURE_FILE)?;
        // 8
        left(deadline)?;
        req.admitted.local_gates()?;
        check_gates(&(req.allowed)(), &req.monitor)?;
        let action = DesktopAction::ScreenCapture {
            monitor: req.monitor.clone(),
        };
        let start = authority::Start::new(req.admitted.clone(), &action);
        // A coordinator capture shows its disclosure first (§4.6).
        within(
            deadline,
            coordinator::before_grim(&start, &ctx.env, gate, &req.monitor),
        )
        .await?;
        within(deadline, start.begin(&ctx.env)).await?;
        // 9
        let (env, launch, monitor) = (
            ctx.env.clone(),
            req.params.launch.clone(),
            req.monitor.clone(),
        );
        let (written, back) = tokio::task::spawn_blocking(move || {
            let result = match staging.dir().try_clone() {
                Ok(dir) => capture_png(
                    &env,
                    &launch,
                    &monitor,
                    staging.file(),
                    MAX_PNG_BYTES,
                    deadline,
                    &|n| room(&dir, n),
                ),
                Err(_) => Err(capture_failed()),
            };
            (result, staging)
        })
        .await
        .map_err(|e| join_failed(&e))?;
        let len = written?;
        staging = back;
        // 10
        let (checked, back) = tokio::task::spawn_blocking(move || {
            let result = check_png(staging.file(), len);
            (result, staging)
        })
        .await
        .map_err(|e| join_failed(&e))?;
        checked?;
        staging = back;
        let fresh = within(deadline, self.fresh_info(ctx, gate, true)).await?;
        check_unlocked(&fresh)?;
        monitor_of(&fresh, &req.monitor)?;
        *last = fresh;
        let (hashed, back) = tokio::task::spawn_blocking(move || {
            let result = staging
                .sha256()
                .and_then(|sha| staging.recheck(len).map(|()| sha));
            (result, staging)
        })
        .await
        .map_err(|e| join_failed(&e))?;
        let sha256 = hashed?;
        staging = back;
        // 11
        left(deadline)?;
        within(deadline, coordinator::before_import(&start, &ctx.env)).await?;
        let (root, id, fs) = (
            req.service.root().to_path_buf(),
            artifact_id.to_owned(),
            Arc::clone(&req.params.fs),
        );
        let (imported, back) = tokio::task::spawn_blocking(move || {
            let result = import_png(&root, &id, staging.file(), len, &*fs, &room);
            (result, staging)
        })
        .await
        .map_err(|e| join_failed(&e))?;
        imported?;
        drop(back);
        // 12
        let (caller, caller_station, caller_session, caller_grant_id) =
            coordinator::caller(start.coordinator());
        let title: String = format!(
            "Desktop capture \u{b7} {}/{}",
            req.station_name, req.monitor
        )
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect();
        let new = NewCaptureArtifact {
            artifact_id: artifact_id.to_owned(),
            title,
            size: len,
            sha256,
            provenance: CaptureProvenance {
                capture_id: req.capture_id.clone(),
                station_id: req.station_id.clone(),
                monitor: req.monitor.clone(),
                kind: CaptureKind::Screenshot,
                caller,
                caller_station,
                caller_session,
                caller_grant_id,
                at,
                recording: None,
            },
        };
        let store = req.service.store();
        let published = match store
            .capture_commit(&req.deck_fp, &req.capture_id, new)
            .await
        {
            Ok(c) => ArtifactPublished {
                artifact: c.artifact,
                version: c.version,
                created: c.created,
            },
            Err(e) => {
                tracing::warn!(error = %e, "the screenshot commit failed");
                // The commit may have happened: read the row again.
                match store.capture_lookup(&req.deck_fp, &req.capture_id).await {
                    Ok(Lookup::Committed {
                        artifact, version, ..
                    }) => ArtifactPublished {
                        artifact,
                        version,
                        created: true,
                    },
                    Ok(_) => {
                        return Err(Fail::new(
                            code::CAPTURE_PUBLISH_FAILED,
                            text::CAPTURE_PUBLISH_FAILED,
                        )
                        .into());
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "the screenshot row cannot be read");
                        return Err(Stage {
                            fail: Fail::new(
                                code::CAPTURE_PUBLISH_FAILED,
                                text::CAPTURE_PUBLISH_FAILED,
                            ),
                            cleanup: false,
                        });
                    }
                }
            }
        };
        // The staging goes durably; a screenshot reserved nothing at its commit.
        finish_staging(
            &req.service,
            &req.deck_fp,
            &req.capture_id,
            Arc::clone(&req.params.fs),
            false,
        )
        .await;
        Ok(published)
    }

    /// Step 13: the fresh info, or the last valid one with `info_fresh:
    /// false` when the refresh fails (design §6.4).
    async fn answer(
        &self,
        req: &Request,
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
                    "the desktop info refresh after a screenshot failed"
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

    /// A repeated request: the answer comes from the row, no capture runs.
    async fn answer_existing(
        &self,
        req: &Request,
        ctx: &Ctx,
        gate: &(dyn Fn() -> bool + Send + Sync),
        info: DesktopInfo,
        row: CaptureOp,
        deadline: Instant,
    ) -> Result<DesktopActResult, Fail> {
        tracing::debug!(state = ?row.state, "a repeated screenshot request");
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
                Ok(self.answer(req, ctx, gate, info, published, deadline).await)
            }
        }
    }
}

/// The time left until `deadline`.
fn left(deadline: Instant) -> Result<Duration, Fail> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(timeout_fail)
}

/// Runs `fut` for at most the time left until `deadline`.
async fn within<T>(
    deadline: Instant,
    fut: impl std::future::Future<Output = Result<T, Fail>>,
) -> Result<T, Fail> {
    let limit = left(deadline)?;
    tokio::time::timeout(limit, fut)
        .await
        .map_err(|_| timeout_fail())?
}
