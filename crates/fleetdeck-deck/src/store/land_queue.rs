//! The land queue of the deck (design §11.2; FG-8). One row for each land
//! request: `queued`, then `running` while the station works, then a
//! terminal state. The partial unique indexes keep one `running` land for
//! each project and each lock key.
//!
//! - [`DeckStore::land_request`]: the owner route and the coordinator op
//!   queue a land (decisions Q2, Q6).
//! - The fold: a `session.land` event, a `fleet_git.land_status` answer and
//!   a `fleet_git.land_abandon` answer change a `running` row (Q11).
//! - The owner actions: cancel, "Stop checking" (abandon) and forget.
//!
//! Admission is in [`super::land_admit`], the outbox in [`super::land_outbox`].

use std::collections::BTreeSet;

use fleetdeck_proto::fleet_git::{LandInfo, LandPhase, LandPlan, LandState, LandStatus, LandWrite};
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_proto::{SessionSummary, StationId};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::Serialize;

pub use super::land_ready::LandRefusal;
use super::land_ready::{Env, check, summary_of};
use super::land_text;
use super::{DeckStore, LinkFeature, Result, call, new_id, to_u64};

/// The life of a coordinator's land request: the ADR-008 fleet intent TTL.
pub const AUTHORITY_TTL_MS: i64 = 600_000;

/// The columns of a land row, in the order of [`read`].
pub(super) const COLS: &str = "land_id, command_id, requested_by, request_command_id, authority, authority_expires_at, project_id, lock_key, station_id, session_id, plan, settings_revision, state, phase, reconciling, abandon_state, abandon_attempts, abandon_next_at, abandon_write, attempt, source_oid, base_oid, rebased_oid, branch_published_oid, default_published_oid, pr_url, files, code, message, created_at, started_at, ended_at";

/// One row of `land_queue`.
#[derive(Debug, Clone, PartialEq)]
pub struct LandRow {
    pub land_id: String,
    pub command_id: String,
    pub requested_by: String,
    pub request_command_id: String,
    pub authority: Option<CommandAuthority>,
    pub authority_expires_at: Option<i64>,
    pub project_id: String,
    pub lock_key: String,
    pub station_id: String,
    pub session_id: String,
    pub plan: Option<LandPlan>,
    pub settings_revision: Option<u64>,
    pub state: String,
    pub phase: Option<String>,
    pub reconciling: bool,
    pub abandon_state: Option<String>,
    pub abandon_attempts: u64,
    pub abandon_next_at: Option<i64>,
    pub abandon_write: Option<String>,
    pub attempt: u32,
    pub source_oid: Option<String>,
    pub base_oid: Option<String>,
    pub rebased_oid: Option<String>,
    pub branch_published_oid: Option<String>,
    pub default_published_oid: Option<String>,
    pub pr_url: Option<String>,
    pub files: Vec<String>,
    pub code: Option<String>,
    pub message: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
}

fn json_of<T: serde::de::DeserializeOwned>(text: Option<String>) -> Result<Option<T>> {
    Ok(text.map(|t| serde_json::from_str(&t)).transpose()?)
}

pub(super) fn read(r: &Row<'_>) -> Result<LandRow> {
    Ok(LandRow {
        land_id: r.get(0)?,
        command_id: r.get(1)?,
        requested_by: r.get(2)?,
        request_command_id: r.get(3)?,
        authority: json_of(r.get(4)?)?,
        authority_expires_at: r.get(5)?,
        project_id: r.get(6)?,
        lock_key: r.get(7)?,
        station_id: r.get(8)?,
        session_id: r.get(9)?,
        plan: json_of(r.get(10)?)?,
        settings_revision: r.get::<_, Option<i64>>(11)?.map(to_u64).transpose()?,
        state: r.get(12)?,
        phase: r.get(13)?,
        reconciling: r.get::<_, i64>(14)? == 1,
        abandon_state: r.get(15)?,
        abandon_attempts: to_u64(r.get(16)?)?,
        abandon_next_at: r.get(17)?,
        abandon_write: r.get(18)?,
        attempt: u32::try_from(r.get::<_, i64>(19)?).unwrap_or(0),
        source_oid: r.get(20)?,
        base_oid: r.get(21)?,
        rebased_oid: r.get(22)?,
        branch_published_oid: r.get(23)?,
        default_published_oid: r.get(24)?,
        pr_url: r.get(25)?,
        files: serde_json::from_str(&r.get::<_, String>(26)?)?,
        code: r.get(27)?,
        message: r.get(28)?,
        created_at: r.get(29)?,
        started_at: r.get(30)?,
        ended_at: r.get(31)?,
    })
}

