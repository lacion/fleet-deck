//! The deck audit log (PLAN §3 rule 7, §10 `audit`): one row for every
//! mutation of the board API and of `deck.sock`, and for every login
//! attempt: who, what, when, and the result. A row never holds a secret.

use fleetdeck_proto::CommandId;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::{DeckStore, Result, call};

/// The outcome of an audited action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditResult {
    Ok,
    /// Refused by authentication, a scope, CSRF, the Origin check or input
    /// validation.
    Refused,
    /// The action was allowed but did not succeed.
    Failed,
}

impl AuditResult {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "ok" => Self::Ok,
            "refused" => Self::Refused,
            _ => Self::Failed,
        }
    }
}

/// A new audit row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAudit {
    /// `owner`, `token:<name>`, `local` (deck.sock) or `anonymous`.
    pub actor: String,
    /// The API method or `deck.sock` request, for example `auth.login`.
    pub action: String,
    /// The station, session, token or other object of the action.
    pub target: Option<String>,
    pub command_id: Option<CommandId>,
    pub result: AuditResult,
    /// A short reason; never a secret.
    pub detail: Option<String>,
}

impl NewAudit {
    #[must_use]
    pub fn new(actor: impl Into<String>, action: impl Into<String>, result: AuditResult) -> Self {
        Self {
            actor: actor.into(),
            action: action.into(),
            target: None,
            command_id: None,
            result,
            detail: None,
        }
    }

    #[must_use]
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    #[must_use]
    pub fn command(mut self, id: CommandId) -> Self {
        self.command_id = Some(id);
        self
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// A stored audit row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub audit_id: i64,
    /// Unix ms.
    pub at: i64,
    pub actor: String,
    pub action: String,
    pub target: Option<String>,
    pub command_id: Option<CommandId>,
    pub result: AuditResult,
    pub detail: Option<String>,
}

impl DeckStore {
    /// Appends one audit row.
    ///
    /// # Errors
    /// A database error.
    pub async fn audit(&self, entry: NewAudit) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "INSERT INTO audit (at, actor, action, target, command_id, result, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    now,
                    entry.actor,
                    entry.action,
                    entry.target,
                    entry.command_id.as_ref().map(CommandId::as_str),
                    entry.result.as_str(),
                    entry.detail,
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Appends one audit row and logs a database error instead of
    /// returning it: an audit failure must not hide the action's answer.
    pub async fn audit_or_log(&self, entry: NewAudit) {
        if let Err(e) = self.audit(entry).await {
            tracing::error!(error = %e, "audit row");
        }
    }

    /// The newest `limit` audit rows, newest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn audit_log(&self, limit: u32) -> Result<Vec<AuditEntry>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT audit_id, at, actor, action, target, command_id, result, detail
                 FROM audit ORDER BY audit_id DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map([limit], |r| {
                Ok(AuditEntry {
                    audit_id: r.get(0)?,
                    at: r.get(1)?,
                    actor: r.get(2)?,
                    action: r.get(3)?,
                    target: r.get(4)?,
                    command_id: r.get::<_, Option<String>>(5)?.map(CommandId::from),
                    result: AuditResult::parse(&r.get::<_, String>(6)?),
                    detail: r.get(7)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }
}
