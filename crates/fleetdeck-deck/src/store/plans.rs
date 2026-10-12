//! The plan library (M3 6-6, DL-301; PLAN §10 `plans`, §16).
//!
//! [`record_plan`] runs inside the read-model transaction of
//! `apply_events`, so a plan row commits with the cursor that folds its
//! event: a replay after a link drop skips the event (it is at or below the
//! cursor), and a second fold of the same plan id keeps the first row
//! (`INSERT OR IGNORE`; the owner's status is never reset).

use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::plans::{DeckPlan, PlanStatus, bounded_plan, plan_key};
use fleetdeck_proto::{EventEnvelope, SessionSummary, StationId};
use rusqlite::{Transaction, params};

use super::{DeckError, DeckStore, Result, call};

/// Stores the plan of a `plan.proposed` event; other events do nothing.
pub(super) fn record_plan(
    tx: &Transaction<'_>,
    station: &StationId,
    e: &EventEnvelope,
    summary: &SessionSummary,
    now: i64,
) -> Result<()> {
    let EventBody::Known(Event::PlanProposed {
        plan_id,
        turn_id,
        markdown,
    }) = &e.event
    else {
        return Ok(());
    };
    let (text, truncated) = bounded_plan(markdown);
    tx.execute(
        "INSERT OR IGNORE INTO plans (plan_key, station_id, session_id, plan_id, turn_id,
             session_title, markdown, truncated, status, proposed_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'proposed', ?9, ?10)",
        params![
            plan_key(station, &e.session_id, plan_id),
            station.as_str(),
            e.session_id.as_str(),
            plan_id.as_str(),
            turn_id.as_ref().map(|t| t.as_str().to_owned()),
            summary.title,
            text,
            i64::from(truncated),
            e.at,
            now,
        ],
    )?;
    Ok(())
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(DeckPlan, String)> {
    let status: String = r.get(8)?;
    let turn: Option<String> = r.get(4)?;
    Ok((
        DeckPlan {
            id: r.get(0)?,
            station_id: StationId::from(r.get::<_, String>(1)?),
            session_id: r.get::<_, String>(2)?.into(),
            plan_id: r.get::<_, String>(3)?.into(),
            turn_id: turn.map(Into::into),
            session_title: r.get(5)?,
            markdown: r.get(6)?,
            truncated: r.get::<_, i64>(7)? != 0,
            status: PlanStatus::Proposed,
            proposed_at: r.get(9)?,
            updated_at: r.get(10)?,
        },
        status,
    ))
}

const COLUMNS: &str = "plan_key, station_id, session_id, plan_id, turn_id, session_title,
     markdown, truncated, status, proposed_at, updated_at";

impl DeckStore {
    /// The library, newest first. Archived plans only with `archived`.
    ///
    /// # Errors
    /// A database error, or [`DeckError::Corrupt`] for an unknown status.
    pub async fn plans(&self, archived: bool) -> Result<Vec<DeckPlan>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM plans WHERE ?1 OR status <> 'archived'
                 ORDER BY proposed_at DESC, plan_key"
            ))?;
            let rows = stmt
                .query_map([archived], row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|(mut plan, status)| {
                    plan.status = PlanStatus::parse(&status)
                        .ok_or_else(|| DeckError::Corrupt(format!("plan status {status}")))?;
                    Ok(plan)
                })
                .collect()
        })
        .await
    }

    /// Sets the owner's status of one plan and returns the row.
    ///
    /// # Errors
    /// [`DeckError::UnknownPlan`] for an unknown plan; a database error.
    pub async fn mark_plan(&self, id: String, status: PlanStatus) -> Result<DeckPlan> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let n = tx.execute(
                "UPDATE plans SET status = ?2, updated_at = ?3 WHERE plan_key = ?1",
                params![id, status.as_str(), now],
            )?;
            if n != 1 {
                return Err(DeckError::UnknownPlan(id));
            }
            let (mut plan, _) = tx.query_row(
                &format!("SELECT {COLUMNS} FROM plans WHERE plan_key = ?1"),
                [&id],
                row,
            )?;
            tx.commit()?;
            plan.status = status;
            Ok(plan)
        })
        .await
    }
}
