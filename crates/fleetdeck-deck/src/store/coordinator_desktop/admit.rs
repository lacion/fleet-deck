//! The admission transaction of a coordinator desktop mutation (R13 P6 §6.2
//! step 9, §7.1, §8). One write transaction does the grant recheck, the
//! duplicate check, the attempt budget, the rates, the budgets and the commit,
//! in this order. No network await happens inside it.

use std::sync::Arc;

use fleetdeck_proto::coordinator_desktop_text::{code, limits};
use rusqlite::{Transaction, params};

use super::ops::{
    self, Admission, Charges, DesktopOp, NewOp, OpClass, OpState, OpTool, StoredCharges,
};
use super::rate;
use crate::store::grant_scopes::expire_idle;
use crate::store::grants::{GrantClass, GrantState, get_grant};
use crate::store::{DeckError, DeckStore, Inner, Result, call};

/// A refusal that stores nothing.
fn refuse(code: &'static str) -> Result<Admission> {
    Ok(Admission::Rejected {
        code,
        stored: false,
    })
}

/// The code of a stored refusal as a constant. A stored refusal is only
/// ever one of the budget codes.
fn stored_code(text: &str) -> Result<&'static str> {
    match text {
        code::DESKTOP_CAP_REACHED => Ok(code::DESKTOP_CAP_REACHED),
        code::CAPTURE_CAP_REACHED => Ok(code::CAPTURE_CAP_REACHED),
        code::RECORDING_CAP_REACHED => Ok(code::RECORDING_CAP_REACHED),
        code::RECORDING_SECONDS_REACHED => Ok(code::RECORDING_SECONDS_REACHED),
        _ => Err(DeckError::Corrupt(
            "a stored desktop refusal has an unknown code".to_owned(),
        )),
    }
}

/// A budget refusal: the key, the payload hash and the code are stored and
/// one attempt is counted, so that a replay returns the same code.
fn refuse_stored(
    tx: Transaction<'_>,
    op: &NewOp,
    code: &'static str,
    now: i64,
) -> Result<Admission> {
    tx.execute(
        "INSERT INTO coordinator_desktop_rejects (grant_id, request_key, payload_hash, code, at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![op.grant_id, op.request_key, op.payload_hash, code, now],
    )?;
    tx.execute(
        "UPDATE tokens SET desktop_attempts = desktop_attempts + 1 WHERE grant_id = ?1",
        [&op.grant_id],
    )?;
    tx.commit()?;
    Ok(Admission::Rejected { code, stored: true })
}

fn needs(class: OpClass) -> GrantClass {
    match class {
        OpClass::Desktop => GrantClass::Desktop,
        OpClass::Capture => GrantClass::Capture,
        OpClass::Input => GrantClass::Input,
    }
}

