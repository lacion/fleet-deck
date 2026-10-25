//! The provider evidence that settles an effect (PLAN §6.4, ADR-001 §3).
//! Moved from `effects.rs` for the module size rule (B18 E8).

use fleetdeck_proto::event::{EffortSetOutcome, InputState};
use fleetdeck_proto::{Event, EventBody, EventEnvelope};
use fleetdeck_providers::adapter::answered_request;
use rusqlite::{Connection, params};

use super::Result;
use super::effects::{REASON_REFUSED, REASON_SUPERSEDED};

/// Folds provider evidence into the outbox: called for every appended
/// event, inside its transaction. It appends no events itself.
pub(super) fn observe(conn: &Connection, envelope: &EventEnvelope, now: i64) -> Result<()> {
    let EventBody::Known(event) = &envelope.event else {
        return Ok(());
    };
    let session = envelope.session_id.as_str();
    let open = "state IN ('claimed', 'outcome_unknown')";
    let confirm = |filter: &str, args: &[&dyn rusqlite::ToSql]| -> Result<()> {
        let sql = format!(
            "UPDATE effects SET state = 'confirmed', updated_at = ?1
             WHERE session_id = ?2 AND {open} AND {filter}
             RETURNING kind"
        );
        let mut all: Vec<&dyn rusqlite::ToSql> = vec![&now, &session];
        all.extend_from_slice(args);
        let mut stmt = conn.prepare(&sql)?;
        let kinds = stmt
            .query_map(all.as_slice(), |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for kind in kinds {
            // Crash tests: the evidence and the confirmation are in this
            // uncommitted transaction (M2 P2-8).
            crate::fault::hit(crate::fault::FaultPoint::BeforeConfirm, &kind);
        }
        Ok(())
    };
    match event {
        // A message answer starts as a provider input (GATE-M2 P2-b): its
        // start confirms the answer effect of its request.
        Event::InputUpdated {
            input_id,
            state: InputState::Started,
            ..
        } if answered_request(input_id).is_some() => {
            if let Some(request) = answered_request(input_id) {
                confirm("kind = 'answer' AND correlation = ?3", &[&request.as_str()])?;
            }
        }
        Event::InputUpdated {
            input_id,
            state: InputState::Refused,
            ..
        } if answered_request(input_id).is_some() => {
            if let Some(request) = answered_request(input_id) {
                conn.execute(
                    "UPDATE effects SET state = 'retired_not_dispatched', reason = ?4, updated_at = ?1
                     WHERE session_id = ?2 AND state IN ('claimed', 'outcome_unknown')
                       AND kind = 'answer' AND correlation = ?3",
                    params![now, session, request.as_str(), REASON_REFUSED],
                )?;
            }
        }
        Event::InputUpdated {
            input_id,
            state: InputState::Started,
            ..
        } => confirm(
            "kind IN ('prompt', 'steer', 'follow_up') AND correlation = ?3",
            &[&input_id.as_str()],
        )?,
        Event::InputUpdated {
            input_id,
            state: InputState::Refused,
            ..
        } => {
            conn.execute(
                "UPDATE effects SET state = 'retired_not_dispatched', reason = ?4, updated_at = ?1
                 WHERE session_id = ?2 AND state IN ('queued', 'claimed', 'outcome_unknown')
                   AND kind IN ('prompt', 'steer', 'follow_up') AND correlation = ?3",
                params![now, session, input_id.as_str(), REASON_REFUSED],
            )?;
        }
        Event::SessionStarted { .. } => confirm("kind IN ('start', 'resume')", &[])?,
        Event::ToolOutput { node_id, .. } | Event::ToolCompleted { node_id, .. } => confirm(
            "kind = 'answer' AND correlation IN
                 (SELECT request_id FROM runtime_requests WHERE session_id = ?2 AND node_id = ?3)",
            &[&node_id.as_str()],
        )?,
        // Only the provider's answer to this effect's own control request
        // settles an interrupt (ADR-001 §3.1); a turn that ends is no
        // evidence, it can end for any reason.
        Event::InterruptResult {
            control_id: Some(effect_id),
            accepted: true,
            ..
        } => confirm(
            "kind = 'interrupt' AND effect_id = ?3",
            &[&effect_id.as_str()],
        )?,
        Event::InterruptResult {
            control_id: Some(effect_id),
            accepted: false,
            ..
        } => {
            conn.execute(
                "UPDATE effects SET state = 'retired_not_dispatched', reason = ?4, updated_at = ?1
                 WHERE session_id = ?2 AND state IN ('claimed', 'outcome_unknown')
                   AND kind = 'interrupt' AND effect_id = ?3",
                params![now, session, effect_id.as_str(), REASON_REFUSED],
            )?;
        }
        Event::ModeChanged { mode } => {
            confirm("kind = 'set_mode' AND correlation = ?3", &[&mode.as_str()])?;
        }
        Event::ModelChanged { model } => {
            confirm(
                "kind = 'set_model' AND correlation = ?3",
                &[&model.as_str()],
            )?;
        }
        // A set-effort effect settles only on the provider's own answer with
        // its effect id (B18 Phase 2, design §9.2 item 4). `accepted`
        // confirms first; a later verification result matches no open row.
        // `effort.changed` confirms nothing.
        Event::EffortSetResult {
            control_id: Some(effect_id),
            outcome:
                EffortSetOutcome::Accepted
                | EffortSetOutcome::Applied
                | EffortSetOutcome::Adjusted
                | EffortSetOutcome::Unverified,
            ..
        } => confirm(
            "kind = 'set_effort' AND effect_id = ?3",
            &[&effect_id.as_str()],
        )?,
        Event::EffortSetResult {
            control_id: Some(effect_id),
            outcome: outcome @ (EffortSetOutcome::Refused | EffortSetOutcome::Superseded),
            ..
        } => {
            let reason = if *outcome == EffortSetOutcome::Refused {
                REASON_REFUSED
            } else {
                REASON_SUPERSEDED
            };
            conn.execute(
                "UPDATE effects SET state = 'retired_not_dispatched', reason = ?4, updated_at = ?1
                 WHERE session_id = ?2 AND state IN ('claimed', 'outcome_unknown')
                   AND kind = 'set_effort' AND effect_id = ?3",
                params![now, session, effect_id.as_str(), reason],
            )?;
        }
        Event::SessionEnded { .. } => confirm("kind = 'stop'", &[])?,
        _ => {}
    }
    Ok(())
}
