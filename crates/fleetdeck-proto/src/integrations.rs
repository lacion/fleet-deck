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
    /// The scrubbed start of the CLI's own status text.
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

// ----- Checks shared by the station and the deck -----

fn plain(s: &str, max: usize) -> bool {
    !s.is_empty() && s.chars().count() <= max && !s.chars().any(char::is_control)
}

/// A host name: letters, digits, `.`, `-`, optional `:port`.
#[must_use]
pub fn valid_host(host: &str) -> bool {
    let (name, port) = host
        .split_once(':')
        .map_or((host, None), |(n, p)| (n, Some(p)));
    let name_ok = !name.is_empty()
        && name.len() <= 253
        && !name.starts_with(['-', '.'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    let port_ok =
        port.is_none_or(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()));
    name_ok && port_ok
}

/// A repository path: 2 to 10 segments of `[A-Za-z0-9._-]`, none starting
/// with `-` or `.`.
#[must_use]
pub fn valid_repo(repo: &str) -> bool {
    let parts: Vec<&str> = repo.split('/').collect();
    repo.chars().count() <= MAX_NAME_CHARS
        && (2..=10).contains(&parts.len())
        && parts.iter().all(|p| {
            !p.is_empty()
                && !p.starts_with(['-', '.'])
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

/// A tracker entry id: 1 to 32 of `[a-z0-9_-]`, starting with a letter or
/// a digit.
#[must_use]
pub fn valid_tracker_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=32).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
}

/// A Jira project key or a Linear team key: 1 to 10 of `[A-Za-z0-9_]`,
/// starting with a letter.
#[must_use]
pub fn valid_project_key(key: &str) -> bool {
    let b = key.as_bytes();
    (1..=10).contains(&b.len())
        && b[0].is_ascii_alphabetic()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// An issue key for `binding`: `KEY-123` for a tracker; `#123` or `123`
/// for a forge.
#[must_use]
pub fn valid_issue_key(binding: &Binding, key: &str) -> bool {
    match binding {
        Binding::Tracker(_) => key.split_once('-').is_some_and(|(p, n)| {
            valid_project_key(p)
                && (1..=9).contains(&n.len())
                && n.bytes().all(|b| b.is_ascii_digit())
        }),
        Binding::Forge(_) => {
            let n = key.strip_prefix('#').unwrap_or(key);
            (1..=9).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit())
        }
    }
}

/// The issue number of a forge issue key (`#123` or `123`).
#[must_use]
pub fn issue_number(key: &str) -> Option<u64> {
    key.strip_prefix('#').unwrap_or(key).parse().ok()
}

/// A git branch name for a pull request: 1 to 200 characters without
/// control characters, spaces, `~^:?*[\` or `..`, not starting with `-`.
#[must_use]
pub fn valid_branch(branch: &str) -> bool {
    plain(branch, MAX_NAME_CHARS)
        && !branch.starts_with(['-', '/'])
        && !branch.ends_with(['/', '.'])
        && !branch.contains("..")
        && !branch.contains("@{")
        && !branch
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
}

/// An id of a write: 1 to 64 of `[0-9A-Za-z_-]`.
#[must_use]
pub fn valid_command_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// The systems of an [`IssueRef`].
pub const ISSUE_SYSTEMS: &[&str] = &["jira", "linear", "github", "gitlab"];

impl IssueRef {
    /// # Errors
    /// The reason text for a malformed field.
    pub fn check(&self) -> Result<(), &'static str> {
        if !ISSUE_SYSTEMS.contains(&self.system.as_str()) {
            return Err("the issue system is not jira, linear, github or gitlab");
        }
        if !plain(&self.key, 64) {
            return Err("the issue key is 1 to 64 printable characters");
        }
        let http = self.url.starts_with("https://") || self.url.starts_with("http://");
        if !http || !plain(&self.url, 2048) {
            return Err("the issue URL must be an http(s) URL of at most 2048 characters");
        }
        if !plain(&self.title, 300) {
            return Err("the issue title is 1 to 300 printable characters");
        }
        Ok(())
    }
}

impl ForgeBinding {
    /// # Errors
    /// The reason text for a malformed host or repository.
    pub fn check(&self) -> Result<(), &'static str> {
        if !valid_host(&self.host) {
            return Err("the forge host is not a valid host name");
        }
        if !valid_repo(&self.repo) {
            return Err(
                "the repository must be owner/name (2 to 10 segments of letters, digits, . _ -)",
            );
        }
        Ok(())
    }
}

impl TrackerBinding {
    /// # Errors
    /// The reason text for a malformed tracker id or key.
    pub fn check(&self) -> Result<(), &'static str> {
        if !valid_tracker_id(&self.tracker) {
            return Err("the tracker id is not valid");
        }
        if !valid_project_key(&self.key) {
            return Err("the project key is not valid");
        }
        Ok(())
    }
}