pub(super) fn find(conn: &Connection, land_id: &str) -> Result<Option<LandRow>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM land_queue WHERE land_id = ?1"),
        [land_id],
        |r| Ok(read(r)),
    )
    .optional()?
    .transpose()
}

/// The stored name of a land state.
#[must_use]
pub fn state_name(state: LandState) -> &'static str {
    match state {
        LandState::Queued => "queued",
        LandState::Running => "running",
        LandState::Landed => "landed",
        LandState::Conflict => "conflict",
        LandState::CheckFailed => "check_failed",
        LandState::CheckMutated => "check_mutated",
        LandState::RemoteMoved => "remote_moved",
        LandState::Failed => "failed",
        LandState::Unresolved => "unresolved",
        LandState::Cancelled => "cancelled",
        LandState::Unknown => "unknown",
    }
}

/// The stored name of a land phase.
#[must_use]
pub fn phase_name(phase: LandPhase) -> &'static str {
    match phase {
        LandPhase::Admitted => "admitted",
        LandPhase::Captured => "captured",
        LandPhase::WorktreeReady => "worktree_ready",
        LandPhase::Rebased => "rebased",
        LandPhase::Checking => "checking",
        LandPhase::Checked => "checked",
        LandPhase::BranchPushIntent => "branch_push_intent",
        LandPhase::BranchPushed => "branch_pushed",
        LandPhase::PrIntent => "pr_intent",
        LandPhase::DefaultPushIntent => "default_push_intent",
        LandPhase::Lost => "lost",
        LandPhase::Done => "done",
        LandPhase::Unknown => "unknown",
    }
}

/// What the station reports about a land, from an event or a status.
#[derive(Debug, Clone)]
pub(super) struct Progress {
    pub(super) state: LandState,
    pub(super) phase: LandPhase,
    pub(super) reconciling: bool,
    pub(super) attempt: u32,
    pub(super) oids: [Option<String>; 5],
    pub(super) pr_url: Option<String>,
    pub(super) files: Vec<String>,
    pub(super) code: Option<String>,
    pub(super) text: Option<String>,
}

impl Progress {
    pub(super) fn of_info(i: &LandInfo) -> Self {
        Self {
            state: i.state,
            phase: i.phase,
            reconciling: i.reconciling,
            attempt: i.attempt,
            oids: [
                i.source_oid.clone(),
                i.base_oid.clone(),
                i.rebased_oid.clone(),
                i.branch_published_oid.clone(),
                i.default_published_oid.clone(),
            ],
            pr_url: i.pr_url.clone(),
            files: i.files.clone(),
            code: i.code.clone(),
            text: i.text.clone(),
        }
    }

    pub(super) fn of_status(s: &LandStatus) -> Self {
        Self {
            state: s.state,
            phase: s.phase,
            reconciling: s.reconciling,
            attempt: s.attempt,
            oids: [
                s.source_oid.clone(),
                s.base_oid.clone(),
                s.rebased_oid.clone(),
                s.branch_published_oid.clone(),
                s.default_published_oid.clone(),
            ],
            pr_url: s.pr_url.clone(),
            files: s.files.clone(),
            code: s.code.clone(),
            text: s.text.clone(),
        }
    }
}

/// Folds what the station reports into a `running` row (Q11). A terminal
/// state ends the row; `Queued`, `Cancelled` and `Unknown` leave it
/// `running` (fail closed, decision 82). Any report also proves that the
/// station took the command, so the outbox row becomes `accepted`. Returns
/// whether a `running` row changed.
pub(super) fn apply_progress(
    tx: &Connection,
    land_id: &str,
    p: &Progress,
    now: i64,
) -> Result<bool> {
    let ends = p.state.is_terminal() && p.state != LandState::Cancelled;
    let changed = tx.execute(
        "UPDATE land_queue SET phase = ?2, reconciling = ?3, attempt = ?4,
             source_oid = ?5, base_oid = ?6, rebased_oid = ?7, branch_published_oid = ?8,
             default_published_oid = ?9, pr_url = ?10, files = ?11, code = ?12, message = ?13,
             state = ?14, ended_at = ?15
         WHERE land_id = ?1 AND state = 'running'",
        params![
            land_id,
            phase_name(p.phase),
            i64::from(p.reconciling),
            i64::from(p.attempt),
            p.oids[0],
            p.oids[1],
            p.oids[2],
            p.oids[3],
            p.oids[4],
            p.pr_url,
            serde_json::to_string(&p.files)?,
            p.code,
            p.text,
            if ends { state_name(p.state) } else { "running" },
            ends.then_some(now),
        ],
    )? > 0;
    if changed {
        tx.execute(
            "UPDATE land_outbox SET state = 'accepted', updated_at = ?2
             WHERE land_id = ?1 AND state = 'pending'",
            params![land_id, now],
        )?;
    }
    Ok(changed)
}

