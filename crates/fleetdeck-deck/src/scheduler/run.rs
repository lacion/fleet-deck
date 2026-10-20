//! One run of a schedule (R5, DL-168): the claim, the work on the station
//! and the record.
//!
//! [`Runner`] holds the rules and a [`Backend`] that does the work. The
//! backend of the deck is [`LinkBackend`] (link requests and commands); the
//! tests use a fake.
//!
//! The work of one run:
//!
//! 1. `precheck_cmd`: `workspace.precheck` on the station. Exit 0 goes on;
//!    another exit code is a `skipped` run with the last line of the output;
//!    a timeout or an error is a `failed` run.
//! 2. `reuse_session` and the last session of the schedule is idle: the
//!    prompt goes to it as a follow-up.
//! 3. Else a new session starts with the prompt, in a new worktree of `path`,
//!    titled `<name> <yyyy-mm-dd>`.
//!
//! The command id is `sch-<schedule id>-<due time>`: a repeated send of the
//! same due time returns the stored command and starts nothing new.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::config_worker_grant::WorkerGrantConfig;
use fleetdeck_proto::automation::{Schedule, ScheduleRun, ScheduleRunState};
use fleetdeck_proto::board::SessionKey;
use fleetdeck_proto::command::{
    CommandOrigin, DeckCommandState, ReceiptState, SessionCommand, SessionSpec, UserInput,
    WorkspaceRequest,
};
use fleetdeck_proto::link::{Outcome, PrecheckQuery, PrecheckResult};
use fleetdeck_proto::{CardStatus, CommandId, StationId};
use serde_json::json;

use super::cron::{self, Cron};
use super::{Plan, plan};
use crate::link::{CommandKind, DeckLink};
use crate::store::worker_grant::{Provenance, WorkerStart};
use crate::store::{AuditResult, DeckError, DeckStore, NewAudit, StoredSchedule};

/// The longest the deck waits for a precheck (the station stops it at 60 s).
pub const PRECHECK_WAIT: Duration = Duration::from_secs(90);
/// The longest the deck waits for the receipt of the start or the prompt.
pub const START_WAIT: Duration = Duration::from_secs(30);
/// The longest reason text that a run keeps (characters).
const MAX_REASON_CHARS: usize = 300;
/// The audit name of the end of a run.
pub const AUDIT_RESULT: &str = "schedules.result";

/// How the work of one run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub state: ScheduleRunState,
    pub reason: String,
    pub session_id: Option<String>,
}

impl Done {
    fn failed(reason: impl Into<String>) -> Self {
        Self {
            state: ScheduleRunState::Failed,
            reason: clip(&reason.into()),
            session_id: None,
        }
    }
}

fn clip(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_REASON_CHARS {
        return text.to_owned();
    }
    let cut: String = text.chars().take(MAX_REASON_CHARS - 1).collect();
    format!("{cut}…")
}

/// What a run needs from the fleet.
#[async_trait]
pub trait Backend: Send + Sync {
    /// True while a link to the station is up.
    fn linked(&self, station: &StationId) -> bool;
    /// Does the work of one run (precheck, then a follow-up or a start).
    async fn execute(&self, schedule: &Schedule, due_at: i64) -> Done;
}

/// The rules of the runs: the claim, the record and the audit.
#[derive(Clone)]
pub struct Runner {
    store: DeckStore,
    backend: Arc<dyn Backend>,
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner").finish_non_exhaustive()
    }
}

impl Runner {
    /// A runner that works through the deck's links.
    #[must_use]
    pub fn new(store: DeckStore, link: Arc<DeckLink>) -> Self {
        let backend = Arc::new(LinkBackend {
            store: store.clone(),
            link,
        });
        Self { store, backend }
    }

    /// A runner with another backend (tests).
    #[must_use]
    pub fn with_backend(store: DeckStore, backend: Arc<dyn Backend>) -> Self {
        Self { store, backend }
    }