impl Binding {
    /// # Errors
    /// The reason text for a malformed binding.
    pub fn check(&self) -> Result<(), &'static str> {
        match self {
            Self::Tracker(t) => t.check(),
            Self::Forge(f) => f.check(),
        }
    }

    /// The system name of an [`IssueRef`] for this binding.
    #[must_use]
    pub fn system(&self) -> &'static str {
        match self {
            Self::Tracker(t) => match t.kind {
                TrackerKind::Jira => "jira",
                TrackerKind::Linear => "linear",
            },
            Self::Forge(f) => match f.kind {
                ForgeKind::Github => "github",
                ForgeKind::Gitlab => "gitlab",
            },
        }
    }
}

impl IssueSearch {
    /// # Errors
    /// The reason text for a malformed field.
    pub fn check(&self) -> Result<(), &'static str> {
        self.binding.check()?;
        for text in [&self.query, &self.status].into_iter().flatten() {
            if !plain(text, MAX_NAME_CHARS) {
                return Err("query and status are 1 to 200 printable characters");
            }
        }
        if self
            .limit
            .is_some_and(|l| !(1..=MAX_SEARCH_LIMIT).contains(&l))
        {
            return Err("limit is 1 to 100");
        }
        Ok(())
    }

    /// The limit with its default.
    #[must_use]
    pub fn limit(&self) -> u32 {
        self.limit.unwrap_or(DEFAULT_SEARCH_LIMIT)
    }
}

impl IssueGet {
    /// # Errors
    /// The reason text for a malformed field.
    pub fn check(&self) -> Result<(), &'static str> {
        self.binding.check()?;
        if !valid_issue_key(&self.binding, &self.key) {
            return Err("the issue key is not valid");
        }
        Ok(())
    }
}

impl AssetQuery {
    /// # Errors
    /// The reason text for a malformed field.
    pub fn check(&self) -> Result<(), &'static str> {
        self.binding.check()?;
        if !self.url.starts_with("https://") || !plain(&self.url, 2048) || self.url.contains(' ') {
            return Err("the asset URL must be an https URL");
        }
        Ok(())
    }
}

fn check_body(body: &str, required: bool) -> Result<(), &'static str> {
    if body.chars().count() > MAX_BODY_CHARS || body.contains('\0') {
        return Err("the text is longer than 65536 characters or holds NUL");
    }
    if required && body.trim().is_empty() {
        return Err("the text is empty");
    }
    Ok(())
}

fn check_number(number: u64) -> Result<(), &'static str> {
    if number == 0 || number > 999_999_999 {
        return Err("the number is not valid");
    }
    Ok(())
}

impl PrCreate {
    /// # Errors
    /// The reason text for a malformed field.
    pub fn check(&self) -> Result<(), &'static str> {
        check_id(&self.command_id)?;
        self.forge.check()?;
        if !plain(&self.path, 4096) || !self.path.starts_with('/') {
            return Err("the path must be an absolute path");
        }
        if !valid_branch(&self.head) || !valid_branch(&self.base) {
            return Err("head and base must be branch names");
        }
        if !plain(&self.title, MAX_TITLE_CHARS) {
            return Err("the title is 1 to 256 printable characters");
        }
        check_body(&self.body, false)
    }
}

fn check_id(id: &str) -> Result<(), &'static str> {
    if valid_command_id(id) {
        Ok(())
    } else {
        Err("a command id has 1 to 64 characters from [0-9A-Za-z_-]")
    }
}

impl PrComment {
    /// # Errors
    /// The reason text for a malformed field.
    pub fn check(&self) -> Result<(), &'static str> {
        check_id(&self.command_id)?;
        self.forge.check()?;
        check_number(self.number)?;
        check_body(&self.body, true)
    }
}

impl PrReview {
    /// # Errors
    /// The reason text for a malformed field.
    pub fn check(&self) -> Result<(), &'static str> {
        check_id(&self.command_id)?;
        self.forge.check()?;
        check_number(self.number)?;
        // An approval may come without text.
        check_body(&self.body, self.event != ReviewEvent::Approve)
    }
}

impl IssueCommentWrite {
    /// # Errors
    /// The reason text for a malformed field.
    pub fn check(&self) -> Result<(), &'static str> {
        check_id(&self.command_id)?;
        self.binding.check()?;
        if !valid_issue_key(&self.binding, &self.key) {
            return Err("the issue key is not valid");
        }
        check_body(&self.body, true)
    }
}

