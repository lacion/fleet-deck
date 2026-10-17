//! The land outbox (design §11.2 "Outbox"; FG-8): one row for each admitted
//! land, with the `session.land` command fixed at admission. The sender
//! resends it with the same `command_id` and a backoff of 2, 4, 8, ... 60 s,
//! so a lost answer never makes a second land (the station keeps the
//! receipt). A coordinator land passes the authority gate of the design
//! table at the first send and at each resend.

use fleetdeck_proto::command::{CommandOrigin, SessionCommand};
use fleetdeck_proto::fleet_git::LandState;
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_proto::{SessionId, StationId};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::coordinator::{Check, recheck};
use super::home_attempts::backoff_ms;
use super::land_queue::{COLS, LandRow, end_row, find, read};
use super::land_text;
use super::{DeckError, DeckStore, Result, call, to_u64};

/// What the sender needs for one send.
#[derive(Debug, Clone)]
pub struct DueSend {
    pub command_id: String,
    pub land_id: String,
    pub station_id: String,
    pub command: SessionCommand,
    pub authority: Option<CommandAuthority>,
    pub origin: CommandOrigin,
}

/// What the authority gate decides for one send.
#[derive(Debug, Clone)]
pub enum SendGate {
    /// Send now (the send is counted and the next time is set).
    Send(Box<DueSend>),
    /// Not now: the backoff goes on.
    Wait,
    /// The land ended `failed` at the first send, or it is not running.
    Ended,
    /// A resend lost its authority: the earlier send may have started a run,
    /// so the deck asks the station before it ends the row (design §11.2).
    Verify {
        send: Box<DueSend>,
        code: &'static str,
        reason: String,
    },
}

/// The deck-local part of `coordinator.authority_check` for a land (design
/// §11.2, B3): the station asks about a land command. The command must be a
/// `running` land of `station` with exactly this authority and this target
/// session; the target rule of ADR-008 §4.2 gives `target_excluded`.
pub(super) fn authority_local(
    tx: &rusqlite::Connection,
    station: &StationId,
    check: &fleetdeck_proto::mutation::AuthorityCheck,
    grant: &super::grants::Grant,
    now: i64,
) -> Result<std::result::Result<super::coordinator::AuthorityLocal, &'static str>> {
    let found: Option<LandRow> = tx
        .query_row(
            &format!(
                "SELECT {COLS} FROM land_queue
                 WHERE command_id = ?1 AND station_id = ?2 AND state = 'running'"
            ),
            params![check.command_id.as_str(), station.as_str()],
            |r| Ok(read(r)),
        )
        .optional()?
        .transpose()?;
    let asked = CommandAuthority {
        grant_id: check.grant_id.clone(),
        caller_station: check.caller_station.clone(),
        caller_session: check.caller_session.clone(),
        generation: check.generation,
    };
    let Some(row) = found.filter(|r| {
        r.authority.as_ref() == Some(&asked) && r.session_id == check.target_session.as_str()
    }) else {
        return Ok(Err("unknown_command"));
    };
    let session = SessionId::from(row.session_id);
    let excluded =
        super::coordinator::strict_refusal(tx, grant, (station, &session), (false, now))?.is_some();
    Ok(Ok(super::coordinator::AuthorityLocal {
        target_excluded: excluded,
    }))
}

