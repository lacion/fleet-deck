//! The storage of the model help (R10, DL-167): the output cache, the daily
//! token use, and the reads of a session's own events that build a model
//! input. Nothing here runs a model.

use fleetdeck_proto::event::EventBody;
use fleetdeck_proto::{Event, EventEnvelope, SessionId};
use rusqlite::{OptionalExtension, params};

use super::{Result, Store, call, to_i64, to_u64};

/// How long a cached output stays.
pub const CACHE_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// The length of a UTC day.
pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The event kinds that make a line of a model input.
const LINE_KINDS: &str =
    "'run.started', 'message.completed', 'tool.started', 'file.changed', 'error'";

/// A cached model output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedAssist {
    pub output: String,
}

impl Store {
    /// The cached output of `input_hash`.
    ///
    /// # Errors
    /// A database error.
    pub async fn assist_cache_get(&self, input_hash: String) -> Result<Option<CachedAssist>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT output FROM assist_cache WHERE input_hash = ?1",
                    [input_hash],
                    |r| Ok(CachedAssist { output: r.get(0)? }),
                )
                .optional()?)
        })
        .await
    }

    /// Stores one output and adds its tokens to today's use, in one
    /// transaction. Cache rows and day rows older than
    /// [`CACHE_RETENTION_MS`] go.
    ///
    /// # Errors
    /// A database error; nothing is stored.
    pub async fn assist_record(
        &self,
        input_hash: String,
        kind: String,
        output: String,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<()> {
        let now = self.now();
        let day = now / DAY_MS;
        let tokens = to_i64(input_tokens.saturating_add(output_tokens))?;
        let (input_tokens, output_tokens) = (to_i64(input_tokens)?, to_i64(output_tokens)?);
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO assist_cache
                     (input_hash, kind, output, input_tokens, output_tokens, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (input_hash) DO UPDATE SET output = excluded.output,
                     input_tokens = excluded.input_tokens,
                     output_tokens = excluded.output_tokens,
                     created_at = excluded.created_at",
                params![input_hash, kind, output, input_tokens, output_tokens, now],
            )?;
            tx.execute(
                "INSERT INTO assist_usage (day, tokens) VALUES (?1, ?2)
                 ON CONFLICT (day) DO UPDATE SET tokens = tokens + excluded.tokens",
                params![day, tokens],
            )?;
            tx.execute(
                "DELETE FROM assist_cache WHERE created_at < ?1",
                [now.saturating_sub(CACHE_RETENTION_MS)],
            )?;
            tx.execute(
                "DELETE FROM assist_usage WHERE day < ?1",
                [day.saturating_sub(CACHE_RETENTION_MS / DAY_MS)],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// The tokens that the model help used in the current UTC day.
    ///
    /// # Errors
    /// A database error.
    pub async fn assist_tokens_today(&self) -> Result<u64> {
        let day = self.now() / DAY_MS;
        call(&self.inner.db, move |conn| {
            let tokens: Option<i64> = conn
                .query_row(
                    "SELECT tokens FROM assist_usage WHERE day = ?1",
                    [day],
                    |r| r.get(0),
                )
                .optional()?;
            tokens.map_or(Ok(0), to_u64)
        })
        .await
    }

    /// The events of `session_id` that make lines of a model input
    /// (prompts, agent messages, tool starts, file changes, errors), the
    /// newest `limit`, oldest first.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn assist_session_events(
        &self,
        session_id: SessionId,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT station_seq, session_seq, payload, at FROM events
                 WHERE session_id = ?1 AND kind IN ({LINE_KINDS})
                 ORDER BY session_seq DESC LIMIT ?2"
            ))?;
            let rows = stmt.query_map(params![session_id.as_str(), limit], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (station_seq, session_seq, payload, at) = row?;
                out.push(EventEnvelope {
                    station_seq: to_u64(station_seq)?,
                    session_id: session_id.clone(),
                    session_seq: to_u64(session_seq)?,
                    at,
                    proto_version: fleetdeck_proto::PROTO_MAX,
                    raw_ref: None,
                    event: serde_json::from_str::<EventBody>(&payload)?,
                });
            }
            out.reverse();
            Ok(out)
        })
        .await
    }

    /// The input summary of the first run of `session_id`: its first
    /// prompt.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn assist_first_prompt(&self, session_id: SessionId) -> Result<Option<String>> {
        call(&self.inner.db, move |conn| {
            let payload: Option<String> = conn
                .query_row(
                    "SELECT payload FROM events WHERE session_id = ?1 AND kind = 'run.started'
                     ORDER BY session_seq LIMIT 1",
                    [session_id.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(
                match payload.map(|p| serde_json::from_str::<EventBody>(&p)) {
                    Some(Ok(EventBody::Known(Event::RunStarted { input_summary, .. }))) => {
                        Some(input_summary)
                    }
                    Some(Err(e)) => return Err(e.into()),
                    _ => None,
                },
            )
        })
        .await
    }
}
