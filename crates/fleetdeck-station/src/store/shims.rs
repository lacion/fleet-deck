//! The `shims` table (ADR-005 §3, §4.1, §4.3; slice 4, DL-104).
//!
//! - [`Store::shim_starting`] commits the launch identity before the shim
//!   starts (state `starting`, generation 0, cursor 0, write sequence 1).
//! - [`Store::shim_running`] commits the first provider identity; a
//!   running row never changes it.
//! - [`Store::shim_next_generation`] commits `owner_generation + 1` before
//!   each attach, so a crash between the commit and the attach only skips a
//!   generation and a shim never accepts a lower one again.
//! - [`Store::shim_commit_output`] is the output transaction of §4.1: the
//!   events and the new cursor commit together, only for the current
//!   generation and only forward. Adapter state joins this transaction with
//!   the adapter checkpoint slice (§2.1).
//! - [`Store::shim_claim_write_seq`] gives each stdin write a unique,
//!   strictly rising sequence number of the current generation (§4.3).

use std::path::PathBuf;

use fleetdeck_proto::{EventEnvelope, SessionId};
use fleetdeck_providers::host::shim::host::ShimRecord;
use fleetdeck_providers::host::shim::wire::ProviderIdentity;
use rusqlite::{Connection, OptionalExtension as _, Row, params};

use super::{NewEvent, Result, Store, StoreError, call, log, to_i64, to_u64};

/// The life of a shim row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimState {
    Starting,
    Running,
    Exited,
    /// Recovery proved from an intact journal that no provider started.
    NeverStarted,
    Lost,
}

impl ShimState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Exited => "exited",
            Self::NeverStarted => "never_started",
            Self::Lost => "lost",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "starting" => Self::Starting,
            "running" => Self::Running,
            "exited" => Self::Exited,
            "never_started" => Self::NeverStarted,
            "lost" => Self::Lost,
            other => return Err(StoreError::Corrupt(format!("shim state {other}"))),
        })
    }

    /// True for a row that recovery still settles.
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }
}

/// A shim that the station is about to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewShim {
    pub shim_id: String,
    pub session_id: SessionId,
    pub socket_path: PathBuf,
}

/// One `shims` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimRow {
    pub shim_id: String,
    pub session_id: SessionId,
    pub incarnation: String,
    pub state: ShimState,
    /// The last committed attach generation.
    pub generation: u64,
    pub socket_path: PathBuf,
    pub provider_identity: Option<ProviderIdentity>,
    pub cursor: u64,
    pub next_write_seq: u64,
    pub reason: Option<String>,
}

impl ShimRow {
    /// The host's view of the row. Its `generation` is the committed one.
    #[must_use]
    pub fn record(&self) -> ShimRecord {
        ShimRecord {
            shim_id: self.shim_id.clone(),
            socket_path: self.socket_path.clone(),
            provider_identity: self.provider_identity.clone(),
            generation: self.generation,
            cursor: self.cursor,
            next_write_seq: self.next_write_seq,
        }
    }
}

/// The result of [`Store::shim_commit_output`].
#[derive(Debug, Clone, PartialEq)]
pub enum OutputCommit {
    /// The events and the cursor committed.
    Committed(Vec<EventEnvelope>),
    /// Another attachment took over (or the row ended): nothing committed.
    StaleGeneration,
    /// The cursor is below the committed one: nothing committed.
    CursorBehind { committed: u64 },
}

const COLUMNS: &str = "shim_id, session_id, incarnation, state, owner_generation, socket_path,
    provider_identity, cursor, next_write_seq, reason";

fn row(r: &Row<'_>) -> rusqlite::Result<ShimRowRaw> {
    Ok(ShimRowRaw {
        shim_id: r.get(0)?,
        session_id: r.get(1)?,
        incarnation: r.get(2)?,
        state: r.get(3)?,
        generation: r.get(4)?,
        socket_path: r.get(5)?,
        identity: r.get(6)?,
        cursor: r.get(7)?,
        next_write_seq: r.get(8)?,
        reason: r.get(9)?,
    })
}

struct ShimRowRaw {
    shim_id: String,
    session_id: String,
    incarnation: String,
    state: String,
    generation: i64,
    socket_path: String,
    identity: Option<String>,
    cursor: i64,
    next_write_seq: i64,
    reason: Option<String>,
}