/// The `session.land` event hook of the read model (FG-8): the event of a
/// land that the queue knows, for its own station and session.
pub(super) fn record_land(
    tx: &Transaction<'_>,
    station: &StationId,
    e: &fleetdeck_proto::EventEnvelope,
    now: i64,
) -> Result<()> {
    let fleetdeck_proto::event::EventBody::Known(fleetdeck_proto::event::Event::SessionLand(info)) =
        &e.event
    else {
        return Ok(());
    };
    let owner: Option<(String, String)> = tx
        .query_row(
            "SELECT station_id, session_id FROM land_queue WHERE land_id = ?1",
            [&info.land_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match owner {
        Some((st, se)) if st == station.as_str() && se == e.session_id.as_str() => {
            if !apply_progress(tx, &info.land_id, &Progress::of_info(info), now)? {
                tracing::debug!(land = %info.land_id, "session.land for a land that is not running");
            }
        }
        Some(_) => tracing::warn!(land = %info.land_id, "session.land from another station"),
        None => {}
    }
    Ok(())
}

/// Ends a `queued` or `running` row with `state` (Q13: the deck-made end).
/// A pending outbox row is rejected with it. Returns whether a row ended.
pub(super) fn end_row(
    tx: &Connection,
    land_id: &str,
    state: LandState,
    (code, message): (&str, &str),
    now: i64,
) -> Result<bool> {
    let ended = tx.execute(
        "UPDATE land_queue SET state = ?2, code = ?3, message = ?4, ended_at = ?5
         WHERE land_id = ?1 AND state IN ('queued', 'running')",
        params![land_id, state_name(state), code, message, now],
    )? > 0;
    if ended {
        tx.execute(
            "UPDATE land_outbox SET state = 'rejected', updated_at = ?2
             WHERE land_id = ?1 AND state = 'pending'",
            params![land_id, now],
        )?;
    }
    Ok(ended)
}

/// What a fence does to a row (design §11.2, revision 4): the answer's
/// terminal `run_state` other than `unresolved`, else `unresolved` with
/// `abandoned` and the text of the write. Returns whether a `running` row
/// ended.
pub(super) fn fence_row(
    tx: &Connection,
    row: &LandRow,
    (write, run_state): (LandWrite, Option<LandState>),
    now: i64,
) -> Result<bool> {
    let write_name = serde_json::to_value(write)?
        .as_str()
        .unwrap_or("unknown")
        .to_owned();
    tx.execute(
        "UPDATE land_queue SET abandon_state = 'fenced', abandon_write = ?2 WHERE land_id = ?1",
        params![row.land_id, write_name],
    )?;
    if row.state != "running" {
        return Ok(false);
    }
    match run_state.filter(|s| s.is_terminal() && !matches!(s, LandState::Unresolved | LandState::Cancelled)) {
        Some(state) => Ok(tx.execute(
            "UPDATE land_queue SET state = ?2, ended_at = ?3 WHERE land_id = ?1 AND state = 'running'",
            params![row.land_id, state_name(state), now],
        )? > 0),
        None => end_row(
            tx,
            &row.land_id,
            LandState::Unresolved,
            ("abandoned", &land_text::abandoned(row, write)),
            now,
        ),
    }
}

/// How many `queued` or `running` rows of the project or the lock key are
/// ahead of `row` (Q9).
fn position(conn: &Connection, row: &LandRow) -> Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM land_queue
         WHERE state IN ('queued', 'running') AND land_id <> ?1
           AND (project_id = ?2 OR lock_key = ?3)
           AND (created_at < ?4 OR (created_at = ?4 AND land_id < ?1))",
        params![row.land_id, row.project_id, row.lock_key, row.created_at],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

/// The row of the board (decision Q9).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LandView {
    pub land_id: String,
    pub session_id: String,
    pub station_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub requested_by: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub reconciling: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abandon_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abandon_write: Option<String>,
    pub attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rebased_oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_published_oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_published_oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
}

pub(super) fn view(conn: &Connection, row: LandRow) -> Result<LandView> {
    let summary: Option<SessionSummary> = summary_of(conn, &row.station_id, &row.session_id)?;
    let position = if row.state == "queued" {
        Some(position(conn, &row)?)
    } else {
        None
    };
    let branch = row
        .plan
        .as_ref()
        .map(|p| p.branch_ref.clone())
        .or_else(|| summary.as_ref().and_then(|s| s.branch.clone()));
    Ok(LandView {
        callsign: summary.as_ref().and_then(|s| s.callsign.clone()),
        title: summary.as_ref().and_then(|s| s.title.clone()),
        branch,
        land_id: row.land_id,
        session_id: row.session_id,
        station_id: row.station_id,
        requested_by: row.requested_by,
        state: row.state,
        phase: row.phase,
        reconciling: row.reconciling,
        abandon_state: row.abandon_state,
        abandon_write: row.abandon_write,
        attempt: row.attempt,
        source_oid: row.source_oid,
        base_oid: row.base_oid,
        rebased_oid: row.rebased_oid,
        branch_published_oid: row.branch_published_oid,
        default_published_oid: row.default_published_oid,
        pr_url: row.pr_url,
        files: row.files,
        code: row.code,
        message: row.message,
        position,
        created_at: row.created_at,
        started_at: row.started_at,
        ended_at: row.ended_at,
    })
}

/// The queue and the last 20 ends of a project (design §11.9).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LandsView {
    pub queue: Vec<LandView>,
    pub recent: Vec<LandView>,
}

