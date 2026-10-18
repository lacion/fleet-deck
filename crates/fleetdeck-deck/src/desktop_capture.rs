//! The deck side of the owner screenshot (R13 P5a task E, design
//! `docs/v2/design/omarchy-p5a.md` §2.2-§2.5, §6.2-§6.4, §7.1).
//!
//! - [`act`]: the `screen_capture` branch of `POST .../desktop/act`. It records
//!   the capture id and the action hash (`prepared`, then `attempted`), then
//!   sends `desktop.act` once with the capture id as the authority `action_id`.
//!   The deck never sends a recorded id: a repeated POST answers from the row.
//! - [`lookup`]: `GET .../captures/{capture_id}`, from the row, after one
//!   `desktop.capture_status` call (5 s) while the result is unresolved.
//! - [`spawn`]: the recovery task. At the start, on each change of the board
//!   and every 30 s it reconciles each unresolved row. It never sends
//!   `desktop.act`.
//! - [`stamp_features`] fills `DesktopInfo.features` from the live link.
//!
//! A station text never reaches an answer: see [`CaptureError`].

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use fleetdeck_link::control::Control;
use fleetdeck_link::desktop_authority::action_hash;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::ArtifactPublished;
use fleetdeck_proto::capture::{
    CaptureCaller, CaptureKind, CaptureProvenance, CaptureResult, CaptureStatus,
    CaptureStatusQuery, code, text,
};
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction, DesktopInfo};
use fleetdeck_proto::link::Outcome as Wire;
use fleetdeck_proto::version::feature;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::desktop_relay::{RelayCtx, RelayError};
use crate::link::DeckLink;
use crate::link::desktop_authority::{
    CallError, Route, Waits, map_wire, resolve, send_act_raw, send_info,
};
use crate::store::desktop_captures::{
    CaptureRow, CaptureState, Completed, Prepared, UNKNOWN_AFTER_MS, stamp_published,
};
use crate::store::displays_binding::check_monitor;
use crate::store::{AuditResult, DeckError, DeckStore, LinkFeature, NewAudit};

/// The deck wait for the station action (§7.1).
pub const STATION_WAIT: Duration = Duration::from_secs(35);
/// The whole action handler, with the indexing (§7.1).
pub const HANDLER_DEADLINE: Duration = Duration::from_secs(40);
/// One station lookup (§7.1).
pub const STATUS_WAIT: Duration = Duration::from_secs(5);
/// The whole lookup handler (§7.1).
pub const LOOKUP_DEADLINE: Duration = Duration::from_secs(10);
/// The pass interval of the recovery task (§6.3).
pub const RECOVERY_TICK: Duration = Duration::from_secs(30);

/// The audit action of a screenshot.
const AUDIT_ACTION: &str = "desktop.screen_capture";

/// The waits of one capture request (a test makes them short).
#[derive(Debug, Clone, Copy)]
pub struct CaptureOptions {
    /// The deck wait for `desktop.act` and for the fresh info of a repeated
    /// POST (`short`).
    pub waits: Waits,
    /// The whole handler.
    pub handler: Duration,
    /// One `desktop.capture_status` call.
    pub status_wait: Duration,
    /// The whole lookup handler.
    pub lookup: Duration,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            waits: Waits {
                long: STATION_WAIT,
                short: STATION_WAIT,
            },
            handler: HANDLER_DEADLINE,
            status_wait: STATUS_WAIT,
            lookup: LOOKUP_DEADLINE,
        }
    }
}

/// Sets `info.features` to the P5a features that the live link of `station`
/// negotiated (every live link, fail closed; none without a link).
pub fn stamp_features(store: &DeckStore, station: &StationId, info: &mut DesktopInfo) {
    info.features = [feature::DESKTOP_CAPTURE, feature::ARTIFACTS_DELETE]
        .into_iter()
        .filter(|f| store.link_feature(station, f) == LinkFeature::Negotiated)
        .map(str::to_owned)
        .collect();
}

/// Why a capture request failed. `RelayError` has the fixed texts of every
/// other desktop failure; `Coded` is a capture code with its own status and
/// text (§4.1, §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureError {
    Relay(RelayError),
    Coded {
        status: StatusCode,
        code: &'static str,
        text: &'static str,
    },
}

