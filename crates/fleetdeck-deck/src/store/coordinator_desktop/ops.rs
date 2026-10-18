//! The operation ledger of the coordinator desktop tools (R13 P6 §7.2). One
//! row of `coordinator_desktop_ops` is one accepted mutation: it is written
//! `prepared` in the admission transaction ([`super::admit`]), then
//! `attempted` before the send, then `done`, `failed` or `unknown`. A row in
//! a final state never changes. The raw `request_id` is never stored: the
//! row holds its hash ([`request_key`], O12-4 item 14), and `resource` holds
//! validated identifiers only (never a URL or a notification text).

use fleetdeck_proto::coordinator_desktop_text::code;
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::{SessionId, StationId};
use ring::digest::{SHA256, digest};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::store::{DeckError, DeckStore, Result, call};

/// The tool of an operation (the SQL values of the `tool` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpTool {
    WallShow,
    Notify,
    PreviewOpen,
    WindowClose,
    Screenshot,
    RecordMonitor,
}

impl OpTool {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WallShow => "wall_show",
            Self::Notify => "notify",
            Self::PreviewOpen => "preview_open",
            Self::WindowClose => "window_close",
            Self::Screenshot => "screenshot",
            Self::RecordMonitor => "record_monitor",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "wall_show" => Self::WallShow,
            "notify" => Self::Notify,
            "preview_open" => Self::PreviewOpen,
            "window_close" => Self::WindowClose,
            "screenshot" => Self::Screenshot,
            "record_monitor" => Self::RecordMonitor,
            _ => return None,
        })
    }
}

/// The grant class that an operation needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpClass {
    Desktop,
    Capture,
}

impl OpClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Capture => "capture",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "desktop" => Some(Self::Desktop),
            "capture" => Some(Self::Capture),
            _ => None,
        }
    }
}

/// The state of an operation (§7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpState {
    Prepared,
    Attempted,
    Done,
    Failed,
    Unknown,
}

impl OpState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Attempted => "attempted",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    /// The state of a stored text; `None` for any other text.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "prepared" => Self::Prepared,
            "attempted" => Self::Attempted,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// What an operation charged to its grant, stored as JSON in `charged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Charges {
    pub desktop: u32,
    pub capture: u32,
}

/// An operation before the admission transaction stores it.
#[derive(Debug, Clone)]
pub struct NewOp {
    pub operation_id: String,
    pub grant_id: String,
    pub request_key: String,
    pub payload_hash: String,
    pub tool: OpTool,
    pub class: OpClass,
    /// `caller_label` is not stored (it is derived again from the session).
    pub ctx: CoordinatorContext,
    pub target_station: StationId,
    pub action_hash: Option<String>,
    /// Validated identifiers only (§7.2).
    pub resource: serde_json::Value,
    /// The requested recording seconds (1 to 120); `None` for any other
    /// tool (R13 P5b-2).
    pub seconds: Option<u32>,
}

/// A stored operation.
#[derive(Debug, Clone, PartialEq)]
pub struct DesktopOp {
    pub operation_id: String,
    pub grant_id: String,
    pub request_key: String,
    pub payload_hash: String,
    pub tool: OpTool,
    pub class: OpClass,
    pub binding_id: String,
    pub generation: u32,
    pub caller_station: StationId,
    pub caller_session: SessionId,
    pub target_station: StationId,
    pub action_hash: Option<String>,
    pub resource: serde_json::Value,
    pub state: OpState,
    pub result: Option<serde_json::Value>,
    pub code: Option<String>,
    pub charged: Charges,
    pub created_at: i64,
    pub updated_at: i64,
    /// The requested recording seconds; `None` for any other tool.
    pub seconds: Option<u32>,
}

/// The answer of the admission transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum Admission {
    /// A new operation, stored `prepared`.
    Accepted(DesktopOp),
    /// The same request key and payload were stored before. The caller
    /// re-authorizes before it returns the state (§6.2).
    Existing(DesktopOp),
    /// The same request key with another payload (`request_id_conflict`).
    Conflict,
    /// A refusal; `stored` is true when the key and the code were stored
    /// (and one attempt was counted), so that a replay returns the same code.
    Rejected { code: &'static str, stored: bool },
}

