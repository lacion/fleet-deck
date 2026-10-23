//! The stored MCP Apps (B15, design `mcp-apps.md` §4.4, table `mcp_apps`).
//!
//! A row and its `tool.app` event are written in one transaction, then the
//! caps delete the oldest rows (the events stay). A row that the caps deleted
//! but whose `tool.app` event is in the session's log is [`AppLookup::Gone`].

use fleetdeck_core::new_id;
use fleetdeck_proto::apps::{AppCsp, AppMeta, MAX_APP_INPUT_BYTES, MAX_APP_RESULT_BYTES};
use fleetdeck_proto::{Event, NodeId, SessionId};
use ring::digest::{SHA256, digest};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use super::{NewEvent, Result, Store, StoreError, call, log};

/// The most stored apps of one session; more delete the oldest rows.
pub const MAX_APPS_PER_SESSION: i64 = 50;
/// The most stored apps of one station; more delete the oldest rows.
pub const MAX_APPS_PER_STATION: i64 = 500;

/// The id of an app: a ULID that the store makes.
pub type AppId = String;

/// The part of a new app row that the `tool.app` event does not carry.
#[derive(Debug, Clone, PartialEq)]
pub struct NewApp {
    pub session_id: SessionId,
    /// The document as read, at most `MAX_APP_DOC_BYTES`.
    pub html: Vec<u8>,
    pub csp: AppCsp,
    /// Stored as `null` when its JSON is over `MAX_APP_INPUT_BYTES`.
    pub tool_input: Value,
    /// Not stored when its JSON is over `MAX_APP_RESULT_BYTES`.
    pub tool_result: Option<Value>,
}

/// The `tool.app` event of a new app, without the app id (the store makes the
/// id). Its fields are also the columns `node_id`, `server`, `tool`,
/// `resource_uri` and `prefers_border` of the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolAppEvent {
    pub node_id: NodeId,
    pub server: String,
    pub tool: String,
    pub resource_uri: String,
    pub prefers_border: Option<bool>,
}

/// The answer of [`Store::app_lookup`].
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "one answer per lookup; it lives only until the caller matches it"
)]
pub enum AppLookup {
    Found(AppMeta),
    /// No row, and the session's log has a `tool.app` event with this app id:
    /// the caps deleted it.
    Gone,
    Unknown,
}

/// The JSON text of a value, or `None` when it is longer than `max` bytes.
fn bounded(value: &Value, max: usize) -> Result<Option<String>> {
    let text = serde_json::to_string(value)?;
    Ok((text.len() <= max).then_some(text))
}

/// Inserts the row and appends the event, then applies the caps.
fn insert(
    conn: &Connection,
    id: AppId,
    app: &NewApp,
    event: ToolAppEvent,
    now: i64,
    out: &mut Vec<fleetdeck_proto::EventEnvelope>,
) -> Result<AppId> {
    let input = bounded(&app.tool_input, MAX_APP_INPUT_BYTES)?.unwrap_or_else(|| "null".to_owned());
    let result = match &app.tool_result {
        Some(value) => bounded(value, MAX_APP_RESULT_BYTES)?,
        None => None,
    };
    let sha = hex(digest(&SHA256, &app.html).as_ref());
    conn.execute(
        "INSERT INTO mcp_apps (app_id, session_id, node_id, server, tool, resource_uri, html,
             doc_sha256, csp_json, prefers_border, tool_input, tool_result, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            id,
            app.session_id.as_str(),
            event.node_id.as_str(),
            event.server,
            event.tool,
            event.resource_uri,
            app.html,
            sha,
            serde_json::to_string(&app.csp)?,
            event.prefers_border.map(i64::from),
            input,
            result,
            now,
        ],
    )?;
    let body = NewEvent::from(Event::ToolApp {
        node_id: event.node_id,
        app_id: id.clone(),
        server: event.server,
        tool: event.tool,
        resource_uri: event.resource_uri,
        prefers_border: event.prefers_border,
    });
    out.push(log::append_one(conn, &app.session_id, body, now)?);
    // The caps: the oldest rows go, the events stay.
    conn.execute(
        "DELETE FROM mcp_apps WHERE session_id = ?1 AND app_id NOT IN
           (SELECT app_id FROM mcp_apps WHERE session_id = ?1
            ORDER BY created_at DESC, app_id DESC LIMIT ?2)",
        params![app.session_id.as_str(), MAX_APPS_PER_SESSION],
    )?;
    conn.execute(
        "DELETE FROM mcp_apps WHERE app_id NOT IN
           (SELECT app_id FROM mcp_apps ORDER BY created_at DESC, app_id DESC LIMIT ?1)",
        [MAX_APPS_PER_STATION],
    )?;
    Ok(id)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