impl From<RelayError> for CaptureError {
    fn from(e: RelayError) -> Self {
        Self::Relay(e)
    }
}

/// One station capture code: its status and its fixed text.
struct Known {
    code: &'static str,
    status: StatusCode,
    text: &'static str,
}

const fn known(code: &'static str, status: StatusCode, text: &'static str) -> Known {
    Known { code, status, text }
}

const KNOWN: [Known; 10] = [
    known(code::BUSY, StatusCode::CONFLICT, text::BUSY),
    known(
        code::DISK_FULL,
        StatusCode::INSUFFICIENT_STORAGE,
        text::DISK_FULL,
    ),
    known(
        code::CAPTURE_QUOTA,
        StatusCode::CONFLICT,
        text::CAPTURE_QUOTA,
    ),
    known(
        code::CAPTURE_TOO_LARGE,
        StatusCode::PAYLOAD_TOO_LARGE,
        text::CAPTURE_TOO_LARGE,
    ),
    known(
        code::CAPTURE_DIMENSIONS,
        StatusCode::UNPROCESSABLE_ENTITY,
        text::CAPTURE_DIMENSIONS,
    ),
    known(
        code::CAPTURE_FAILED,
        StatusCode::BAD_GATEWAY,
        text::CAPTURE_FAILED,
    ),
    known(
        code::CAPTURE_PUBLISH_FAILED,
        StatusCode::BAD_GATEWAY,
        text::CAPTURE_PUBLISH_FAILED,
    ),
    known(
        code::SCREEN_LOCKED,
        StatusCode::CONFLICT,
        text::SCREEN_LOCKED,
    ),
    known(
        code::CAPTURE_ID_CONFLICT,
        StatusCode::CONFLICT,
        text::CAPTURE_ID_CONFLICT,
    ),
    known(
        code::CAPTURE_INTERRUPTED,
        StatusCode::BAD_GATEWAY,
        text::CAPTURE_INTERRUPTED,
    ),
];

/// The station code that the deck stores for the missing `grim`.
const NO_GRIM_CODE: &str = "unsupported";

impl CaptureError {
    fn of_known(k: &Known) -> Self {
        Self::Coded {
            status: k.status,
            code: k.code,
            text: k.text,
        }
    }

    /// The error of a station capture code, with the code and the fixed text
    /// to store: the ten codes of §4.1 and `unsupported` with the grim text.
    /// Any other answer is `None`.
    #[must_use]
    pub fn from_station(
        station_code: &str,
        message: &str,
    ) -> Option<(Self, &'static str, &'static str)> {
        if station_code == NO_GRIM_CODE && message == text::NO_GRIM {
            let e = Self::Coded {
                status: StatusCode::CONFLICT,
                code: "desktop_unsupported",
                text: text::NO_GRIM,
            };
            return Some((e, NO_GRIM_CODE, text::NO_GRIM));
        }
        KNOWN
            .iter()
            .find(|k| k.code == station_code)
            .map(|k| (Self::of_known(k), k.code, k.text))
    }

    /// The error of a stored `failed` row.
    fn from_row(row: &CaptureRow) -> Self {
        let stored_code = row.code.as_deref().unwrap_or(code::CAPTURE_FAILED);
        let message = row.message.as_deref().unwrap_or(text::CAPTURE_FAILED);
        Self::from_station(stored_code, message)
            .map_or_else(|| Self::of_known(&KNOWN[5]), |(e, _, _)| e)
    }

    fn invalid_body() -> Self {
        Self::Coded {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_body",
            text: "the body is not a valid desktop action",
        }
    }

    fn id_conflict() -> Self {
        Self::of_known(&KNOWN[8])
    }

    /// The HTTP status, error code and fixed text.
    #[must_use]
    pub fn http(self) -> (StatusCode, &'static str, &'static str) {
        match self {
            Self::Relay(e) => {
                let (status, code) = e.http();
                (status, code, e.message())
            }
            Self::Coded { status, code, text } => (status, code, text),
        }
    }

    fn audit_result(self) -> AuditResult {
        match self {
            Self::Relay(
                RelayError::ControlOff
                | RelayError::Unsupported
                | RelayError::Forbidden
                | RelayError::Invalid
                | RelayError::NotFound
                | RelayError::Conflict,
            ) => AuditResult::Refused,
            Self::Coded { code, .. } if code == "invalid_body" || code == "capture_id_conflict" => {
                AuditResult::Refused
            }
            _ => AuditResult::Failed,
        }
    }
}

