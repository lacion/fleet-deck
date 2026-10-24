//! Synchronous event-log operations on the DB thread. Every function works
//! inside the caller's transaction (or one statement), so a command decision
//! and a provider append use the same code.

use fleetdeck_proto::command::check_native_session_id;
use fleetdeck_proto::event::{Event, RawRef};
use fleetdeck_proto::{EventBody, EventEnvelope, PROTO_MAX, SessionId, SessionSummary};
use rusqlite::{Connection, OptionalExtension, params};

use super::{
    NewEvent, NewSession, PruneReport, ReadResult, Result, StoreConfig, StoreError, to_i64, to_u64,
};

fn text<T: serde::Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

/// A unit enum's serde name (`"running"`).
fn name<T: serde::Serialize>(value: &T) -> Result<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(s) => Ok(s),
        other => Ok(other.to_string()),
    }
}

/// The last allocated `station_seq` (0 when none).
pub(super) fn head(conn: &Connection) -> Result<u64> {
    let next: i64 = conn.query_row("SELECT station_seq_next FROM meta WHERE id = 1", [], |r| {
        r.get(0)
    })?;
    Ok(to_u64(next)?.saturating_sub(1))
}

pub(super) fn insert_session(conn: &Connection, s: &NewSession, now: i64) -> Result<()> {
    let mut summary = SessionSummary::new(s.session_id.clone());
    summary.provider = Some(s.provider);
    summary.provider_instance = Some(s.provider_instance.clone());
    summary.cwd = Some(s.cwd.clone());
    summary.branch.clone_from(&s.branch);
    // The repository of the workspace for the conflict radar (M3 6-7, DL-306).
    summary.repo =
        fleetdeck_core::git::repo_of(std::path::Path::new(&s.cwd)).map(|(root, origin)| {
            fleetdeck_proto::session::RepoInfo {
                root: root.to_string_lossy().into_owned(),
                origin,
            }
        });
    summary.title.clone_from(&s.title);
    summary.mode.clone_from(&s.mode);
    summary.updated_at = now;
    let inserted = conn.execute(
        "INSERT INTO sessions (session_id, provider, provider_instance, cwd, worktree, mode,
             state, offline_approval_policy, summary, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
         ON CONFLICT (session_id) DO NOTHING",
        params![
            s.session_id.as_str(),
            name(&s.provider)?,
            s.provider_instance.as_str(),
            s.cwd,
            s.worktree,
            s.mode,
            name(&summary.state)?,
            text(&s.offline_approval_policy)?,
            text(&summary)?,
            now,
        ],
    )?;
    if inserted == 0 {
        return Err(StoreError::SessionExists(s.session_id.clone()));
    }
    Ok(())
}

/// Allocates both sequence numbers, stores the event and folds it into the
/// session projection.
pub(super) fn append_one(
    conn: &Connection,
    session_id: &SessionId,
    event: NewEvent,
    now: i64,
) -> Result<EventEnvelope> {
    append_at(
        conn,
        session_id,
        event,
        now,
        super::requests::RequestTime::Now,
    )
}

