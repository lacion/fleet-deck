//! Two reads of the coordinator desktop tools (R13 P6): the grant of a caller
//! session at the start of a call ([`DeckStore::desktop_caller_grant`], with
//! the lazy idle expiry of `decide_coordinator`), and the newest operations
//! of a grant for the board ([`DeckStore::desktop_ops_recent`]).

use std::sync::Arc;

use fleetdeck_proto::{SessionId, StationId};
use rusqlite::params;
use serde::Serialize;

use crate::store::grant_scopes::expire_idle;
use crate::store::grants::{Grant, GrantState, caller_grant};
use crate::store::{DeckStore, Result, call};

/// One row of the recent operations of a grant. It holds no resource, no
/// result and no hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecentOp {
    pub operation_id: String,
    pub tool: String,
    pub target_station: StationId,
    pub state: String,
    pub code: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl DeckStore {
    /// The newest grant of `(caller_station, caller_session)` when it can
    /// still back a call: `None` when there is none, or it is revoked,
    /// expired or idle. An idle grant is expired in this transaction (the
    /// rule of `decide_coordinator`). The binding is not compared here.
    ///
    /// # Errors
    /// A database error.
    pub async fn desktop_caller_grant(
        &self,
        caller_station: &StationId,
        caller_session: &SessionId,
        now: i64,
    ) -> Result<Option<Grant>> {
        let (station, session) = (caller_station.clone(), caller_session.clone());
        let inner = Arc::clone(&self.inner);
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(grant) = caller_grant(&tx, &station, &session)? else {
                return Ok(None);
            };
            if grant.state != GrantState::Active || now > grant.expires_at {
                return Ok(None);
            }
            if grant
                .last_used_at
                .is_some_and(|t| now.saturating_sub(t) > grant.idle_ttl_ms)
            {
                // Consumes the transaction: commits the expiry.
                expire_idle(tx, &inner, &grant.grant_id, now)?;
                return Ok(None);
            }
            Ok(Some(grant))
        })
        .await
    }

    /// The newest `limit` operations of a grant, newest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn desktop_ops_recent(&self, grant_id: &str, limit: u32) -> Result<Vec<RecentOp>> {
        let grant_id = grant_id.to_owned();
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT operation_id, tool, target_station, state, code, created_at, updated_at
                 FROM coordinator_desktop_ops WHERE grant_id = ?1
                 ORDER BY created_at DESC, operation_id DESC LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![grant_id, limit], |r| {
                    Ok(RecentOp {
                        operation_id: r.get(0)?,
                        tool: r.get(1)?,
                        target_station: StationId(r.get(2)?),
                        state: r.get(3)?,
                        code: r.get(4)?,
                        created_at: r.get(5)?,
                        updated_at: r.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }
}
