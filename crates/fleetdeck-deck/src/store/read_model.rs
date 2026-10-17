//! The session read model and the condensed timeline (PLAN §7.1, §7.2,
//! ADR-004 §9.4, G4).
//!
//! - **Events** apply only for the incarnation that the read model holds.
//!   An event at or below the cursor is a duplicate and is skipped, so a
//!   replay after a link drop or a deck restart applies each event once.
//!   The fold, the timeline rows and the new cursor commit in one
//!   transaction; the deck acknowledges only a committed cursor.
//! - **A snapshot batch** replaces the station's sessions (sessions that
//!   the batch does not name are removed) and sets the incarnation and the
//!   cursor to the watermark. A new incarnation also clears the timeline:
//!   the old `station_seq` values mean nothing in the new database.
//! - **The timeline** keeps every event except the streaming deltas, at
//!   most [`TIMELINE_CAP`] rows per session (DL-049).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::link::ReplayParams;
use fleetdeck_proto::{
    EventEnvelope, IncarnationId, SessionId, SessionSummary, StationId, card_status,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::stations::station_by_id;
use super::{DeckError, DeckStore, LiveEvents, Result, call, to_i64};

/// The timeline rows kept per session.
pub const TIMELINE_CAP: u32 = 500;

/// Event kinds that the condensed timeline drops (the board streams them
/// live; the completed message carries the full text).
const DROPPED_KINDS: [&str; 2] = ["message.delta", "reasoning.delta"];

pub use fleetdeck_proto::board::DeckSession;

/// What [`DeckStore::apply_events`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    /// Events folded (duplicates at or below the cursor are not counted).
    pub applied: usize,
    /// The committed cursor; the deck acknowledges this value.
    pub cursor: u64,
}

fn load_summary(
    tx: &Transaction<'_>,
    station: &StationId,
    session: &SessionId,
) -> Result<SessionSummary> {
    let text: Option<String> = tx
        .query_row(
            "SELECT summary FROM sessions WHERE station_id = ?1 AND session_id = ?2",
            params![station.as_str(), session.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    match text {
        Some(text) => Ok(serde_json::from_str(&text)?),
        None => Ok(SessionSummary::new(session.clone())),
    }
}

fn store_summary(
    tx: &Transaction<'_>,
    station: &StationId,
    summary: &SessionSummary,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO sessions (station_id, session_id, summary, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (station_id, session_id) DO UPDATE SET summary = excluded.summary,
             updated_at = excluded.updated_at",
        params![
            station.as_str(),
            summary.session_id.as_str(),
            serde_json::to_string(summary)?,
            now
        ],
    )?;
    // A session that can take input again gets the items that wait for it
    // (the outbox sender polls, so nothing wakes it).
    if super::outbox::can_take_input(summary.state) {
        super::outbox::release_held_tx(tx, station, &summary.session_id, now)?;
    }
    Ok(())
}

/// R2.2: a session that exited or was lost ends its open tasks as `blocked`.
fn end_worker_tasks(
    tx: &Transaction<'_>,
    station: &StationId,
    summary: &SessionSummary,
    now: i64,
) -> Result<()> {
    let cause = match summary.state {
        SessionState::Exited => "exited",
        SessionState::Lost => "lost",
        SessionState::Starting | SessionState::Running | SessionState::Stopping => return Ok(()),
    };
    super::fleet::block_open_tasks(tx, station, summary.session_id.as_str(), cause, now)?;
    Ok(())
}

fn append_timeline(tx: &Transaction<'_>, station: &StationId, e: &EventEnvelope) -> Result<()> {
    let kind = e.event.kind_name();
    if DROPPED_KINDS.contains(&kind) {
        return Ok(());
    }
    tx.execute(
        "INSERT OR IGNORE INTO timeline
             (station_id, station_seq, session_id, session_seq, kind, envelope)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            station.as_str(),
            to_i64(e.station_seq)?,
            e.session_id.as_str(),
            to_i64(e.session_seq)?,
            kind,
            serde_json::to_string(e)?,
        ],
    )?;
    Ok(())
}

fn cap_timeline(tx: &Transaction<'_>, station: &StationId, session: &SessionId) -> Result<()> {
    tx.execute(
        "DELETE FROM timeline WHERE station_id = ?1 AND session_id = ?2 AND station_seq <= (
             SELECT station_seq FROM timeline WHERE station_id = ?1 AND session_id = ?2
             ORDER BY station_seq DESC LIMIT 1 OFFSET ?3)",
        params![station.as_str(), session.as_str(), TIMELINE_CAP],
    )?;
    Ok(())
}

fn set_cursor(
    conn: &Connection,
    station: &StationId,
    incarnation: &IncarnationId,
    cursor: u64,
) -> Result<()> {
    conn.execute(
        "UPDATE stations SET incarnation_id = ?2, cursor = ?3 WHERE station_id = ?1",
        params![station.as_str(), incarnation.as_str(), to_i64(cursor)?],
    )?;
    Ok(())
}