/// What a capture request answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// `200` with the result.
    Done(Box<DesktopActResult>),
    /// A state body `{"capture_id", "state"}` with its status: `202` for
    /// `pending` and `unknown`, `200` for `deleted`.
    State(StatusCode, Box<CaptureStatus>),
}

impl Reply {
    fn state(status: StatusCode, body: CaptureStatus) -> Self {
        Self::State(status, Box::new(body))
    }
}

fn pending(capture_id: &str) -> Reply {
    Reply::state(
        StatusCode::ACCEPTED,
        CaptureStatus::Pending {
            capture_id: capture_id.to_owned(),
        },
    )
}

fn store_error(e: &DeckError) -> CaptureError {
    tracing::warn!(error = %e, "desktop capture store");
    CaptureError::Relay(RelayError::Failed)
}

fn control_on(ctx: &RelayCtx) -> Result<(), RelayError> {
    if ctx.live.get().desktop_control {
        Ok(())
    } else {
        Err(RelayError::ControlOff)
    }
}

/// The immutable provenance of a capture, built by the deck (never taken
/// from the station): `at` is the creation time of the station's version.
#[must_use]
pub fn provenance_of(
    station: &StationId,
    capture_id: &str,
    monitor: &str,
    published: &ArtifactPublished,
) -> CaptureProvenance {
    CaptureProvenance {
        capture_id: capture_id.to_owned(),
        station_id: station.as_str().to_owned(),
        monitor: monitor.to_owned(),
        kind: CaptureKind::Screenshot,
        caller: CaptureCaller::Owner,
        caller_station: None,
        at: published.version.created_at,
    }
}

/// Runs one owner screenshot: the checks, the durable record, one send, and
/// the indexing. One audit row (`desktop.screen_capture`) for each call.
///
/// # Errors
/// [`CaptureError`]. A [`RelayError::Timeout`] means that the screenshot may
/// have been taken: the row stays `attempted` and the recovery resolves it.
pub async fn act(
    ctx: &RelayCtx,
    station: StationId,
    capture_id: String,
    monitor: String,
    opts: CaptureOptions,
) -> Result<Reply, CaptureError> {
    let result = run(ctx, &station, &capture_id, &monitor, opts).await;
    let (status, code) = match &result {
        Ok(Reply::Done(_)) => (AuditResult::Ok, "ok".to_owned()),
        Ok(Reply::State(_, s)) => (AuditResult::Ok, state_name(s).to_owned()),
        Err(e) => (e.audit_result(), e.http().1.to_owned()),
    };
    ctx.store
        .audit_or_log(
            NewAudit::new("owner", AUDIT_ACTION, status)
                .target(station.as_str())
                .detail(format!("capture={capture_id} code={code}")),
        )
        .await;
    result
}

fn state_name(s: &CaptureStatus) -> &'static str {
    match s {
        CaptureStatus::Pending { .. } => "pending",
        CaptureStatus::Unknown { .. } => "unknown",
        CaptureStatus::Committed { .. } => "committed",
        CaptureStatus::Failed { .. } => "failed",
        CaptureStatus::Deleted { .. } => "deleted",
    }
}

