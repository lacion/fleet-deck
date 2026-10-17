//! The projects of the board (deck routes, DL-164): the stored project, its
//! bodies and the batch start, with their field checks.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::checks::{check_number, plain, valid_command_id};
use super::*;
use crate::fleet_git::{GitSettings, ProjectHome, SlugReservation};

/// The longest project name, in characters.
pub const MAX_PROJECT_NAME_CHARS: usize = 80;
/// The longest prompt template of a project, in characters.
pub const MAX_PROMPT_CHARS: usize = 4_000;
/// The most tracker bindings of one project.
pub const MAX_PROJECT_TRACKERS: usize = 8;
/// The most issues in one session batch.
pub const MAX_BATCH_ISSUES: usize = 20;
/// The most sessions of one `POST /api/projects/{id}/sessions`.
pub const MAX_BATCH_SESSIONS: u32 = 50;
/// The prompt of a session from a ticket when the project has no template.
pub const DEFAULT_PROMPT: &str = "Work on this ticket. When done, summarize what you changed.";
/// The prompt of a review session when the project has no review prompt.
pub const DEFAULT_REVIEW_PROMPT: &str =
    "Review this pull request. Report problems with file and line.";

/// A Fleet Deck project: a repository on a station with optional bindings.
/// The deck stores bindings only; it never holds a token.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct Project {
    pub project_id: String,
    pub name: String,
    pub station_id: String,
    /// The repository on the station (an absolute path).
    pub path: String,
    pub forge: Option<ForgeBinding>,
    pub trackers: Vec<TrackerBinding>,
    /// The prompt of a session from a ticket; absent: [`DEFAULT_PROMPT`].
    pub prompt_template: Option<String>,
    /// The prompt of a review session; absent: [`DEFAULT_REVIEW_PROMPT`].
    pub review_prompt: Option<String>,
    /// Unix ms.
    pub created_at: i64,
    /// The homes of the project (Fleet Git, design §8.1). Phase 1: one,
    /// derived from `station_id` and `path`.
    #[serde(default)]
    pub homes: Vec<ProjectHome>,
    /// The grouping key of the remote (`host/path`); set with `remote_url`.
    #[serde(default)]
    pub repo: Option<String>,
    /// The remote URL without a login name or a password (O16).
    #[serde(default)]
    pub remote_url: Option<String>,
    /// The SSH login name of the remote URL (design §3.8, B2).
    #[serde(default)]
    pub remote_ssh_user: Option<String>,
    /// The write authority of the remote (design §3.8).
    #[serde(default)]
    pub remote_dest: Option<String>,
    /// The Fleet Git settings; the defaults when the project has none.
    #[serde(default)]
    pub git: GitSettings,
    /// Grows at each save of the Git settings.
    #[serde(default)]
    pub git_revision: u64,
    /// The Git names that stations reserved for the repository.
    #[serde(default)]
    pub slugs: Vec<SlugReservation>,
}

/// The body of `POST /api/projects` and `PUT /api/projects/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct ProjectInput {
    pub name: String,
    /// The station's name or id.
    pub station: String,
    pub path: String,
    #[serde(default)]
    pub forge: Option<ForgeBinding>,
    #[serde(default)]
    pub trackers: Vec<TrackerBinding>,
    #[serde(default)]
    pub prompt_template: Option<String>,
    #[serde(default)]
    pub review_prompt: Option<String>,
}

/// The `GET /api/projects` answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct ProjectList {
    pub projects: Vec<Project>,
}

/// One check of a project preflight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct BindingCheck {
    /// What was checked, for example `tracker acme PROJ`.
    pub label: String,
    pub ok: bool,
    /// The sentence for the owner, ok or not.
    pub message: String,
}

/// The `POST /api/projects/{id}/preflight` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct ProjectPreflight {
    /// The station's `integrations.status`; absent when the station did not
    /// answer it.
    pub status: Option<IntegrationsStatus>,
    pub checks: Vec<BindingCheck>,
}

/// The body of `POST /api/projects/{id}/sessions` (R3.4, R3.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct StartSessions {
    /// Chosen by the board. The command ids are `<batch_id>-<n>`, so a
    /// resend after a lost answer starts nothing new.
    pub batch_id: String,
    /// The issue keys; empty for a review.
    #[serde(default)]
    pub issues: Vec<String>,
    pub provider_instance: crate::ids::ProviderInstanceId,
    /// Absent: the most restrictive mode of the provider. The station
    /// refuses any other mode for these starts.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// 1 to 10 sessions for each issue; 1 for a review.
    pub count_per_issue: u32,
    /// Review this pull request instead of working on issues.
    #[serde(default)]
    pub review_of_pr: Option<u64>,
}