impl DeckStore {
    /// The resync request for `station`: its incarnation and cursor (G4).
    ///
    /// # Errors
    /// [`DeckError::UnknownStation`], or a database error.
    pub async fn replay_params(&self, station: StationId) -> Result<ReplayParams> {
        let rec = self
            .station(station.clone())
            .await?
            .ok_or_else(|| DeckError::UnknownStation(station.0))?;
        Ok(ReplayParams {
            incarnation: rec.incarnation,
            after: rec.cursor,
        })
    }

    /// Folds `events` of `incarnation` into the read model in one
    /// transaction and returns the committed cursor.
    ///
    /// # Errors
    /// [`DeckError::IncarnationMismatch`] when the read model holds another
    /// incarnation (or none: the first stream starts with a snapshot);
    /// [`DeckError::UnknownStation`]; or a database error.
    pub async fn apply_events(
        &self,
        station: StationId,
        incarnation: IncarnationId,
        events: Vec<EventEnvelope>,
    ) -> Result<Applied> {
        let now = self.now_ms();
        let mail_ids = self
            .inner
            .conflict_mail
            .load(Ordering::Relaxed)
            .then(|| Arc::clone(&self.inner.ids));
        // The stations whose model help is on (DL-167): a conflict mail to
        // one of their sessions waits for a generated sentence.
        let assist: BTreeSet<StationId> = self
            .station_infos()
            .into_iter()
            .filter(|(_, (info, _))| info.assist_cap.is_some())
            .map(|(id, _)| id)
            .collect();
        let (id, inc) = (station.clone(), incarnation.clone());
        let (applied, folded, jobs) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let rec = station_by_id(&tx, &station)?
                .ok_or_else(|| DeckError::UnknownStation(station.0.clone()))?;
            if rec.incarnation.as_ref() != Some(&incarnation) {
                return Err(DeckError::IncarnationMismatch {
                    station,
                    expected: rec.incarnation,
                    got: incarnation,
                });
            }
            let mut cursor = rec.cursor;
            let mut applied = 0;
            let mut touched = BTreeSet::new();
            let mut folded = Vec::new();
            let jobs = std::cell::RefCell::new(Vec::new());
            for e in events {
                if e.station_seq <= cursor {
                    continue;
                }
                let mut summary = load_summary(&tx, &station, &e.session_id)?;
                summary.apply(&e);
                store_summary(&tx, &station, &summary, now)?;
                end_worker_tasks(&tx, &station, &summary, now)?;
                append_timeline(&tx, &station, &e)?;
                super::plans::record_plan(&tx, &station, &e, &summary, now)?;
                super::artifacts::record_artifact(&tx, &station, &e)?;
                super::land_queue::record_land(&tx, &station, &e, now)?;
                super::grants::record_lifecycle(&tx, &station, &e)?;
                let mail = mail_ids.as_ref().map(|ids| super::conflicts::MailContext {
                    ids: ids.as_ref(),
                    now,
                    assist: &assist,
                    jobs: &jobs,
                });
                super::conflicts::record_touches(&tx, &station, &e, &summary, mail.as_ref())?;
                touched.insert(e.session_id.clone());
                cursor = e.station_seq;
                applied += 1;
                folded.push(e);
            }
            for session in &touched {
                cap_timeline(&tx, &station, session)?;
            }
            set_cursor(&tx, &station, &incarnation, cursor)?;
            tx.commit()?;
            Ok((Applied { applied, cursor }, folded, jobs.into_inner()))
        })
        .await?;
        if !jobs.is_empty() {
            self.inner
                .sentence_jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend(jobs);
        }
        if !folded.is_empty() {
            self.board_changed();
            self.publish_live(LiveEvents {
                station_id: id,
                incarnation: inc,
                events: folded,
            });
        }
        Ok(applied)
    }

    /// Replaces the read model of `station` by a complete snapshot batch at
    /// `watermark` (one transaction). A new incarnation clears the timeline.
    ///
    /// # Errors
    /// [`DeckError::UnknownStation`], [`DeckError::StaleSnapshot`], or a
    /// database error.
    pub async fn apply_snapshot(
        &self,
        station: StationId,
        incarnation: IncarnationId,
        watermark: u64,
        sessions: Vec<SessionSummary>,
    ) -> Result<()> {
        self.apply_snapshot_with(station, incarnation, watermark, sessions, None, None)
            .await
    }

    /// [`Self::apply_snapshot`] with the coordinator binding projection of
    /// a coordinator-capable snapshot (ADR-006 §6.1, test 25e): the
    /// projection, the summaries and the cursor commit in one transaction,
    /// before the link acknowledges the snapshot. `None` is an ordinary
    /// summary snapshot and changes no grant.
    ///
    /// `current` tells whether the link of the snapshot is still the current
    /// link of the station. The transaction asks it before any write. A
    /// snapshot of a superseded link, or an older snapshot of the stored
    /// incarnation, changes nothing (NV-12).
    ///
    /// # Errors
    /// [`DeckError::UnknownStation`], [`DeckError::Superseded`],
    /// [`DeckError::StaleSnapshot`], or a database error.
    pub async fn apply_snapshot_with(
        &self,
        station: StationId,
        incarnation: IncarnationId,
        watermark: u64,
        sessions: Vec<SessionSummary>,
        coordinator: Option<Vec<fleetdeck_proto::CoordinatorBindingState>>,
        current: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let rec = station_by_id(&tx, &station)?
                .ok_or_else(|| DeckError::UnknownStation(station.0.clone()))?;
            // Nothing is written before these two checks (NV-12).
            if current.as_ref().is_some_and(|is_current| !is_current()) {
                return Err(DeckError::Superseded);
            }
            if rec.incarnation.as_ref() == Some(&incarnation) && rec.cursor > watermark {
                return Err(DeckError::StaleSnapshot);
            }
            if rec.incarnation.as_ref() != Some(&incarnation) {
                tx.execute(
                    "DELETE FROM timeline WHERE station_id = ?1",
                    [station.as_str()],
                )?;
                // The sequence numbers of the new incarnation start again.
                tx.execute(
                    "DELETE FROM read_cursors WHERE station_id = ?1",
                    [station.as_str()],
                )?;
            }
            tx.execute(
                "DELETE FROM sessions WHERE station_id = ?1",
                [station.as_str()],
            )?;
            let mut named = BTreeSet::new();
            for summary in &sessions {
                store_summary(&tx, &station, summary, now)?;
                end_worker_tasks(&tx, &station, summary, now)?;
                named.insert(summary.session_id.as_str().to_owned());
            }
            // A session that the batch does not name is gone (R2.2).
            super::fleet::block_unlisted_tasks(&tx, &station, &named, now)?;
            // The timeline of a session that the batch does not name goes too.
            let mut stmt =
                tx.prepare("SELECT DISTINCT session_id FROM timeline WHERE station_id = ?1")?;
            let old: Vec<String> = stmt
                .query_map([station.as_str()], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            for id in old.iter().filter(|id| !named.contains(*id)) {
                tx.execute(
                    "DELETE FROM timeline WHERE station_id = ?1 AND session_id = ?2",
                    params![station.as_str(), id],
                )?;
            }
            // The review rows of a session that the batch does not name go
            // too (DL-179). No foreign key joins them to `sessions`: this
            // path deletes and inserts the session rows.
            super::review::delete_unlisted(&tx, &station, &named)?;
            if let Some(projection) = &coordinator {
                super::grants::apply_projection(&tx, &station, now, projection)?;
            }
            set_cursor(&tx, &station, &incarnation, watermark)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.board_changed();
        Ok(())
    }

    /// Every card of the board, with the status projection of §7.2.
    ///
    /// # Errors
    /// A database error.
    pub async fn sessions(&self) -> Result<Vec<DeckSession>> {
        let rows: Vec<(String, String)> = call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(
                "SELECT s.station_id, s.summary FROM sessions s
                 JOIN stations t ON t.station_id = s.station_id
                 ORDER BY t.name, s.session_id",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (station, text) in rows {
            let station_id = StationId::from(station);
            let summary: SessionSummary = serde_json::from_str(&text)?;
            let status = card_status(&summary, self.is_reachable(&station_id));
            out.push(DeckSession {
                station_id,
                summary,
                status,
                unread: None,
                project_id: None,
                review_rev: None,
            });
        }
        Ok(out)
    }

    /// The condensed timeline of one session, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn timeline(
        &self,
        station: StationId,
        session: SessionId,
    ) -> Result<Vec<EventEnvelope>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT envelope FROM timeline WHERE station_id = ?1 AND session_id = ?2
                 ORDER BY station_seq",
            )?;
            let rows: Vec<String> = stmt
                .query_map(params![station.as_str(), session.as_str()], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut out = Vec::with_capacity(rows.len());
            for text in rows {
                out.push(serde_json::from_str(&text)?);
            }
            Ok(out)
        })
        .await
    }

    /// The condensed timeline of one session after `after`, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn timeline_after(
        &self,
        station: StationId,
        session: SessionId,
        after: u64,
    ) -> Result<Vec<EventEnvelope>> {
        let after = to_i64(after)?;
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT envelope FROM timeline
                 WHERE station_id = ?1 AND session_id = ?2 AND station_seq > ?3
                 ORDER BY station_seq",
            )?;
            let rows: Vec<String> = stmt
                .query_map(params![station.as_str(), session.as_str(), after], |r| {
                    r.get(0)
                })?
                .collect::<rusqlite::Result<_>>()?;
            let mut out = Vec::with_capacity(rows.len());
            for text in rows {
                out.push(serde_json::from_str(&text)?);
            }
            Ok(out)
        })
        .await
    }
}
