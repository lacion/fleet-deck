//! GitHub, GitLab, Jira and Linear on the wire (R3, DL-164): the bindings
//! of a project, the issue and pull request shapes, and the params and
//! answers of the `integrations.v1` link methods.
//!
//! The station does all forge and tracker I/O with its own credentials. The
//! deck relays the answers and stores only bindings; it never holds a token.
//! Every third-party text that a session may read (issue bodies, comments,
//! pull request descriptions, CI log tails) arrives inside the injection
//! envelope of [`crate::untrusted::fence`].

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The longest issue key, repository path or status name, in characters.
pub const MAX_NAME_CHARS: usize = 200;
/// The longest title of a pull request, in characters.
pub const MAX_TITLE_CHARS: usize = 256;
/// The longest comment, review or pull request body, in characters.
pub const MAX_BODY_CHARS: usize = 65_536;
/// The most issues in one `issues.search` answer.
pub const MAX_SEARCH_LIMIT: u32 = 100;
/// The default number of issues in one `issues.search` answer.
pub const DEFAULT_SEARCH_LIMIT: u32 = 25;
/// The largest `issues.asset` image in bytes.
pub const MAX_ASSET_BYTES: usize = 5 * 1024 * 1024;
/// The log lines per failing check in a `prs.checks` answer.
pub const CHECK_LOG_LINES: usize = 200;

/// The code hosting systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "integrations/")]
pub enum ForgeKind {
    Github,
    Gitlab,
}

/// The issue trackers that need a `[[trackers]]` entry on the station.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "integrations/")]
pub enum TrackerKind {
    Jira,
    Linear,
}

/// The remote repository of a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct ForgeBinding {
    pub kind: ForgeKind,
    /// The host name, for example `github.com`.
    pub host: String,
    /// `owner/name`, or `group/sub/project` for GitLab.
    pub repo: String,
}

/// A tracker project of a station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct TrackerBinding {
    /// The `id` of the station's `[[trackers]]` entry.
    pub tracker: String,
    pub kind: TrackerKind,
    /// The Jira project key or the Linear team key.
    pub key: String,
}

/// Where an issue or a search lives (tag `type`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export_to = "integrations/")]
pub enum Binding {
    Tracker(TrackerBinding),
    Forge(ForgeBinding),
}

/// A link from a session to an issue (also the `issue` of a start).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct IssueRef {
    /// `jira`, `linear`, `github` or `gitlab`.
    pub system: String,
    /// `PROJ-123`, `ENG-42` or `#123`.
    pub key: String,
    pub url: String,
    pub title: String,
}

/// One row of an issue list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct IssueSummary {
    pub r#ref: IssueRef,
    pub status: String,
    pub assignee: Option<String>,
    pub priority: Option<String>,
    pub labels: Vec<String>,
    /// Unix ms.
    pub updated_at: i64,
}

/// A comment on an issue or a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct IssueComment {
    pub author: String,
    /// Unix ms.
    pub at: i64,
    /// Markdown inside the injection envelope.
    pub body_markdown: String,
}

/// An issue with its body and comments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct IssueDetail {
    pub summary: IssueSummary,
    /// Markdown inside the injection envelope. A Jira body (Atlassian
    /// Document Format) is converted on the station.
    pub body_markdown: String,
    pub estimate: Option<String>,
    pub comments: Vec<IssueComment>,
    /// URLs of the pull requests that the issue links to.
    pub linked_prs: Vec<String>,
    /// URLs of the images in the body; fetch each with `issues.asset`.
    pub images: Vec<String>,
}

/// `open`, `merged` or `closed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "integrations/")]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

/// The review state of a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "integrations/")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    ReviewRequired,
    None,
}

/// The CI state of a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "integrations/")]
pub enum ChecksState {
    Passing,
    Failing,
    Pending,
    None,
}

/// One pull request (GitHub) or merge request (GitLab).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub head_branch: String,
    pub base_branch: String,
    pub draft: bool,
    pub state: PrState,
    pub review_state: ReviewState,
    pub checks: ChecksState,
    pub mergeable: Option<bool>,
    /// The first ticket key in the title or the head branch.
    pub ticket_key: Option<String>,
    /// Unix ms.
    pub updated_at: i64,
    pub review_requested_from_me: bool,
    pub mine: bool,
}

/// The groups of the pull request view.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct PrGroups {
    pub review_requested: Vec<PrSummary>,
    pub mine: Vec<PrSummary>,
    pub pending: Vec<PrSummary>,
    pub recent_closed: Vec<PrSummary>,
}

/// The `prs.list` answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct PrList {
    pub groups: PrGroups,
}

/// One changed file of a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct PrFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

