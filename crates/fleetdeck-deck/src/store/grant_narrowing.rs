//! What a narrowing or a revoke of a grant does to the fleet commands that it
//! admitted (MCP P4b §7.1, §7.2). Every fleet command stores the full class
//! set that admitted it (`required_classes`, a JSON array). The station's
//! pre-execution check answers `class_removed` when the grant lost one of
//! them ([`required_hold`]); a narrowing or a revoke fails the pending items
//! at once ([`fail_removed_tx`]). Ordinary items carry no authority and are
//! never touched (§7.3).

use std::collections::BTreeSet;

use fleetdeck_proto::fleet_git::LandState;
use rusqlite::{Connection, params};

use crate::store::Result;
use crate::store::grants::GrantClass;
use crate::store::land_queue::{end_row, find};
use crate::store::land_text;
use crate::store::session_moves::MoveState;
use crate::store::session_moves_steps::{MovePatch, authority_text, set_in_tx};

/// The moves that have not reached their commit point.
const PRE_COMMIT: [MoveState; 4] = [
    MoveState::Holding,
    MoveState::Exporting,
    MoveState::Importing,
    MoveState::Starting,
];

/// Why the pending items fail: a narrowing writes `authority_removed`; a
/// revoke writes what the existing gates write for a revoked grant
/// (`authority_lost`, `grant_revoked`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Removal {
    /// The class change route (P4b-3b) uses it; only the tests do until then.
    #[allow(dead_code)]
    Narrowed,
    Revoked,
}

/// Does `grant` still hold every class of `required`? An item without the
/// set (admitted before V46) holds; a set that does not parse does not.
pub(crate) fn required_hold(required: Option<&str>, grant: &BTreeSet<GrantClass>) -> bool {
    let Some(text) = required else {
        return true;
    };
    match serde_json::from_str::<BTreeSet<GrantClass>>(text) {
        Ok(set) => set.is_subset(grant),
        Err(_) => false,
    }
}

/// Fails the pending fleet items of `grant_id` whose class set is no longer
/// within `classes_now` (the code of `cause`), in the caller's transaction:
/// the queued and sent outbox items, the queued lands (and the running ones
/// with no send yet) and the moves before their commit point.
pub(crate) fn fail_removed_tx(
    tx: &Connection,
    grant_id: &str,
    classes_now: &BTreeSet<GrantClass>,
    cause: Removal,
    now: i64,
) -> Result<()> {
    let code = match cause {
        Removal::Narrowed => "authority_removed",
        Removal::Revoked => "authority_lost",
    };
    let items = rows(
        tx,
        "SELECT outbox_id, command_id, required_classes FROM outbox
         WHERE state IN ('queued', 'sending') AND json_extract(authority, '$.grant_id') = ?1",
        grant_id,
    )?;
    for (outbox_id, command_id, required) in items {
        if required_hold(required.as_deref(), classes_now) {
            continue;
        }
        tx.execute(
            "UPDATE outbox SET state = 'dead', reason = ?3, updated_at = ?2
             WHERE outbox_id = ?1 AND state IN ('queued', 'sending')",
            params![outbox_id, now, code],
        )?;
        if let Some(command_id) = command_id {
            tx.execute(
                "UPDATE commands SET state = 'failed', reason = ?3, updated_at = ?2
                 WHERE command_id = ?1 AND state = 'pending'",
                params![command_id, now, code],
            )?;
        }
    }
    let lands = rows(
        tx,
        "SELECT land_id, NULL, required_classes FROM land_queue
         WHERE json_extract(authority, '$.grant_id') = ?1
           AND (state = 'queued'
                OR (state = 'running' AND EXISTS (
                    SELECT 1 FROM land_outbox o
                    WHERE o.land_id = land_queue.land_id AND o.state = 'pending'
                      AND o.attempts = 0)))",
        grant_id,
    )?;
    for (land_id, _, required) in lands {
        if required_hold(required.as_deref(), classes_now) {
            continue;
        }
        let text = match (cause, find(tx, &land_id)?) {
            (Removal::Revoked, Some(row)) => land_text::authority_lost(&row, "grant_revoked"),
            _ => "the grant no longer allows this land".to_owned(),
        };
        end_row(tx, &land_id, LandState::Failed, (code, &text), now)?;
    }
    let moves = rows(
        tx,
        "SELECT move_id, NULL, required_classes FROM session_moves
         WHERE json_extract(authority, '$.grant_id') = ?1
           AND state IN ('holding', 'exporting', 'importing', 'starting')",
        grant_id,
    )?;
    for (move_id, _, required) in moves {
        if required_hold(required.as_deref(), classes_now) {
            continue;
        }
        let text = match cause {
            Removal::Revoked => authority_text(code, "grant_revoked"),
            Removal::Narrowed => "the grant no longer allows this move".to_owned(),
        };
        set_in_tx(
            tx,
            &move_id,
            &PRE_COMMIT,
            MoveState::Aborting,
            &MovePatch::note(code, &text),
            now,
        )?;
    }
    Ok(())
}

/// The id, the optional command id and the stored class set of each item.
type Item = (String, Option<String>, Option<String>);

fn rows(tx: &Connection, sql: &str, grant_id: &str) -> Result<Vec<Item>> {
    let mut stmt = tx.prepare(sql)?;
    let found = stmt
        .query_map([grant_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<Item>>>()?;
    Ok(found)
}
