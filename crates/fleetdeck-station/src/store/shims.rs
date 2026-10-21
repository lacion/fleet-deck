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
use fleetdeck_providers::checkpoint::AdapterCheckpoint;
use fleetdeck_providers::host::shim::host::ShimRecord;
use fleetdeck_providers::host::shim::wire::ProviderIdentity;
use rusqlite::{Connection, OptionalExtension as _, Row, params};

use super::requests::RequestTime;
use super::session_grants;
use super::shim_bindings::{self, IdentitySignal};
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
    /// The boot id that the station read before the spawn and wrote into
    /// the launch journal (DL-182 §4.1).
    pub launch_boot_id: Option<String>,
    /// The native id that a resume or an import continues; `None` for a new
    /// conversation (DL-182 §3).
    pub native_target: Option<String>,
}

/// The facts that the station learns when a shim reports its provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunningFacts {
    /// The shim protocol version that the attach picked.
    pub protocol: Option<u32>,
    /// The process id of the shim.
    pub shim_pid: Option<u32>,
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
    /// The inspection version of the exit report; 0 for an older shim or
    /// a row that has no exit report (DL-182 §1a).
    pub inspection_version: u32,
    pub shim_pid: Option<u32>,
    /// `ended_by_reboot` or `reconciled_by_owner` (DL-182 §4).
    pub proof: Option<String>,
    pub proof_boot_id: Option<String>,
    pub launch_boot_id: Option<String>,
    pub native_target: Option<String>,
    /// The creation time (Unix ms): the order of the executions.
    pub created_at: i64,
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
            shim_pid: self.shim_pid,
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

pub(super) const COLUMNS: &str = "shim_id, session_id, incarnation, state, owner_generation,
    socket_path, provider_identity, cursor, next_write_seq, reason, inspection_version, shim_pid,
    proof, proof_boot_id, launch_boot_id, native_target, created_at";

pub(super) fn row(r: &Row<'_>) -> rusqlite::Result<ShimRowRaw> {
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
        inspection_version: r.get(10)?,
        shim_pid: r.get(11)?,
        proof: r.get(12)?,
        proof_boot_id: r.get(13)?,
        launch_boot_id: r.get(14)?,
        native_target: r.get(15)?,
        created_at: r.get(16)?,
    })
}

pub(super) struct ShimRowRaw {
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
    inspection_version: i64,
    shim_pid: Option<i64>,
    proof: Option<String>,
    proof_boot_id: Option<String>,
    launch_boot_id: Option<String>,
    native_target: Option<String>,
    created_at: i64,
}

impl ShimRowRaw {
    pub(super) fn decode(self) -> Result<ShimRow> {
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
            inspection_version: u32::try_from(self.inspection_version)
                .map_err(|_| StoreError::Corrupt("inspection version".to_owned()))?,
            shim_pid: self
                .shim_pid
                .map(|p| u32::try_from(p).map_err(|_| StoreError::Corrupt("shim pid".to_owned())))
                .transpose()?,
            proof: self.proof,
            proof_boot_id: self.proof_boot_id,
            launch_boot_id: self.launch_boot_id,
            native_target: self.native_target,
            created_at: self.created_at,
        })
    }
}