/// [`append_one`] for output with a known read time: a runtime request
/// that the event opens gets `time` as its request time (ADR-005 §6).
pub(super) fn append_at(
    conn: &Connection,
    session_id: &SessionId,
    event: NewEvent,
    now: i64,
    time: super::requests::RequestTime,
) -> Result<EventEnvelope> {
    let (last_session_seq, summary): (i64, String) = conn
        .query_row(
            "SELECT session_seq, summary FROM sessions WHERE session_id = ?1",
            [session_id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| StoreError::UnknownSession(session_id.clone()))?;
    let mut summary: SessionSummary = serde_json::from_str(&summary)?;
    let mut event = event;
    // The station is the source of the title and the branch (DL-061).
    if let EventBody::Known(Event::SessionStarted {
        title,
        branch,
        repo,
        ..
    }) = &mut event.event
    {
        title.clone_from(&summary.title);
        branch.clone_from(&summary.branch);
        *repo = summary.repo.clone().map(Box::new);
    }
    let station_seq = head(conn)? + 1;
    let session_seq = to_u64(last_session_seq)? + 1;
    let envelope = EventEnvelope {
        station_seq,
        session_id: session_id.clone(),
        session_seq,
        at: now,
        proto_version: PROTO_MAX,
        raw_ref: event.raw_ref,
        event: event.event,
    };
    conn.execute(
        "UPDATE meta SET station_seq_next = ?1 WHERE id = 1",
        [to_i64(station_seq + 1)?],
    )?;
    conn.execute(
        "INSERT INTO events (station_seq, session_id, session_seq, kind, payload, raw_ref, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            to_i64(station_seq)?,
            session_id.as_str(),
            to_i64(session_seq)?,
            envelope.event.kind_name(),
            text(&envelope.event)?,
            envelope.raw_ref.as_ref().map(text).transpose()?,
            now,
        ],
    )?;
    let before = summary.state;
    summary.apply(&envelope);
    note_end(conn, session_id, (before, summary.state), now)?;
    conn.execute(
        "UPDATE sessions SET session_seq = ?2, summary = ?3, state = ?4, mode = ?5,
             updated_at = ?6
         WHERE session_id = ?1",
        params![
            session_id.as_str(),
            to_i64(session_seq)?,
            text(&summary)?,
            name(&summary.state)?,
            summary.mode,
            now,
        ],
    )?;
    // The native resume identity (Claude `session_id`, Codex `thread_id`)
    // that a later `session.resume` continues (DL-118).
    if let EventBody::Known(Event::SessionStarted { provider_refs, .. }) = &envelope.event
        && let Some(native) = provider_refs
            .get("session_id")
            .or_else(|| provider_refs.get("thread_id"))
            .and_then(serde_json::Value::as_str)
    {
        // Only a valid id is stored: a later handover puts it into a command
        // line (NV-23).
        if check_native_session_id(native).is_ok() {
            conn.execute(
                "UPDATE sessions SET native_session_id = ?2 WHERE session_id = ?1",
                params![session_id.as_str(), native],
            )?;
        } else {
            tracing::warn!(
                session = %session_id,
                "the provider reported a native session id that is not valid; it is not stored"
            );
        }
    }
    // The request table first: an answer is confirmed through its node.
    super::requests::observe(conn, &envelope, now, time)?;
    super::effects::observe(conn, &envelope, now)?;
    super::shell_observe::observe(conn, &envelope, now)?;
    super::workflow_state::observe(conn, &envelope, now)?;
    Ok(envelope)
}

/// Keeps `session_git.ended_at` (FG-3, design §6.3 item 1): a change of the
/// state to `exited` or `lost` sets it to `now` (each such change, so the
/// latest end counts); a change from those to a running state clears it. A
/// session without a `session_git` row is not touched.
fn note_end(
    conn: &Connection,
    session_id: &SessionId,
    (before, after): (
        fleetdeck_proto::event::SessionState,
        fleetdeck_proto::event::SessionState,
    ),
    now: i64,
) -> Result<()> {
    use fleetdeck_proto::event::SessionState::{Exited, Lost};
    if before == after {
        return Ok(());
    }
    let ended = |s| matches!(s, Exited | Lost);
    if ended(after) {
        conn.execute(
            "UPDATE session_git SET ended_at = ?2 WHERE session_id = ?1",
            params![session_id.as_str(), now],
        )?;
    } else if ended(before) {
        conn.execute(
            "UPDATE session_git SET ended_at = NULL WHERE session_id = ?1",
            params![session_id.as_str()],
        )?;
    }
    Ok(())
}

fn envelope_of(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<(i64, String, i64, String, Option<String>, i64)> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
    ))
}