/// What a request asks (decision Q2).
#[derive(Debug, Clone)]
pub struct LandRequest {
    /// `owner`, or `coordinator:<grant>`.
    pub requested_by: String,
    pub request_command_id: String,
    pub station_id: String,
    pub session_id: String,
    /// The authority of a coordinator land (design §11.2, B3).
    pub authority: Option<CommandAuthority>,
}

/// The answer to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandRequested {
    Queued { land_id: String, position: u32 },
    Refused(LandRefusal),
}

/// The refusal of an action on an unknown land.
#[must_use]
pub fn unknown_land() -> LandRefusal {
    LandRefusal {
        code: "unknown_land",
        message: "Unknown land.".to_owned(),
    }
}

/// The result of an owner action: the row, or a refusal.
pub type LandOutcome = std::result::Result<LandView, LandRefusal>;

impl DeckStore {
    /// The state of the links for `fleet_git.land.v1`, by station id.
    pub(super) fn land_features(&self) -> std::collections::BTreeMap<String, LinkFeature> {
        use fleetdeck_proto::version::feature;
        self.link_features()
            .iter()
            .map(|(s, links)| {
                let all = !links.is_empty()
                    && links.values().all(|f| f.contains(feature::FLEET_GIT_LAND));
                (
                    s.as_str().to_owned(),
                    if all {
                        LinkFeature::Negotiated
                    } else {
                        LinkFeature::Without
                    },
                )
            })
            .collect()
    }

    /// The stations whose `station.info` allows local remotes (B10).
    pub(super) fn land_local_ok(&self) -> BTreeSet<String> {
        self.station_infos()
            .into_iter()
            .filter(|(_, (info, _))| {
                info.fleet_git
                    .as_ref()
                    .is_some_and(|g| g.allow_local_remotes)
            })
            .map(|(id, _)| id.as_str().to_owned())
            .collect()
    }

