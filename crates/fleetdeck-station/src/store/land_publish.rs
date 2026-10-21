//! The store transactions of the land publication (FG-8, design §11.5 steps
//! 5 to 7, the reconciliation and the abandonment, §11.2 M3). Every
//! transaction here follows the fence rule: it runs only when the land is
//! `running` and its id has no fence (`WHERE ... NOT EXISTS (SELECT 1 FROM
//! land_fences ...)`). Zero rows: the result is dropped.
//!
//! [`Step`] is the one flexible UPDATE of a run; the other functions add the
//! `git_pushes` rows (the branch success and "Push now" after a land) and the
//! fence of an abandonment.

use fleetdeck_proto::fleet_git::{LandPhase, LandState, PushState, code, same_dest};
use fleetdeck_proto::{Event, EventEnvelope, SessionId};
use rusqlite::{Connection, OptionalExtension, named_params, params};

use super::git_state::{push_row, save_push};
use super::land_runs::{LandRunRow, get, info_of, text_of};
use super::{NewEvent, Result, Store, call, land_fences, log, session_holds};

/// The `last_write_outcome` values (design §11.5, M2, M3).
/// The `stop_code` of a push row after a land (design §11.5 step 5).
pub const STOP_LAND_PUBLISHED: &str = "land_published";
pub const WRITE_APPLIED: &str = "applied";
pub const WRITE_REJECTED: &str = "rejected";
pub const WRITE_UNKNOWN: &str = "unknown";
pub const WRITE_NOT_STARTED: &str = "not_started";

/// The operands of an outstanding remote write (design §11.5, M1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// `branch_push`, `pr_create` or `default_push`.
    pub action: &'static str,
    pub attempt: u32,
    pub url: String,
    pub ref_name: String,
    pub oid: String,
    pub lease: Option<String>,
    pub expect: Option<String>,
}

/// What a step does with the `pending_*` columns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PendingChange {
    #[default]
    Keep,
    Set(Pending),
    Clear,
}

/// What a step does with `last_write_outcome`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutcomeChange {
    #[default]
    Keep,
    Set(&'static str),
    Clear,
}

/// What a step does with the reconciliation columns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReconcileChange {
    #[default]
    Keep,
    /// `reconciling = 1`, the counter and the next time.
    Set { attempts: u32, next_at: i64 },
    /// `reconciling = 0`, no next time.
    Clear,
}

/// What a step needs of `pending_action` before it runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Require {
    #[default]
    Nothing,
    /// No write is outstanding (a first write, an end without `Clear`).
    NoPending,
    /// A write is outstanding (a recovery write).
    Pending,
}

/// The end of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepEnd {
    pub state: LandState,
    pub code: Option<String>,
    /// `None` for `landed`.
    pub text: Option<String>,
}

/// One transaction on a run (design §11.5, journal rule).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Step {
    pub phase: Option<LandPhase>,
    pub attempt: Option<u32>,
    pub pending: PendingChange,
    pub outcome: OutcomeChange,
    pub reconcile: ReconcileChange,
    pub require: Require,
    pub branch_published_oid: Option<String>,
    pub default_published_oid: Option<String>,
    pub pr_url: Option<String>,
    pub pr_number: Option<u64>,
    pub end: Option<StepEnd>,
    /// Append a `session.land` event.
    pub event: bool,
}

fn pending_args(change: &PendingChange) -> (i64, Option<&Pending>) {
    match change {
        PendingChange::Keep => (0, None),
        PendingChange::Set(p) => (1, Some(p)),
        PendingChange::Clear => (2, None),
    }
}

