//! Field checks shared by the station and the deck (DL-164).

use super::*;

pub(super) fn plain(s: &str, max: usize) -> bool {
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

/// A repository path: `owner/name` on GitHub; 2 to 20 segments on GitLab
/// (groups and subgroups). A segment has 1 to 255 characters from
/// `[A-Za-z0-9._-]`, does not start with `-` or `.`, and is not `.` or `..`.
#[must_use]
pub fn valid_repo(kind: ForgeKind, repo: &str) -> bool {
    let parts: Vec<&str> = repo.split('/').collect();
    let count = match kind {
        ForgeKind::Github => 2..=2,
        ForgeKind::Gitlab => 2..=20,
    };
    repo.chars().count() <= MAX_NAME_CHARS
        && count.contains(&parts.len())
        && parts.iter().all(|p| {
            (1..=255).contains(&p.len())
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
        if !valid_repo(self.kind, &self.repo) {
            return Err(match self.kind {
                ForgeKind::Github => "the repository must be owner/name (letters, digits, . _ -)",
                ForgeKind::Gitlab => {
                    "the repository must be 2 to 20 segments of letters, digits, . _ -"
                }
            });
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

pub(super) fn check_number(number: u64) -> Result<(), &'static str> {
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

impl PrFetchHead {
    /// # Errors
    /// The reason text for a malformed field.
    pub fn check(&self) -> Result<(), &'static str> {
        self.forge.check()?;
        check_number(self.number)?;
        if !plain(&self.path, 4096) || !self.path.starts_with('/') {
            return Err("the path must be an absolute path");
        }
        Ok(())
    }
}
