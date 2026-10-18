//! `desktop.capture_lease` (R13 P5b-1 task K1, design `omarchy-p5b.md` §6.3,
//! §6.4, §7.5; decisions O12-6 and O12-7): the station asks, once a second,
//! whether the recording may go on. The deck answers from the durable row,
//! its own switches, the live policy of the target, and the stop intent, in
//! this order, and repeats the deck-local checks after the awaited policy
//! query. The whole request has 5 s.
//!
//! - A request that is not for a live owner recording of this link in a phase
//!   that its state allows is the wire error `forbidden` (the station treats
//!   it as `lease_denied`): an unknown capture, another hash, a screenshot, a
//!   `link_epoch` that is not the epoch of this connection, a state that
//!   does not allow the phase.
//! - A target policy that is false or uncertain is also `forbidden`.
//! - A lost deck-local authority is a `discard` answer with a reason of the
//!   allowed set.
//! - A coordinator recording (R13 P5b-2, §6.3) takes the coordinator branch:
//!   the deck switches, the P6 local pass of the row's context, the tombstone
//!   and the deadline, the live caller policy for the current full binding of
//!   the grant, the target policy, and the local pass again. A false caller
//!   policy is `discard` with `coordinator_authority_lost`; an uncertain one
//!   is the wire error `forbidden`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use fleetdeck_link::control::{Control, InboundRequest};
use fleetdeck_link::methods::code as link_code;
use fleetdeck_proto::StationId;
use fleetdeck_proto::capture::{code as capture_code, valid_capture_id};
use fleetdeck_proto::coordinator_desktop_text::code as cd_code;
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::recording::{
    CaptureLeaseAnswer, CaptureLeaseRequest, LeaseDecision, LeasePhase, StopReason,
};
use tokio::sync::Semaphore;

use super::policy;
use super::{CaptureAudit, WAIT_EXTRA, audit_row};
use crate::coordinator_desktop::{caller_policy, recording_pass};
use crate::link::DeckLink;
use crate::store::desktop_captures::CaptureState;
use crate::store::desktop_recordings::{OperationKind, RecordingRow, StopIntent};
use crate::store::{AuditResult, DeckError};

/// The most lease handlers at the same time on one deck (§6.4: bounded
/// dedicated capacity).
pub(super) const MAX_HANDLERS: usize = 8;

/// The whole lease request, with the policy query (§6.3, §10).
pub(super) const LEASE_DEADLINE: Duration = Duration::from_secs(5);

/// The fixed text of a refused lease.
const FORBIDDEN_TEXT: &str = "The recording lease is not valid.";
/// The fixed text of an overloaded deck.
const BUSY_TEXT: &str = "The deck is answering too many recording leases.";
/// The fixed text of a lease request that does not parse.
const INVALID_TEXT: &str = "The recording lease request is not valid.";

/// Why the deck does not answer with a decision.
enum Refuse {
    /// The wire error `forbidden`.
    Forbidden,
    /// A request that does not parse.
    Invalid,
    /// A store error.
    Store(DeckError),
}

impl From<DeckError> for Refuse {
    fn from(e: DeckError) -> Self {
        Self::Store(e)
    }
}

/// The lease slots of a deck, found by the identity of the deck: a process
/// that holds several decks (a test) keeps their capacity apart.
fn slots(ctx: &DeckLink) -> Arc<Semaphore> {
    static SLOTS: OnceLock<Mutex<HashMap<String, Arc<Semaphore>>>> = OnceLock::new();
    let key = ctx.node.identity.fingerprint().to_string();
    let mut map = SLOTS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    Arc::clone(
        map.entry(key)
            .or_insert_with(|| Arc::new(Semaphore::new(MAX_HANDLERS))),
    )
}

fn forbidden() -> Outcome {
    Outcome::Err(WireError::new(link_code::FORBIDDEN, FORBIDDEN_TEXT))
}

/// `desktop.capture_lease` of a station.
pub(crate) async fn lease(
    ctx: &DeckLink,
    control: &Control,
    station: StationId,
    req: &InboundRequest,
) -> Outcome {
    let Ok(_permit) = slots(ctx).try_acquire_owned() else {
        return Outcome::Err(WireError::new(capture_code::BUSY, BUSY_TEXT));
    };
    let work = decide(ctx, control, &station, req);
    match tokio::time::timeout(LEASE_DEADLINE, work).await {
        Ok(Ok(answer)) => match serde_json::to_value(answer) {
            Ok(v) => Outcome::Ok(v),
            Err(_) => Outcome::Err(WireError::new(link_code::PROTOCOL_ERROR, INVALID_TEXT)),
        },
        Ok(Err(Refuse::Forbidden)) | Err(_) => forbidden(),
        Ok(Err(Refuse::Invalid)) => {
            Outcome::Err(WireError::new(link_code::PROTOCOL_ERROR, INVALID_TEXT))
        }
        Ok(Err(Refuse::Store(e))) => {
            tracing::warn!(error = %e, "desktop recording lease");
            Outcome::Err(WireError::new(
                link_code::PROTOCOL_ERROR,
                "deck store error",
            ))
        }
    }
}