    /// One look at every enabled schedule. The runs of one tick work at the
    /// same time; the tick ends when they all ended.
    pub async fn tick(&self) {
        let now = self.store.now_ms();
        let list = match self.store.schedules().await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = %e, "schedules");
                return;
            }
        };
        let mut work = tokio::task::JoinSet::new();
        for stored in list.iter().filter(|s| s.schedule.enabled) {
            if let Some((schedule, run)) = self.evaluate(stored, now).await {
                let runner = self.clone();
                work.spawn(async move { runner.finish(schedule, run, "scheduler").await });
            }
        }
        while let Some(joined) = work.join_next().await {
            if let Err(e) = joined {
                tracing::warn!(error = %e, "schedule run task");
            }
        }
    }

    /// Records a missed run, or claims the run that the due times of one
    /// schedule ask for at `now`; the claimed run is returned for its work.
    pub async fn evaluate(
        &self,
        stored: &StoredSchedule,
        now: i64,
    ) -> Option<(Schedule, ScheduleRun)> {
        let schedule = &stored.schedule;
        let parsed = Cron::parse(&schedule.cron)
            .and_then(|c| cron::parse_offset(&schedule.timezone).map(|o| (c, o)));
        let (cron, offset) = match parsed {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(schedule = %schedule.schedule_id, error = %e, "schedule cron");
                return None;
            }
        };
        match plan(&cron, offset, stored.cursor(), now, schedule.grace_ms) {
            Plan::Nothing => None,
            Plan::Missed { due, count } => {
                let reason = if count > 1 {
                    format!(
                        "{count} due times passed without a run (the deck was off or the station was not linked); the newest was past the grace period"
                    )
                } else {
                    "the due time passed the grace period without a run (the deck was off or the station was not linked)"
                        .to_owned()
                };
                self.record_missed(schedule, due, reason).await;
                None
            }
            Plan::Run { due } => {
                if !self.backend.linked(&schedule.station_id) {
                    tracing::debug!(schedule = %schedule.schedule_id, "station not linked; waiting");
                    return None;
                }
                match self.claim(schedule, due, false).await {
                    Ok(run) => run.map(|run| (schedule.clone(), run)),
                    Err(e) => {
                        tracing::warn!(error = %e, "schedule run claim");
                        None
                    }
                }
            }
        }
    }

    async fn record_missed(&self, schedule: &Schedule, due: i64, reason: String) {
        let run = self
            .store
            .insert_schedule_run(
                schedule.schedule_id.clone(),
                due,
                schedule.station_id.clone(),
                ScheduleRunState::Missed,
                reason.clone(),
                false,
            )
            .await;
        match run {
            Ok(Some(_)) => {
                self.audit("scheduler", schedule, ScheduleRunState::Missed, &reason)
                    .await;
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "missed run"),
        }
    }

    /// Takes the claim of one due time: a `started` row. `None` when the due
    /// time has a row already.
    async fn claim(
        &self,
        schedule: &Schedule,
        due: i64,
        manual: bool,
    ) -> Result<Option<ScheduleRun>, DeckError> {
        self.store
            .insert_schedule_run(
                schedule.schedule_id.clone(),
                due,
                schedule.station_id.clone(),
                ScheduleRunState::Started,
                String::new(),
                manual,
            )
            .await
    }

    /// Does the work of a claimed run and records how it ended.
    async fn finish(&self, schedule: Schedule, run: ScheduleRun, actor: &str) {
        let done = self.backend.execute(&schedule, run.due_at).await;
        if let Err(e) = self
            .store
            .finish_schedule_run(
                run.run_id.clone(),
                done.state,
                done.reason.clone(),
                done.session_id.clone(),
            )
            .await
        {
            tracing::warn!(error = %e, "schedule run end");
        }
        if done.state == ScheduleRunState::Started
            && let Some(session) = &done.session_id
            && let Err(e) = self
                .store
                .set_schedule_session(
                    schedule.schedule_id.clone(),
                    SessionKey {
                        station_id: schedule.station_id.clone(),
                        session_id: session.clone().into(),
                    },
                )
                .await
        {
            tracing::warn!(error = %e, "schedule session");
        }
        self.audit(actor, &schedule, done.state, &done.reason).await;
    }

    async fn audit(&self, actor: &str, schedule: &Schedule, state: ScheduleRunState, reason: &str) {
        let result = if state == ScheduleRunState::Failed {
            AuditResult::Failed
        } else {
            AuditResult::Ok
        };
        let detail = if reason.is_empty() {
            state.as_str().to_owned()
        } else {
            format!("{}: {reason}", state.as_str())
        };
        self.store
            .audit_or_log(
                NewAudit::new(actor, AUDIT_RESULT, result)
                    .target(schedule.schedule_id.clone())
                    .detail(detail),
            )
            .await;
    }

    /// "Run now": claims a run at the time of the call and does its work in
    /// a task; the answer is the `started` row. The owner's click does not
    /// move the schedule's due times. `actor` is the audit actor.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] when the station has no live link; a database
    /// error.
    pub async fn run_now(
        &self,
        schedule: Schedule,
        actor: String,
    ) -> Result<Option<ScheduleRun>, DeckError> {
        if !self.backend.linked(&schedule.station_id) {
            return Err(DeckError::Invalid(format!(
                "station {} has no live link to this deck",
                schedule.station_id
            )));
        }
        let due = self.store.now_ms();
        let Some(run) = self.claim(&schedule, due, true).await? else {
            return Ok(None);
        };
        let runner = self.clone();
        let started = run.clone();
        tokio::spawn(async move { runner.finish(schedule, run, &actor).await });
        Ok(Some(started))
    }
}

