//! The transaction of `session.move_discard` (FG-9, design §12.3, "Abort",
//! step 2): by the state of the `move_in_runs` row, one transaction that
//! inserts the tombstone, changes the state with a conditional transition,
//! and deletes the target hold. Nothing is deleted from the disk here: the
//! caller deletes the files after the settle, from the identities that this
//! transaction returns.

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{MoveDiscardState, MoveInState};
use fleetdeck_proto::{Event, EventEnvelope};
use rusqlite::Connection;

use super::move_runs::{self, ImportedFile, Note, Staging, Tombstone};
use super::move_staging::{self, StagingState};
use super::session_holds;
use super::{NewEvent, Result, Store, call, log};

/// The code of a discard in `launching` (design §12.7).
pub const MOVE_CANCELLED: &str = "move_cancelled";
/// The session end reason of a discarded move.
pub const END_DISCARDED: &str = "move_discarded";

/// The files that a discard may delete after the settle, by identity.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cleanup {
    pub staging: Option<Staging>,
    pub imported: Vec<ImportedFile>,
}

/// What the discard transaction decided.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscardTx {
    pub state: MoveDiscardState,
    /// Set for `Discarded` only (an abandoned copy keeps every file).
    pub cleanup: Option<Cleanup>,
    /// The new session of the move, when the row exists.
    pub session_id: Option<fleetdeck_proto::SessionId>,
    pub events: Vec<EventEnvelope>,
}

fn provider_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Claude => "Claude",
        ProviderKind::Codex => "Codex",
        ProviderKind::Unknown => "The provider",
    }
}

/// The staging identity of a move that has no row (or none in its row).
fn staged_folder(conn: &Connection, move_id: &str) -> Result<Option<Staging>> {
    Ok(move_staging::get(conn, move_id)?
        .filter(|s| s.state != StagingState::Deleted)
        .map(|s| Staging {
            path: s.path,
            dev: s.dev,
            ino: s.ino,
        }))
}

/// The transaction of the discard. `station` is the name of this station (the
/// text of `move_cancelled`).
///
/// # Errors
/// A database error.
pub fn discard(conn: &Connection, move_id: &str, station: &str, now: i64) -> Result<DiscardTx> {
    let mut out = DiscardTx {
        state: MoveDiscardState::Discarded,
        cleanup: None,
        session_id: None,
        events: Vec::new(),
    };
    let Some(row) = move_runs::get_in(conn, move_id)? else {
        move_runs::insert_tombstone(conn, move_id, now)?;
        out.cleanup = Some(Cleanup {
            staging: staged_folder(conn, move_id)?,
            imported: Vec::new(),
        });
        return Ok(out);
    };
    out.session_id = Some(row.session_id.clone());
    match row.state {
        MoveInState::Staged | MoveInState::Imported | MoveInState::LaunchRefused => {
            move_runs::insert_tombstone(conn, move_id, now)?;
            let event = move_runs::advance_in(
                conn,
                move_id,
                row.state,
                MoveInState::Discarded,
                None,
                Tombstone::Ignore,
                now,
            )?;
            if let Some(event) = event {
                out.events.push(event);
            }
            session_holds::delete(conn, &row.session_id, move_id)?;
            out.events.push(log::append_one(
                conn,
                &row.session_id,
                NewEvent::from(Event::SessionEnded {
                    reason: END_DISCARDED.to_owned(),
                    exit_code: None,
                }),
                now,
            )?);
            out.cleanup = Some(Cleanup {
                staging: row.staging.clone().or(staged_folder(conn, move_id)?),
                imported: row.imported,
            });
        }
        MoveInState::Launching => {
            move_runs::insert_tombstone(conn, move_id, now)?;
            let source = row
                .source_station
                .as_deref()
                .unwrap_or("the source station");
            let text = format!(
                "The move was cancelled after {} started on {station}. This copy of the conversation was not used. The session stays on {source}.",
                provider_name(row.provider)
            );
            let event = move_runs::advance_in(
                conn,
                move_id,
                MoveInState::Launching,
                MoveInState::Abandoned,
                Some(Note {
                    code: MOVE_CANCELLED,
                    text: &text,
                }),
                Tombstone::Ignore,
                now,
            )?;
            out.events.extend(event);
            out.state = MoveDiscardState::Abandoned;
        }
        MoveInState::Abandoned => {
            move_runs::insert_tombstone(conn, move_id, now)?;
            out.state = MoveDiscardState::Abandoned;
        }
        MoveInState::Discarded => {
            move_runs::insert_tombstone(conn, move_id, now)?;
            // A repeat: the files are deleted again by identity (a missing
            // file is not an error).
            out.cleanup = Some(Cleanup {
                staging: row.staging.clone().or(staged_folder(conn, move_id)?),
                imported: row.imported,
            });
        }
        // The commit point has passed (or the state is not known): no
        // tombstone, no change.
        MoveInState::Resumed
        | MoveInState::Released
        | MoveInState::Confirmed
        | MoveInState::ResumeFailed
        | MoveInState::Failed
        | MoveInState::Unknown => out.state = MoveDiscardState::Resumed,
    }
    Ok(out)
}

impl Store {
    /// The transaction of `session.move_discard` (see [`discard`]); the events
    /// are published.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_discard_tx(&self, move_id: String, station: String) -> Result<DiscardTx> {
        let now = self.now();
        let done = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let done = discard(&tx, &move_id, &station, now)?;
            tx.commit()?;
            Ok(done)
        })
        .await?;
        if let Some(last) = done.events.last() {
            self.publish(last.station_seq);
        }
        Ok(done)
    }
}