type RawMeta = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<i64>,
    i64,
    String,
    String,
    Option<String>,
    i64,
);

fn lookup(conn: &Connection, session: &SessionId, app_id: &str) -> Result<AppLookup> {
    let raw: Option<RawMeta> = conn
        .query_row(
            "SELECT app_id, node_id, server, tool, resource_uri, csp_json, prefers_border,
                    length(html), doc_sha256, tool_input, tool_result, created_at
             FROM mcp_apps WHERE app_id = ?1 AND session_id = ?2",
            params![app_id, session.as_str()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                ))
            },
        )
        .optional()?;
    if let Some((
        app_id,
        node_id,
        server,
        tool,
        resource_uri,
        csp,
        border,
        doc_bytes,
        doc_sha256,
        input,
        result,
        created_at,
    )) = raw
    {
        return Ok(AppLookup::Found(AppMeta {
            app_id,
            node_id: NodeId::from(node_id),
            server,
            tool,
            resource_uri,
            csp: serde_json::from_str(&csp)?,
            prefers_border: border.map(|b| b != 0),
            doc_bytes: u64::try_from(doc_bytes)
                .map_err(|_| StoreError::Corrupt("app document length".to_owned()))?,
            doc_sha256,
            tool_input: serde_json::from_str(&input)?,
            tool_result: result.map(|text| serde_json::from_str(&text)).transpose()?,
            created_at,
        }));
    }
    let evented = conn
        .query_row(
            "SELECT 1 FROM events WHERE session_id = ?1 AND kind = 'tool.app'
               AND json_extract(payload, '$.app_id') = ?2 LIMIT 1",
            params![session.as_str(), app_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    Ok(if evented {
        AppLookup::Gone
    } else {
        AppLookup::Unknown
    })
}

fn chunk(
    conn: &Connection,
    session: &SessionId,
    app_id: &str,
    offset: u64,
    len: usize,
) -> Result<Option<(Vec<u8>, u64)>> {
    // SQLite counts the bytes of a BLOB from 1.
    let start = i64::try_from(offset).unwrap_or(i64::MAX).saturating_add(1);
    let len_arg = i64::try_from(len).unwrap_or(i64::MAX);
    let found: Option<(Vec<u8>, i64)> = conn
        .query_row(
            "SELECT substr(html, ?3, ?4), length(html) FROM mcp_apps
             WHERE app_id = ?1 AND session_id = ?2",
            params![app_id, session.as_str(), start, len_arg],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    found
        .map(|(bytes, total)| {
            let total = u64::try_from(total)
                .map_err(|_| StoreError::Corrupt("app document length".to_owned()))?;
            Ok((bytes, total))
        })
        .transpose()
}

impl Store {
    /// Stores an app row and appends its `tool.app` event in one transaction,
    /// then applies the caps (more than [`MAX_APPS_PER_SESSION`] rows of the
    /// session or [`MAX_APPS_PER_STATION`] rows of the station delete the
    /// oldest rows; the events stay).
    ///
    /// # Errors
    /// [`StoreError::UnknownSession`] or a database error; nothing is stored.
    pub async fn insert_app(&self, app: NewApp, event: ToolAppEvent) -> Result<AppId> {
        let id: AppId = new_id(self.inner.ids.as_ref());
        self.write(move |conn, now, out| insert(conn, id, &app, event, now, out))
            .await
            .map(|(id, _)| id)
    }

    /// The app of a session. A query with a session id that is not the row's
    /// session finds no row.
    ///
    /// # Errors
    /// A database error, or a row that does not decode.
    pub async fn app_lookup(&self, session: SessionId, app_id: String) -> Result<AppLookup> {
        call(&self.inner.db, move |conn| lookup(conn, &session, &app_id)).await
    }

    /// At most `len` bytes of the document from `offset`, and the total size.
    /// `None` when the session has no such row.
    ///
    /// # Errors
    /// A database error.
    pub async fn app_chunk(
        &self,
        session: SessionId,
        app_id: String,
        offset: u64,
        len: usize,
    ) -> Result<Option<(Vec<u8>, u64)>> {
        call(&self.inner.db, move |conn| {
            chunk(conn, &session, &app_id, offset, len)
        })
        .await
    }

    /// How many app rows the station keeps (tests and diagnostics).
    ///
    /// # Errors
    /// A database error.
    pub async fn app_count(&self) -> Result<u64> {
        call(&self.inner.db, move |conn| {
            let n: i64 = conn.query_row("SELECT count(*) FROM mcp_apps", [], |r| r.get(0))?;
            u64::try_from(n).map_err(|_| StoreError::Corrupt("app count".to_owned()))
        })
        .await
    }
}

#[cfg(test)]
#[path = "apps_tests.rs"]
mod tests;