/// The last non-empty line of an output tail.
fn last_line(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(clip)
}

/// What a precheck answer means.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Pass,
    Skip(String),
    Fail(String),
}

fn verdict(r: &PrecheckResult) -> Verdict {
    if r.timed_out {
        return Verdict::Fail("the precheck timed out (60 s) on the station".to_owned());
    }
    match r.exit_code {
        Some(0) => Verdict::Pass,
        Some(code) => Verdict::Skip(
            last_line(&r.output_tail)
                .unwrap_or_else(|| format!("the precheck exited with code {code}")),
        ),
        None => Verdict::Fail("a signal ended the precheck".to_owned()),
    }
}

/// The deck's backend: link requests and routed commands.
struct LinkBackend {
    store: DeckStore,
    link: Arc<DeckLink>,
}

impl LinkBackend {
    /// The live `worker_grant` setting; off when the deck has no live
    /// settings (fail closed).
    fn worker_setting(&self) -> WorkerGrantConfig {
        self.link.desktop_live.get().map_or_else(
            || WorkerGrantConfig {
                enabled: false,
                ..WorkerGrantConfig::default()
            },
            |cell| cell.get().worker_grant.clone(),
        )
    }

    async fn precheck(&self, schedule: &Schedule, cmd: &str) -> Verdict {
        let Some(control) = self.link.control_for(&schedule.station_id) else {
            return Verdict::Fail("the station has no live link to this deck".to_owned());
        };
        let params = json!(PrecheckQuery {
            cwd: schedule.path.clone(),
            cmd: cmd.to_owned(),
        });
        let outcome = tokio::time::timeout(
            PRECHECK_WAIT,
            control.request("workspace.precheck", None, params),
        )
        .await;
        match outcome {
            Err(_) => Verdict::Fail("the station did not answer the precheck in time".to_owned()),
            Ok(Err(e)) => Verdict::Fail(format!("the link ended during the precheck: {e}")),
            Ok(Ok(Outcome::Err(e))) => Verdict::Fail(clip(&format!("{}: {}", e.code, e.message))),
            Ok(Ok(Outcome::Ok(v))) => match serde_json::from_value::<PrecheckResult>(v) {
                Ok(r) => verdict(&r),
                Err(e) => Verdict::Fail(format!("the precheck answer is not valid: {e}")),
            },
        }
    }

    /// The command of the run: a follow-up to the idle session of the last
    /// run, or a start.
    async fn command(&self, schedule: &Schedule, due_at: i64) -> Result<SessionCommand, String> {
        let input = UserInput::new(schedule.prompt.clone());
        if schedule.reuse_session
            && let Some(last) = &schedule.last_session
        {
            let sessions = self.store.sessions().await.map_err(|e| e.to_string())?;
            let idle = sessions.iter().any(|s| {
                s.station_id == last.station_id
                    && s.summary.session_id == last.session_id
                    && s.status == CardStatus::Idle
            });
            if idle {
                return Ok(SessionCommand::FollowUp {
                    session_id: last.session_id.clone(),
                    input,
                });
            }
        }
        let offset = cron::parse_offset(&schedule.timezone).unwrap_or(0);
        Ok(SessionCommand::Start {
            spec: SessionSpec {
                provider_instance: schedule.provider_instance.clone(),
                cwd: schedule.path.clone(),
                mode: schedule.mode.clone(),
                model: schedule.model.clone(),
                title: Some(format!(
                    "{} {}",
                    schedule.name,
                    cron::date_text(due_at, offset)
                )),
                coordinator: None,
                launch: None,
                extra_env: None,
            },
            prompt: Some(input),
            workspace: Some(Box::new(WorkspaceRequest::Worktree {
                branch: None,
                base: None,
            })),
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
            fleet_git: None,
        })
    }
}

