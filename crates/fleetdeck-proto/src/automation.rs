//! Automations (R5, DL-168): the schedules that the deck owns and the runs
//! that it records. The deck is always on, so it holds the schedule; the
//! station only runs the precheck and the session.
//!
//! A schedule has a cron expression (five fields or a preset) and a time
//! zone that is `UTC` or a fixed offset such as `+02:00`. A run is recorded
//! once for each due time.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::board::SessionKey;
use crate::ids::{ProviderInstanceId, StationId};

/// The longest schedule name (characters).
pub const MAX_NAME_CHARS: usize = 120;
/// The longest prompt of a schedule (characters).
pub const MAX_PROMPT_CHARS: usize = 16_384;
/// The longest precheck command (characters); the setup command limit.
pub const MAX_PRECHECK_CHARS: usize = 4096;
/// The longest path, mode, model, cron text and time zone (characters).
pub const MAX_FIELD_CHARS: usize = 4096;
/// The default grace period of a missed run: one hour.
pub const DEFAULT_GRACE_MS: i64 = 3_600_000;
/// The shortest grace period: one minute (the scheduler wakes every 30 s).
pub const MIN_GRACE_MS: i64 = 60_000;
/// The longest grace period: seven days.
pub const MAX_GRACE_MS: i64 = 7 * 24 * 3_600_000;
/// The most runs that `GET /api/schedules/{id}/runs` returns.
pub const MAX_RUNS: u32 = 100;

/// How one run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "automation/")]
pub enum ScheduleRunState {
    /// A session started, or the prompt went to the session of the last run.
    Started,
    /// The precheck exited non-zero; nothing started.
    Skipped,
    /// The run could not start (a refusal, an error, a timeout).
    Failed,
    /// The due time passed the grace period without a run.
    Missed,
}

impl ScheduleRunState {
    /// The database and wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::Missed => "missed",
        }
    }

    /// The state of a database name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "started" => Some(Self::Started),
            "skipped" => Some(Self::Skipped),
            "failed" => Some(Self::Failed),
            "missed" => Some(Self::Missed),
            _ => None,
        }
    }
}

/// One schedule of the deck.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "automation/")]
pub struct Schedule {
    pub schedule_id: String,
    pub name: String,
    pub enabled: bool,
    /// Five cron fields (`m h dom mon dow`) or `@hourly`, `@daily`,
    /// `@weekdays`, `@weekly`.
    pub cron: String,
    /// `UTC` or a fixed offset `+HH:MM` / `-HH:MM`.
    pub timezone: String,
    pub station_id: StationId,
    /// Reserved for the projects of R3 (no project exists yet).
    pub project_id: Option<String>,
    /// The repository folder on the station; a new session gets a worktree
    /// of it.
    pub path: String,
    pub provider_instance: ProviderInstanceId,
    pub mode: Option<String>,
    pub model: Option<String>,
    pub prompt: String,
    /// A shell command that the station runs in `path` first; a non-zero
    /// exit records a skipped run.
    pub precheck_cmd: Option<String>,
    /// Send the prompt to the session of the last run when it is idle.
    pub reuse_session: bool,
    pub last_session: Option<SessionKey>,
    #[ts(type = "number")]
    pub grace_ms: i64,
    #[ts(type = "number")]
    pub created_at: i64,
}

/// One recorded run of a schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "automation/")]
pub struct ScheduleRun {
    pub run_id: String,
    pub schedule_id: String,
    /// The due time (Unix ms); a "run now" uses the time of the click.
    #[ts(type = "number")]
    pub due_at: i64,
    /// When the deck handled the due time (Unix ms).
    #[ts(type = "number")]
    pub started_at: i64,
    pub state: ScheduleRunState,
    /// Why the run ended so: the last line of the precheck output, an
    /// error, or a note. Empty for a plain start.
    pub reason: String,
    pub station_id: StationId,
    pub session_id: Option<String>,
    /// "Run now": the owner started it; it does not move the schedule.
    pub manual: bool,
}

/// A schedule with what the list shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "automation/")]
pub struct ScheduleView {
    pub schedule: Schedule,
    /// The next due time after now; `None` when the schedule is off.
    #[ts(type = "number | null")]
    pub next_run_at: Option<i64>,
    pub last_run: Option<ScheduleRun>,
}

/// The body of `POST /api/schedules` and `PUT /api/schedules/{id}` (the
/// update replaces every field).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "automation/")]
pub struct ScheduleInput {
    pub name: String,
    pub enabled: bool,
    pub cron: String,
    pub timezone: String,
    /// The name or the id of the station.
    pub station: String,
    #[serde(default)]
    #[ts(optional)]
    pub project_id: Option<String>,
    pub path: String,
    pub provider_instance: ProviderInstanceId,
    #[serde(default)]
    #[ts(optional)]
    pub mode: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub model: Option<String>,
    pub prompt: String,
    #[serde(default)]
    #[ts(optional)]
    pub precheck_cmd: Option<String>,
    pub reuse_session: bool,
    /// Absent: [`DEFAULT_GRACE_MS`].
    #[serde(default)]
    #[ts(optional, type = "number")]
    pub grace_ms: Option<i64>,
}

