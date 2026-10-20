//! The station's per-session grant record (MCP P4b §2, V42): one desired
//! grant for each session, ordered by the per-session sequence of
//! `session.coordinator_set`. The record is independent of `shim_bindings`.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::CoordinatorSpec;
use rusqlite::{Connection, OptionalExtension as _, params};

use super::{Decision, Result, Store, StoreError, call, to_i64, to_u64};

const COLUMNS: &str =
    "session_id, grant_id, seq, spec, generation, state, ended_reason, updated_at";

/// The state of a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordState {
    Attached,
    Ended,
}

impl RecordState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Attached => "attached",
            Self::Ended => "ended",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "attached" => Ok(Self::Attached),
            "ended" => Ok(Self::Ended),
            other => Err(StoreError::Corrupt(format!("session grant state {other}"))),
        }
    }
}

/// One `session_grants` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRecord {
    pub session_id: SessionId,
    pub grant_id: String,
    /// The per-session sequence of the latest accepted command.
    pub seq: u64,
    /// The desired grant. An `Attached` record always has one; an end that
    /// was never recorded has none.
    pub spec: Option<CoordinatorSpec>,
    /// The binding generation of the latest process of this record.
    pub generation: u32,
    pub state: RecordState,
    pub ended_reason: Option<String>,
    /// Unix ms.
    pub updated_at: i64,
}

type RawRecord = (
    String,
    String,
    i64,
    Option<String>,
    i64,
    String,
    Option<String>,
    i64,
);

fn raw(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRecord> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
    ))
}

fn decode(raw: RawRecord) -> Result<GrantRecord> {
    let (session_id, grant_id, seq, spec, generation, state, ended_reason, updated_at) = raw;
    Ok(GrantRecord {
        session_id: SessionId::from(session_id),
        grant_id,
        seq: to_u64(seq)?,
        spec: spec
            .map(|text| serde_json::from_str::<CoordinatorSpec>(&text))
            .transpose()?,
        generation: u32::try_from(generation)
            .map_err(|_| StoreError::Corrupt("session grant generation".to_owned()))?,
        state: RecordState::parse(&state)?,
        ended_reason,
        updated_at,
    })
}

fn get(conn: &Connection, session: &SessionId) -> Result<Option<GrantRecord>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM session_grants WHERE session_id = ?1"),
        [session.as_str()],
        raw,
    )
    .optional()?
    .map(decode)
    .transpose()
}

impl Decision<'_> {
    /// The record of `session`, read in the decide transaction.
    ///
    /// # Errors
    /// A database error, or a row that does not decode.
    pub fn session_grant(&self, session: &SessionId) -> Result<Option<GrantRecord>> {
        get(self.connection(), session)
    }

    /// Writes the record of its session (an upsert), in the decide
    /// transaction.
    ///
    /// # Errors
    /// A database error, or a sequence above `i64::MAX`.
    pub fn session_grant_put(&mut self, record: &GrantRecord) -> Result<()> {
        let spec = record
            .spec
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.connection().execute(
            "INSERT INTO session_grants (session_id, grant_id, seq, spec, generation, state,
                 ended_reason, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (session_id) DO UPDATE SET
                 grant_id = excluded.grant_id, seq = excluded.seq, spec = excluded.spec,
                 generation = excluded.generation, state = excluded.state,
                 ended_reason = excluded.ended_reason, updated_at = excluded.updated_at",
            params![
                record.session_id.as_str(),
                record.grant_id,
                to_i64(record.seq)?,
                spec,
                record.generation,
                record.state.as_str(),
                record.ended_reason,
                record.updated_at,
            ],
        )?;
        Ok(())
    }
}

impl Store {
    /// The record of `session`, if any.
    ///
    /// # Errors
    /// A database error, or a row that does not decode.
    pub async fn session_grant(&self, session: SessionId) -> Result<Option<GrantRecord>> {
        call(&self.inner.db, move |conn| get(conn, &session)).await
    }

    /// Every record, ordered by session id.
    ///
    /// # Errors
    /// A database error, or a row that does not decode.
    pub async fn session_grants(&self) -> Result<Vec<GrantRecord>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM session_grants ORDER BY session_id"
            ))?;
            let raws = stmt
                .query_map([], raw)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            raws.into_iter().map(decode).collect()
        })
        .await
    }
}
