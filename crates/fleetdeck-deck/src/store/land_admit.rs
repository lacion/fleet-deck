//! Admission of a land (design §11.2 "Admission (atomic)"; FG-8): one
//! `BEGIN IMMEDIATE` transaction takes the first `queued` land that no
//! `running` land blocks, checks the request again, pins the plan, and
//! stores the outbox row. The partial unique indexes make a second `running`
//! row of one project or one lock key impossible.

use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::fleet_git::LandState;
use fleetdeck_proto::{SessionId, StationId};
use rusqlite::{ErrorCode, TransactionBehavior, params};

use super::coordinator::{Check, recheck};
use super::land_queue::{COLS, LandRow, end_row, read};
use super::land_ready::{Env, check};
use super::land_text;
use super::{DeckStore, LinkFeature, Result, call};

/// The `failed` code of a request that no longer passes the checks.
const NOT_READY: &str = "land_not_ready";

impl DeckStore {
    /// Admits the first land that can run, and ends the queued lands that
    /// fail their checks. Returns whether anything changed; the caller
    /// repeats it until it returns `false`.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn land_admit_next(&self) -> Result<bool> {
        let now = self.now_ms();
        let features = self.land_features();
        let local_ok = self.land_local_ok();
        let inner = std::sync::Arc::clone(&self.inner);
        let changed = call(&self.inner.db, move |conn| {
            let fleet = inner.fleet_env();
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let candidates: Vec<LandRow> = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {COLS} FROM land_queue q
                     WHERE q.state = 'queued'
                       AND NOT EXISTS (SELECT 1 FROM land_queue r
                                        WHERE r.state = 'running'
                                          AND (r.project_id = q.project_id OR r.lock_key = q.lock_key))
                     ORDER BY q.created_at, q.land_id"
                ))?;
                let rows = stmt.query_map([], |r| Ok(read(r)))?;
                rows.map(|r| r?).collect::<Result<_>>()?
            };
            let mut changed = false;
            for row in candidates {
                let feature = features
                    .get(&row.station_id)
                    .copied()
                    .unwrap_or(LinkFeature::Down);
                // The link must be up with the feature; else the row waits.
                if feature != LinkFeature::Negotiated {
                    continue;
                }
                let station = StationId::from(row.station_id.clone());
                let session = SessionId::from(row.session_id.clone());
                if let Some(authority) = &row.authority {
                    if now > row.authority_expires_at.unwrap_or(i64::MAX) {
                        let text = land_text::authority_expired(&row);
                        end_row(&tx, &row.land_id, LandState::Failed, ("authority_expired", &text), now)?;
                        changed = true;
                        continue;
                    }
                    match recheck(&tx, &fleet, authority, (&station, Some(&session)), now)? {
                        Check::Valid => {}
                        Check::Wait => continue,
                        Check::Lost(reason) => {
                            let text = land_text::authority_lost(&row, reason);
                            end_row(&tx, &row.land_id, LandState::Failed, ("authority_lost", &text), now)?;
                            changed = true;
                            continue;
                        }
                    }
                }
                let env = Env { feature, local_ok: local_ok.clone() };
                let ready = match check(&tx, &env, &row.station_id, &row.session_id)? {
                    Ok(ready) => ready,
                    Err(refusal) => {
                        end_row(&tx, &row.land_id, LandState::Failed, (NOT_READY, &refusal.message), now)?;
                        changed = true;
                        continue;
                    }
                };
                if ready.project.project_id != row.project_id {
                    let text = "The project of the session changed.";
                    end_row(&tx, &row.land_id, LandState::Failed, (NOT_READY, text), now)?;
                    changed = true;
                    continue;
                }
                let payload = serde_json::to_string(&SessionCommand::Land {
                    session_id: session,
                    land_id: row.land_id.clone(),
                    plan: Box::new(ready.plan.clone()),
                })?;
                let admitted = tx.execute(
                    "UPDATE land_queue SET state = 'running', plan = ?2, settings_revision = ?3,
                         attempt = 1, started_at = ?4
                     WHERE land_id = ?1 AND state = 'queued'",
                    params![
                        row.land_id,
                        serde_json::to_string(&ready.plan)?,
                        i64::try_from(ready.plan.settings_revision).unwrap_or(i64::MAX),
                        now
                    ],
                );
                match admitted {
                    Ok(_) => {}
                    // Another running row of the project: nothing changes.
                    Err(rusqlite::Error::SqliteFailure(e, _))
                        if e.code == ErrorCode::ConstraintViolation =>
                    {
                        return Ok(false);
                    }
                    Err(e) => return Err(e.into()),
                }
                tx.execute(
                    "INSERT INTO land_outbox (command_id, land_id, station_id, payload, state,
                         next_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?5)",
                    params![row.command_id, row.land_id, row.station_id, payload, now],
                )?;
                changed = true;
                break;
            }
            tx.commit()?;
            Ok(changed)
        })
        .await?;
        if changed {
            self.board_changed();
        }
        Ok(changed)
    }
}
