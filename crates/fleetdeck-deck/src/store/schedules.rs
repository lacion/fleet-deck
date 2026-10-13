//! The schedules and their runs (R5, DL-168; `deck.db` V16).
//!
//! The deck owns the schedule. A run row is the claim of one due time:
//! `UNIQUE (schedule_id, due_at)` and `INSERT OR IGNORE` make the claim
//! idempotent, so a second tick, a restart or a "run now" in the same
//! millisecond never starts a due time twice. A row is created before the
//! work and changes once, when the work ends; a crash in between leaves
//! `started` with no session (the run is never repeated).

use std::collections::BTreeMap;

use fleetdeck_proto::automation::{Schedule, ScheduleInput, ScheduleRun, ScheduleRunState};
use fleetdeck_proto::board::SessionKey;
use fleetdeck_proto::ids::{ProviderInstanceId, StationId};
use rusqlite::{OptionalExtension, Row, params};

use super::{DeckError, DeckStore, Result, call};
use fleetdeck_core::ids::new_id;

/// The newest runs that the deck keeps for each schedule.
pub const KEEP_RUNS: i64 = 500;

/// A schedule with the time since which its due times count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSchedule {
    pub schedule: Schedule,
    /// Unix ms; the scheduler looks for due times after the later of this
    /// and the newest run.
    pub armed_at: i64,
    /// The due time of the newest run, if any.
    pub last_due: Option<i64>,
}

impl StoredSchedule {
    /// The time after which the next due time is computed.
    #[must_use]
    pub fn cursor(&self) -> i64 {
        self.last_due
            .map_or(self.armed_at, |d| d.max(self.armed_at))
    }
}

const COLUMNS: &str = "schedule_id, name, enabled, cron, timezone, station_id, project_id, path,
     provider_instance, mode, model, prompt, precheck_cmd, reuse_session, last_station,
     last_session, grace_ms, created_at, armed_at";

fn schedule_row(r: &Row<'_>) -> rusqlite::Result<(Schedule, i64)> {
    let last_station: Option<String> = r.get(14)?;
    let last_session: Option<String> = r.get(15)?;
    Ok((
        Schedule {
            schedule_id: r.get(0)?,
            name: r.get(1)?,
            enabled: r.get::<_, i64>(2)? != 0,
            cron: r.get(3)?,
            timezone: r.get(4)?,
            station_id: StationId::from(r.get::<_, String>(5)?),
            project_id: r.get(6)?,
            path: r.get(7)?,
            provider_instance: ProviderInstanceId::from(r.get::<_, String>(8)?),
            mode: r.get(9)?,
            model: r.get(10)?,
            prompt: r.get(11)?,
            precheck_cmd: r.get(12)?,
            reuse_session: r.get::<_, i64>(13)? != 0,
            last_session: last_station
                .zip(last_session)
                .map(|(station, session)| SessionKey {
                    station_id: StationId::from(station),
                    session_id: session.into(),
                }),
            grace_ms: r.get(16)?,
            created_at: r.get(17)?,
        },
        r.get(18)?,
    ))
}

const RUN_COLUMNS: &str =
    "run_id, schedule_id, due_at, started_at, state, reason, station_id, session_id, manual";

fn run_row(r: &Row<'_>) -> rusqlite::Result<(ScheduleRun, String)> {
    let state: String = r.get(4)?;
    Ok((
        ScheduleRun {
            run_id: r.get(0)?,
            schedule_id: r.get(1)?,
            due_at: r.get(2)?,
            started_at: r.get(3)?,
            state: ScheduleRunState::Started,
            reason: r.get(5)?,
            station_id: StationId::from(r.get::<_, String>(6)?),
            session_id: r.get(7)?,
            manual: r.get::<_, i64>(8)? != 0,
        },
        state,
    ))
}

fn parse_run((mut run, state): (ScheduleRun, String)) -> Result<ScheduleRun> {
    run.state = ScheduleRunState::parse(&state)
        .ok_or_else(|| DeckError::Corrupt(format!("schedule run state {state}")))?;
    Ok(run)
}