/// One session start that the station may take.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct StartedSession {
    /// The issue key, or `#<n>` of a review.
    pub key: String,
    pub command: crate::board::DeckCommand,
}

/// An issue or pull request that no session started for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct StartFailure {
    pub key: String,
    pub code: String,
    pub message: String,
}

/// The `POST /api/projects/{id}/sessions` answer: the receipts of the
/// starts, and what could not start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct StartedSessions {
    pub started: Vec<StartedSession>,
    pub failed: Vec<StartFailure>,
}

/// The body of `POST /api/projects/{id}/prs`. `confirm` is `true` only
/// after the owner confirmed this write on the board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct PrCreateBody {
    pub command_id: String,
    pub confirm: bool,
    pub head: String,
    pub base: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub draft: bool,
}

/// The body of `POST /api/projects/{id}/prs/{n}/comments` and
/// `POST /api/projects/{id}/issues/{key}/comments`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct CommentBody {
    pub command_id: String,
    pub confirm: bool,
    pub body: String,
}

/// The body of `POST /api/projects/{id}/prs/{n}/reviews`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct ReviewBody {
    pub command_id: String,
    pub confirm: bool,
    pub event: ReviewEvent,
    #[serde(default)]
    pub body: String,
}

/// The body of `POST /api/projects/{id}/issues/{key}/transition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct TransitionBody {
    pub command_id: String,
    pub confirm: bool,
    /// A status name.
    pub to: String,
}

impl ProjectInput {
    /// # Errors
    /// The reason text for a malformed field.
    pub fn check(&self) -> Result<(), &'static str> {
        if !plain(self.name.trim(), MAX_PROJECT_NAME_CHARS) {
            return Err("the name is 1 to 80 printable characters");
        }
        if !plain(&self.station, MAX_NAME_CHARS) {
            return Err("name the station");
        }
        if !plain(&self.path, 4096) || !self.path.starts_with('/') {
            return Err("the path must be an absolute path on the station");
        }
        if let Some(forge) = &self.forge {
            forge.check()?;
        }
        if self.trackers.len() > MAX_PROJECT_TRACKERS {
            return Err("a project has at most 8 trackers");
        }
        for (i, t) in self.trackers.iter().enumerate() {
            t.check()?;
            if self.trackers[..i]
                .iter()
                .any(|o| o.tracker == t.tracker && o.key.eq_ignore_ascii_case(&t.key))
            {
                return Err("a tracker binding appears twice");
            }
        }
        for text in [&self.prompt_template, &self.review_prompt]
            .into_iter()
            .flatten()
        {
            if text.chars().count() > MAX_PROMPT_CHARS || text.contains('\0') {
                return Err("a prompt has at most 4000 characters and no NUL");
            }
        }
        Ok(())
    }
}

impl StartSessions {
    /// # Errors
    /// The reason text for a malformed field.
    pub fn check(&self) -> Result<(), &'static str> {
        if !valid_command_id(&self.batch_id) || self.batch_id.len() > 48 {
            return Err("a batch id has 1 to 48 characters from [0-9A-Za-z_-]");
        }
        if !(1..=10).contains(&self.count_per_issue) {
            return Err("count_per_issue is 1 to 10");
        }
        match (self.issues.is_empty(), self.review_of_pr) {
            (true, None) => return Err("name issues or a pull request to review"),
            (false, Some(_)) => return Err("name issues or a pull request to review, not both"),
            (true, Some(n)) => {
                check_number(n)?;
                if self.count_per_issue != 1 {
                    return Err("a review starts one session");
                }
            }
            (false, None) => {
                if self.issues.len() > MAX_BATCH_ISSUES {
                    return Err("a batch has at most 20 issues");
                }
                let total = u32::try_from(self.issues.len())
                    .unwrap_or(u32::MAX)
                    .saturating_mul(self.count_per_issue);
                if total > MAX_BATCH_SESSIONS {
                    return Err("a batch starts at most 50 sessions");
                }
                for (i, key) in self.issues.iter().enumerate() {
                    if !plain(key, 64) || self.issues[..i].contains(key) {
                        return Err("an issue key is repeated or malformed");
                    }
                }
            }
        }
        for text in [&self.mode, &self.model].into_iter().flatten() {
            if !plain(text, 200) {
                return Err("mode and model are 1 to 200 printable characters");
            }
        }
        Ok(())
    }
}

impl PrCreateBody {
    /// The station params, for the project's repository and path.
    #[must_use]
    pub fn params(&self, forge: ForgeBinding, path: String) -> PrCreate {
        PrCreate {
            command_id: self.command_id.clone(),
            forge,
            path,
            head: self.head.clone(),
            base: self.base.clone(),
            title: self.title.clone(),
            body: self.body.clone(),
            draft: self.draft,
        }
    }
}