impl ShimRowRaw {
    fn decode(self) -> Result<ShimRow> {
        Ok(ShimRow {
            shim_id: self.shim_id,
            session_id: SessionId::from(self.session_id),
            incarnation: self.incarnation,
            state: ShimState::parse(&self.state)?,
            generation: to_u64(self.generation)?,
            socket_path: PathBuf::from(self.socket_path),
            provider_identity: self
                .identity
                .map(|s| serde_json::from_str(&s))
                .transpose()?,
            cursor: to_u64(self.cursor)?,
            next_write_seq: to_u64(self.next_write_seq)?,
            reason: self.reason,
        })
    }
}

fn get(conn: &Connection, shim_id: &str) -> Result<Option<ShimRow>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM shims WHERE shim_id = ?1"),
        [shim_id],
        row,
    )
    .optional()?
    .map(ShimRowRaw::decode)
    .transpose()
}

fn open_rows(conn: &Connection, incarnation: &str) -> Result<Vec<ShimRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM shims
         WHERE incarnation = ?1 AND state IN ('starting', 'running')
         ORDER BY CASE state WHEN 'starting' THEN 0 ELSE 1 END, created_at, shim_id"
    ))?;
    let raws = stmt
        .query_map([incarnation], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter().map(ShimRowRaw::decode).collect()
}

