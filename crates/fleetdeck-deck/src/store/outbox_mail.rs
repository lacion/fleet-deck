//! The mail gate of the outbox (NV-17): a queued mail from a member goes
//! out only while the share of the member holds.

use fleetdeck_proto::CommandId;
use fleetdeck_proto::share::{ShareKind, ShareRole};
use rusqlite::{Connection, OptionalExtension, params};

use super::{DeckStore, Gate};
use crate::store::members::scope_of;
use crate::store::{Result, call};

impl DeckStore {
    /// The gate before a send or a resend of the command of a mail item
    /// (NV-17). When the mail is from a member whose share is gone, the
    /// command ends `failed` (`authority_lost`) and its item `dead`
    /// (`authority_lost`), in one transaction. Any other command is open.
    ///
    /// The mob-driver state of the member is not rechecked here.
    ///
    /// # Errors
    /// A database error (the caller stops this send and tries on the next
    /// tick).
    pub async fn gate_member_mail(&self, command: CommandId) -> Result<Gate> {
        let now = self.now_ms();
        let gate = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let outbox_id: Option<String> = tx
                .query_row(
                    "SELECT outbox_id FROM outbox WHERE command_id = ?1 AND origin = 'mail'",
                    [command.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(outbox_id) = outbox_id else {
                return Ok(Gate::Open);
            };
            if mail_still_valid(&tx, &outbox_id, now)? {
                return Ok(Gate::Open);
            }
            tx.execute(
                "UPDATE commands SET state = 'failed', reason = 'authority_lost', updated_at = ?2
                 WHERE command_id = ?1 AND state = 'pending'",
                params![command.as_str(), now],
            )?;
            tx.execute(
                "UPDATE outbox SET state = 'dead', reason = 'authority_lost', updated_at = ?2
                 WHERE command_id = ?1 AND state IN ('queued', 'sending')",
                params![command.as_str(), now],
            )?;
            tx.commit()?;
            Ok(Gate::Lost)
        })
        .await?;
        if gate == Gate::Lost {
            self.commands_changed();
        }
        Ok(gate)
    }
}

/// True when the item has no mail row, or its mail still has the authority
/// of its sender ([`member_mail_valid`]).
pub(super) fn mail_still_valid(conn: &Connection, outbox_id: &str, now: i64) -> Result<bool> {
    let mail: Option<(String, String, String)> = conn
        .query_row(
            "SELECT sender, station_id, session_id FROM mail WHERE outbox_id = ?1",
            [outbox_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match mail {
        Some((sender, station, session)) => {
            member_mail_valid(conn, &sender, &station, &session, now)
        }
        None => Ok(true),
    }
}

/// True when the sender of a mail may still mail the session (NV-17). A
/// sender that is not a member (`owner`, `token:<name>`) always may. A
/// member may while its share is active, has at least the collaborator
/// role and covers the session. A live login is not needed: the mail was
/// queued while the member was logged in.
pub(super) fn member_mail_valid(
    conn: &Connection,
    sender: &str,
    station: &str,
    session: &str,
    now: i64,
) -> Result<bool> {
    let Some(name) = sender.strip_prefix("member:") else {
        return Ok(true);
    };
    let mut stmt = conn.prepare(
        "SELECT s.kind, s.target, s.role FROM members m
         JOIN shares s ON s.share_id = m.share_id
         WHERE m.display_name = ?1 COLLATE NOCASE
           AND s.revoked_at IS NULL AND s.expires_at > ?2",
    )?;
    let rows = stmt
        .query_map(params![name, now], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (kind, target, role) in rows {
        let (Some(kind), Some(role)) = (ShareKind::parse(&kind), ShareRole::parse(&role)) else {
            continue;
        };
        if role >= ShareRole::Collaborator
            && scope_of(conn, kind, &target)?.covers(station, session)
        {
            return Ok(true);
        }
    }
    Ok(false)
}
