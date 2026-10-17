//! The owner actions and the status poll on the land queue (design §11.2;
//! FG-8): cancel, "Stop checking", forget, and the `fleet_git.land_status`
//! answer. The rows and the fold are in [`super::land_queue`].

use fleetdeck_proto::fleet_git::{LandState, LandStatus, LandStatusAnswer, LandWrite};
use rusqlite::{Connection, params};

use super::land_queue::{
    LandOutcome, Progress, apply_progress, end_row, fence_row, find, state_name, unknown_land, view,
};
use super::land_ready::LandRefusal;
use super::land_text;
use super::{DeckError, DeckStore, Result, call, to_i64, to_u64};

fn refused<T>(code: &'static str, message: &str) -> Result<std::result::Result<T, LandRefusal>> {
    Ok(Err(LandRefusal {
        code,
        message: message.to_owned(),
    }))
}

impl DeckStore {
    /// Cancels a `queued` land (design §11.2 "Cancel").
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn land_cancel(&self, land_id: String) -> Result<LandOutcome> {
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(row) = find(&tx, &land_id)? else {
                return Ok(Err(unknown_land()));
            };
            match row.state.as_str() {
                "queued" => {
                    tx.execute(
                        "UPDATE land_queue SET state = 'cancelled', ended_at = ?2
                         WHERE land_id = ?1 AND state = 'queued'",
                        params![land_id, now],
                    )?;
                }
                "running" => {
                    return refused(
                        "land_running",
                        "This land is running. It cannot be cancelled.",
                    );
                }
                _ => return refused("land_not_queued", "Only a queued land can be cancelled."),
            }
            let row = find(&tx, &land_id)?.ok_or_else(|| DeckError::Corrupt("land row".into()))?;
            let v = view(&tx, row)?;
            tx.commit()?;
            Ok(Ok(v))
        })
        .await?;
        if out.is_ok() {
            self.board_changed();
        }
        Ok(out)
    }

    /// "Stop checking": records the request (design §11.2, M3). The row stays
    /// `running`; the abandon sender asks the station for the fence.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn land_abandon_request(&self, land_id: String) -> Result<LandOutcome> {
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(row) = find(&tx, &land_id)? else {
                return Ok(Err(unknown_land()));
            };
            let uncertain =
                row.state == "running" && (row.reconciling || row.phase.as_deref() == Some("lost"));
            let repeat =
                row.state == "running" && row.abandon_state.as_deref() == Some("requested");
            if !uncertain && !repeat {
                return refused(
                    "land_not_uncertain",
                    "This land is not waiting for a result.",
                );
            }
            tx.execute(
                "UPDATE land_queue SET abandon_state = 'requested', abandon_next_at = ?2
                 WHERE land_id = ?1 AND abandon_state IS NULL",
                params![land_id, now],
            )?;
            let row = find(&tx, &land_id)?.ok_or_else(|| DeckError::Corrupt("land row".into()))?;
            let v = view(&tx, row)?;
            tx.commit()?;
            Ok(Ok(v))
        })
        .await?;
        Ok(out)
    }

    /// Frees a `requested` row of a removed station (design §11.2).
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn land_forget(&self, land_id: String) -> Result<LandOutcome> {
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(row) = find(&tx, &land_id)? else {
                return Ok(Err(unknown_land()));
            };
            if row.state != "running" || row.abandon_state.as_deref() != Some("requested") {
                return refused(
                    "land_not_uncertain",
                    "This land is not waiting for a result.",
                );
            }
            let paired: i64 = tx.query_row(
                "SELECT COUNT(*) FROM stations WHERE station_id = ?1",
                [&row.station_id],
                |r| r.get(0),
            )?;
            if paired > 0 {
                return refused("station_paired", "The station is still paired.");
            }
            let text = land_text::station_removed(&row, &row.station_id);
            end_row(
                &tx,
                &land_id,
                LandState::Unresolved,
                ("station_removed", &text),
                now,
            )?;
            let row = find(&tx, &land_id)?.ok_or_else(|| DeckError::Corrupt("land row".into()))?;
            let v = view(&tx, row)?;
            tx.commit()?;
            Ok(Ok(v))
        })
        .await?;
        if out.is_ok() {
            self.board_changed();
        }
        Ok(out)
    }

    /// Applies a `fleet_git.land_status` answer (decision Q11). Returns
    /// whether a `running` row changed.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn land_apply_status(
        &self,
        land_id: String,
        answer: LandStatusAnswer,
    ) -> Result<bool> {
        let now = self.now_ms();
        let changed = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(row) = find(&tx, &land_id)? else {
                return Ok(false);
            };
            let changed = match answer {
                LandStatusAnswer::Known(status) => {
                    apply_progress(&tx, &land_id, &Progress::of_status(&status), now)?
                }
                LandStatusAnswer::Fenced => fence_row(&tx, &row, (LandWrite::Unknown, None), now)?,
                LandStatusAnswer::Unknown => {
                    let accepted: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM land_outbox WHERE land_id = ?1 AND state = 'accepted'",
                        [&land_id],
                        |r| r.get(0),
                    )?;
                    row.state == "running"
                        && accepted > 0
                        && tx.execute(
                            "UPDATE land_queue SET phase = 'lost'
                             WHERE land_id = ?1 AND state = 'running'",
                            [&land_id],
                        )? > 0
                }
            };
            tx.commit()?;
            Ok(changed)
        })
        .await?;
        if changed {
            self.board_changed();
        }
        Ok(changed)
    }

    /// Fills the outcome fields of a row that a fence ended with a terminal
    /// `run_state`, from the station's status (design §11.2, revision 4).
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn land_fill_outcome(&self, land_id: String, status: LandStatus) -> Result<()> {
        call(&self.inner.db, move |conn| {
            let p = Progress::of_status(&status);
            conn.execute(
                "UPDATE land_queue SET source_oid = ?2, base_oid = ?3, rebased_oid = ?4,
                     branch_published_oid = ?5, default_published_oid = ?6, pr_url = ?7,
                     files = ?8, code = ?9, message = ?10, attempt = ?11
                 WHERE land_id = ?1 AND state = ?12",
                params![
                    land_id,
                    p.oids[0],
                    p.oids[1],
                    p.oids[2],
                    p.oids[3],
                    p.oids[4],
                    p.pr_url,
                    serde_json::to_string(&p.files)?,
                    p.code,
                    p.text,
                    i64::from(p.attempt),
                    state_name(p.state),
                ],
            )?;
            Ok(())
        })
        .await?;
        self.board_changed();
        Ok(())
    }

    /// The `running` rows with their station, for the status poll.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_running(&self) -> Result<Vec<(String, String)>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn
                .prepare("SELECT land_id, station_id FROM land_queue WHERE state = 'running' ORDER BY created_at")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await
    }
}

/// The counter of a row's abandon sends (a backoff input).
pub(super) fn bump_abandon(conn: &Connection, land_id: &str, now: i64) -> Result<()> {
    let n: i64 = conn.query_row(
        "SELECT abandon_attempts FROM land_queue WHERE land_id = ?1",
        [land_id],
        |r| r.get(0),
    )?;
    let sends = to_u64(n + 1)?;
    conn.execute(
        "UPDATE land_queue SET abandon_attempts = ?2, abandon_next_at = ?3 WHERE land_id = ?1",
        params![
            land_id,
            to_i64(sends)?,
            now + super::home_attempts::backoff_ms(sends)
        ],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "land_actions_tests.rs"]
mod tests;