/// The `prs.get` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct PrDetail {
    pub summary: PrSummary,
    /// The description, inside the injection envelope.
    pub body_markdown: String,
    /// The diff summary: the changed files with their line counts.
    pub files: Vec<PrFile>,
    pub comments: Vec<IssueComment>,
}

/// One failing check of a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct CheckFailure {
    pub name: String,
    pub url: String,
    /// The last [`CHECK_LOG_LINES`] log lines, inside the injection
    /// envelope. Empty when the system gave no log.
    pub log_tail: String,
}

/// The `prs.checks` answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct ChecksFailing {
    pub failures: Vec<CheckFailure>,
}

// ----- Params of the read methods -----

/// `integrations.status` params: none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct StatusQuery {}

/// What the station knows about one `gh` or `glab` CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct CliStatus {
    pub present: bool,
    /// The CLI reports a signed-in user.
    pub authenticated: bool,
    /// The signed-in user, when `authenticated`.
    pub user: Option<String>,
    /// The scrubbed reason when not `authenticated`; empty otherwise.
    pub message: String,
}

/// One `[[trackers]]` entry of the station and its credential check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct TrackerStatus {
    pub id: String,
    pub kind: TrackerKind,
    pub site: Option<String>,
    /// The token was found and the system accepted it.
    pub ok: bool,
    /// The user that the token acts as, when `ok`.
    pub user: Option<String>,
    /// The scrubbed reason when not `ok`; empty otherwise.
    pub message: String,
}

/// The `integrations.status` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct IntegrationsStatus {
    pub trackers: Vec<TrackerStatus>,
    pub gh: CliStatus,
    pub glab: CliStatus,
}

/// `issues.search` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct IssueSearch {
    pub binding: Binding,
    /// Free text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub query: Option<String>,
    /// Only the issues assigned to the signed-in user.
    #[serde(default)]
    pub mine: bool,
    /// A status name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<String>,
    /// 1 to [`MAX_SEARCH_LIMIT`]; absent: [`DEFAULT_SEARCH_LIMIT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limit: Option<u32>,
}

/// The `issues.search` answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct IssueList {
    pub issues: Vec<IssueSummary>,
}

/// `issues.get` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct IssueGet {
    pub binding: Binding,
    pub key: String,
}

/// `issues.asset` params: one image URL from an issue body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct AssetQuery {
    pub binding: Binding,
    pub url: String,
}

/// The `issues.asset` answer: an image of at most [`MAX_ASSET_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct Asset {
    /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
    pub content_type: String,
    pub data_base64: String,
}

/// `prs.list` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct PrsList {
    pub forge: ForgeBinding,
}

/// `prs.get` and `prs.checks` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct PrRef {
    pub forge: ForgeBinding,
    pub number: u64,
}

// ----- Params of the write methods -----

/// `prs.create` params. `path` is a repository on the station inside the
/// allowed roots; the CLI runs there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct PrCreate {
    pub command_id: String,
    pub forge: ForgeBinding,
    pub path: String,
    pub head: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

/// `prs.comment` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct PrComment {
    pub command_id: String,
    pub forge: ForgeBinding,
    pub number: u64,
    pub body: String,
}

/// The verdict of a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "integrations/")]
pub enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

/// `prs.review` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct PrReview {
    pub command_id: String,
    pub forge: ForgeBinding,
    pub number: u64,
    pub event: ReviewEvent,
    pub body: String,
}

/// `issues.comment` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct IssueCommentWrite {
    pub command_id: String,
    pub binding: Binding,
    pub key: String,
    pub body: String,
}

/// `issues.transition` params: `to` is a status name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "integrations/")]
pub struct IssueTransition {
    pub command_id: String,
    pub binding: Binding,
    pub key: String,
    pub to: String,
}

/// The answer of every write method. A repeated `command_id` returns the
/// stored answer and writes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "integrations/")]
pub struct WriteResult {
    /// The new or changed object.
    pub url: String,
    /// The acting user on the system (the identity preview of R3.9).
    pub actor: Option<String>,
}

/// Wire error codes of the integration methods.
pub mod code {
    /// A field is out of range or malformed.
    pub const INVALID_PARAMS: &str = "invalid_params";
    /// The station has no such tracker, or no such CLI.
    pub const UNAVAILABLE: &str = "unavailable";
    /// The system refused the call (credentials, access, a missing object).
    pub const UPSTREAM: &str = "upstream";
    /// The system or the CLI took too long.
    pub const TIMEOUT: &str = "timeout";
    /// The path of `prs.create` is outside the allowed roots.
    pub const OUTSIDE_ROOTS: &str = "outside_roots";
    /// The answer was too large or not in the expected form.
    pub const BAD_ANSWER: &str = "bad_answer";
}

mod checks;
mod projects;
#[cfg(test)]
mod tests;

pub use checks::*;
pub use projects::*;