async fn run(
    ctx: &RelayCtx,
    station: &StationId,
    capture_id: &str,
    monitor: &str,
    opts: CaptureOptions,
) -> Result<Reply, CaptureError> {
    let deadline = Instant::now() + opts.handler;
    control_on(ctx)?;
    let route = resolve(&ctx.link, &ctx.store, station)?;
    control_on(ctx)?;
    if !route.negotiated(feature::DESKTOP_CAPTURE)
        || ctx.store.link_feature(station, feature::DESKTOP_CAPTURE) != LinkFeature::Negotiated
    {
        return Err(RelayError::Unsupported.into());
    }
    check_monitor(monitor).map_err(|_| CaptureError::invalid_body())?;
    let action = DesktopAction::ScreenCapture {
        monitor: monitor.to_owned(),
    };
    let hash = action_hash(&action, route.station_node());
    let prepared = ctx
        .store
        .capture_prepare(
            station.clone(),
            capture_id.to_owned(),
            hash,
            monitor.to_owned(),
        )
        .await
        .map_err(|e| store_error(&e))?;
    match prepared {
        Prepared::Conflict => return Err(CaptureError::id_conflict()),
        Prepared::Existing(row) => return answer_row(ctx, station, &route, *row, opts).await,
        Prepared::New => {}
    }
    // After this commit a crash leaves the outcome unknown: never sent again.
    ctx.store
        .capture_attempted(station.clone(), capture_id.to_owned())
        .await
        .map_err(|e| store_error(&e))?;
    let sent = send_act_raw(
        &ctx.store,
        &route,
        &action,
        None,
        opts.waits,
        Some(capture_id.to_owned()),
    )
    .await;
    match sent {
        Ok(result) => after_station(ctx, station, capture_id, monitor, result, deadline).await,
        Err(CallError::Wire { code, message }) => {
            match CaptureError::from_station(&code, &message) {
                Some((e, stored, fixed)) => {
                    let marked = ctx
                        .store
                        .capture_failed(
                            station.clone(),
                            capture_id.to_owned(),
                            stored.to_owned(),
                            fixed.to_owned(),
                        )
                        .await;
                    if let Err(e) = marked {
                        tracing::warn!(error = %e, "desktop capture: failure not stored");
                    }
                    Err(e)
                }
                // The row stays `attempted`: the station may have run.
                None => Err(CaptureError::Relay(map_wire(&code, &message))),
            }
        }
        Err(CallError::Relay(e)) => Err(e.into()),
    }
}

/// The station answered `Ok`: index the artifact and answer.
async fn after_station(
    ctx: &RelayCtx,
    station: &StationId,
    capture_id: &str,
    monitor: &str,
    result: DesktopActResult,
    deadline: Instant,
) -> Result<Reply, CaptureError> {
    let DesktopActResult {
        mut info,
        capture,
        info_fresh,
        ..
    } = result;
    let Some(sent) = capture.filter(|c| c.capture_id == capture_id) else {
        let marked = ctx
            .store
            .capture_failed(
                station.clone(),
                capture_id.to_owned(),
                code::CAPTURE_FAILED.to_owned(),
                text::CAPTURE_FAILED.to_owned(),
            )
            .await;
        if let Err(e) = marked {
            tracing::warn!(error = %e, "desktop capture: failure not stored");
        }
        return Err(CaptureError::of_known(&KNOWN[5]));
    };
    stamp_features(&ctx.store, station, &mut info);
    ctx.store.update_station_desktop(station, info.clone());
    let provenance = provenance_of(station, capture_id, monitor, &sent.published);
    let done = tokio::time::timeout_at(
        deadline,
        ctx.store.complete_capture(
            station.clone(),
            capture_id.to_owned(),
            sent.published.clone(),
            provenance.clone(),
        ),
    )
    .await;
    let published = match done {
        Ok(Ok(Completed::Indexed)) => stamp_published(station, sent.published, &provenance),
        // A tombstone hides the artifact: the station's result as it came.
        Ok(Ok(Completed::Deleted)) => sent.published,
        Ok(Err(e)) => {
            // After the station commit the screenshot did not fail: keep the
            // result for the recovery and answer `pending` (§6.4).
            tracing::warn!(error = %e, "desktop capture: indexing failed");
            let kept = ctx
                .store
                .capture_keep_published(station.clone(), capture_id.to_owned(), sent.published)
                .await;
            if let Err(e) = kept {
                tracing::warn!(error = %e, "desktop capture: result not kept");
            }
            return Ok(pending(capture_id));
        }
        Err(_) => return Ok(pending(capture_id)),
    };
    Ok(Reply::Done(Box::new(DesktopActResult {
        address: None,
        info,
        capture: Some(CaptureResult {
            capture_id: capture_id.to_owned(),
            published,
        }),
        info_fresh,
    })))
}