pub(super) fn read_after(conn: &mut Connection, cursor: u64, limit: usize) -> Result<ReadResult> {
    let tx = conn.transaction()?;
    let head = head(&tx)?;
    if cursor > head {
        return Err(StoreError::CursorAhead { cursor, head });
    }
    let pruned: i64 = tx.query_row("SELECT pruned_through FROM meta WHERE id = 1", [], |r| {
        r.get(0)
    })?;
    if cursor < to_u64(pruned)? {
        return Ok(ReadResult::SnapshotRequired { head });
    }
    let mut stmt = tx.prepare(
        "SELECT station_seq, session_id, session_seq, payload, raw_ref, at
         FROM events WHERE station_seq > ?1 ORDER BY station_seq LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        params![to_i64(cursor)?, i64::try_from(limit).unwrap_or(i64::MAX)],
        envelope_of,
    )?;
    let mut events = Vec::new();
    for row in rows {
        let (station_seq, session_id, session_seq, payload, raw_ref, at) = row?;
        let raw_ref: Option<RawRef> = raw_ref.map(|r| serde_json::from_str(&r)).transpose()?;
        events.push(EventEnvelope {
            station_seq: to_u64(station_seq)?,
            session_id: SessionId::from(session_id),
            session_seq: to_u64(session_seq)?,
            at,
            proto_version: PROTO_MAX,
            raw_ref,
            event: serde_json::from_str::<EventBody>(&payload)?,
        });
    }
    drop(stmt);
    tx.commit()?;
    Ok(ReadResult::Events { events, head })
}

