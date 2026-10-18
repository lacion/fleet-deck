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
use tokio::time::Instant;

use crate::desktop_relay::{RelayCtx, RelayError};
use crate::link::DeckLink;
use crate::link::desktop_authority::{
    CallError, Route, Waits, map_wire, resolve, send_act_raw, send_info,
};
use crate::store::desktop_captures::{
    CaptureRow, CaptureState, Completed, Prepared, stamp_published,
};
use crate::store::displays_binding::check_monitor;
use crate::store::{AuditResult, DeckError, DeckStore, LinkFeature, NewAudit};

#[path = "desktop_capture_errors.rs"]
mod errors;
#[path = "desktop_capture_recovery.rs"]
mod recovery;
pub use errors::CaptureError;
use errors::KNOWN;
use recovery::refresh;
pub use recovery::{reconcile, spawn, spawn_with};

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

#[cfg(test)]
#[path = "desktop_capture_tests.rs"]
mod tests;