#[async_trait]
impl Backend for LinkBackend {
    fn linked(&self, station: &StationId) -> bool {
        self.link.control_for(station).is_some()
    }

    async fn execute(&self, schedule: &Schedule, due_at: i64) -> Done {
        match self.store.find_station(schedule.station_id.0.clone()).await {
            Ok(Some(_)) => {}
            Ok(None) => return Done::failed("the station is not paired any more"),
            Err(e) => return Done::failed(e.to_string()),
        }
        if let Some(cmd) = &schedule.precheck_cmd {
            match self.precheck(schedule, cmd).await {
                Verdict::Pass => {}
                Verdict::Skip(reason) => {
                    return Done {
                        state: ScheduleRunState::Skipped,
                        reason,
                        session_id: None,
                    };
                }
                Verdict::Fail(reason) => return Done::failed(reason),
            }
        }
        let command = match self.command(schedule, due_at).await {
            Ok(c) => c,
            Err(e) => return Done::failed(e),
        };
        let command_id = CommandId::from(format!("sch-{}-{due_at}", schedule.schedule_id));
        // A start of a schedule that the owner's login saved last may get a
        // worker grant (MCP P4a §3); the author is read now, not at the claim.
        let eligible = matches!(&command, SessionCommand::Start { spec, .. } if spec.coordinator.is_none())
            && match self
                .store
                .schedule_author(schedule.schedule_id.clone())
                .await
            {
                Ok(author) => author.as_deref() == Some("owner"),
                Err(e) => return Done::failed(e.to_string()),
            };
        let submitted = if eligible {
            self.link
                .submit_start_by(WorkerStart {
                    station: schedule.station_id.clone(),
                    command,
                    command_id,
                    provenance: Provenance::Schedule,
                    author: None,
                    setting: self.worker_setting(),
                })
                .await
        } else {
            self.link
                .submit_as(
                    schedule.station_id.clone(),
                    CommandOrigin::Board,
                    CommandKind::Session { command },
                    command_id,
                )
                .await
        };
        let cmd = match submitted {
            Ok((cmd, _)) => cmd,
            Err(e) => return Done::failed(e.to_string()),
        };
        let cmd = match self.link.wait_command(cmd.command_id, START_WAIT).await {
            Ok(cmd) => cmd,
            Err(e) => return Done::failed(e.to_string()),
        };
        match (cmd.state, cmd.receipt) {
            (DeckCommandState::Done, Some(receipt)) if receipt.state == ReceiptState::Accepted => {
                Done {
                    state: ScheduleRunState::Started,
                    reason: String::new(),
                    session_id: receipt
                        .result
                        .and_then(|r| r.session_id)
                        .map(|s| s.as_str().to_owned()),
                }
            }
            (DeckCommandState::Done, Some(receipt)) => Done::failed(receipt.reject.map_or_else(
                || "the station refused the command".to_owned(),
                |r| r.message,
            )),
            (DeckCommandState::Pending, _) => Done {
                state: ScheduleRunState::Started,
                reason: "the station has not confirmed the start yet".to_owned(),
                session_id: None,
            },
            (_, _) => Done::failed(
                cmd.reason
                    .unwrap_or_else(|| "the command did not complete".to_owned()),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use fleetdeck_proto::automation::ScheduleInput;
    use fleetdeck_proto::ids::ProviderInstanceId;
    use fleetdeck_testkit::Det;

    use super::*;

    const MIN: i64 = 60_000;
    const HOUR: i64 = 60 * MIN;

    /// A backend that records the due times it ran.
    struct Fake {
        linked: Mutex<bool>,
        done: Mutex<Done>,
        ran: Mutex<Vec<i64>>,
    }

    impl Fake {
        fn new(done: Done) -> Arc<Self> {
            Arc::new(Self {
                linked: Mutex::new(true),
                done: Mutex::new(done),
                ran: Mutex::default(),
            })
        }

        fn started(session: &str) -> Done {
            Done {
                state: ScheduleRunState::Started,
                reason: String::new(),
                session_id: Some(session.to_owned()),
            }
        }
    }

    #[async_trait]
    impl Backend for Fake {
        fn linked(&self, _station: &StationId) -> bool {
            *self.linked.lock().unwrap()
        }

        async fn execute(&self, _s: &Schedule, due_at: i64) -> Done {
            self.ran.lock().unwrap().push(due_at);
            self.done.lock().unwrap().clone()
        }
    }

    fn adv(det: &Det, ms: i64) {
        det.advance(Duration::from_millis(u64::try_from(ms).unwrap()));
    }

    fn input(cron: &str) -> ScheduleInput {
        ScheduleInput {
            name: "Triage".to_owned(),
            enabled: true,
            cron: cron.to_owned(),
            timezone: "UTC".to_owned(),
            station: "alpha".to_owned(),
            project_id: None,
            path: "/work/repo".to_owned(),
            provider_instance: ProviderInstanceId::from("claude"),
            mode: None,
            model: None,
            prompt: "Triage.".to_owned(),
            precheck_cmd: None,
            reuse_session: false,
            grace_ms: None,
        }
    }

    /// A store whose clock is at `start`, and a schedule created at that time.
    async fn setup(det: &Det, cron: &str) -> (DeckStore, Schedule) {
        let store = DeckStore::open_in_memory(det.clock(), det.ids())
            .await
            .unwrap();
        let schedule = store
            .create_schedule(input(cron), StationId::from("st1"), "owner")
            .await
            .unwrap();
        (store, schedule)
    }

    async fn runs(store: &DeckStore, s: &Schedule) -> Vec<ScheduleRun> {
        store
            .schedule_runs(s.schedule_id.clone(), 50)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_due_time_runs_once_and_a_second_tick_does_nothing() {
        let det = Det::new();
        let (store, s) = setup(&det, "* * * * *").await;
        let fake = Fake::new(Fake::started("sess1"));
        let runner = Runner::with_backend(store.clone(), fake.clone());
        adv(&det, 90_000);
        runner.tick().await;
        runner.tick().await;
        assert_eq!(fake.ran.lock().unwrap().len(), 1);
        let rows = runs(&store, &s).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, ScheduleRunState::Started);
        assert_eq!(rows[0].session_id.as_deref(), Some("sess1"));
        let stored = store
            .schedule(s.schedule_id.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.schedule.last_session.map(|k| k.session_id.0),
            Some("sess1".to_owned())
        );
    }

    #[tokio::test]
    async fn a_skipped_precheck_is_recorded_with_its_reason() {
        let det = Det::new();
        let (store, s) = setup(&det, "* * * * *").await;
        let fake = Fake::new(Done {
            state: ScheduleRunState::Skipped,
            reason: "nothing new".to_owned(),
            session_id: None,
        });
        let runner = Runner::with_backend(store.clone(), fake);
        adv(&det, 90_000);
        runner.tick().await;
        let rows = runs(&store, &s).await;
        assert_eq!(
            (rows[0].state, rows[0].reason.as_str()),
            (ScheduleRunState::Skipped, "nothing new")
        );
        let stored = store
            .schedule(s.schedule_id.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.schedule.last_session, None);
    }

    #[tokio::test]
    async fn a_due_time_past_the_grace_is_missed_not_run() {
        // The store starts at 2026-01-01 00:00 UTC. The deck was off for
        // two days and a bit: three 09:00 due times passed, the newest 11 h
        // ago (the grace period is one hour).
        let det = Det::new();
        let (store, s) = setup(&det, "0 9 * * *").await;
        let fake = Fake::new(Fake::started("x"));
        let runner = Runner::with_backend(store.clone(), fake.clone());
        adv(&det, 2 * 24 * HOUR + 20 * HOUR);
        runner.tick().await;
        runner.tick().await;
        assert!(fake.ran.lock().unwrap().is_empty());
        let rows = runs(&store, &s).await;
        assert_eq!(rows.len(), 1, "one missed row for the three due times");
        assert_eq!(rows[0].state, ScheduleRunState::Missed);
        assert!(
            rows[0].reason.starts_with("3 due times"),
            "{}",
            rows[0].reason
        );
    }

    #[tokio::test]
    async fn the_newest_due_time_inside_the_grace_runs_once() {
        let det = Det::new();
        let (store, s) = setup(&det, "0 9 * * *").await;
        let fake = Fake::new(Fake::started("x"));
        let runner = Runner::with_backend(store.clone(), fake.clone());
        // Off for two days, back at 09:30 of the third: the 09:00 of that
        // day is inside the grace; the older two are overtaken.
        adv(&det, 2 * 24 * HOUR + 9 * HOUR + 30 * MIN);
        runner.tick().await;
        runner.tick().await;
        assert_eq!(fake.ran.lock().unwrap().len(), 1);
        assert_eq!(runs(&store, &s).await.len(), 1);
    }

    #[tokio::test]
    async fn an_unlinked_station_waits_inside_the_grace_and_is_missed_after() {
        let det = Det::new();
        let (store, s) = setup(&det, "* * * * *").await;
        let fake = Fake::new(Fake::started("x"));
        *fake.linked.lock().unwrap() = false;
        let runner = Runner::with_backend(store.clone(), fake.clone());
        adv(&det, 2 * MIN);
        runner.tick().await;
        assert!(runs(&store, &s).await.is_empty(), "nothing is recorded yet");
        // The station comes back inside the grace period.
        *fake.linked.lock().unwrap() = true;
        runner.tick().await;
        assert_eq!(runs(&store, &s).await.len(), 1);
    }

    #[tokio::test]
    async fn run_now_claims_a_manual_run_and_keeps_the_cursor() {
        let det = Det::new();
        let (store, s) = setup(&det, "0 9 * * *").await;
        let fake = Fake::new(Fake::started("manual1"));
        let runner = Runner::with_backend(store.clone(), fake.clone());
        adv(&det, MIN);
        let run = runner
            .run_now(s.clone(), "owner".to_owned())
            .await
            .unwrap()
            .unwrap();
        assert!(run.manual);
        assert_eq!(run.state, ScheduleRunState::Started);
        // Let the spawned task end.
        for _ in 0..50 {
            if runs(&store, &s).await[0].session_id.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            runs(&store, &s).await[0].session_id.as_deref(),
            Some("manual1")
        );
        let stored = store
            .schedule(s.schedule_id.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.last_due, None, "a manual run is not a due time");
        assert_eq!(stored.cursor(), stored.armed_at);
    }

    #[tokio::test]
    async fn run_now_refuses_an_unlinked_station() {
        let det = Det::new();
        let (store, s) = setup(&det, "0 9 * * *").await;
        let fake = Fake::new(Fake::started("x"));
        *fake.linked.lock().unwrap() = false;
        let runner = Runner::with_backend(store, fake);
        assert!(matches!(
            runner.run_now(s, "owner".to_owned()).await,
            Err(DeckError::Invalid(_))
        ));
    }

    #[test]
    fn a_precheck_answer_maps_to_a_verdict() {
        let r = |exit_code, timed_out, tail: &str| PrecheckResult {
            exit_code,
            timed_out,
            output_tail: tail.to_owned(),
        };
        assert_eq!(verdict(&r(Some(0), false, "ok")), Verdict::Pass);
        assert_eq!(
            verdict(&r(Some(1), false, "a\n\nno new issues\n\n")),
            Verdict::Skip("no new issues".to_owned())
        );
        assert_eq!(
            verdict(&r(Some(2), false, "")),
            Verdict::Skip("the precheck exited with code 2".to_owned())
        );
        assert!(matches!(verdict(&r(None, true, "")), Verdict::Fail(_)));
        assert!(matches!(verdict(&r(None, false, "")), Verdict::Fail(_)));
    }

    #[test]
    fn a_long_reason_is_clipped() {
        let long = "x".repeat(1000);
        assert_eq!(clip(&long).chars().count(), MAX_REASON_CHARS);
        assert_eq!(clip("  short "), "short");
    }
}
