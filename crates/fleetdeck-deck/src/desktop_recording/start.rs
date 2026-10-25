//! The start of an owner recording (R13 P5b-1 task K1, design
//! `omarchy-p5b.md` §3.2, §4.1, §5.2, §10). One send, never a resend.

use axum::http::StatusCode;
use fleetdeck_link::desktop_authority::action_hash;
use fleetdeck_proto::StationId;
use fleetdeck_proto::capture::{CaptureResult, CaptureStatus, code, text};
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction};
use fleetdeck_proto::version::feature;
use tokio::time::Instant;

use super::provenance::{Indexed, index};
use super::{CaptureAudit, RecordOptions, RecordingError, audit_capture, audit_terminal};
use crate::desktop_capture::{Reply, stamp_features, status_of};
use crate::desktop_relay::{RelayCtx, RelayError};
use crate::link::desktop_authority::{
    CallError, Route, Waits, map_wire, resolve, send_act_raw, send_info,
};
use crate::store::desktop_captures::{CaptureRow, CaptureState, Prepared};
use crate::store::desktop_recordings::NewRecording;
use crate::store::displays_binding::check_monitor;
use crate::store::{AuditResult, DeckError, LinkFeature};

/// The features of the owner recording (§3.1), besides `desktop.v1` and
/// `desktop_authority.v1` that [`resolve`] checks.
const OWNER_SET: [&str; 4] = [
    feature::DESKTOP_CAPTURE,
    feature::ARTIFACTS_DELETE,
    feature::ARTIFACTS_VIDEO,
    feature::DESKTOP_RECORDING,
];

fn store_error(e: &DeckError) -> RecordingError {
    tracing::warn!(error = %e, "desktop recording store");
    RecordingError::Relay(RelayError::Failed)
}

fn pending(capture_id: &str) -> Reply {
    Reply::State(
        StatusCode::ACCEPTED,
        Box::new(CaptureStatus::Pending {
            capture_id: capture_id.to_owned(),
        }),
    )
}

/// The answer of a recording whose deck wait passed: the outcome is unknown,
/// and the row stays `attempted` for the recovery.
fn unknown(capture_id: &str) -> Reply {
    Reply::State(
        StatusCode::ACCEPTED,
        Box::new(CaptureStatus::Unknown {
            capture_id: capture_id.to_owned(),
        }),
    )
}

fn control_on(ctx: &RelayCtx) -> Result<(), RelayError> {
    if ctx.live.get().desktop_control {
        Ok(())
    } else {
        Err(RelayError::ControlOff)
    }
}

/// What the checks before the row give.
struct Admitted {
    route: Route,
    action: DesktopAction,
    hash: String,
}

/// The checks before any row: the switch, the link, the owner feature set,
/// the monitor name.
fn admit(
    ctx: &RelayCtx,
    station: &StationId,
    monitor: &str,
    seconds: u32,
) -> Result<Admitted, RecordingError> {
    control_on(ctx)?;
    let route = resolve(&ctx.link, &ctx.store, station)?;
    control_on(ctx)?;
    for f in OWNER_SET {
        if !route.negotiated(f) || ctx.store.link_feature(station, f) != LinkFeature::Negotiated {
            return Err(RecordingError::unsupported());
        }
    }
    check_monitor(monitor).map_err(|_| RecordingError::invalid_body())?;
    let action = DesktopAction::ScreenRecord {
        monitor: monitor.to_owned(),
        seconds,
    };
    let hash = action_hash(&action, route.station_node());
    Ok(Admitted {
        route,
        action,
        hash,
    })
}