/// Applies `step` to `land_id`. `None`: the run is fenced, ended, or lacks
/// the required `pending_action` state; nothing was written.
///
/// # Errors
/// A database error.
pub fn apply(
    conn: &Connection,
    land_id: &str,
    step: &Step,
    now: i64,
) -> Result<Option<(LandRunRow, Vec<EventEnvelope>)>> {
    let (pm, pending) = pending_args(&step.pending);
    let (om, outcome) = match step.outcome {
        OutcomeChange::Keep => (0, None),
        OutcomeChange::Set(v) => (1, Some(v)),
        OutcomeChange::Clear => (2, None),
    };
    let (rm, attempts, next_at) = match step.reconcile {
        ReconcileChange::Keep => (0, None, None),
        ReconcileChange::Set { attempts, next_at } => (1, Some(attempts), Some(next_at)),
        ReconcileChange::Clear => (2, None, None),
    };
    // An end needs `pending_action = NULL` after the update (design §11.5
    // step 8): either the step clears it or none is set before.
    let require = match (step.require, &step.end, &step.pending) {
        (Require::Nothing, Some(_), PendingChange::Keep) => Require::NoPending,
        (r, _, _) => r,
    };
    let (end_state, end_code, end_text) = match &step.end {
        Some(end) => (
            Some(text_of(&end.state)),
            end.code.clone(),
            end.text.clone(),
        ),
        None => (None, None, None),
    };
    let phase = match &step.end {
        Some(_) => Some(text_of(&LandPhase::Done)),
        None => step.phase.as_ref().map(text_of),
    };
    let changed = conn.execute(
        "UPDATE land_runs SET
            phase = COALESCE(:phase, phase), state = COALESCE(:state, state),
            attempt = COALESCE(:attempt, attempt),
            code = COALESCE(:code, code), message = COALESCE(:message, message),
            branch_published_oid = COALESCE(:bpo, branch_published_oid),
            default_published_oid = COALESCE(:dpo, default_published_oid),
            pr_url = COALESCE(:pr_url, pr_url), pr_number = COALESCE(:pr_number, pr_number),
            pending_action = CASE :pm WHEN 1 THEN :pa WHEN 2 THEN NULL ELSE pending_action END,
            pending_attempt = CASE :pm WHEN 1 THEN :pt WHEN 2 THEN NULL ELSE pending_attempt END,
            pending_url = CASE :pm WHEN 1 THEN :pu WHEN 2 THEN NULL ELSE pending_url END,
            pending_ref = CASE :pm WHEN 1 THEN :pr WHEN 2 THEN NULL ELSE pending_ref END,
            pending_oid = CASE :pm WHEN 1 THEN :po WHEN 2 THEN NULL ELSE pending_oid END,
            pending_lease = CASE :pm WHEN 1 THEN :pl WHEN 2 THEN NULL ELSE pending_lease END,
            pending_expect = CASE :pm WHEN 1 THEN :pe WHEN 2 THEN NULL ELSE pending_expect END,
            last_write_outcome = CASE :om WHEN 1 THEN :ov WHEN 2 THEN NULL
                                 ELSE last_write_outcome END,
            reconciling = CASE :rm WHEN 1 THEN 1 WHEN 2 THEN 0 ELSE reconciling END,
            reconcile_attempts = COALESCE(:ra, reconcile_attempts),
            next_reconcile_at = CASE :rm WHEN 1 THEN :rn WHEN 2 THEN NULL
                                ELSE next_reconcile_at END,
            ended_at = CASE WHEN :ended THEN :now ELSE ended_at END, updated_at = :now
         WHERE land_id = :id AND state = 'running'
           AND NOT EXISTS (SELECT 1 FROM land_fences WHERE land_id = :id)
           AND (:req = 0 OR (:req = 1 AND pending_action IS NULL)
                         OR (:req = 2 AND pending_action IS NOT NULL))",
        named_params! {
            ":id": land_id,
            ":phase": phase,
            ":state": end_state,
            ":attempt": step.attempt,
            ":code": end_code,
            ":message": end_text,
            ":bpo": step.branch_published_oid,
            ":dpo": step.default_published_oid,
            ":pr_url": step.pr_url,
            ":pr_number": step.pr_number.and_then(|n| i64::try_from(n).ok()),
            ":pm": pm,
            ":pa": pending.map(|p| p.action),
            ":pt": pending.map(|p| p.attempt),
            ":pu": pending.map(|p| p.url.as_str()),
            ":pr": pending.map(|p| p.ref_name.as_str()),
            ":po": pending.map(|p| p.oid.as_str()),
            ":pl": pending.and_then(|p| p.lease.as_deref()),
            ":pe": pending.and_then(|p| p.expect.as_deref()),
            ":om": om,
            ":ov": outcome,
            ":rm": rm,
            ":ra": attempts,
            ":rn": next_at,
            ":ended": step.end.is_some(),
            ":now": now,
            ":req": match require {
                Require::Nothing => 0,
                Require::NoPending => 1,
                Require::Pending => 2,
            },
        },
    )?;
    if changed == 0 {
        return Ok(None);
    }
    let Some(row) = get(conn, land_id)? else {
        return Ok(None);
    };
    let mut events = Vec::new();
    if step.end.is_some() {
        session_holds::delete(conn, &row.session_id, land_id)?;
    }
    if step.event || step.end.is_some() {
        let event = NewEvent::from(Event::SessionLand(Box::new(info_of(&row))));
        events.push(log::append_one(conn, &row.session_id, event, now)?);
    }
    Ok(Some((row, events)))
}