impl ScheduleInput {
    /// Checks the text limits (the cron text and the time zone are checked
    /// by the deck, which parses them).
    ///
    /// # Errors
    /// A message that names the first broken limit.
    pub fn check(&self) -> Result<(), String> {
        let chars = |s: &str| s.chars().count();
        if self.name.trim().is_empty() || chars(&self.name) > MAX_NAME_CHARS {
            return Err(format!("a name has 1 to {MAX_NAME_CHARS} characters"));
        }
        if self.prompt.trim().is_empty() || chars(&self.prompt) > MAX_PROMPT_CHARS {
            return Err(format!("a prompt has 1 to {MAX_PROMPT_CHARS} characters"));
        }
        if self.station.trim().is_empty() {
            return Err("a schedule needs a station".to_owned());
        }
        if self.path.trim().is_empty() {
            return Err("a schedule needs a folder".to_owned());
        }
        if self.provider_instance.as_str().trim().is_empty() {
            return Err("a schedule needs a provider instance".to_owned());
        }
        let fields = [
            ("cron", Some(&self.cron)),
            ("timezone", Some(&self.timezone)),
            ("station", Some(&self.station)),
            ("path", Some(&self.path)),
            ("provider instance", Some(&self.provider_instance.0)),
            ("mode", self.mode.as_ref()),
            ("model", self.model.as_ref()),
            ("project", self.project_id.as_ref()),
        ];
        for (label, value) in fields {
            if let Some(v) = value
                && (chars(v) > MAX_FIELD_CHARS || v.contains('\0'))
            {
                return Err(format!(
                    "the {label} has at most {MAX_FIELD_CHARS} characters and no NUL"
                ));
            }
        }
        if self.name.contains('\0') || self.prompt.contains('\0') {
            return Err("a name and a prompt have no NUL".to_owned());
        }
        if let Some(cmd) = &self.precheck_cmd
            && !cmd.trim().is_empty()
            && (chars(cmd) > MAX_PRECHECK_CHARS || cmd.contains('\0'))
        {
            return Err(format!(
                "a precheck command has at most {MAX_PRECHECK_CHARS} characters and no NUL"
            ));
        }
        if let Some(grace) = self.grace_ms
            && !(MIN_GRACE_MS..=MAX_GRACE_MS).contains(&grace)
        {
            return Err(format!(
                "the grace period is {MIN_GRACE_MS} to {MAX_GRACE_MS} milliseconds"
            ));
        }
        Ok(())
    }

    /// The grace period in milliseconds.
    #[must_use]
    pub fn grace(&self) -> i64 {
        self.grace_ms.unwrap_or(DEFAULT_GRACE_MS)
    }

    /// The precheck command, or `None` when it is blank.
    #[must_use]
    pub fn precheck(&self) -> Option<String> {
        self.precheck_cmd
            .as_ref()
            .filter(|c| !c.trim().is_empty())
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> ScheduleInput {
        ScheduleInput {
            name: "Triage".to_owned(),
            enabled: true,
            cron: "@daily".to_owned(),
            timezone: "UTC".to_owned(),
            station: "alpha".to_owned(),
            project_id: None,
            path: "/work/repo".to_owned(),
            provider_instance: ProviderInstanceId::from("claude"),
            mode: None,
            model: None,
            prompt: "Triage the new issues.".to_owned(),
            precheck_cmd: None,
            reuse_session: false,
            grace_ms: None,
        }
    }

    #[test]
    fn a_valid_input_passes_and_the_limits_refuse() {
        assert_eq!(input().check(), Ok(()));
        assert_eq!(input().grace(), DEFAULT_GRACE_MS);
        let mut i = input();
        i.name = " ".to_owned();
        assert!(i.check().is_err());
        let mut i = input();
        i.prompt = "x".repeat(MAX_PROMPT_CHARS + 1);
        assert!(i.check().is_err());
        let mut i = input();
        i.precheck_cmd = Some("a\0b".to_owned());
        assert!(i.check().is_err());
        let mut i = input();
        i.grace_ms = Some(MIN_GRACE_MS - 1);
        assert!(i.check().is_err());
        i.grace_ms = Some(MIN_GRACE_MS);
        assert_eq!(i.check(), Ok(()));
        i.grace_ms = Some(MAX_GRACE_MS + 1);
        assert!(i.check().is_err());
    }

    #[test]
    fn a_blank_precheck_is_none() {
        let mut i = input();
        i.precheck_cmd = Some("  ".to_owned());
        assert_eq!(i.precheck(), None);
        i.precheck_cmd = Some("test -n \"$(git log -1)\"".to_owned());
        assert!(i.precheck().is_some());
    }

    #[test]
    fn run_states_round_trip() {
        for s in [
            ScheduleRunState::Started,
            ScheduleRunState::Skipped,
            ScheduleRunState::Failed,
            ScheduleRunState::Missed,
        ] {
            assert_eq!(ScheduleRunState::parse(s.as_str()), Some(s));
        }
        assert_eq!(ScheduleRunState::parse("nope"), None);
    }
}