/// Runs one owner recording: the checks, the durable record, one send, and
/// the indexing. Audit rows: one `refused` row for a request that stops before
/// the row, `accepted` before the send, and one `terminal` row when the result
/// is known.
///
/// # Errors
/// [`RecordingError`]. A [`RelayError::Timeout`] of the send is not an error:
/// it answers `202 unknown` (the recording may exist), and the row stays
/// `attempted` for the recovery.
pub async fn start(
    ctx: &RelayCtx,
    station: StationId,
    capture_id: String,
    monitor: String,
    seconds: u32,
    owner_login: String,
    opts: RecordOptions,
) -> Result<Reply, RecordingError> {
    let deadline = Instant::now() + opts.handler(seconds);
    let admitted = match admit(ctx, &station, &monitor, seconds) {
        Ok(a) => a,
        Err(e) => {
            refused(ctx, &station, &capture_id, &monitor, seconds, e).await;
            return Err(e);
        }
    };
    let call = Call {
        station: &station,
        capture_id: &capture_id,
        monitor: &monitor,
        seconds,
        opts,
        deadline,
    };
    dispatch(ctx, &call, admitted, owner_login).await
}

async fn refused(
    ctx: &RelayCtx,
    station: &StationId,
    capture_id: &str,
    monitor: &str,
    seconds: u32,
    e: RecordingError,
) {
    audit_capture(
        &ctx.store,
        station,
        e.audit_result(),
        CaptureAudit {
            capture_id,
            monitor,
            seconds: Some(seconds),
            stage: "refused",
            code: e.http().1,
            stop_reason: None,
            artifact: None,
        },
    )
    .await;
}

/// The identity of one request.
struct Call<'a> {
    station: &'a StationId,
    capture_id: &'a str,
    monitor: &'a str,
    seconds: u32,
    opts: RecordOptions,
    deadline: Instant,
}

async fn dispatch(
    ctx: &RelayCtx,
    call: &Call<'_>,
    admitted: Admitted,
    owner_login: String,
) -> Result<Reply, RecordingError> {
    let Admitted {
        route,
        action,
        hash,
    } = admitted;
    let prepared = ctx
        .store
        .recording_prepare(NewRecording {
            station: call.station.clone(),
            capture_id: call.capture_id.to_owned(),
            action_hash: hash,
            monitor: call.monitor.to_owned(),
            seconds: call.seconds,
            owner_login,
            link_epoch: route.epoch().to_owned(),
        })
        .await
        .map_err(|e| store_error(&e))?;
    match prepared {
        Prepared::Conflict => {
            let e = RecordingError::id_conflict();
            refused(
                ctx,
                call.station,
                call.capture_id,
                call.monitor,
                call.seconds,
                e,
            )
            .await;
            return Err(e);
        }
        Prepared::Existing(row) => return existing(ctx, call, &route, *row).await,
        Prepared::New => {}
    }
    // After this commit a crash leaves the outcome unknown: never sent again.
    ctx.store
        .capture_attempted(call.station.clone(), call.capture_id.to_owned())
        .await
        .map_err(|e| store_error(&e))?;
    audit_capture(
        &ctx.store,
        call.station,
        AuditResult::Ok,
        CaptureAudit {
            capture_id: call.capture_id,
            monitor: call.monitor,
            seconds: Some(call.seconds),
            stage: "accepted",
            code: "ok",
            stop_reason: None,
            artifact: None,
        },
    )
    .await;
    let wait = call.opts.wait(call.seconds);
    // No outer timeout drops the send: the station supervises the recorder.
    let sent = send_act_raw(
        &ctx.store,
        &route,
        &action,
        None,
        Waits {
            long: wait,
            short: wait,
        },
        Some(call.capture_id.to_owned()),
    )
    .await;
    match sent {
        Ok(result) => after_station(ctx, call, result).await,
        Err(CallError::Wire { code, message }) => {
            match RecordingError::from_station(&code, &message) {
                Some((e, stored, fixed)) => {
                    failed(ctx, call, stored, fixed).await;
                    Err(e)
                }
                // The row stays `attempted`: the station may have run.
                None => Err(RecordingError::Relay(map_wire(&code, &message))),
            }
        }
        // The deck wait passed: the recording may exist; never resent.
        Err(CallError::Relay(RelayError::Timeout)) => Ok(unknown(call.capture_id)),
        Err(CallError::Relay(e)) => Err(e.into()),
    }
}

