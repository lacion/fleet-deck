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

use fleetdeck_proto::artifacts::ArtifactKind;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::{
    CoordinatorBinding, CoordinatorQuery, CoordinatorQueryEnvelope, ProjectItemKind,
};
use fleetdeck_proto::orchestration::{MAX_WAIT_IDS, TaskState};
use fleetdeck_proto::{Reject, RejectCode, SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use super::grant_scopes::{NOT_COVERED, station_covered, sync_coordinate_scopes};
use super::grants::{Grant, GrantClass, GrantState, caller_grant};
use super::{DeckStore, Result, call};

/// The longest deadline of a query, and of `wait_task`.
pub const QUERY_DEADLINE_MS: i64 = 30_000;
pub const WAIT_DEADLINE_MS: i64 = 60_000;
const WAIT_TICK: Duration = Duration::from_millis(100);

/// What the deck decided for a query: a value, or a read that a station
/// must serve (ADR-008 §4.1). The store did all the authorization and the
/// `touch` before it returns a [`QueryAnswer::Station`].
#[derive(Debug, Clone, PartialEq)]
pub enum QueryAnswer {
    Value(Value),
    Station(StationRead),
}

/// One read of a station that a coordinator query needs.
#[derive(Debug, Clone, PartialEq)]
pub struct StationRead {
    pub station: StationId,
    /// The link method of the first request.
    pub method: &'static str,
    pub params: Value,
    pub shape: ReadShape,
    /// The label of the untrusted envelope of the answer (an artifact adds
    /// `/<path>` for each file).
    pub source: String,
}

/// How the answer of a station read becomes the answer of the query.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadShape {
    SessionDiff {
        session_id: SessionId,
    },
    FileRead {
        session_id: SessionId,
        path: String,
    },
    FileSearch {
        session_id: SessionId,
    },
    ProjectItems {
        project_id: String,
        kind: ProjectItemKind,
    },
    Artifact {
        kind: ArtifactKind,
        artifact_id: String,
        version: Option<u32>,
    },
}

type Answer = std::result::Result<QueryAnswer, Reject>;

pub(super) fn reject(code: RejectCode, message: &str) -> Reject {
    Reject::new(code, message)
}

/// Steps 2–3 of ADR-006 §6.2 for a query.
fn check(
    conn: &Connection,
    station: &StationId,
    caller: &SessionId,
    binding: &CoordinatorBinding,
    (now, changed): (i64, &mut BTreeSet<StationId>),
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
    // The exact rule (design §2.3a): a grant in effect names the station.
    if !station_covered(conn, station, now)? {
        return Ok(Err(reject(RejectCode::Forbidden, NOT_COVERED)));
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
        // The expiry and the scope rebuild commit with the caller's
        // transaction.
        conn.execute(
            "UPDATE tokens SET state = 'expired', revoked_reason = 'idle'
             WHERE grant_id = ?1 AND state = 'active'",
            [grant.grant_id.as_str()],
        )?;
        *changed = sync_coordinate_scopes(conn, now)?;
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
    /// Answers one `coordinator.query` from `station`: the result value, a
    /// station read to serve, or the rejection (never stored).
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
        let allowed = match &env.query {
            // The new read queries need `inspect` (and `answer` for the
            // open requests), never the ADR-006 `read` class (ADR-008 §3).
            CoordinatorQuery::NeedsYou => {
                grant.classes.contains(&GrantClass::Inspect)
                    && grant.classes.contains(&GrantClass::Answer)
            }
            CoordinatorQuery::SessionTail { .. }
            | CoordinatorQuery::SessionDiff { .. }
            | CoordinatorQuery::Projects
            | CoordinatorQuery::ProjectItems { .. }
            | CoordinatorQuery::Artifacts { .. }
            | CoordinatorQuery::Artifact { .. }
            | CoordinatorQuery::FileRead { .. }
            | CoordinatorQuery::FileSearch { .. } => grant.classes.contains(&GrantClass::Inspect),
            CoordinatorQuery::CommandStatus { .. } => {
                grant.classes.iter().any(|c| *c != GrantClass::Read)
            }
            _ => grant.classes.contains(&GrantClass::Read),
        };
        if !allowed {
            return Ok(Err(reject(
                RejectCode::Forbidden,
                "the grant has no class for this query",
            )));
        }
        // Every new query needs the deck setting `fleet_control` (ADR-008
        // §3, §4.1); the ADR-006 reads do not.
        let new_query = !matches!(
            env.query,
            CoordinatorQuery::ListSessions
                | CoordinatorQuery::CommandStatus { .. }
                | CoordinatorQuery::WaitTask { .. }
                | CoordinatorQuery::WaitTasks { .. }
        );
        if new_query && !self.fleet_control() {
            return Ok(Err(reject(
                RejectCode::Forbidden,
                "fleet_control_off: fleet control is off on this deck",
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
                QueryAnswer::Value(json!({ "sessions": sessions }))
            }
            CoordinatorQuery::CommandStatus { command_id } => {
                let caller = format!("{station}/{}", env.caller);
                let id = command_id.as_str().to_owned();
                let found: Option<(String, Vec<Value>)> = call(&self.inner.db, move |conn| {
                    let receipt: Option<String> = conn
                        .query_row(
                            "SELECT receipt FROM coordinator_receipts
                             WHERE command_id = ?1 AND caller = ?2",
                            params![id, caller],
                            |r| r.get(0),
                        )
                        .optional()?;
                    let Some(receipt) = receipt else {
                        return Ok(None);
                    };
                    // The deliveries of the call (ADR-008 §5.3): the outbox
                    // items that it created, in creation order.
                    let mut stmt = conn.prepare(
                        "SELECT outbox_id, state, reason FROM outbox WHERE call_id = ?1
                         ORDER BY created_at, outbox_id",
                    )?;
                    let deliveries = stmt
                        .query_map([&id], |r| {
                            Ok(json!({
                                "outbox_id": r.get::<_, String>(0)?,
                                "state": r.get::<_, String>(1)?,
                                "reason": r.get::<_, Option<String>>(2)?,
                            }))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(Some((receipt, deliveries)))
                })
                .await?;
                QueryAnswer::Value(match found {
                    Some((text, deliveries)) => json!({
                        "receipt": serde_json::from_str::<Value>(&text)?,
                        "deliveries": deliveries,
                    }),
                    None => json!({ "receipt": null, "deliveries": [] }),
                })
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
                    break QueryAnswer::Value(json!({ "task": task }));
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
                        break QueryAnswer::Value(json!({ "task": ready }));
                    }
                    tokio::time::sleep(WAIT_TICK).await;
                    if let Err(r) = self.query_grant(&station, &env).await? {
                        return Ok(Err(r));
                    }
                }
            }
            // The ADR-008 read queries (`coordinator_query_reads`).
            read => match self.read_query(&grant, read).await? {
                Ok(answer) => answer,
                Err(r) => return Ok(Err(r)),
            },
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
        let (station, caller, binding, clock) = (
            station.clone(),
            env.caller.clone(),
            env.binding.clone(),
            self.inner.clock.clone(),
        );
        let (checked, changed) = call(&self.inner.db, move |conn| {
            // One transaction for the admission and the lazy idle expiry; it
            // commits also for a refusal. The clock is read inside it (GS2).
            let tx = conn.transaction()?;
            let now = super::now_ms(&clock);
            let mut changed = BTreeSet::new();
            let checked = check(&tx, &station, &caller, &binding, (now, &mut changed))?;
            tx.commit()?;
            Ok((checked, changed))
        })
        .await?;
        self.note_scope_changes(changed);
        Ok(checked)
    }
}