    /// Queues a land (design §11.2 "Request"). The same
    /// `(requested_by, request_command_id)` for the same session gives the
    /// first answer again. A refusal stores nothing.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn land_request(&self, req: LandRequest) -> Result<LandRequested> {
        let now = self.now_ms();
        let feature = self
            .land_features()
            .get(&req.station_id)
            .copied()
            .unwrap_or(LinkFeature::Down);
        let env = Env {
            feature,
            local_ok: self.land_local_ok(),
        };
        let land_id = format!("land-{}", new_id::<String>(self.inner.ids.as_ref()));
        let command_id = format!("landcmd-{}", new_id::<String>(self.inner.ids.as_ref()));
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let first: Option<LandRow> = tx
                .query_row(
                    &format!(
                        "SELECT {COLS} FROM land_queue WHERE requested_by = ?1 AND request_command_id = ?2"
                    ),
                    params![req.requested_by, req.request_command_id],
                    |r| Ok(read(r)),
                )
                .optional()?
                .transpose()?;
            if let Some(row) = first {
                if row.station_id != req.station_id || row.session_id != req.session_id {
                    return Ok(LandRequested::Refused(LandRefusal {
                        code: "command_id_reused",
                        message: "This command id was used for another request.".to_owned(),
                    }));
                }
                return Ok(LandRequested::Queued {
                    position: position(&tx, &row)?,
                    land_id: row.land_id,
                });
            }
            let ready = match check(&tx, &env, &req.station_id, &req.session_id)? {
                Ok(ready) => ready,
                Err(refusal) => return Ok(LandRequested::Refused(refusal)),
            };
            let open: i64 = tx.query_row(
                "SELECT COUNT(*) FROM land_queue
                 WHERE station_id = ?1 AND session_id = ?2 AND state IN ('queued', 'running')",
                params![req.station_id, req.session_id],
                |r| r.get(0),
            )?;
            if open > 0 {
                return Ok(LandRequested::Refused(LandRefusal {
                    code: "land_open",
                    message: "This session already has a land in the queue.".to_owned(),
                }));
            }
            let lock_key = ready.project.repo.clone().unwrap_or_default();
            tx.execute(
                "INSERT INTO land_queue (land_id, command_id, requested_by, request_command_id,
                     authority, authority_expires_at, project_id, lock_key, station_id, session_id,
                     state, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'queued', ?11)",
                params![
                    land_id,
                    command_id,
                    req.requested_by,
                    req.request_command_id,
                    req.authority
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                    req.authority.as_ref().map(|_| now + AUTHORITY_TTL_MS),
                    ready.project.project_id,
                    lock_key,
                    req.station_id,
                    req.session_id,
                    now,
                ],
            )?;
            let row = find(&tx, &land_id)?.ok_or_else(|| super::DeckError::Corrupt("land row".into()))?;
            let position = position(&tx, &row)?;
            tx.commit()?;
            Ok(LandRequested::Queued { land_id, position })
        })
        .await?;
        if matches!(out, LandRequested::Queued { .. }) {
            self.board_changed();
        }
        Ok(out)
    }

    /// `Some(true)` when the session is live and idle, `Some(false)` when it
    /// is live and working, `None` when it is ended or lost (or unknown).
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn session_idle(&self, station: String, session: String) -> Result<Option<bool>> {
        use fleetdeck_proto::event::SessionState;
        call(&self.inner.db, move |conn| {
            Ok(
                summary_of(conn, &station, &session)?.and_then(|s| match s.state {
                    SessionState::Exited | SessionState::Lost => None,
                    SessionState::Running => Some(
                        s.current_run.is_none()
                            && s.current_turn.is_none()
                            && s.open_requests.is_empty(),
                    ),
                    SessionState::Starting | SessionState::Stopping => Some(false),
                }),
            )
        })
        .await
    }

    /// One land row, as the board shows it.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn land_view(&self, land_id: String) -> Result<Option<LandView>> {
        call(&self.inner.db, move |conn| {
            find(conn, &land_id)?.map(|row| view(conn, row)).transpose()
        })
        .await
    }

    /// One land row.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn land_row(&self, land_id: String) -> Result<Option<LandRow>> {
        call(&self.inner.db, move |conn| find(conn, &land_id)).await
    }

    /// The queue and the last 20 ends of a project; `None` for an unknown
    /// project.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn project_lands(&self, project: String) -> Result<Option<LandsView>> {
        call(&self.inner.db, move |conn| {
            if super::projects::find(conn, &project)?.is_none() {
                return Ok(None);
            }
            let rows = |sql: &str| -> Result<Vec<LandRow>> {
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map([&project], |r| Ok(read(r)))?;
                rows.map(|r| r?).collect()
            };
            let queue = rows(&format!(
                "SELECT {COLS} FROM land_queue WHERE project_id = ?1 AND state IN ('queued', 'running')
                 ORDER BY created_at, land_id"
            ))?;
            let recent = rows(&format!(
                "SELECT {COLS} FROM land_queue WHERE project_id = ?1 AND state NOT IN ('queued', 'running')
                 ORDER BY ended_at DESC, land_id DESC LIMIT 20"
            ))?;
            Ok(Some(LandsView {
                queue: queue.into_iter().map(|r| view(conn, r)).collect::<Result<_>>()?,
                recent: recent.into_iter().map(|r| view(conn, r)).collect::<Result<_>>()?,
            }))
        })
        .await
    }
}

#[cfg(test)]
#[path = "land_queue_tests.rs"]
mod tests;