/// How an attempted operation ended.
#[derive(Debug, Clone, PartialEq)]
pub enum OpOutcome {
    Done(serde_json::Value),
    Failed(&'static str),
    Unknown,
}

/// The request key of a `request_id`: the lowercase hex SHA-256 of its UTF-8
/// bytes (O12-4 item 14). The `request_id` itself is never stored, logged or
/// put in an error.
#[must_use]
pub fn request_key(request_id: &str) -> String {
    digest(&SHA256, request_id.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

const COLUMNS: &str = "operation_id, grant_id, request_key, payload_hash, tool, class, \
    binding_id, generation, caller_station, caller_session, target_station, action_hash, \
    resource, state, result, code, charged, created_at, updated_at, seconds";

/// A row as the database holds it, before the text is decoded.
struct Raw {
    operation_id: String,
    grant_id: String,
    request_key: String,
    payload_hash: String,
    tool: String,
    class: String,
    binding_id: String,
    generation: i64,
    caller_station: String,
    caller_session: String,
    target_station: String,
    action_hash: Option<String>,
    resource: String,
    state: String,
    result: Option<String>,
    code: Option<String>,
    charged: String,
    created_at: i64,
    updated_at: i64,
    seconds: Option<i64>,
}

impl Raw {
    fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            operation_id: r.get(0)?,
            grant_id: r.get(1)?,
            request_key: r.get(2)?,
            payload_hash: r.get(3)?,
            tool: r.get(4)?,
            class: r.get(5)?,
            binding_id: r.get(6)?,
            generation: r.get(7)?,
            caller_station: r.get(8)?,
            caller_session: r.get(9)?,
            target_station: r.get(10)?,
            action_hash: r.get(11)?,
            resource: r.get(12)?,
            state: r.get(13)?,
            result: r.get(14)?,
            code: r.get(15)?,
            charged: r.get(16)?,
            created_at: r.get(17)?,
            updated_at: r.get(18)?,
            seconds: r.get(19)?,
        })
    }

    /// Unknown stored text is corruption: the check constraints of the table
    /// make it impossible, and the row is never guessed.
    fn decode(self) -> Result<DesktopOp> {
        let corrupt = |what: &str| DeckError::Corrupt(format!("desktop operation {what}"));
        Ok(DesktopOp {
            tool: OpTool::parse(&self.tool).ok_or_else(|| corrupt("tool"))?,
            class: OpClass::parse(&self.class).ok_or_else(|| corrupt("class"))?,
            generation: u32::try_from(self.generation).map_err(|_| corrupt("generation"))?,
            state: OpState::parse(&self.state).ok_or_else(|| corrupt("state"))?,
            resource: serde_json::from_str(&self.resource)?,
            result: self
                .result
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            charged: serde_json::from_str(&self.charged)?,
            seconds: self
                .seconds
                .map(u32::try_from)
                .transpose()
                .map_err(|_| corrupt("seconds"))?,
            operation_id: self.operation_id,
            grant_id: self.grant_id,
            request_key: self.request_key,
            payload_hash: self.payload_hash,
            binding_id: self.binding_id,
            caller_station: StationId(self.caller_station),
            caller_session: SessionId(self.caller_session),
            target_station: StationId(self.target_station),
            action_hash: self.action_hash,
            code: self.code,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// The row of `operation_id`.
pub(super) fn find_op(conn: &Connection, operation_id: &str) -> Result<Option<DesktopOp>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM coordinator_desktop_ops WHERE operation_id = ?1"),
        [operation_id],
        Raw::read,
    )
    .optional()?
    .map(Raw::decode)
    .transpose()
}

/// The row of `(grant_id, request_key)`.
pub(super) fn find_by_key(
    conn: &Connection,
    grant_id: &str,
    request_key: &str,
) -> Result<Option<DesktopOp>> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM coordinator_desktop_ops
             WHERE grant_id = ?1 AND request_key = ?2"
        ),
        params![grant_id, request_key],
        Raw::read,
    )
    .optional()?
    .map(Raw::decode)
    .transpose()
}