pub(super) fn get(conn: &Connection, shim_id: &str) -> Result<Option<ShimRow>> {
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
                     launch_boot_id, native_target, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'starting', ?4, ?5, ?6, ?7, ?7)",
                params![
                    shim.shim_id,
                    shim.session_id.as_str(),
                    incarnation,
                    socket,
                    shim.launch_boot_id,
                    shim.native_target,
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
        self.shim_running_with(shim_id, identity, RunningFacts::default())
            .await
    }

    /// [`Self::shim_running`] with the protocol version and the shim's
    /// process id, which the station keeps for the upgrade trial (ADR-005
    /// §5.3) and the owner's execution check (DL-182 §4.2).
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_running_with(
        &self,
        shim_id: String,
        identity: ProviderIdentity,
        facts: RunningFacts,
    ) -> Result<bool> {
        let json = serde_json::to_string(&identity)?;
        self.write(move |conn, now, _| {
            let Some(found) = get(conn, &shim_id)? else {
                return Ok(false);
            };
            match found.state {
                ShimState::Starting => {
                    conn.execute(
                        "UPDATE shims SET state = 'running', provider_identity = ?2,
                             protocol = COALESCE(?4, protocol), shim_pid = COALESCE(?5, shim_pid),
                             updated_at = ?3
                         WHERE shim_id = ?1 AND state = 'starting'",
                        params![shim_id, json, now, facts.protocol, facts.shim_pid],
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

    /// Rewrites the boot ids that a row stores (its launch boot id and the
    /// boot id of its provider identity), so that a test can make a run look
    /// as if it ended with another boot (feature `fault-injection`).
    ///
    /// # Errors
    /// A database error.
    #[cfg(feature = "fault-injection")]
    pub async fn shim_rewrite_boot(&self, shim_id: String, boot: String) -> Result<()> {
        self.write(move |conn, now, _| {
            conn.execute(
                "UPDATE shims SET launch_boot_id = ?2,
                     provider_identity = CASE WHEN provider_identity IS NULL THEN NULL
                         ELSE json_set(provider_identity, '$.boot_id', ?2) END,
                     updated_at = ?3
                 WHERE shim_id = ?1",
                params![shim_id, boot, now],
            )?;
            Ok(())
        })
        .await
        .map(|((), _)| ())
    }

    /// Stores the protocol version that an attach picked for an open row
    /// (ADR-005 §5.3, DL-182): the upgrade trial reads it without an
    /// attach.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_picked_protocol(&self, shim_id: String, protocol: u32) -> Result<()> {
        self.write(move |conn, now, _| {
            conn.execute(
                "UPDATE shims SET protocol = ?2, updated_at = ?3
                 WHERE shim_id = ?1 AND state IN ('starting', 'running')",
                params![shim_id, protocol, now],
            )?;
            Ok(())
        })
        .await
        .map(|((), _)| ())
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
        self.shim_commit_output_with(shim_id, generation, cursor, events, OutputMeta::default())
            .await
    }

    /// [`Self::shim_commit_output`] with the read time of the output and
    /// the adapter checkpoint after it, both in the same transaction
    /// (ADR-005 §2.1 and §6; DL-108). A request that the events open gets
    /// `meta.read_at` as its request time.
    ///
    /// # Errors
    /// As [`Self::shim_commit_output`]; a checkpoint that does not
    /// serialize is [`StoreError::Corrupt`].
    pub async fn shim_commit_output_with(
        &self,
        shim_id: String,
        generation: u64,
        cursor: u64,
        events: Vec<NewEvent>,
        meta: OutputMeta,
    ) -> Result<OutputCommit> {
        let checkpoint = meta
            .checkpoint
            .map(|c| c.to_json())
            .transpose()
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let time = meta.read_at;
        let frames = meta.frames;
        let (identities, exited) = (meta.identities, meta.exited);
        let sink = self.binding_sink();
        let activate = sink.clone();
        let ((result, _), out) = self
            .write_then(
                move |conn, now, out| {
                    let Some(found) = get(conn, &shim_id)? else {
                        return Err(StoreError::Corrupt(format!("unknown shim {shim_id}")));
                    };
                    if !found.state.is_open() || found.generation != generation {
                        return Ok((OutputCommit::StaleGeneration, Vec::new()));
                    }
                    if cursor < found.cursor {
                        return Ok((
                            OutputCommit::CursorBehind {
                                committed: found.cursor,
                            },
                            Vec::new(),
                        ));
                    }
                    // The binding transitions of the batch (DL-194): the
                    // invalidation reaches memory before the commit, the
                    // row changes and lifecycle events join this transaction.
                    let rows = shim_bindings::rows_of_shim(conn, &shim_id)?;
                    let ts = shim_bindings::transitions(&rows, &events, &identities, exited);
                    let (ended, bound): (Vec<_>, Vec<_>) = ts
                        .iter()
                        .cloned()
                        .partition(|t| matches!(t, shim_bindings::Transition::Ended { .. }));
                    if let (false, Some(sink)) = (ended.is_empty(), &sink) {
                        sink.invalidate(&ended);
                    }
                    for event in events {
                        out.push(log::append_at(conn, &found.session_id, event, now, time)?);
                    }
                    shim_bindings::write_changes(conn, &ts)?;
                    let attached = session_grants::attached_grant(conn, &found.session_id)?;
                    let evented = shim_bindings::evented(&rows, &ts, attached.as_deref());
                    for event in shim_bindings::lifecycle_events(&rows, &evented) {
                        out.push(log::append_at(conn, &found.session_id, event, now, time)?);
                    }
                    // A resume that reports another native id ends the
                    // record of its grant (MCP P4b §5).
                    if shim_bindings::ends_record(&rows, &evented, attached.as_deref()) {
                        session_grants::end_attached(
                            conn,
                            &found.session_id,
                            shim_bindings::REASON_IDENTITY_CHANGED,
                            now,
                        )?;
                    }
                    if let Some(text) = &checkpoint {
                        store_checkpoint(conn, &shim_id, text, now)?;
                        super::shim_driver::clear_transitions(conn, &shim_id)?;
                    }
                    super::shim_driver::store_frames(conn, &shim_id, &frames, now)?;
                    conn.execute(
                        "UPDATE shims SET cursor = ?2, updated_at = ?3 WHERE shim_id = ?1",
                        params![shim_id, to_i64(cursor)?, now],
                    )?;
                    Ok((OutputCommit::Committed(Vec::new()), bound))
                },
                // Right after the commit, on the database thread, before the
                // next job (DL-194 §9.15).
                move |(_, bound): &(OutputCommit, Vec<shim_bindings::Transition>)| {
                    if let (false, Some(sink)) = (bound.is_empty(), &activate) {
                        sink.activate(bound);
                    }
                },
            )
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
    /// `inspection_version` is the version of the shim's exit report (0
    /// when the row has none, DL-182 §1a).
    ///
    /// # Errors
    /// A database error, or an open `state`.
    pub async fn shim_end(
        &self,
        shim_id: String,
        state: ShimState,
        reason: String,
        inspection_version: u32,
    ) -> Result<bool> {
        if state.is_open() {
            return Err(StoreError::Corrupt(format!(
                "a shim row cannot end in {}",
                state.as_str()
            )));
        }
        self.write(move |conn, now, _| {
            let n = conn.execute(
                "UPDATE shims SET state = ?2, reason = ?3, inspection_version = ?5,
                     updated_at = ?4
                 WHERE shim_id = ?1 AND state IN ('starting', 'running')",
                params![shim_id, state.as_str(), reason, now, inspection_version],
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

    /// The ids of the open shim rows of `session`.
    ///
    /// # Errors
    /// A database error.
    pub async fn shims_of_session(&self, session: SessionId) -> Result<Vec<String>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT shim_id FROM shims
                 WHERE session_id = ?1 AND state IN ('starting', 'running')",
            )?;
            let rows = stmt
                .query_map([session.as_str()], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(rows)
        })
        .await
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

/// The extra contents of an output transaction (DL-108).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputMeta {
    /// The read time of the output chunk (the shim's `read_at`), or
    /// [`RequestTime::Unprovable`]; [`RequestTime::Now`] for a live read.
    pub read_at: RequestTime,
    /// The adapter state after the output.
    pub checkpoint: Option<AdapterCheckpoint>,
    /// Stdin frames that the output generated (JSON lines without their
    /// newline); stored as pending frames (DL-110).
    pub frames: Vec<String>,
    /// The root native identities that the adapter stated in this batch, in
    /// order with the events (DL-194).
    pub identities: Vec<IdentitySignal>,
    /// The provider process ended: the open bindings end with
    /// `process_exit` (DL-194).
    pub exited: bool,
}

pub(super) fn store_checkpoint(
    conn: &Connection,
    shim_id: &str,
    text: &str,
    now: i64,
) -> Result<()> {
    // An invalidated adapter state never gets a checkpoint again (DL-124).
    conn.execute(
        "UPDATE shims SET adapter_checkpoint = ?2, updated_at = ?3
         WHERE shim_id = ?1 AND adapter_gap = 0",
        params![shim_id, text, now],
    )?;
    Ok(())
}

impl Store {
    /// The latest committed adapter checkpoint of a shim, as stored text
    /// (the restorer decodes it and checks its version).
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_checkpoint(&self, shim_id: String) -> Result<Option<String>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT adapter_checkpoint FROM shims WHERE shim_id = ?1",
                    [shim_id.as_str()],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
        })
        .await
    }
}