fn current_generation(conn: &Connection, shim_id: &str) -> Result<Option<(u64, u64)>> {
    let found: Option<(i64, i64)> = conn
        .query_row(
            "SELECT owner_generation, cursor FROM shims
             WHERE shim_id = ?1 AND state IN ('starting', 'running')",
            [shim_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    found.map(|(g, c)| Ok((to_u64(g)?, to_u64(c)?))).transpose()
}

impl Store {
    /// Commits a `starting` row (ADR-005 §3 "Launch"): before the shim
    /// starts, after the key file and the journal exist.
    ///
    /// # Errors
    /// An unknown session, a duplicate shim id, a socket path that is not
    /// UTF-8, or a database error.
    pub async fn shim_starting(&self, shim: NewShim) -> Result<()> {
        let incarnation = self.incarnation().as_str().to_owned();
        let socket = shim
            .socket_path
            .to_str()
            .ok_or_else(|| StoreError::Corrupt("the socket path is not UTF-8".to_owned()))?
            .to_owned();
        self.write(move |conn, now, _| {
            let known: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM sessions WHERE session_id = ?1)",
                [shim.session_id.as_str()],
                |r| r.get(0),
            )?;
            if !known {
                return Err(StoreError::UnknownSession(shim.session_id));
            }
            conn.execute(
                "INSERT INTO shims (shim_id, session_id, incarnation, state, socket_path,
                     created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'starting', ?4, ?5, ?5)",
                params![
                    shim.shim_id,
                    shim.session_id.as_str(),
                    incarnation,
                    socket,
                    now
                ],
            )?;
            Ok(())
        })
        .await
        .map(|((), _)| ())
    }

    /// Commits the provider identity. A `starting` row becomes `running`;
    /// a `running` row accepts only its own identity again. `false` for
    /// another identity or an ended row: nothing changed.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_running(&self, shim_id: String, identity: ProviderIdentity) -> Result<bool> {
        let json = serde_json::to_string(&identity)?;
        self.write(move |conn, now, _| {
            let Some(found) = get(conn, &shim_id)? else {
                return Ok(false);
            };
            match found.state {
                ShimState::Starting => {
                    conn.execute(
                        "UPDATE shims SET state = 'running', provider_identity = ?2,
                             updated_at = ?3
                         WHERE shim_id = ?1 AND state = 'starting'",
                        params![shim_id, json, now],
                    )?;
                    Ok(true)
                }
                ShimState::Running => Ok(found.provider_identity.as_ref() == Some(&identity)),
                _ => Ok(false),
            }
        })
        .await
        .map(|(ok, _)| ok)
    }

    /// Commits and returns the next attach generation of an open row
    /// (ADR-005 §3 step 3); `None` for an ended or unknown row.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_next_generation(&self, shim_id: String) -> Result<Option<u64>> {
        self.write(move |conn, now, _| {
            let next: Option<i64> = conn
                .query_row(
                    "UPDATE shims SET owner_generation = owner_generation + 1, updated_at = ?2
                     WHERE shim_id = ?1 AND state IN ('starting', 'running')
                     RETURNING owner_generation",
                    params![shim_id, now],
                    |r| r.get(0),
                )
                .optional()?;
            next.map(to_u64).transpose()
        })
        .await
        .map(|(g, _)| g)
    }

    /// The output transaction (ADR-005 §4.1): appends `events` to the
    /// shim's session and stores `cursor`, in one transaction, only for the
    /// current `generation` of an open row and only forward. The caller
    /// acknowledges `cursor` to the shim after this returns `Committed`.
    ///
    /// # Errors
    /// An unknown shim or a database error.
    pub async fn shim_commit_output(
        &self,
        shim_id: String,
        generation: u64,
        cursor: u64,
        events: Vec<NewEvent>,
    ) -> Result<OutputCommit> {
        let (result, out) = self
            .write(move |conn, now, out| {
                let Some(found) = get(conn, &shim_id)? else {
                    return Err(StoreError::Corrupt(format!("unknown shim {shim_id}")));
                };
                if !found.state.is_open() || found.generation != generation {
                    return Ok(OutputCommit::StaleGeneration);
                }
                if cursor < found.cursor {
                    return Ok(OutputCommit::CursorBehind {
                        committed: found.cursor,
                    });
                }
                for event in events {
                    out.push(log::append_one(conn, &found.session_id, event, now)?);
                }
                conn.execute(
                    "UPDATE shims SET cursor = ?2, updated_at = ?3 WHERE shim_id = ?1",
                    params![shim_id, to_i64(cursor)?, now],
                )?;
                Ok(OutputCommit::Committed(Vec::new()))
            })
            .await?;
        Ok(match result {
            OutputCommit::Committed(_) => OutputCommit::Committed(out),
            other => other,
        })
    }

    /// Claims the next stdin write sequence number of the current
    /// generation (ADR-005 §4.3); `None` for a stale generation or an ended
    /// row.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_claim_write_seq(
        &self,
        shim_id: String,
        generation: u64,
    ) -> Result<Option<u64>> {
        self.write(move |conn, now, _| {
            if current_generation(conn, &shim_id)?.map(|(g, _)| g) != Some(generation) {
                return Ok(None);
            }
            let seq: i64 = conn.query_row(
                "UPDATE shims SET next_write_seq = next_write_seq + 1, updated_at = ?2
                 WHERE shim_id = ?1 RETURNING next_write_seq - 1",
                params![shim_id, now],
                |r| r.get(0),
            )?;
            to_u64(seq).map(Some)
        })
        .await
        .map(|(s, _)| s)
    }

    /// Ends an open row with `state` (`exited`, `never_started` or `lost`)
    /// and `reason`; `false` when the row had ended already.
    ///
    /// # Errors
    /// A database error, or an open `state`.
    pub async fn shim_end(
        &self,
        shim_id: String,
        state: ShimState,
        reason: String,
    ) -> Result<bool> {
        if state.is_open() {
            return Err(StoreError::Corrupt(format!(
                "a shim row cannot end in {}",
                state.as_str()
            )));
        }
        self.write(move |conn, now, _| {
            let n = conn.execute(
                "UPDATE shims SET state = ?2, reason = ?3, updated_at = ?4
                 WHERE shim_id = ?1 AND state IN ('starting', 'running')",
                params![shim_id, state.as_str(), reason, now],
            )?;
            Ok(n == 1)
        })
        .await
        .map(|(ok, _)| ok)
    }

    /// One shim row.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim(&self, shim_id: String) -> Result<Option<ShimRow>> {
        call(&self.inner.db, move |conn| get(conn, &shim_id)).await
    }

    /// The open rows of this incarnation, `starting` rows first (ADR-005
    /// §3.2).
    ///
    /// # Errors
    /// A database error.
    pub async fn shims_to_recover(&self) -> Result<Vec<ShimRow>> {
        let incarnation = self.incarnation().as_str().to_owned();
        call(&self.inner.db, move |conn| open_rows(conn, &incarnation)).await
    }
}

#[cfg(test)]
#[path = "shims_tests.rs"]
mod tests;