/// The stored refusal of `(grant_id, request_key)`: its payload hash and code.
pub(super) fn find_reject(
    conn: &Connection,
    grant_id: &str,
    request_key: &str,
) -> Result<Option<(String, String)>> {
    Ok(conn
        .query_row(
            "SELECT payload_hash, code FROM coordinator_desktop_rejects
             WHERE grant_id = ?1 AND request_key = ?2",
            params![grant_id, request_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

impl DeckStore {
    /// `prepared` becomes `attempted`, before the send. False when the row is
    /// not `prepared`.
    ///
    /// # Errors
    /// A database error.
    pub async fn desktop_op_attempted(&self, operation_id: &str, now: i64) -> Result<bool> {
        let operation_id = operation_id.to_owned();
        call(&self.inner.db, move |conn| {
            let n = conn.execute(
                "UPDATE coordinator_desktop_ops SET state = 'attempted', updated_at = ?2
                 WHERE operation_id = ?1 AND state = 'prepared'",
                params![operation_id, now],
            )?;
            Ok(n == 1)
        })
        .await
    }

    /// Ends an operation that is `prepared` or `attempted`. A row in `done`,
    /// `failed` or `unknown` never changes (false). `unknown` is final for a
    /// non-capture operation.
    ///
    /// # Errors
    /// A database error.
    pub async fn desktop_op_finish(
        &self,
        operation_id: &str,
        outcome: OpOutcome,
        now: i64,
    ) -> Result<bool> {
        let operation_id = operation_id.to_owned();
        let (state, result, code) = match outcome {
            OpOutcome::Done(value) => ("done", Some(serde_json::to_string(&value)?), None),
            OpOutcome::Failed(code) => ("failed", None, Some(code)),
            OpOutcome::Unknown => ("unknown", None, None),
        };
        call(&self.inner.db, move |conn| {
            let n = conn.execute(
                "UPDATE coordinator_desktop_ops
                 SET state = ?2, result = ?3, code = ?4, updated_at = ?5
                 WHERE operation_id = ?1 AND state IN ('prepared', 'attempted')",
                params![operation_id, state, result, code, now],
            )?;
            Ok(n == 1)
        })
        .await
    }

    /// One operation.
    ///
    /// # Errors
    /// A database error.
    pub async fn desktop_op(&self, operation_id: &str) -> Result<Option<DesktopOp>> {
        let operation_id = operation_id.to_owned();
        call(&self.inner.db, move |conn| find_op(conn, &operation_id)).await
    }

    /// The operation of a grant and a request key.
    ///
    /// # Errors
    /// A database error.
    pub async fn desktop_op_by_key(
        &self,
        grant_id: &str,
        request_key: &str,
    ) -> Result<Option<DesktopOp>> {
        let (grant_id, request_key) = (grant_id.to_owned(), request_key.to_owned());
        call(&self.inner.db, move |conn| {
            find_by_key(conn, &grant_id, &request_key)
        })
        .await
    }

    /// The stored refusal of a grant and a request key: its payload hash and
    /// its code.
    ///
    /// # Errors
    /// A database error.
    pub async fn desktop_reject_by_key(
        &self,
        grant_id: &str,
        request_key: &str,
    ) -> Result<Option<(String, String)>> {
        let (grant_id, request_key) = (grant_id.to_owned(), request_key.to_owned());
        call(&self.inner.db, move |conn| {
            find_reject(conn, &grant_id, &request_key)
        })
        .await
    }

    /// The start recovery: every `prepared` row never reached the send and
    /// becomes `failed` with `authority_unknown`; every `attempted` row may
    /// have run and becomes `unknown`. Returns the two counts.
    ///
    /// # Errors
    /// A database error.
    pub async fn desktop_ops_recover(&self, now: i64) -> Result<(usize, usize)> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let prepared = tx.execute(
                "UPDATE coordinator_desktop_ops SET state = 'failed', code = ?2, updated_at = ?1
                 WHERE state = 'prepared'",
                params![now, code::AUTHORITY_UNKNOWN],
            )?;
            let attempted = tx.execute(
                "UPDATE coordinator_desktop_ops SET state = 'unknown', updated_at = ?1
                 WHERE state = 'attempted'",
                params![now],
            )?;
            tx.commit()?;
            Ok((prepared, attempted))
        })
        .await
    }
}