/// The `git_pushes` row that a published branch leaves (design §11.5 step 5,
/// decision 53, H3 Q17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushUpsert {
    pub session_id: SessionId,
    /// `plan.branch_target.target`.
    pub target: String,
    /// `plan.branch_target.dest`.
    pub target_dest: String,
    pub publish_ref: String,
    /// `R`.
    pub oid: String,
    /// The local tip of the session branch, when it could be read.
    pub tip: Option<String>,
    /// The local tip contains `R`.
    pub tip_contains: bool,
}

/// The result of [`Store::land_branch_applied`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchStored {
    /// The branch success was stored (phase `branch_pushed`).
    Stored,
    /// The run was fenced or ended: nothing was written.
    Dropped,
    /// A push row of another destination exists: the run ended
    /// `branch_moved` and the row is unchanged.
    Moved,
}

/// The upsert of the push row. `false`: the row has another destination.
fn upsert_push(
    conn: &Connection,
    up: &PushUpsert,
    now: i64,
    events: &mut Vec<EventEnvelope>,
) -> Result<bool> {
    let existing = push_row(conn, &up.session_id)?;
    if existing
        .as_ref()
        .is_some_and(|r| !same_dest(&r.target_dest, &up.target_dest))
    {
        return Ok(false);
    }
    if existing.is_none() {
        conn.execute(
            "INSERT INTO git_pushes (session_id, target, target_dest, ref, state, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'idle', ?5)",
            params![
                up.session_id.as_str(),
                up.target,
                up.target_dest,
                up.publish_ref,
                now
            ],
        )?;
    }
    let Some(mut row) = push_row(conn, &up.session_id)? else {
        return Ok(true);
    };
    row.last_pushed_oid = Some(up.oid.clone());
    row.code = None;
    row.message = None;
    row.attempts = 0;
    row.stop_code = None;
    let tip = up.tip.clone().unwrap_or_else(|| up.oid.clone());
    row.desired_oid = Some(tip.clone());
    if !up.tip_contains {
        row.state = PushState::Stopped;
        row.stop_code = Some(STOP_LAND_PUBLISHED.to_owned());
        row.next_attempt_at = None;
    } else if tip == up.oid {
        row.state = PushState::Pushed;
        row.next_attempt_at = None;
    } else {
        row.state = PushState::Due;
        row.next_attempt_at = Some(now);
    }
    conn.execute(
        "UPDATE git_pushes SET last_push_kind = 'land' WHERE session_id = ?1",
        params![up.session_id.as_str()],
    )?;
    save_push(conn, &row, now, events)?;
    Ok(true)
}

/// The `abandon_write` of an abandonment, from the write that the handler
/// waited for.
fn abandon_write_of(waited: bool, outcome: Option<&str>) -> &'static str {
    if !waited {
        return "none";
    }
    match outcome {
        Some(WRITE_APPLIED) => "applied",
        Some(WRITE_REJECTED) => "rejected",
        Some(WRITE_NOT_STARTED) => "none",
        _ => "unknown",
    }
}