/// True when the state of `row` allows `phase` (§6.3): `start` and `record`
/// while `attempted`; `import` while `attempted` with the intent `none` or
/// `save`.
fn phase_allowed(row: &RecordingRow, phase: LeasePhase) -> bool {
    if row.capture.state != CaptureState::Attempted {
        return false;
    }
    match phase {
        LeasePhase::Start | LeasePhase::Record => true,
        LeasePhase::Import => matches!(row.stop_intent, StopIntent::None | StopIntent::Save),
    }
}

/// The durable row of the request, with the identity checks. Each call reads
/// the row again.
async fn load(
    ctx: &DeckLink,
    control: &Control,
    station: &StationId,
    req: &CaptureLeaseRequest,
) -> Result<RecordingRow, Refuse> {
    let row = ctx
        .store
        .recording_get(station.clone(), req.capture_id.clone())
        .await?
        .ok_or(Refuse::Forbidden)?;
    let same_link = control.params().peer_desktop_epoch.is_some()
        && control.params().peer_desktop_epoch == row.link_epoch
        && control.is_current();
    if row.kind != OperationKind::Recording
        || row.capture.action_hash != req.action_hash
        || row.requested_seconds.is_none()
        || !same_link
        || !phase_allowed(&row, req.phase)
    {
        return Err(Refuse::Forbidden);
    }
    Ok(row)
}

/// The deck-local checks of an owner recording (§6.3 steps 1 and 3), in
/// order: the deck switch, the initiating login, the tombstone, the deck
/// deadline.
async fn local(ctx: &DeckLink, row: &RecordingRow) -> Result<Option<StopReason>, DeckError> {
    // An empty cell counts as off.
    let control_on = ctx
        .desktop_live
        .get()
        .is_some_and(|live| live.get().desktop_control);
    if !control_on {
        return Ok(Some(StopReason::DeckPolicy));
    }
    let login = match &row.owner_login {
        Some(hash) => ctx.store.login_alive(hash.clone()).await?,
        None => false,
    };
    if !login {
        return Ok(Some(StopReason::OwnerLoginEnded));
    }
    if row.capture.deleted {
        return Ok(Some(StopReason::ArtifactDeleted));
    }
    // `updated_at` of an `attempted` row is the time of the `attempted` commit.
    let seconds = i64::from(row.requested_seconds.unwrap_or(0));
    let allowed_ms = seconds
        .saturating_mul(1_000)
        .saturating_add(i64::try_from(WAIT_EXTRA.as_millis()).unwrap_or(i64::MAX));
    if ctx.store.now_ms() > row.capture.updated_at.saturating_add(allowed_ms) {
        return Ok(Some(StopReason::Deadline));
    }
    Ok(None)
}

/// What the deck-local checks of a coordinator recording found.
enum Pass {
    /// A discard with this reason.
    Discard(StopReason),
    /// The checks hold: the stored context and the current binding of the
    /// grant.
    Held(Box<CoordinatorContext>, Box<CoordinatorBinding>),
}

/// The deck-local checks of a coordinator recording (§6.3), in order: the
/// deck switches (`deck_policy`); the P6 local pass of the row's context
/// with the recording allowance (`coordinator_authority_lost`; a deck store
/// error is uncertain and answers `forbidden`); the tombstone and the deck
/// deadline as for an owner recording.
async fn local_coordinator(ctx: &DeckLink, row: &RecordingRow) -> Result<Pass, Refuse> {
    // An empty cell counts as off.
    let control_on = ctx
        .desktop_live
        .get()
        .is_some_and(|live| live.get().desktop_control);
    if !ctx.store.fleet_control() || !control_on {
        return Ok(Pass::Discard(StopReason::DeckPolicy));
    }
    let lost = Pass::Discard(StopReason::CoordinatorAuthorityLost);
    let Some(context) = row
        .capture
        .coordinator
        .as_deref()
        .and_then(|raw| serde_json::from_str::<CoordinatorContext>(raw).ok())
    else {
        return Ok(lost);
    };
    let grant = match recording_pass(ctx, &context, &row.capture.station_id).await {
        Ok(grant) => grant,
        Err(refusal) if refusal == cd_code::AUTHORITY_UNKNOWN => return Err(Refuse::Forbidden),
        Err(_) => return Ok(lost),
    };
    let Some(binding) = grant.binding else {
        return Ok(lost);
    };
    if row.capture.deleted {
        return Ok(Pass::Discard(StopReason::ArtifactDeleted));
    }
    if past_deadline(ctx, row) {
        return Ok(Pass::Discard(StopReason::Deadline));
    }
    Ok(Pass::Held(Box::new(context), Box::new(binding)))
}

/// True when the deck deadline of the recording has passed: `updated_at` of
/// an `attempted` row is the time of the `attempted` commit.
fn past_deadline(ctx: &DeckLink, row: &RecordingRow) -> bool {
    let seconds = i64::from(row.requested_seconds.unwrap_or(0));
    let allowed_ms = seconds
        .saturating_mul(1_000)
        .saturating_add(i64::try_from(WAIT_EXTRA.as_millis()).unwrap_or(i64::MAX));
    ctx.store.now_ms() > row.capture.updated_at.saturating_add(allowed_ms)
}