/// The answer of a repeated POST: the state of the row, no send (§6.2).
async fn answer_row(
    ctx: &RelayCtx,
    station: &StationId,
    route: &Route,
    row: CaptureRow,
    opts: CaptureOptions,
) -> Result<Reply, CaptureError> {
    let id = row.capture_id.clone();
    if row.deleted {
        return Ok(Reply::state(
            StatusCode::OK,
            CaptureStatus::Deleted { capture_id: id },
        ));
    }
    match (row.state, row.indexed, row.published.clone()) {
        (CaptureState::Committed, true, Some(published)) => {
            let fresh = send_info(route, true, opts.waits).await;
            let (info, info_fresh) = match fresh {
                Ok(mut info) => {
                    stamp_features(&ctx.store, station, &mut info);
                    ctx.store.update_station_desktop(station, info.clone());
                    (info, None)
                }
                Err(_) => match ctx.store.station_info(station).and_then(|(i, _)| i.desktop) {
                    Some(info) => (info, Some(false)),
                    None => return Ok(pending(&id)),
                },
            };
            Ok(Reply::Done(Box::new(DesktopActResult {
                address: None,
                info,
                capture: Some(CaptureResult {
                    capture_id: id,
                    published,
                }),
                info_fresh,
            })))
        }
        (CaptureState::Failed, _, _) => Err(CaptureError::from_row(&row)),
        (CaptureState::Unknown, _, _) => Ok(Reply::state(
            StatusCode::ACCEPTED,
            CaptureStatus::Unknown { capture_id: id },
        )),
        _ => Ok(pending(&id)),
    }
}

/// The status of a row, as the lookup answers it (§2.4): a tombstone wins.
#[must_use]
pub fn status_of(row: &CaptureRow) -> CaptureStatus {
    let capture_id = row.capture_id.clone();
    if row.deleted {
        return CaptureStatus::Deleted { capture_id };
    }
    match (row.state, row.indexed, &row.published) {
        (CaptureState::Committed, true, Some(published)) => CaptureStatus::Committed {
            capture_id,
            published: published.clone(),
        },
        (CaptureState::Failed, _, _) => CaptureStatus::Failed {
            capture_id,
            error: row
                .code
                .clone()
                .unwrap_or_else(|| code::CAPTURE_FAILED.to_owned()),
            message: row
                .message
                .clone()
                .unwrap_or_else(|| text::CAPTURE_FAILED.to_owned()),
        },
        (CaptureState::Unknown, _, _) => CaptureStatus::Unknown { capture_id },
        _ => CaptureStatus::Pending { capture_id },
    }
}

/// One `desktop.capture_status` call on the live link of `station`. `None`
/// without a link, without `desktop_capture.v1`, on a timeout, an error
/// answer or an answer for another capture.
async fn ask_status(
    link: &DeckLink,
    station: &StationId,
    capture_id: &str,
    wait: Duration,
) -> Option<CaptureStatus> {
    let control: Control = link.control_for(station)?;
    if !control.params().negotiated.has(feature::DESKTOP_CAPTURE) {
        return None;
    }
    let params = serde_json::json!(CaptureStatusQuery {
        capture_id: capture_id.to_owned(),
    });
    let outcome = tokio::time::timeout(
        wait,
        control.request("desktop.capture_status", None, params),
    )
    .await
    .ok()?
    .ok()?;
    let Wire::Ok(value) = outcome else {
        return None;
    };
    // A parse error is not logged: the answer could hold desktop data.
    let status: CaptureStatus = serde_json::from_value(value).ok()?;
    let same = match &status {
        CaptureStatus::Pending { capture_id: id }
        | CaptureStatus::Unknown { capture_id: id }
        | CaptureStatus::Committed { capture_id: id, .. }
        | CaptureStatus::Failed { capture_id: id, .. }
        | CaptureStatus::Deleted { capture_id: id } => id == capture_id,
    };
    same.then_some(status)
}