fn admit(tx: Transaction<'_>, inner: &Inner, op: NewOp, now: i64) -> Result<Admission> {
    // R13 P7 task F: no input path is built yet. An input tool is refused
    // before anything is read or stored (the deck task adds the admission).
    if matches!(
        op.tool,
        OpTool::KeyPress | OpTool::TypeText | OpTool::PointerMove
    ) {
        return refuse(code::DESKTOP_UNSUPPORTED);
    }
    // 1. The grant, checked again in this transaction, also for a replay.
    let ctx = &op.ctx;
    let Some(grant) = get_grant(&tx, &op.grant_id)? else {
        return refuse(code::NOT_GRANTED);
    };
    if grant.station_id != ctx.caller_station
        || grant.session_id != ctx.caller_session
        || grant.state != GrantState::Active
        || now > grant.expires_at
    {
        return refuse(code::NOT_GRANTED);
    }
    match &grant.binding {
        Some(b) if b.binding_id == ctx.binding_id && b.generation == ctx.generation => {}
        _ => return refuse(code::BINDING_STALE),
    }
    if grant
        .last_used_at
        .is_some_and(|t| now.saturating_sub(t) > grant.idle_ttl_ms)
    {
        // Consumes the transaction: commits the expiry.
        expire_idle(tx, inner, &grant.grant_id, now)?;
        return refuse(code::NOT_GRANTED);
    }
    if !grant.classes.contains(&needs(op.class)) || !grant.stations.contains(&op.target_station) {
        return refuse(code::NOT_GRANTED);
    }
    // 3. The request key: the same payload replays, another one conflicts.
    //    Only a grant that holds now sees a stored result (§6.2).
    if let Some(row) = ops::find_by_key(&tx, &op.grant_id, &op.request_key)? {
        return Ok(if row.payload_hash == op.payload_hash {
            Admission::Existing(row)
        } else {
            Admission::Conflict
        });
    }
    if let Some((hash, stored)) = ops::find_reject(&tx, &op.grant_id, &op.request_key)? {
        if hash != op.payload_hash {
            return Ok(Admission::Conflict);
        }
        return Ok(Admission::Rejected {
            code: stored_code(&stored)?,
            stored: true,
        });
    }
    // 4. The attempts of the grant.
    if grant.desktop_attempts >= limits::ATTEMPTS_MAX {
        return refuse(code::REQUEST_BUDGET_REACHED);
    }
    // 5. The rates. A refusal charges nothing.
    let grant_scope = format!("grant:{}", op.grant_id);
    let session_scope = format!("session:{}/{}", ctx.caller_station, ctx.caller_session);
    // A recording shares the screenshot bucket (R13 P5b-2 §13.2).
    let capturing = matches!(op.tool, OpTool::Screenshot | OpTool::RecordMonitor);
    let limited = rate::count(&tx, &grant_scope, rate::MUTATION, now)?
        >= limits::MUTATIONS_PER_WINDOW
        || (capturing
            && rate::count(&tx, &grant_scope, rate::SCREENSHOT, now)?
                >= limits::SCREENSHOTS_PER_WINDOW)
        || (op.tool == OpTool::Notify
            && rate::count(&tx, &session_scope, rate::NOTIFY, now)? >= limits::NOTIFY_PER_WINDOW);
    if limited {
        return refuse(code::RATE_LIMITED);
    }
    // 6. The budgets (a cap of `None` is 0). A refusal is stored.
    let recording = op.tool == OpTool::RecordMonitor;
    // The seconds of a recording are always set (the call layer builds them
    // from the validated argument).
    let seconds = match (recording, op.seconds) {
        (true, Some(seconds)) => seconds,
        (true, None) => {
            return Err(DeckError::Invalid(
                "a recording operation has no seconds".to_owned(),
            ));
        }
        (false, _) => 0,
    };
    if grant.desktop_used >= grant.desktop_cap.unwrap_or(0) {
        return refuse_stored(tx, &op, code::DESKTOP_CAP_REACHED, now);
    }
    if capturing && grant.capture_used >= grant.capture_cap.unwrap_or(0) {
        return refuse_stored(tx, &op, code::CAPTURE_CAP_REACHED, now);
    }
    if recording && grant.recording_used.saturating_add(1) > grant.recording_cap {
        return refuse_stored(tx, &op, code::RECORDING_CAP_REACHED, now);
    }
    if recording && grant.recording_second_used.saturating_add(seconds) > grant.recording_second_cap
    {
        return refuse_stored(tx, &op, code::RECORDING_SECONDS_REACHED, now);
    }
    // 7. Accept.
    let charged = Charges {
        desktop: 1,
        capture: u32::from(capturing),
    };
    // Only a recording stores its two counters in the JSON (§13.2); the other
    // tools keep the JSON that they always had.
    let stored_charges = if recording {
        serde_json::to_string(&StoredCharges {
            desktop: charged.desktop,
            capture: charged.capture,
            recording: 1,
            recording_seconds: seconds,
        })?
    } else {
        serde_json::to_string(&charged)?
    };
    tx.execute(
        "INSERT INTO coordinator_desktop_ops
         (operation_id, grant_id, request_key, payload_hash, tool, class, binding_id,
          generation, caller_station, caller_session, target_station, action_hash, resource,
          state, result, code, charged, created_at, updated_at, seconds, input_length)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'prepared', NULL, NULL,
                 ?14, ?15, ?15, ?16, ?17)",
        params![
            op.operation_id,
            op.grant_id,
            op.request_key,
            op.payload_hash,
            op.tool.as_str(),
            op.class.as_str(),
            ctx.binding_id,
            ctx.generation,
            ctx.caller_station.as_str(),
            ctx.caller_session.0,
            op.target_station.as_str(),
            op.action_hash,
            serde_json::to_string(&op.resource)?,
            stored_charges,
            now,
            op.seconds,
            op.input_length,
        ],
    )?;
    tx.execute(
        "UPDATE tokens SET desktop_used = desktop_used + 1, capture_used = capture_used + ?2,
             desktop_attempts = desktop_attempts + 1, last_used_at = ?3,
             recording_used = recording_used + ?4,
             recording_second_used = recording_second_used + ?5
         WHERE grant_id = ?1",
        params![
            op.grant_id,
            charged.capture,
            now,
            u32::from(recording),
            seconds
        ],
    )?;
    rate::charge(&tx, &grant_scope, rate::MUTATION, now)?;
    if capturing {
        rate::charge(&tx, &grant_scope, rate::SCREENSHOT, now)?;
    }
    if op.tool == OpTool::Notify {
        rate::charge(&tx, &session_scope, rate::NOTIFY, now)?;
    }
    tx.commit()?;
    Ok(Admission::Accepted(DesktopOp {
        operation_id: op.operation_id,
        grant_id: op.grant_id,
        request_key: op.request_key,
        payload_hash: op.payload_hash,
        tool: op.tool,
        class: op.class,
        binding_id: op.ctx.binding_id,
        generation: op.ctx.generation,
        caller_station: op.ctx.caller_station,
        caller_session: op.ctx.caller_session,
        target_station: op.target_station,
        action_hash: op.action_hash,
        resource: op.resource,
        state: OpState::Prepared,
        result: None,
        code: None,
        charged,
        created_at: now,
        updated_at: now,
        seconds: op.seconds,
        input_length: op.input_length,
    }))
}

impl DeckStore {
    /// The one admission transaction of a desktop mutation (see the module
    /// doc): the duplicate check, the grant recheck, the attempt budget, the
    /// rates, the budgets and the commit.
    ///
    /// # Errors
    /// A database error.
    pub async fn desktop_admit(&self, op: NewOp, now: i64) -> Result<Admission> {
        let inner = Arc::clone(&self.inner);
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            admit(tx, &inner, op, now)
        })
        .await
    }
}