impl DeckStore {
    /// Every schedule, by name, with its cursor data.
    ///
    /// # Errors
    /// A database error.
    pub async fn schedules(&self) -> Result<Vec<StoredSchedule>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS}, (SELECT MAX(due_at) FROM schedule_runs r
                     WHERE r.schedule_id = schedules.schedule_id AND r.manual = 0)
                 FROM schedules ORDER BY name, schedule_id"
            ))?;
            let rows = stmt
                .query_map([], |r| Ok((schedule_row(r)?, r.get::<_, Option<i64>>(19)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows
                .into_iter()
                .map(|((schedule, armed_at), last_due)| StoredSchedule {
                    schedule,
                    armed_at,
                    last_due,
                })
                .collect())
        })
        .await
    }

    /// One schedule.
    ///
    /// # Errors
    /// A database error.
    pub async fn schedule(&self, id: String) -> Result<Option<StoredSchedule>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {COLUMNS}, (SELECT MAX(due_at) FROM schedule_runs r
                             WHERE r.schedule_id = schedules.schedule_id AND r.manual = 0)
                         FROM schedules WHERE schedule_id = ?1"
                    ),
                    [id],
                    |r| Ok((schedule_row(r)?, r.get::<_, Option<i64>>(19)?)),
                )
                .optional()?
                .map(|((schedule, armed_at), last_due)| StoredSchedule {
                    schedule,
                    armed_at,
                    last_due,
                }))
        })
        .await
    }

    /// Stores a new schedule. The caller checked the input, the cron text,
    /// the time zone and the station, and passes the canonical station id.
    ///
    /// # Errors
    /// A database error.
    pub async fn create_schedule(
        &self,
        input: ScheduleInput,
        station_id: StationId,
    ) -> Result<Schedule> {
        let id: String = new_id(self.inner.ids.as_ref());
        let now = self.now_ms();
        let schedule = Schedule {
            schedule_id: id,
            name: input.name.trim().to_owned(),
            enabled: input.enabled,
            cron: input.cron.trim().to_owned(),
            timezone: input.timezone.trim().to_owned(),
            station_id,
            project_id: input.project_id.clone().filter(|p| !p.is_empty()),
            path: input.path.trim().to_owned(),
            provider_instance: input.provider_instance.clone(),
            mode: input.mode.clone().filter(|m| !m.trim().is_empty()),
            model: input.model.clone().filter(|m| !m.trim().is_empty()),
            prompt: input.prompt.clone(),
            precheck_cmd: input.precheck(),
            reuse_session: input.reuse_session,
            last_session: None,
            grace_ms: input.grace(),
            created_at: now,
        };
        let row = schedule.clone();
        call(&self.inner.db, move |conn| {
            conn.execute(
                &format!(
                    "INSERT INTO schedules ({COLUMNS})
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                             NULL, NULL, ?15, ?16, ?16)"
                ),
                params![
                    row.schedule_id,
                    row.name,
                    i64::from(row.enabled),
                    row.cron,
                    row.timezone,
                    row.station_id.as_str(),
                    row.project_id,
                    row.path,
                    row.provider_instance.as_str(),
                    row.mode,
                    row.model,
                    row.prompt,
                    row.precheck_cmd,
                    i64::from(row.reuse_session),
                    row.grace_ms,
                    row.created_at,
                ],
            )?;
            Ok(())
        })
        .await?;
        Ok(schedule)
    }

    /// Replaces every field of a schedule. Turning a schedule on, or changing
    /// its cron or time zone, arms it again: the due times of the time before
    /// do not run.
    ///
    /// # Errors
    /// [`DeckError::UnknownSchedule`]; a database error.
    pub async fn update_schedule(
        &self,
        id: String,
        input: ScheduleInput,
        station_id: StationId,
    ) -> Result<Schedule> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let (old, _) = tx
                .query_row(
                    &format!("SELECT {COLUMNS} FROM schedules WHERE schedule_id = ?1"),
                    [&id],
                    schedule_row,
                )
                .optional()?
                .ok_or_else(|| DeckError::UnknownSchedule(id.clone()))?;
            let cron = input.cron.trim().to_owned();
            let timezone = input.timezone.trim().to_owned();
            let rearm =
                (input.enabled && !old.enabled) || cron != old.cron || timezone != old.timezone;
            // A new folder or station is a new target: the old session is
            // no longer the one to reuse.
            let same_target = station_id == old.station_id && input.path.trim() == old.path;
            let schedule = Schedule {
                schedule_id: id.clone(),
                name: input.name.trim().to_owned(),
                enabled: input.enabled,
                cron,
                timezone,
                station_id,
                project_id: input.project_id.clone().filter(|p| !p.is_empty()),
                path: input.path.trim().to_owned(),
                provider_instance: input.provider_instance.clone(),
                mode: input.mode.clone().filter(|m| !m.trim().is_empty()),
                model: input.model.clone().filter(|m| !m.trim().is_empty()),
                prompt: input.prompt.clone(),
                precheck_cmd: input.precheck(),
                reuse_session: input.reuse_session,
                last_session: old.last_session.filter(|_| same_target),
                grace_ms: input.grace(),
                created_at: old.created_at,
            };
            tx.execute(
                "UPDATE schedules SET name = ?2, enabled = ?3, cron = ?4, timezone = ?5,
                     station_id = ?6, project_id = ?7, path = ?8, provider_instance = ?9,
                     mode = ?10, model = ?11, prompt = ?12, precheck_cmd = ?13,
                     reuse_session = ?14, last_station = ?15, last_session = ?16,
                     grace_ms = ?17, armed_at = CASE WHEN ?18 THEN ?19 ELSE armed_at END
                 WHERE schedule_id = ?1",
                params![
                    schedule.schedule_id,
                    schedule.name,
                    i64::from(schedule.enabled),
                    schedule.cron,
                    schedule.timezone,
                    schedule.station_id.as_str(),
                    schedule.project_id,
                    schedule.path,
                    schedule.provider_instance.as_str(),
                    schedule.mode,
                    schedule.model,
                    schedule.prompt,
                    schedule.precheck_cmd,
                    i64::from(schedule.reuse_session),
                    schedule
                        .last_session
                        .as_ref()
                        .map(|k| k.station_id.as_str().to_owned()),
                    schedule
                        .last_session
                        .as_ref()
                        .map(|k| k.session_id.as_str().to_owned()),
                    schedule.grace_ms,
                    rearm,
                    now,
                ],
            )?;
            tx.commit()?;
            Ok(schedule)
        })
        .await
    }

    /// Deletes a schedule and its runs. Returns whether it existed.
    ///
    /// # Errors
    /// A database error.
    pub async fn delete_schedule(&self, id: String) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM schedule_runs WHERE schedule_id = ?1", [&id])?;
            let n = tx.execute("DELETE FROM schedules WHERE schedule_id = ?1", [&id])?;
            tx.commit()?;
            Ok(n == 1)
        })
        .await
    }

    /// The newest runs of a schedule, newest due time first.
    ///
    /// # Errors
    /// A database error.
    pub async fn schedule_runs(&self, id: String, limit: u32) -> Result<Vec<ScheduleRun>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {RUN_COLUMNS} FROM schedule_runs WHERE schedule_id = ?1
                 ORDER BY due_at DESC LIMIT ?2"
            ))?;
            let rows = stmt
                .query_map(params![id, i64::from(limit)], run_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().map(parse_run).collect()
        })
        .await
    }

    /// The newest run of each schedule, by schedule id.
    ///
    /// # Errors
    /// A database error.
    pub async fn last_schedule_runs(&self) -> Result<BTreeMap<String, ScheduleRun>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {RUN_COLUMNS} FROM schedule_runs r
                 WHERE due_at = (SELECT MAX(due_at) FROM schedule_runs
                                 WHERE schedule_id = r.schedule_id)"
            ))?;
            let rows = stmt
                .query_map([], run_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|row| parse_run(row).map(|run| (run.schedule_id.clone(), run)))
                .collect()
        })
        .await
    }

    /// Records one run of a due time. Returns `None` when the due time has a
    /// run already (the claim was taken before). Older runs beyond
    /// [`KEEP_RUNS`] are deleted.
    ///
    /// # Errors
    /// A database error.
    pub async fn insert_schedule_run(
        &self,
        schedule_id: String,
        due_at: i64,
        station_id: StationId,
        state: ScheduleRunState,
        reason: String,
        manual: bool,
    ) -> Result<Option<ScheduleRun>> {
        let run_id: String = new_id(self.inner.ids.as_ref());
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let n = tx.execute(
                "INSERT OR IGNORE INTO schedule_runs
                     (run_id, schedule_id, due_at, started_at, state, reason, station_id, manual)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    run_id,
                    schedule_id,
                    due_at,
                    now,
                    state.as_str(),
                    reason,
                    station_id.as_str(),
                    i64::from(manual)
                ],
            )?;
            if n == 0 {
                return Ok(None);
            }
            tx.execute(
                "DELETE FROM schedule_runs WHERE schedule_id = ?1 AND run_id NOT IN
                     (SELECT run_id FROM schedule_runs WHERE schedule_id = ?1
                      ORDER BY due_at DESC LIMIT ?2)",
                params![schedule_id, KEEP_RUNS],
            )?;
            tx.commit()?;
            Ok(Some(ScheduleRun {
                run_id,
                schedule_id,
                due_at,
                started_at: now,
                state,
                reason,
                station_id,
                session_id: None,
                manual,
            }))
        })
        .await
    }

    /// Ends a claimed run: its state, the reason and the session.
    ///
    /// # Errors
    /// A database error.
    pub async fn finish_schedule_run(
        &self,
        run_id: String,
        state: ScheduleRunState,
        reason: String,
        session_id: Option<String>,
    ) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE schedule_runs SET state = ?2, reason = ?3, session_id = ?4
                 WHERE run_id = ?1",
                params![run_id, state.as_str(), reason, session_id],
            )?;
            Ok(())
        })
        .await
    }

    /// Remembers the session of the last run, for `reuse_session`.
    ///
    /// # Errors
    /// A database error.
    pub async fn set_schedule_session(&self, id: String, key: SessionKey) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE schedules SET last_station = ?2, last_session = ?3
                 WHERE schedule_id = ?1",
                params![id, key.station_id.as_str(), key.session_id.as_str()],
            )?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fleetdeck_testkit::Det;

    use super::*;

    fn input(name: &str) -> ScheduleInput {
        ScheduleInput {
            name: name.to_owned(),
            enabled: true,
            cron: "@daily".to_owned(),
            timezone: "UTC".to_owned(),
            station: "alpha".to_owned(),
            project_id: None,
            path: "/work/repo".to_owned(),
            provider_instance: ProviderInstanceId::from("claude"),
            mode: Some("plan".to_owned()),
            model: None,
            prompt: "Triage.".to_owned(),
            precheck_cmd: Some("test -n x".to_owned()),
            reuse_session: true,
            grace_ms: None,
        }
    }

    async fn store(det: &Det) -> DeckStore {
        DeckStore::open_in_memory(det.clock(), det.ids())
            .await
            .unwrap()
    }

    fn st() -> StationId {
        StationId::from("st1")
    }

    #[tokio::test]
    async fn a_schedule_is_created_listed_and_read_back() {
        let det = Det::new();
        let s = store(&det).await;
        let made = s.create_schedule(input("b"), st()).await.unwrap();
        let other = s.create_schedule(input("a"), st()).await.unwrap();
        assert_eq!(made.grace_ms, fleetdeck_proto::automation::DEFAULT_GRACE_MS);
        assert_eq!(made.precheck_cmd.as_deref(), Some("test -n x"));
        let list = s.schedules().await.unwrap();
        assert_eq!(
            list.iter()
                .map(|x| x.schedule.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        let found = s.schedule(made.schedule_id.clone()).await.unwrap().unwrap();
        assert_eq!(found.schedule, made);
        assert_eq!(found.armed_at, made.created_at);
        assert_eq!(found.cursor(), made.created_at);
        assert_ne!(other.schedule_id, made.schedule_id);
        assert_eq!(s.schedule("none".to_owned()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_run_is_claimed_once_for_a_due_time() {
        let det = Det::new();
        let s = store(&det).await;
        let sc = s.create_schedule(input("a"), st()).await.unwrap();
        let id = sc.schedule_id.clone();
        let first = s
            .insert_schedule_run(
                id.clone(),
                1000,
                st(),
                ScheduleRunState::Started,
                String::new(),
                false,
            )
            .await
            .unwrap();
        assert!(first.is_some());
        let again = s
            .insert_schedule_run(
                id.clone(),
                1000,
                st(),
                ScheduleRunState::Started,
                String::new(),
                false,
            )
            .await
            .unwrap();
        assert_eq!(again, None, "the same due time has a row already");
        s.finish_schedule_run(
            first.unwrap().run_id,
            ScheduleRunState::Skipped,
            "no news".to_owned(),
            None,
        )
        .await
        .unwrap();
        let rows = s.schedule_runs(id.clone(), 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, ScheduleRunState::Skipped);
        assert_eq!(rows[0].reason, "no news");
        let found = s.schedule(id.clone()).await.unwrap().unwrap();
        assert_eq!(found.last_due, Some(1000));
        assert_eq!(found.cursor(), 1000.max(found.armed_at));
    }

    #[tokio::test]
    async fn a_manual_run_does_not_move_the_cursor() {
        let det = Det::new();
        let s = store(&det).await;
        let sc = s.create_schedule(input("a"), st()).await.unwrap();
        let id = sc.schedule_id.clone();
        s.insert_schedule_run(
            id.clone(),
            i64::MAX / 2,
            st(),
            ScheduleRunState::Started,
            String::new(),
            true,
        )
        .await
        .unwrap();
        let found = s.schedule(id.clone()).await.unwrap().unwrap();
        assert_eq!(found.last_due, None);
        let last = s.last_schedule_runs().await.unwrap();
        assert!(last[&id].manual);
    }

    #[tokio::test]
    async fn the_last_run_of_each_schedule_is_the_newest_due_time() {
        let det = Det::new();
        let s = store(&det).await;
        let a = s
            .create_schedule(input("a"), st())
            .await
            .unwrap()
            .schedule_id;
        let b = s
            .create_schedule(input("b"), st())
            .await
            .unwrap()
            .schedule_id;
        for (id, due, state) in [
            (&a, 1, ScheduleRunState::Failed),
            (&a, 3, ScheduleRunState::Missed),
            (&a, 2, ScheduleRunState::Started),
            (&b, 5, ScheduleRunState::Skipped),
        ] {
            s.insert_schedule_run(id.clone(), due, st(), state, String::new(), false)
                .await
                .unwrap();
        }
        let last = s.last_schedule_runs().await.unwrap();
        assert_eq!(last[&a].state, ScheduleRunState::Missed);
        assert_eq!(last[&b].state, ScheduleRunState::Skipped);
        let rows = s.schedule_runs(a.clone(), 2).await.unwrap();
        assert_eq!(rows.iter().map(|r| r.due_at).collect::<Vec<_>>(), [3, 2]);
    }

    #[tokio::test]
    async fn an_update_rearms_only_on_enable_or_a_new_clock() {
        let det = Det::new();
        let s = store(&det).await;
        let mut off = input("a");
        off.enabled = false;
        let sc = s.create_schedule(off, st()).await.unwrap();
        let id = sc.schedule_id.clone();
        let armed = |s: &StoredSchedule| s.armed_at;
        let first = armed(&s.schedule(id.clone()).await.unwrap().unwrap());

        det.advance(Duration::from_secs(10));
        // A rename keeps the arming.
        let mut change = input("renamed");
        change.enabled = false;
        s.update_schedule(id.clone(), change, st()).await.unwrap();
        assert_eq!(
            armed(&s.schedule(id.clone()).await.unwrap().unwrap()),
            first
        );

        // Turning it on arms it again.
        s.update_schedule(id.clone(), input("renamed"), st())
            .await
            .unwrap();
        let on = armed(&s.schedule(id.clone()).await.unwrap().unwrap());
        assert_eq!(on, first + 10_000);

        // The same cron keeps it; another cron arms it again.
        det.advance(Duration::from_secs(5));
        s.update_schedule(id.clone(), input("renamed"), st())
            .await
            .unwrap();
        assert_eq!(armed(&s.schedule(id.clone()).await.unwrap().unwrap()), on);
        let mut other = input("renamed");
        other.cron = "0 9 * * *".to_owned();
        s.update_schedule(id.clone(), other, st()).await.unwrap();
        assert_eq!(
            armed(&s.schedule(id.clone()).await.unwrap().unwrap()),
            on + 5_000
        );
        assert!(matches!(
            s.update_schedule("none".to_owned(), input("x"), st()).await,
            Err(DeckError::UnknownSchedule(_))
        ));
    }

    #[tokio::test]
    async fn a_new_folder_forgets_the_session_to_reuse() {
        let det = Det::new();
        let s = store(&det).await;
        let sc = s.create_schedule(input("a"), st()).await.unwrap();
        let id = sc.schedule_id.clone();
        s.set_schedule_session(
            id.clone(),
            SessionKey {
                station_id: st(),
                session_id: "sess".to_owned().into(),
            },
        )
        .await
        .unwrap();
        s.update_schedule(id.clone(), input("a"), st())
            .await
            .unwrap();
        assert!(
            s.schedule(id.clone())
                .await
                .unwrap()
                .unwrap()
                .schedule
                .last_session
                .is_some()
        );
        let mut moved = input("a");
        moved.path = "/work/other".to_owned();
        s.update_schedule(id.clone(), moved, st()).await.unwrap();
        assert!(
            s.schedule(id.clone())
                .await
                .unwrap()
                .unwrap()
                .schedule
                .last_session
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_delete_removes_the_schedule_and_its_runs() {
        let det = Det::new();
        let s = store(&det).await;
        let sc = s.create_schedule(input("a"), st()).await.unwrap();
        let id = sc.schedule_id.clone();
        s.insert_schedule_run(
            id.clone(),
            1,
            st(),
            ScheduleRunState::Missed,
            String::new(),
            false,
        )
        .await
        .unwrap();
        assert!(s.delete_schedule(id.clone()).await.unwrap());
        assert!(!s.delete_schedule(id.clone()).await.unwrap());
        assert!(s.schedules().await.unwrap().is_empty());
        assert!(s.schedule_runs(id, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_oldest_runs_beyond_the_keep_limit_are_deleted() {
        let det = Det::new();
        let s = store(&det).await;
        let id = s
            .create_schedule(input("a"), st())
            .await
            .unwrap()
            .schedule_id;
        for due in 1..=KEEP_RUNS + 3 {
            s.insert_schedule_run(
                id.clone(),
                due,
                st(),
                ScheduleRunState::Missed,
                String::new(),
                false,
            )
            .await
            .unwrap();
        }
        let rows = s.schedule_runs(id, 1000).await.unwrap();
        assert_eq!(i64::try_from(rows.len()).unwrap(), KEEP_RUNS);
        assert_eq!(rows.last().unwrap().due_at, 4);
    }
}