impl IssueTransition {
    /// # Errors
    /// The reason text for a malformed field.
    pub fn check(&self) -> Result<(), &'static str> {
        check_id(&self.command_id)?;
        self.binding.check()?;
        if !valid_issue_key(&self.binding, &self.key) {
            return Err("the issue key is not valid");
        }
        if !plain(&self.to, 100) {
            return Err("the status name is 1 to 100 printable characters");
        }
        Ok(())
    }
}

impl PrRef {
    /// # Errors
    /// The reason text for a malformed field.
    pub fn check(&self) -> Result<(), &'static str> {
        self.forge.check()?;
        check_number(self.number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forge() -> ForgeBinding {
        ForgeBinding {
            kind: ForgeKind::Gitlab,
            host: "gitlab.com".into(),
            repo: "group/sub/project".into(),
        }
    }

    #[test]
    fn bindings_check_their_fields() {
        assert!(forge().check().is_ok());
        let mut bad = forge();
        bad.repo = "../etc/passwd".into();
        assert!(bad.check().is_err());
        bad.repo = "solo".into();
        assert!(bad.check().is_err());
        bad.repo = "a/-flag".into();
        assert!(bad.check().is_err());
        let mut bad = forge();
        bad.host = "evil.com/x".into();
        assert!(bad.check().is_err());
        bad.host = "host:8443".into();
        assert!(bad.check().is_ok());
    }

    #[test]
    fn issue_keys_follow_the_binding() {
        let t = Binding::Tracker(TrackerBinding {
            tracker: "acme".into(),
            kind: TrackerKind::Jira,
            key: "PROJ".into(),
        });
        let f = Binding::Forge(forge());
        assert!(valid_issue_key(&t, "PROJ-123"));
        assert!(!valid_issue_key(&t, "PROJ-"));
        assert!(!valid_issue_key(&t, "#12"));
        assert!(valid_issue_key(&f, "#12"));
        assert!(valid_issue_key(&f, "12"));
        assert!(!valid_issue_key(&f, "PROJ-1"));
        assert_eq!(issue_number("#12"), Some(12));
    }

    #[test]
    fn branch_names_refuse_flags_and_ref_syntax() {
        assert!(valid_branch("PROJ-1-fix-login"));
        assert!(valid_branch("feature/x"));
        for bad in ["-x", "a b", "a..b", "a:b", "a~1", "x/", "", "a@{1}"] {
            assert!(!valid_branch(bad), "{bad}");
        }
    }

    #[test]
    fn a_write_checks_its_id_and_text() {
        let comment = |id: &str, body: &str| PrComment {
            command_id: id.into(),
            forge: forge(),
            number: 3,
            body: body.into(),
        };
        assert!(comment("c-1", "hello").check().is_ok());
        assert!(comment("", "hello").check().is_err());
        assert!(comment("a b", "hello").check().is_err());
        assert!(comment("c-1", "  ").check().is_err());
        assert!(
            comment("c-1", &"x".repeat(MAX_BODY_CHARS + 1))
                .check()
                .is_err()
        );
        let approve = PrReview {
            command_id: "r-1".into(),
            forge: forge(),
            number: 3,
            event: ReviewEvent::Approve,
            body: String::new(),
        };
        assert!(approve.check().is_ok());
        let request = PrReview {
            event: ReviewEvent::RequestChanges,
            ..approve
        };
        assert!(request.check().is_err(), "a change request needs text");
    }

    #[test]
    fn the_search_limit_is_bounded() {
        let search = |limit| IssueSearch {
            binding: Binding::Forge(forge()),
            query: None,
            mine: false,
            status: None,
            limit,
        };
        assert!(search(None).check().is_ok());
        assert_eq!(search(None).limit(), DEFAULT_SEARCH_LIMIT);
        assert!(search(Some(0)).check().is_err());
        assert!(search(Some(101)).check().is_err());
    }

    #[test]
    fn an_issue_ref_checks_its_fields() {
        let ok = IssueRef {
            system: "jira".into(),
            key: "PROJ-1".into(),
            url: "https://acme.atlassian.net/browse/PROJ-1".into(),
            title: "Fix login".into(),
        };
        assert!(ok.check().is_ok());
        for bad in [
            IssueRef {
                system: "svn".into(),
                ..ok.clone()
            },
            IssueRef {
                url: "javascript:alert(1)".into(),
                ..ok.clone()
            },
            IssueRef {
                title: "a\nb".into(),
                ..ok.clone()
            },
            IssueRef {
                key: String::new(),
                ..ok.clone()
            },
        ] {
            assert!(bad.check().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_unknown_field_fails_closed() {
        let r = serde_json::from_value::<PrRef>(serde_json::json!({
            "forge": {"kind": "github", "host": "github.com", "repo": "a/b"},
            "number": 1, "extra": true
        }));
        assert!(r.is_err());
    }
}