/// What the fence transaction found (design §11.2, revision 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fenced {
    /// `none`, `applied`, `rejected` or `unknown`.
    pub write: &'static str,
    pub run_state: LandState,
}

impl Store {
    /// One transaction on a run (see [`Step`]). `false`: fenced, ended, or the
    /// required `pending_action` state is not there.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_step(&self, land_id: String, step: Step) -> Result<bool> {
        let now = self.now();
        let events = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let done = apply(&tx, &land_id, &step, now)?;
            tx.commit()?;
            Ok(done.map(|(_, events)| events))
        })
        .await?;
        let stored = events.is_some();
        self.publish_land_events(&events.unwrap_or_default());
        Ok(stored)
    }

    /// Records only the outcome of a write (design §11.5, M3). It does not
    /// need the fence rule: the writer holds the per-land lock, and the
    /// outcome is a fact about the write.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_write_outcome(&self, land_id: String, outcome: &'static str) -> Result<()> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE land_runs SET last_write_outcome = ?2, updated_at = ?3
                 WHERE land_id = ?1",
                params![land_id, outcome, now],
            )?;
            Ok(())
        })
        .await
    }

    /// The success of a branch push (design §11.5 step 5): `branch_pushed`,
    /// `branch_published_oid = R`, the pending write cleared and the push row
    /// `stopped` with `land_published` unless the local tip contains `R`. One
    /// transaction. `conflict_text` is the end text when the push row has
    /// another destination (`branch_moved`, no overwrite).
    ///
    /// # Errors
    /// A database error.
    pub async fn land_branch_applied(
        &self,
        land_id: String,
        outcome: OutcomeChange,
        up: PushUpsert,
        conflict_text: String,
    ) -> Result<BranchStored> {
        let now = self.now();
        let (result, events) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let mut events = Vec::new();
            let mut step = Step {
                phase: Some(LandPhase::BranchPushed),
                pending: PendingChange::Clear,
                outcome,
                reconcile: ReconcileChange::Clear,
                branch_published_oid: Some(up.oid.clone()),
                event: true,
                ..Step::default()
            };
            let mut moved = false;
            // The push row first: a conflict changes the step.
            let fenced = !super::land_runs::guard_running(&tx, &land_id)?;
            if !fenced {
                let mut push_events = Vec::new();
                if upsert_push(&tx, &up, now, &mut push_events)? {
                    events = push_events;
                } else {
                    moved = true;
                    step.phase = None;
                    step.end = Some(StepEnd {
                        state: LandState::Failed,
                        code: Some(code::BRANCH_MOVED.to_owned()),
                        text: Some(conflict_text),
                    });
                }
            }
            let done = apply(&tx, &land_id, &step, now)?;
            let result = match (&done, moved) {
                (None, _) => BranchStored::Dropped,
                (Some(_), true) => BranchStored::Moved,
                (Some(_), false) => BranchStored::Stored,
            };
            if let Some((_, run_events)) = done {
                events.extend(run_events);
            } else {
                events.clear();
            }
            if result == BranchStored::Dropped {
                // Roll back the push row: the run was fenced meanwhile.
                return Ok((result, Vec::new()));
            }
            tx.commit()?;
            Ok((result, events))
        })
        .await?;
        self.publish_land_events(&events);
        Ok(result)
    }

    /// The abandonment (design §11.2, revision 4, M3), with the per-land lock
    /// held by the caller: reads the outcome of the write that the caller
    /// waited for, records the fence, and ends a `running` run `unresolved`
    /// with `code = abandoned`. A repeat answers the same values.
    /// `text_of` builds the end text from the run and the `abandon_write`.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_abandon(
        &self,
        land_id: String,
        waited: bool,
        text: impl FnOnce(&LandRunRow, &str) -> String + Send + 'static,
    ) -> Result<Fenced> {
        let now = self.now();
        let (fenced, events) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let row = get(&tx, &land_id)?;
            let known = land_fences::is_fenced(&tx, &land_id)?;
            if known {
                let (write, state) = match &row {
                    Some(row) => (
                        match row.abandon_write.as_deref() {
                            Some("applied") => "applied",
                            Some("rejected") => "rejected",
                            Some("unknown") => "unknown",
                            _ => "none",
                        },
                        row.state,
                    ),
                    None => ("none", LandState::Unresolved),
                };
                return Ok((
                    Fenced {
                        write,
                        run_state: state,
                    },
                    Vec::new(),
                ));
            }
            let write = abandon_write_of(
                waited && row.is_some(),
                row.as_ref().and_then(|r| r.last_write_outcome.as_deref()),
            );
            let mut events = Vec::new();
            let mut state = LandState::Unresolved;
            if let Some(row) = &row {
                state = row.state;
                tx.execute(
                    "UPDATE land_runs SET abandon_write = ?2 WHERE land_id = ?1",
                    params![land_id, write],
                )?;
                if row.state == LandState::Running {
                    let end = Step {
                        pending: PendingChange::Clear,
                        reconcile: ReconcileChange::Clear,
                        end: Some(StepEnd {
                            state: LandState::Unresolved,
                            code: Some(code::ABANDONED.to_owned()),
                            text: Some(text(row, write)),
                        }),
                        ..Step::default()
                    };
                    if let Some((_, ended)) = apply(&tx, &land_id, &end, now)? {
                        events = ended;
                        state = LandState::Unresolved;
                    }
                }
            }
            land_fences::insert(&tx, &land_id, now)?;
            tx.commit()?;
            Ok((
                Fenced {
                    write,
                    run_state: state,
                },
                events,
            ))
        })
        .await?;
        self.publish_land_events(&events);
        Ok(fenced)
    }

    /// "Push now" after a land (design §11.7, H3 Q14): a `stopped` row with
    /// `land_published` becomes `due` with `desired_oid = tip`. The lease is
    /// `last_pushed_oid` (`R`). Returns the row state, or `None` when the row
    /// is not in that state.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_push_reopen(
        &self,
        session_id: SessionId,
        tip: String,
    ) -> Result<Option<PushState>> {
        let now = self.now();
        let (state, events) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(mut row) = push_row(&tx, &session_id)? else {
                return Ok((None, Vec::new()));
            };
            if row.state != PushState::Stopped
                || row.stop_code.as_deref() != Some(STOP_LAND_PUBLISHED)
                || row.last_pushed_oid.as_deref() == Some(tip.as_str())
            {
                return Ok((None, Vec::new()));
            }
            row.state = PushState::Due;
            row.stop_code = None;
            row.desired_oid = Some(tip);
            row.next_attempt_at = Some(now);
            let mut events = Vec::new();
            save_push(&tx, &row, now, &mut events)?;
            tx.commit()?;
            Ok((Some(PushState::Due), events))
        })
        .await?;
        self.publish_land_events(&events);
        Ok(state)
    }

    /// The `land_fences` time and the run of an id (for the status).
    ///
    /// # Errors
    /// A database error.
    pub async fn land_known(&self, land_id: String) -> Result<(Option<LandRunRow>, bool)> {
        call(&self.inner.db, move |conn| {
            let row = get(conn, &land_id)?;
            let fenced = land_fences::is_fenced(conn, &land_id)?;
            Ok((row, fenced))
        })
        .await
    }

    /// Whether a run row exists for `land_id` (for tests and the handlers).
    ///
    /// # Errors
    /// A database error.
    pub async fn land_exists(&self, land_id: String) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT 1 FROM land_runs WHERE land_id = ?1",
                    params![land_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
        .await
    }
}

#[cfg(test)]
#[path = "land_publish_tests.rs"]
mod tests;

#[cfg(test)]
impl Store {
    /// Runs one statement with text parameters (a test stages a row).
    pub(crate) async fn land_test_sql(&self, sql: &'static str, args: Vec<String>) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute(sql, rusqlite::params_from_iter(args))?;
            Ok(())
        })
        .await
    }
}

impl Store {
    fn publish_land_events(&self, events: &[EventEnvelope]) {
        if let Some(last) = events.last() {
            self.publish(last.station_seq);
        }
    }
}