impl DeckStore {
    /// The command ids of the `pending` outbox rows that are due.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_outbox_due(&self) -> Result<Vec<String>> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT command_id FROM land_outbox WHERE state = 'pending' AND next_at <= ?1
                 ORDER BY next_at, command_id",
            )?;
            let rows = stmt
                .query_map([now], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await
    }

    /// The earliest `next_at` of a `pending` row.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_outbox_next_due(&self) -> Result<Option<i64>> {
        call(&self.inner.db, |conn| {
            Ok(conn.query_row(
                "SELECT MIN(next_at) FROM land_outbox WHERE state = 'pending'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
    }

    /// The gate before a send or a resend (design §11.2). A send is counted
    /// here, in the transaction that decides it.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn land_send_gate(&self, command_id: String) -> Result<SendGate> {
        let now = self.now_ms();
        let inner = std::sync::Arc::clone(&self.inner);
        let gate = call(&self.inner.db, move |conn| {
            let fleet = inner.fleet_env();
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let pending: Option<(i64, String)> = tx
                .query_row(
                    "SELECT attempts, payload FROM land_outbox
                     WHERE command_id = ?1 AND state = 'pending'",
                    [&command_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((attempts, payload)) = pending else {
                return Ok(SendGate::Ended);
            };
            let attempts = to_u64(attempts)?;
            let row: LandRow = tx
                .query_row(
                    &format!("SELECT {COLS} FROM land_queue WHERE command_id = ?1"),
                    [&command_id],
                    |r| Ok(read(r)),
                )
                .optional()?
                .transpose()?
                .ok_or_else(|| DeckError::Corrupt("land outbox without a land".into()))?;
            if row.state != "running" {
                tx.execute(
                    "UPDATE land_outbox SET state = 'rejected', updated_at = ?2 WHERE command_id = ?1",
                    params![command_id, now],
                )?;
                tx.commit()?;
                return Ok(SendGate::Ended);
            }
            let due = Box::new(DueSend {
                command_id: command_id.clone(),
                land_id: row.land_id.clone(),
                station_id: row.station_id.clone(),
                command: serde_json::from_str(&payload)?,
                authority: row.authority.clone(),
                origin: if row.authority.is_some() {
                    CommandOrigin::Coordinator
                } else {
                    CommandOrigin::Board
                },
            });
            // The authority of a coordinator land (design §11.2, B3).
            let mut lost: Option<(&'static str, String)> = None;
            let mut wait = false;
            if let Some(authority) = &row.authority {
                if now > row.authority_expires_at.unwrap_or(i64::MAX) {
                    lost = Some(("authority_expired", String::new()));
                } else {
                    let station = StationId::from(row.station_id.clone());
                    let session = SessionId::from(row.session_id.clone());
                    match recheck(&tx, &fleet, authority, (&station, Some(&session)), now)? {
                        Check::Valid => {}
                        Check::Wait => wait = true,
                        Check::Lost(reason) => lost = Some(("authority_lost", reason.to_owned())),
                    }
                }
            }
            let next = now + backoff_ms(attempts + 1);
            let bump = |tx: &rusqlite::Transaction<'_>, count: bool| -> Result<()> {
                tx.execute(
                    "UPDATE land_outbox SET attempts = attempts + ?2, next_at = ?3, updated_at = ?4
                     WHERE command_id = ?1",
                    params![command_id, i64::from(count), next, now],
                )?;
                Ok(())
            };
            let gate = match lost {
                Some((code, reason)) if attempts == 0 => {
                    let text = if code == "authority_expired" {
                        land_text::authority_expired(&row)
                    } else {
                        land_text::authority_lost(&row, &reason)
                    };
                    end_row(&tx, &row.land_id, LandState::Failed, (code, &text), now)?;
                    SendGate::Ended
                }
                Some((code, reason)) => {
                    bump(&tx, true)?;
                    SendGate::Verify {
                        send: due,
                        code,
                        reason,
                    }
                }
                None if wait => {
                    bump(&tx, false)?;
                    SendGate::Wait
                }
                None => {
                    bump(&tx, true)?;
                    SendGate::Send(due)
                }
            };
            tx.commit()?;
            Ok(gate)
        })
        .await?;
        if matches!(gate, SendGate::Ended) {
            self.board_changed();
        }
        Ok(gate)
    }

    /// The station answered with an accepted receipt.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_outbox_accepted(&self, command_id: String, receipt: String) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE land_outbox SET state = 'accepted', receipt = ?2, updated_at = ?3
                 WHERE command_id = ?1 AND state = 'pending'",
                params![command_id, receipt, now],
            )?;
            Ok(())
        })
        .await
    }

    /// The station stored a rejected receipt: the outbox row is `rejected`
    /// and the land ends `failed` with the station's code (decision Q5).
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn land_outbox_rejected(
        &self,
        command_id: String,
        (code, message): (String, String),
        receipt: String,
    ) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let row: Option<LandRow> = tx
                .query_row(
                    &format!("SELECT {COLS} FROM land_queue WHERE command_id = ?1"),
                    [&command_id],
                    |r| Ok(read(r)),
                )
                .optional()?
                .transpose()?;
            if let Some(row) = row {
                end_row(&tx, &row.land_id, LandState::Failed, (&code, &message), now)?;
            }
            tx.execute(
                "UPDATE land_outbox SET state = 'rejected', receipt = ?2, updated_at = ?3
                 WHERE command_id = ?1",
                params![command_id, receipt, now],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.board_changed();
        Ok(())
    }

    /// The station fenced an unknown land after a resend lost its authority:
    /// the row ends `failed` with the authority code (design §11.2).
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn land_verify_end(
        &self,
        land_id: String,
        code: &'static str,
        reason: String,
    ) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if let Some(row) = find(&tx, &land_id)? {
                let text = if code == "authority_expired" {
                    land_text::authority_expired(&row)
                } else {
                    land_text::authority_lost(&row, &reason)
                };
                end_row(&tx, &land_id, LandState::Failed, (code, &text), now)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.board_changed();
        Ok(())
    }

    /// The `requested` abandon rows that are due: land id and station id.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_abandon_due(&self) -> Result<Vec<(String, String)>> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT land_id, station_id FROM land_queue
                 WHERE state = 'running' AND abandon_state = 'requested' AND abandon_next_at <= ?1
                 ORDER BY abandon_next_at, land_id",
            )?;
            let rows = stmt
                .query_map([now], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await
    }

    /// The earliest `abandon_next_at` of a `requested` row.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_abandon_next_due(&self) -> Result<Option<i64>> {
        call(&self.inner.db, |conn| {
            Ok(conn.query_row(
                "SELECT MIN(abandon_next_at) FROM land_queue
                 WHERE state = 'running' AND abandon_state = 'requested'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
    }

    /// Counts one abandon send and sets the next time (2, 4, 8, ... 60 s).
    ///
    /// # Errors
    /// A database error.
    pub async fn land_abandon_sent(&self, land_id: String) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            super::land_actions::bump_abandon(conn, &land_id, now)
        })
        .await
    }

    /// The station answered `fenced` to `fleet_git.land_abandon`: one
    /// transaction frees the row (design §11.2, revision 4).
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn land_abandon_fenced(
        &self,
        land_id: String,
        write: fleetdeck_proto::fleet_git::LandWrite,
        run_state: LandState,
    ) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if let Some(row) = find(&tx, &land_id)? {
                super::land_queue::fence_row(&tx, &row, (write, Some(run_state)), now)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.board_changed();
        Ok(())
    }
}