fn answer(
    req: &CaptureLeaseRequest,
    decision: LeaseDecision,
    reason: Option<StopReason>,
) -> CaptureLeaseAnswer {
    CaptureLeaseAnswer {
        sequence: req.sequence,
        decision,
        reason,
    }
}

async fn decide(
    ctx: &DeckLink,
    control: &Control,
    station: &StationId,
    raw: &InboundRequest,
) -> Result<CaptureLeaseAnswer, Refuse> {
    let req: CaptureLeaseRequest =
        serde_json::from_value(raw.params.clone()).map_err(|_| Refuse::Invalid)?;
    if !valid_capture_id(&req.capture_id) {
        return Err(Refuse::Forbidden);
    }
    let row = load(ctx, control, station, &req).await?;
    if row.capture.coordinator.is_some() {
        return decide_coordinator(ctx, control, station, (&req, row)).await;
    }
    // Step 1: deck-local.
    if let Some(reason) = local(ctx, &row).await? {
        return Ok(answer(&req, LeaseDecision::Discard, Some(reason)));
    }
    // Step 2: the live policy of the target.
    if !policy::allowed(control, &row).await {
        return Err(Refuse::Forbidden);
    }
    // Step 3: deck-local again, on a fresh row.
    let row = load(ctx, control, station, &req).await?;
    if let Some(reason) = local(ctx, &row).await? {
        return Ok(answer(&req, LeaseDecision::Discard, Some(reason)));
    }
    // Step 4: the stored intent.
    intent(ctx, &req, &row).await
}

/// The coordinator branch of a lease (§6.3): the deck-local checks, the live
/// caller policy for the current binding, the target policy, the deck-local
/// checks again on a fresh row, and the stored intent.
async fn decide_coordinator(
    ctx: &DeckLink,
    control: &Control,
    station: &StationId,
    (req, row): (&CaptureLeaseRequest, RecordingRow),
) -> Result<CaptureLeaseAnswer, Refuse> {
    let discard = |reason| Ok(answer(req, LeaseDecision::Discard, Some(reason)));
    // Steps (a), (b) and (c): deck-local.
    let (context, binding) = match local_coordinator(ctx, &row).await? {
        Pass::Discard(reason) => return discard(reason),
        Pass::Held(context, binding) => (context, binding),
    };
    // Step (d): the live policy of the caller station for the current
    // binding. False: authority lost; uncertain: `forbidden`.
    let deadline = std::time::Instant::now() + LEASE_DEADLINE;
    match caller_policy(
        ctx,
        &context.caller_station,
        &context.caller_session,
        &binding,
        deadline,
    )
    .await
    {
        Ok(()) => {}
        Err(refusal) if refusal == cd_code::AUTHORITY_UNKNOWN => return Err(Refuse::Forbidden),
        Err(_) => return discard(StopReason::CoordinatorAuthorityLost),
    }
    // Step (e): the live policy of the target.
    if !policy::allowed(control, &row).await {
        return Err(Refuse::Forbidden);
    }
    // Step (f): deck-local again, on a fresh row.
    let row = load(ctx, control, station, req).await?;
    if let Pass::Discard(reason) = local_coordinator(ctx, &row).await? {
        return discard(reason);
    }
    // Step (g): the stored intent.
    intent(ctx, req, &row).await
}

/// The stored intent of a recording that passed every check (§6.3): `save`,
/// a stored `discard`, or `continue`.
async fn intent(
    ctx: &DeckLink,
    req: &CaptureLeaseRequest,
    row: &RecordingRow,
) -> Result<CaptureLeaseAnswer, Refuse> {
    match row.stop_intent {
        StopIntent::Save => Ok(answer(
            req,
            LeaseDecision::Save,
            Some(StopReason::OwnerStop),
        )),
        StopIntent::Discard => {
            let reason = row
                .stop_reason
                .filter(|r| r.lease_allowed(LeaseDecision::Discard))
                .ok_or(Refuse::Forbidden)?;
            Ok(answer(req, LeaseDecision::Discard, Some(reason)))
        }
        StopIntent::None => {
            if req.phase == LeasePhase::Record {
                started(ctx, row).await;
            }
            Ok(answer(req, LeaseDecision::Continue, None))
        }
    }
}

/// The first `record` lease that continues sets `started_at` once and writes
/// the `started` audit row (deduplicated by `started_at`).
async fn started(ctx: &DeckLink, row: &RecordingRow) {
    let station = row.capture.station_id.clone();
    let first = ctx
        .store
        .recording_started(station.clone(), row.capture.capture_id.clone())
        .await;
    match first {
        Ok(true) => {
            audit_row(
                &ctx.store,
                row,
                AuditResult::Ok,
                CaptureAudit {
                    capture_id: &row.capture.capture_id,
                    monitor: &row.capture.monitor,
                    seconds: row.requested_seconds,
                    stage: "started",
                    code: "ok",
                    stop_reason: None,
                    artifact: None,
                },
            )
            .await;
        }
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "desktop recording: start not marked"),
    }
}