pub(super) fn summary(conn: &Connection, session_id: &SessionId) -> Result<Option<SessionSummary>> {
    let text: Option<String> = conn
        .query_row(
            "SELECT summary FROM sessions WHERE session_id = ?1",
            [session_id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(text.map(|t| serde_json::from_str(&t)).transpose()?)
}

/// The session projections of a snapshot, oldest first: every session that
/// has not exited and then the newest exited ones, `limit` at most (DL-051).
pub(super) fn summaries(conn: &Connection, limit: u64) -> Result<Vec<SessionSummary>> {
    let mut stmt = conn.prepare(
        "SELECT summary FROM (
             SELECT summary, created_at, session_id FROM sessions
             ORDER BY state IN ('exited', 'lost'), created_at DESC, session_id DESC
             LIMIT ?1)
         ORDER BY created_at, session_id",
    )?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let rows = stmt.query_map([limit], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(serde_json::from_str(&row?)?);
    }
    Ok(out)
}

pub(super) fn ack(conn: &mut Connection, deck_key: &str, seq: u64, now: i64) -> Result<()> {
    let tx = conn.transaction()?;
    let paired: bool = tx
        .query_row(
            "SELECT 1 FROM pairing WHERE deck_key = ?1",
            [deck_key],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !paired {
        return Err(StoreError::UnknownDeck(deck_key.to_owned()));
    }
    let head = head(&tx)?;
    if seq > head {
        return Err(StoreError::CursorAhead { cursor: seq, head });
    }
    tx.execute(
        "INSERT INTO deck_acks (deck_key, station_seq, at) VALUES (?1, ?2, ?3)
         ON CONFLICT (deck_key) DO UPDATE
             SET station_seq = max(station_seq, excluded.station_seq), at = excluded.at",
        params![deck_key, to_i64(seq)?, now],
    )?;
    tx.commit()?;
    Ok(())
}

/// Removes the longest prefix of events that every paired deck acknowledged
/// and that is older than the window; the removed prefix stays contiguous, so
/// `pruned_through` decides between a replay and a snapshot.
///
/// The same transaction removes the finished history rows that are older than
/// the history retention (NV-02, NV-10, NV-26, NV-28): coordinator outbox rows,
/// shell effects, effects, runtime requests, dead processes and artifact
/// suggestions. A row that recovery or a live row can still need stays.
///
/// An `outcome_unknown` effect is never removed: the station could not prove
/// what happened, and the row is the only record of that doubt.
///
/// These tables are never pruned: `sessions`, `turn_checkpoints`, `reverts`,
/// `shims` and the `shim_*` tables, `setup_runs`, `worktree_ops` and
/// `artifacts`. Other rows refer to them, or the user reads them as history.
pub(super) fn prune(conn: &mut Connection, now: i64, config: StoreConfig) -> Result<PruneReport> {
    let tx = conn.transaction()?;
    let head = head(&tx)?;
    let decks: i64 = tx.query_row("SELECT count(*) FROM pairing", [], |r| r.get(0))?;
    // Without a paired deck, nobody needs the events beyond the window.
    let acked = if decks == 0 {
        head
    } else {
        let min: i64 = tx.query_row(
            "SELECT min(coalesce(a.station_seq, 0))
             FROM pairing p LEFT JOIN deck_acks a ON a.deck_key = p.deck_key",
            [],
            |r| r.get(0),
        )?;
        to_u64(min)?
    };
    let cutoff = now.saturating_sub(config.event_retention_ms);
    let first_young: Option<i64> = tx.query_row(
        "SELECT min(station_seq) FROM events WHERE at >= ?1",
        [cutoff],
        |r| r.get(0),
    )?;
    let young_limit = match first_young {
        Some(seq) => to_u64(seq)?.saturating_sub(1),
        None => head,
    };
    // The analytics cursor fence (B29): an event that the cursor did not count
    // yet is never deleted.
    let counted = super::analytics::next_seq(&tx)?.saturating_sub(1);
    let through = acked.min(young_limit).min(counted);
    let pruned: i64 = tx.query_row("SELECT pruned_through FROM meta WHERE id = 1", [], |r| {
        r.get(0)
    })?;
    let mut report = PruneReport {
        pruned_through: to_u64(pruned)?,
        ..PruneReport::default()
    };
    if through > report.pruned_through {
        report.events = tx.execute(
            "DELETE FROM events WHERE station_seq <= ?1",
            [to_i64(through)?],
        )?;
        tx.execute(
            "UPDATE meta SET pruned_through = ?1 WHERE id = 1",
            [to_i64(through)?],
        )?;
        report.pruned_through = through;
    }
    report.receipts = tx.execute(
        "DELETE FROM commands WHERE at < ?1",
        [now.saturating_sub(config.receipt_retention_ms)],
    )?;
    report.history = prune_history_rows(&tx, now.saturating_sub(config.history_retention_ms))?;
    tx.commit()?;
    Ok(report)
}

/// Removes the finished history rows older than `cutoff`, children before
/// parents so that no foreign key blocks the prune (NV-26). Returns the number
/// of removed rows.
fn prune_history_rows(tx: &rusqlite::Transaction<'_>, cutoff: i64) -> Result<usize> {
    let mut removed = 0;
    removed += tx.execute(
        "DELETE FROM coordinator_outbox
         WHERE state IN ('done', 'failed', 'ambiguous') AND updated_at < ?1",
        [cutoff],
    )?;
    removed += tx.execute(
        // A handover effect stays: the recovery of a terminal hold reads it
        // (a hold without a pid and an `outcome_unknown` effect keeps the
        // native session held).
        "DELETE FROM shell_effects
         WHERE state IN ('confirmed', 'failed', 'retired_not_dispatched', 'outcome_unknown')
           AND op <> 'handover' AND updated_at < ?1",
        [cutoff],
    )?;
    removed += tx.execute(
        "DELETE FROM effects
         WHERE state IN ('confirmed', 'retired_not_dispatched') AND updated_at < ?1
           AND NOT EXISTS (SELECT 1 FROM shim_writes w WHERE w.effect_id = effects.effect_id)
           AND NOT EXISTS (SELECT 1 FROM shim_transitions t WHERE t.effect_id = effects.effect_id)",
        [cutoff],
    )?;
    removed += tx.execute(
        "DELETE FROM runtime_requests
         WHERE state IN ('resolved', 'expired') AND updated_at < ?1
           AND (process_incarnation IS NULL OR EXISTS (
                SELECT 1 FROM processes p
                WHERE p.process_incarnation = runtime_requests.process_incarnation
                  AND p.state = 'dead'))
           AND NOT EXISTS (
                SELECT 1 FROM effects e
                WHERE e.session_id = runtime_requests.session_id
                  AND e.state IN ('queued', 'claimed', 'outcome_unknown'))",
        [cutoff],
    )?;
    removed += tx.execute(
        "DELETE FROM processes
         WHERE state = 'dead' AND COALESCE(ended_at, started_at) < ?1
           AND NOT EXISTS (
                SELECT 1 FROM effects e WHERE e.process_incarnation = processes.process_incarnation)
           AND NOT EXISTS (
                SELECT 1 FROM runtime_requests r
                WHERE r.process_incarnation = processes.process_incarnation)",
        [cutoff],
    )?;
    removed += tx.execute("DELETE FROM artifact_suggestions WHERE at < ?1", [cutoff])?;
    Ok(removed)
}

/// Whether a session of this station has `callsign` already.
pub(super) fn callsign_taken(conn: &Connection, callsign: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sessions WHERE json_extract(summary, '$.callsign') = ?1",
        [callsign],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}