/// Applies a station status to a row (§6.3, decision 8). A committed result
/// indexes the artifact; a failure stores only a capture code (any other code
/// is `capture_failed`); `unknown` ends the row 24 h after its creation.
///
/// # Errors
/// A database error.
pub async fn reconcile(
    store: &DeckStore,
    row: &CaptureRow,
    status: CaptureStatus,
) -> Result<(), DeckError> {
    let station = row.station_id.clone();
    match status {
        CaptureStatus::Committed { published, .. } => {
            let provenance = provenance_of(&station, &row.capture_id, &row.monitor, &published);
            store
                .complete_capture(station, row.capture_id.clone(), published, provenance)
                .await?;
        }
        CaptureStatus::Failed { error, .. } => {
            let (stored, fixed) = KNOWN
                .iter()
                .find(|k| k.code == error)
                .map_or((code::CAPTURE_FAILED, text::CAPTURE_FAILED), |k| {
                    (k.code, k.text)
                });
            store
                .capture_failed(
                    station,
                    row.capture_id.clone(),
                    stored.to_owned(),
                    fixed.to_owned(),
                )
                .await?;
        }
        CaptureStatus::Unknown { .. } => {
            if store.now_ms().saturating_sub(row.created_at) >= UNKNOWN_AFTER_MS {
                store
                    .capture_unknown(station, row.capture_id.clone())
                    .await?;
            }
        }
        // Nothing to do: `pending` waits, and a tombstone answers `deleted`.
        CaptureStatus::Pending { .. } | CaptureStatus::Deleted { .. } => {}
    }
    Ok(())
}

/// Asks the station about an unresolved row and reconciles it.
async fn refresh(ctx: &RelayCtx, row: &CaptureRow, status_wait: Duration) {
    let Some(status) = ask_status(&ctx.link, &row.station_id, &row.capture_id, status_wait).await
    else {
        return;
    };
    if let Err(e) = reconcile(&ctx.store, row, status).await {
        tracing::warn!(error = %e, "desktop capture: not reconciled");
    }
}

/// The lookup of one capture (§2.4, §6.3): the status of its row, after one
/// station call and the reconcile while the row is unresolved. `None`: no
/// such capture.
///
/// # Errors
/// A database error.
pub async fn lookup(
    ctx: &RelayCtx,
    station: &StationId,
    capture_id: &str,
    opts: CaptureOptions,
) -> Result<Option<CaptureStatus>, DeckError> {
    let Some(mut row) = ctx
        .store
        .capture_get(station.clone(), capture_id.to_owned())
        .await?
    else {
        return Ok(None);
    };
    if row.unresolved() {
        // The whole handler stops at its deadline; the recovery goes on.
        let _ = tokio::time::timeout(opts.lookup, refresh(ctx, &row, opts.status_wait)).await;
        if let Some(fresh) = ctx
            .store
            .capture_get(station.clone(), capture_id.to_owned())
            .await?
        {
            row = fresh;
        }
    }
    Ok(Some(status_of(&row)))
}

/// Starts the recovery task. It stops when `shutdown` turns true.
pub fn spawn(
    store: DeckStore,
    link: Arc<DeckLink>,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    spawn_with(store, link, shutdown, RECOVERY_TICK, STATUS_WAIT)
}

/// [`spawn`] with another pass interval and status wait (a test makes them
/// short).
pub fn spawn_with(
    store: DeckStore,
    link: Arc<DeckLink>,
    shutdown: watch::Receiver<bool>,
    tick: Duration,
    status_wait: Duration,
) -> JoinHandle<()> {
    tokio::spawn(run_recovery(store, link, shutdown, tick, status_wait))
}

async fn run_recovery(
    store: DeckStore,
    link: Arc<DeckLink>,
    mut shutdown: watch::Receiver<bool>,
    tick: Duration,
    status_wait: Duration,
) {
    let mut board = store.watch_board();
    let mut timer = tokio::time::interval(tick);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            () = pass(&store, &link, status_wait) => {}
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
        }
        tokio::select! {
            result = board.changed() => {
                if result.is_err() {
                    break;
                }
            }
            _ = timer.tick() => {}
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

/// One pass: each unresolved row whose station link is live and has
/// `desktop_capture.v1` is asked once and reconciled. It sends no action.
async fn pass(store: &DeckStore, link: &DeckLink, status_wait: Duration) {
    let rows = match store.captures_unresolved().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "desktop capture recovery: rows not read");
            return;
        }
    };
    for row in rows {
        let Some(status) = ask_status(link, &row.station_id, &row.capture_id, status_wait).await
        else {
            continue;
        };
        if let Err(e) = reconcile(store, &row, status).await {
            tracing::warn!(error = %e, "desktop capture recovery: not reconciled");
        }
    }
}

#[cfg(test)]
#[path = "desktop_capture_tests.rs"]
mod tests;
