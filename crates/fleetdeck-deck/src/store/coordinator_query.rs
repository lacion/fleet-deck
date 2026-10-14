//! `coordinator.query` (M3 6-5, ADR-006 §6.6). A query keeps no receipt.
//!
//! The deck runs the decision steps 1–3 of `coordinator.call` for each
//! query (incarnation, `coordinate` scope, grant and binding), checks the
//! deadline and the class, and filters the result to the grant's stations.
//! `wait_task` rechecks the grant each time it wakes, so a revocation or an
//! expiry during the wait ends it with `forbidden`. A successful query moves
//! `last_used_at`.

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::{CoordinatorBinding, CoordinatorQuery, CoordinatorQueryEnvelope};
use fleetdeck_proto::orchestration::{MAX_WAIT_IDS, TaskState};
use fleetdeck_proto::{Reject, RejectCode, SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use super::grants::{Grant, GrantClass, GrantState, caller_grant};
use super::{DeckStore, Result, call};

/// The longest deadline of a query, and of `wait_task`.
pub const QUERY_DEADLINE_MS: i64 = 30_000;
pub const WAIT_DEADLINE_MS: i64 = 60_000;
const WAIT_TICK: Duration = Duration::from_millis(100);

type Answer = std::result::Result<Value, Reject>;

fn reject(code: RejectCode, message: &str) -> Reject {
    Reject::new(code, message)
}

/// Steps 2–3 of ADR-006 §6.2 for a query.
fn check(
    conn: &Connection,
    station: &StationId,
    caller: &SessionId,
    binding: &CoordinatorBinding,
    now: i64,
) -> Result<std::result::Result<Grant, Reject>> {
    let scopes: Option<String> = conn
        .query_row(
            "SELECT scopes FROM stations WHERE station_id = ?1",
            [station.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let granted = match scopes {
        Some(text) => serde_json::from_str::<BTreeSet<Scope>>(&text)?.contains(&Scope::Coordinate),
        None => false,
    };
    if !granted {
        return Ok(Err(reject(RejectCode::Forbidden, "no `coordinate` grant")));
    }
    let Some(grant) = caller_grant(conn, station, caller)? else {
        return Ok(Err(reject(
            RejectCode::Forbidden,
            "the caller has no grant",
        )));
    };
    if grant.state != GrantState::Active || now > grant.expires_at {
        return Ok(Err(reject(
            RejectCode::Forbidden,
            "the grant is revoked or expired",
        )));
    }
    if grant.binding.as_ref() != Some(binding) {
        return Ok(Err(reject(
            RejectCode::BindingStale,
            "the binding is stale",
        )));
    }
    if grant
        .last_used_at
        .is_some_and(|t| now.saturating_sub(t) > grant.idle_ttl_ms)
    {
        conn.execute(
            "UPDATE tokens SET state = 'expired', revoked_reason = 'idle'
             WHERE grant_id = ?1 AND state = 'active'",
            [grant.grant_id.as_str()],
        )?;
        return Ok(Err(reject(
            RejectCode::Forbidden,
            "the grant expired after its idle time",
        )));
    }
    Ok(Ok(grant))
}

fn touch(conn: &Connection, grant: &Grant, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE tokens SET last_used_at = ?2 WHERE grant_id = ?1",
        params![grant.grant_id, now],
    )?;
    Ok(())
}

impl DeckStore {
    /// Answers one `coordinator.query` from `station`: the result value or
    /// the rejection (never stored).
    ///
    /// # Errors
    /// A database error.
    pub async fn coordinator_query(
        &self,
        station: StationId,
        env: CoordinatorQueryEnvelope,
    ) -> Result<Answer> {
        let now = self.now_ms();
        if &env.target_incarnation != self.incarnation() {
            return Ok(Err(reject(
                RejectCode::IncarnationMismatch,
                "the query names another deck database",
            )));
        }
        let longest = match env.query {
            CoordinatorQuery::WaitTask { .. } | CoordinatorQuery::WaitTasks { .. } => {
                WAIT_DEADLINE_MS
            }
            _ => QUERY_DEADLINE_MS,
        };
        if env.deadline < now {
            return Ok(Err(reject(
                RejectCode::Expired,
                "the query deadline passed",
            )));
        }
        if env.deadline > now + longest {
            return Ok(Err(reject(
                RejectCode::InvalidCommand,
                "the query deadline is too far away",
            )));
        }
        let grant = match self.query_grant(&station, &env).await? {
            Ok(grant) => grant,
            Err(r) => return Ok(Err(r)),
        };
        let needs_read = !matches!(env.query, CoordinatorQuery::CommandStatus { .. });
        let allowed = if needs_read {
            grant.classes.contains(&GrantClass::Read)
        } else {
            grant.classes.iter().any(|c| *c != GrantClass::Read)
        };
        if !allowed {
            return Ok(Err(reject(
                RejectCode::Forbidden,
                "the grant has no class for this query",
            )));
        }
        let answer = match &env.query {
            CoordinatorQuery::ListSessions => {
                let sessions: Vec<Value> = self
                    .sessions()
                    .await?
                    .into_iter()
                    .filter(|s| grant.stations.contains(&s.station_id))
                    .map(|s| {
                        json!({"station": s.station_id, "session_id": s.summary.session_id,
                               "title": s.summary.title, "callsign": s.summary.callsign,
                               "cwd": s.summary.cwd, "status": s.status})
                    })
                    .collect();
                json!({ "sessions": sessions })
            }
            CoordinatorQuery::CommandStatus { command_id } => {
                let caller = format!("{station}/{}", env.caller);
                let id = command_id.as_str().to_owned();
                let receipt: Option<String> = call(&self.inner.db, move |conn| {
                    Ok(conn
                        .query_row(
                            "SELECT receipt FROM coordinator_receipts
                             WHERE command_id = ?1 AND caller = ?2",
                            params![id, caller],
                            |r| r.get(0),
                        )
                        .optional()?)
                })
                .await?;
                match receipt {
                    Some(text) => json!({ "receipt": serde_json::from_str::<Value>(&text)? }),
                    None => json!({ "receipt": null }),
                }
            }
            CoordinatorQuery::WaitTask { task_id } => loop {
                let Some(task) = self.task(task_id.clone()).await? else {
                    return Ok(Err(reject(RejectCode::UnknownRequest, "no such task")));
                };
                if !grant.stations.contains(&task.station_id) {
                    return Ok(Err(reject(
                        RejectCode::Forbidden,
                        "the task is outside the grant",
                    )));
                }
                if task.state != TaskState::Open || self.now_ms() >= env.deadline {
                    break json!({ "task": task });
                }
                tokio::time::sleep(WAIT_TICK).await;
                if let Err(r) = self.query_grant(&station, &env).await? {
                    return Ok(Err(r));
                }
            },
            CoordinatorQuery::WaitTasks { task_ids } => {
                if task_ids.is_empty() || task_ids.len() > MAX_WAIT_IDS {
                    return Ok(Err(reject(
                        RejectCode::InvalidCommand,
                        "a wait names 1 to 50 tasks",
                    )));
                }
                loop {
                    let tasks = match self.tasks_by_ids(task_ids.clone()).await? {
                        Ok(tasks) => tasks,
                        Err(_) => {
                            return Ok(Err(reject(RejectCode::UnknownRequest, "no such task")));
                        }
                    };
                    if tasks
                        .iter()
                        .any(|t| !grant.stations.contains(&t.station_id))
                    {
                        return Ok(Err(reject(
                            RejectCode::Forbidden,
                            "a task is outside the grant",
                        )));
                    }
                    let ready = tasks.into_iter().find(|t| t.state != TaskState::Open);
                    if ready.is_some() || self.now_ms() >= env.deadline {
                        break json!({ "task": ready });
                    }
                    tokio::time::sleep(WAIT_TICK).await;
                    if let Err(r) = self.query_grant(&station, &env).await? {
                        return Ok(Err(r));
                    }
                }
            }
            // Placeholder until the ADR-008 deck task replaces it.
            CoordinatorQuery::SessionTail { .. }
            | CoordinatorQuery::SessionDiff { .. }
            | CoordinatorQuery::NeedsYou
            | CoordinatorQuery::Projects
            | CoordinatorQuery::ProjectItems { .. }
            | CoordinatorQuery::Artifacts { .. }
            | CoordinatorQuery::Artifact { .. }
            | CoordinatorQuery::FileRead { .. }
            | CoordinatorQuery::FileSearch { .. } => {
                let name = serde_json::to_value(&env.query)
                    .ok()
                    .and_then(|v| v.get("query").and_then(|n| n.as_str().map(str::to_owned)))
                    .unwrap_or_default();
                return Ok(Err(reject(
                    RejectCode::Unsupported,
                    &format!("{name} is not implemented yet"),
                )));
            }
        };
        let now = self.now_ms();
        call(&self.inner.db, move |conn| touch(conn, &grant, now)).await?;
        Ok(Ok(answer))
    }

    async fn query_grant(
        &self,
        station: &StationId,
        env: &CoordinatorQueryEnvelope,
    ) -> Result<std::result::Result<Grant, Reject>> {
        let (station, caller, binding, now) = (
            station.clone(),
            env.caller.clone(),
            env.binding.clone(),
            self.now_ms(),
        );
        call(&self.inner.db, move |conn| {
            check(conn, &station, &caller, &binding, now)
        })
        .await
    }
}