/// Stores a station failure and writes the terminal audit row.
async fn failed(ctx: &RelayCtx, call: &Call<'_>, stored: &str, fixed: &str) {
    let marked = ctx
        .store
        .capture_failed(
            call.station.clone(),
            call.capture_id.to_owned(),
            stored.to_owned(),
            fixed.to_owned(),
        )
        .await;
    if let Err(e) = marked {
        tracing::warn!(error = %e, "desktop recording: failure not stored");
        return;
    }
    let row = ctx
        .store
        .recording_get(call.station.clone(), call.capture_id.to_owned())
        .await;
    match row {
        Ok(Some(row)) if row.capture.state == CaptureState::Failed => {
            audit_terminal(&ctx.store, &row, stored, None, None).await;
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "desktop recording: row not read"),
    }
}

/// The station answered `Ok`: index the artifact and answer.
async fn after_station(
    ctx: &RelayCtx,
    call: &Call<'_>,
    result: DesktopActResult,
) -> Result<Reply, RecordingError> {
    let DesktopActResult {
        mut info,
        capture,
        info_fresh,
        ..
    } = result;
    let Some(sent) = capture.filter(|c| c.capture_id == call.capture_id) else {
        failed(ctx, call, code::RECORDING_FAILED, text::RECORDING_FAILED).await;
        return Err(RecordingError::from_station(code::RECORDING_FAILED, "")
            .map_or(RecordingError::Relay(RelayError::Failed), |(e, _, _)| e));
    };
    stamp_features(&ctx.store, call.station, &mut info);
    ctx.store.update_station_desktop(call.station, info.clone());
    let row = ctx
        .store
        .recording_get(call.station.clone(), call.capture_id.to_owned())
        .await
        .map_err(|e| store_error(&e))?;
    let Some(row) = row else {
        return Ok(pending(call.capture_id));
    };
    let indexed = tokio::time::timeout_at(
        call.deadline,
        index(&ctx.store, &row, sent.published.clone()),
    )
    .await;
    let published = match indexed {
        Ok(Indexed::Done(published)) => *published,
        // A tombstone hides the artifact: the station's result as it came.
        Ok(Indexed::Deleted) => sent.published,
        // After the station commit the recording did not fail: the result
        // stays for the recovery (§4.1).
        Ok(Indexed::Pending) | Err(_) => return Ok(pending(call.capture_id)),
    };
    Ok(Reply::Done(Box::new(DesktopActResult {
        address: None,
        window_id: None,
        info,
        capture: Some(CaptureResult {
            capture_id: call.capture_id.to_owned(),
            published,
        }),
        info_fresh,
    })))
}

/// The answer of a repeated POST: the state of the row, no send (§5.2).
async fn existing(
    ctx: &RelayCtx,
    call: &Call<'_>,
    route: &Route,
    row: CaptureRow,
) -> Result<Reply, RecordingError> {
    let id = row.capture_id.clone();
    if row.deleted {
        return Ok(Reply::State(
            StatusCode::OK,
            Box::new(CaptureStatus::Deleted { capture_id: id }),
        ));
    }
    match (row.state, row.indexed, row.published.clone()) {
        (CaptureState::Committed, true, Some(published)) => {
            let wait = call.opts.wait_extra;
            let fresh = send_info(
                route,
                true,
                Waits {
                    long: wait,
                    short: wait,
                },
            )
            .await;
            let (info, info_fresh) = match fresh {
                Ok(mut info) => {
                    stamp_features(&ctx.store, call.station, &mut info);
                    ctx.store.update_station_desktop(call.station, info.clone());
                    (info, None)
                }
                Err(_) => match ctx
                    .store
                    .station_info(call.station)
                    .and_then(|(i, _)| i.desktop)
                {
                    Some(info) => (info, Some(false)),
                    None => return Ok(pending(&id)),
                },
            };
            Ok(Reply::Done(Box::new(DesktopActResult {
                address: None,
                window_id: None,
                info,
                capture: Some(CaptureResult {
                    capture_id: id,
                    published,
                }),
                info_fresh,
            })))
        }
        (CaptureState::Failed, _, _) => Err(RecordingError::from_row(&row)),
        (CaptureState::Unknown, _, _) => Ok(Reply::State(
            StatusCode::ACCEPTED,
            Box::new(status_of(&row)),
        )),
        _ => Ok(pending(&id)),
    }
}
