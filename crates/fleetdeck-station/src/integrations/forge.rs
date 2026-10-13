//! GitHub and GitLab through the user's `gh` and `glab` CLIs (`gh api`,
//! `glab api`). The station uses the CLI's own login; the deck and the board
//! never see a token.

use std::path::PathBuf;

use fleetdeck_proto::integrations::{
    Asset, ChecksFailing, ForgeBinding, ForgeKind, IssueDetail, IssueSearch, IssueSummary,
    PrDetail, PrGroups, PrSummary, ReviewEvent,
};
use serde_json::Value;

use super::exec::{self, Fail, Programs, Spec};
use super::{github, gitlab};

/// One repository on one forge, with the helpers that reach it.
#[derive(Debug, Clone, Copy)]
pub struct Forge<'a> {
    pub programs: &'a Programs,
    /// Variable names that must not reach the child (tracker tokens).
    pub hidden: &'a [String],
    pub binding: &'a ForgeBinding,
}

impl Forge<'_> {
    fn cli(&self) -> (PathBuf, &'static str) {
        match self.binding.kind {
            ForgeKind::Github => (self.programs.gh.clone(), "gh"),
            ForgeKind::Gitlab => (self.programs.glab.clone(), "glab"),
        }
    }

    fn is_default_host(&self) -> bool {
        match self.binding.kind {
            ForgeKind::Github => self.binding.host == "github.com",
            ForgeKind::Gitlab => self.binding.host == "gitlab.com",
        }
    }

    /// `<cli> api [--hostname H] -X METHOD ENDPOINT [--input -]`.
    fn args(&self, method: &str, endpoint: &str, body: bool) -> Vec<String> {
        let mut args = vec!["api".to_owned()];
        if !self.is_default_host() {
            args.push("--hostname".into());
            args.push(self.binding.host.clone());
        }
        args.push("-X".into());
        args.push(method.to_owned());
        if body {
            args.push("--input".into());
            args.push("-".into());
            if self.binding.kind == ForgeKind::Gitlab {
                args.push("-H".into());
                args.push("Content-Type: application/json".into());
            }
        }
        args.push(endpoint.to_owned());
        args
    }

    async fn call(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<&Value>,
        cwd: Option<PathBuf>,
        max_stdout: usize,
    ) -> Result<Vec<u8>, Fail> {
        let (program, name) = self.cli();
        let spec = Spec {
            program,
            args: self.args(method, endpoint, body.is_some()),
            cwd,
            env: exec::cli_env(self.hidden),
            stdin: body.map(|b| b.to_string().into_bytes()),
            max_stdout,
        };
        let out = exec::run(spec).await.map_err(|e| exec::fail_of(&e, name))?;
        if out.code == 0 {
            return Ok(out.stdout);
        }
        let body = String::from_utf8_lossy(&out.stdout);
        let detail = if out.stderr.is_empty() {
            body.to_string()
        } else {
            format!("{name}: {} {}", out.stderr, body)
        };
        Err(Fail::upstream(detail))
    }

    /// A JSON call.
    pub async fn json(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<&Value>,
    ) -> Result<Value, Fail> {
        self.json_in(method, endpoint, body, None).await
    }

    /// A JSON call that runs in `cwd` (the repository of a write).
    pub async fn json_in(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<&Value>,
        cwd: Option<PathBuf>,
    ) -> Result<Value, Fail> {
        let bytes = self
            .call(method, endpoint, body, cwd, exec::MAX_STDOUT)
            .await?;
        serde_json::from_slice(&bytes).map_err(|_| Fail::bad_answer("the forge answer is not JSON"))
    }

    /// A call whose answer is text (a job log).
    pub async fn text(&self, endpoint: &str) -> Result<String, Fail> {
        let bytes = self
            .call("GET", endpoint, None, None, exec::MAX_STDOUT)
            .await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The path of this repository in the REST endpoints.
    pub fn repo_path(&self) -> String {
        match self.binding.kind {
            ForgeKind::Github => self.binding.repo.clone(),
            ForgeKind::Gitlab => super::util::urlencode(&self.binding.repo),
        }
    }

    /// The login of the signed-in user.
    pub async fn viewer(&self) -> Result<String, Fail> {
        let user = self.json("GET", "user", None).await?;
        let field = match self.binding.kind {
            ForgeKind::Github => "login",
            ForgeKind::Gitlab => "username",
        };
        user[field]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Fail::bad_answer("the forge gave no user name"))
    }

    pub async fn issues_search(&self, q: &IssueSearch) -> Result<Vec<IssueSummary>, Fail> {
        match self.binding.kind {
            ForgeKind::Github => github::issues_search(self, q).await,
            ForgeKind::Gitlab => gitlab::issues_search(self, q).await,
        }
    }

    pub async fn issue_get(&self, number: u64) -> Result<IssueDetail, Fail> {
        match self.binding.kind {
            ForgeKind::Github => github::issue_get(self, number).await,
            ForgeKind::Gitlab => gitlab::issue_get(self, number).await,
        }
    }

    pub async fn prs_list(&self) -> Result<PrGroups, Fail> {
        match self.binding.kind {
            ForgeKind::Github => github::prs_list(self).await,
            ForgeKind::Gitlab => gitlab::prs_list(self).await,
        }
    }

    pub async fn pr_get(&self, number: u64) -> Result<PrDetail, Fail> {
        match self.binding.kind {
            ForgeKind::Github => github::pr_get(self, number).await,
            ForgeKind::Gitlab => gitlab::pr_get(self, number).await,
        }
    }

    pub async fn pr_checks(&self, number: u64) -> Result<ChecksFailing, Fail> {
        match self.binding.kind {
            ForgeKind::Github => github::pr_checks(self, number).await,
            ForgeKind::Gitlab => gitlab::pr_checks(self, number).await,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn pr_create(
        &self,
        cwd: PathBuf,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<String, Fail> {
        match self.binding.kind {
            ForgeKind::Github => github::pr_create(self, cwd, head, base, title, body, draft).await,
            ForgeKind::Gitlab => gitlab::pr_create(self, cwd, head, base, title, body, draft).await,
        }
    }

    pub async fn comment(&self, pr: bool, number: u64, body: &str) -> Result<String, Fail> {
        match self.binding.kind {
            ForgeKind::Github => github::comment(self, number, body).await,
            ForgeKind::Gitlab => gitlab::comment(self, pr, number, body).await,
        }
    }

    pub async fn review(
        &self,
        number: u64,
        event: ReviewEvent,
        body: &str,
    ) -> Result<String, Fail> {
        match self.binding.kind {
            ForgeKind::Github => github::review(self, number, event, body).await,
            ForgeKind::Gitlab => gitlab::review(self, number, event, body).await,
        }
    }

    pub async fn transition(&self, number: u64, to: &str) -> Result<String, Fail> {
        let state = open_state(to)?;
        match self.binding.kind {
            ForgeKind::Github => github::transition(self, number, state).await,
            ForgeKind::Gitlab => gitlab::transition(self, number, state).await,
        }
    }

    /// An image of an issue body from a forge host. The CLIs cannot fetch an
    /// arbitrary URL, so only public images are served (no token is sent).
    pub async fn asset(&self, url: &str) -> Result<Asset, Fail> {
        super::asset::public(self.programs, url, &asset_hosts(self.binding)).await
    }
}

/// The state that a forge issue moves to: `true` for open.
///
/// # Errors
/// `invalid_params` for any other status name.
pub fn open_state(to: &str) -> Result<bool, Fail> {
    match to.to_ascii_lowercase().as_str() {
        "open" | "reopen" => Ok(true),
        "closed" | "close" => Ok(false),
        _ => Err(Fail::new(
            fleetdeck_proto::integrations::code::INVALID_PARAMS,
            "a forge issue moves to `open` or `closed`",
        )),
    }
}

/// The hosts that serve the images of a forge's issues.
fn asset_hosts(binding: &ForgeBinding) -> Vec<String> {
    let mut hosts = vec![binding.host.clone()];
    if binding.kind == ForgeKind::Github {
        hosts.extend(
            [
                "github.com",
                "user-images.githubusercontent.com",
                "private-user-images.githubusercontent.com",
                "raw.githubusercontent.com",
                "github.githubassets.com",
            ]
            .map(str::to_owned),
        );
    }
    hosts
}

/// The group of a pull request in the pull request view.
pub fn group(list: Vec<PrSummary>) -> PrGroups {
    use fleetdeck_proto::integrations::{ChecksState, PrState, ReviewState};
    let mut groups = PrGroups::default();
    for pr in list {
        match pr.state {
            PrState::Merged | PrState::Closed => groups.recent_closed.push(pr),
            PrState::Open if pr.review_requested_from_me && !pr.mine => {
                groups.review_requested.push(pr);
            }
            PrState::Open if pr.mine => groups.mine.push(pr),
            PrState::Open
                if pr.review_state == ReviewState::ReviewRequired
                    || pr.checks == ChecksState::Pending =>
            {
                groups.pending.push(pr);
            }
            // Another author's pull request that waits for nothing is not
            // part of the view.
            PrState::Open => {}
        }
    }
    for g in [
        &mut groups.review_requested,
        &mut groups.mine,
        &mut groups.pending,
        &mut groups.recent_closed,
    ] {
        g.sort_by_key(|p| std::cmp::Reverse(p.updated_at));
    }
    groups
}

/// The first ticket key in the title or the head branch.
pub fn ticket_of(title: &str, branch: &str) -> Option<String> {
    crate::workspace::provision::ticket_key(branch)
        .or_else(|| crate::workspace::provision::ticket_key(title))
}

/// The most recent comments that an answer keeps.
pub const MAX_COMMENTS: usize = 30;

#[cfg(test)]
mod tests {
    use fleetdeck_proto::integrations::{ChecksState, PrState, ReviewState};

    use super::*;

    fn pr(n: u64, state: PrState, mine: bool, asked: bool, at: i64) -> PrSummary {
        PrSummary {
            number: n,
            title: format!("t{n}"),
            url: String::new(),
            author: "a".into(),
            head_branch: "h".into(),
            base_branch: "main".into(),
            draft: false,
            state,
            review_state: ReviewState::None,
            checks: ChecksState::None,
            mergeable: None,
            ticket_key: None,
            updated_at: at,
            review_requested_from_me: asked,
            mine,
        }
    }

    #[test]
    fn pull_requests_fall_into_the_four_groups_newest_first() {
        let g = group(vec![
            pr(1, PrState::Open, false, true, 10),
            pr(2, PrState::Open, true, false, 20),
            PrSummary {
                review_state: ReviewState::ReviewRequired,
                ..pr(3, PrState::Open, false, false, 30)
            },
            pr(8, PrState::Open, false, false, 60),
            pr(4, PrState::Merged, true, false, 5),
            pr(5, PrState::Closed, false, false, 9),
            pr(6, PrState::Open, true, true, 40),
            pr(7, PrState::Open, true, false, 50),
        ]);
        let nums = |l: &[PrSummary]| l.iter().map(|p| p.number).collect::<Vec<_>>();
        assert_eq!(nums(&g.review_requested), [1]);
        assert_eq!(
            nums(&g.mine),
            [7, 6, 2],
            "an own PR is never a review request"
        );
        assert_eq!(nums(&g.pending), [3], "a quiet foreign PR is left out");
        assert_eq!(nums(&g.recent_closed), [5, 4]);
    }

    #[test]
    fn the_ticket_key_comes_from_the_branch_then_the_title() {
        assert_eq!(
            ticket_of("Fix login", "PROJ-12-fix"),
            Some("PROJ-12".into())
        );
        assert_eq!(ticket_of("ENG-7: speed", "fd/otter"), Some("ENG-7".into()));
        assert_eq!(ticket_of("fix", "fd/otter"), None);
    }

    #[test]
    fn the_cli_arguments_name_the_host_only_off_the_default() {
        let programs = Programs::default();
        let binding = |kind, host: &str| ForgeBinding {
            kind,
            host: host.into(),
            repo: "a/b".into(),
        };
        let gh = binding(ForgeKind::Github, "github.com");
        let f = Forge {
            programs: &programs,
            hidden: &[],
            binding: &gh,
        };
        assert_eq!(f.args("GET", "user", false), ["api", "-X", "GET", "user"]);
        let ghe = binding(ForgeKind::Github, "git.corp.test");
        let f = Forge { binding: &ghe, ..f };
        assert_eq!(
            f.args("POST", "graphql", true),
            [
                "api",
                "--hostname",
                "git.corp.test",
                "-X",
                "POST",
                "--input",
                "-",
                "graphql"
            ]
        );
        let gl = binding(ForgeKind::Gitlab, "gitlab.com");
        let f = Forge { binding: &gl, ..f };
        assert_eq!(
            f.args("POST", "projects/a%2Fb/issues", true),
            [
                "api",
                "-X",
                "POST",
                "--input",
                "-",
                "-H",
                "Content-Type: application/json",
                "projects/a%2Fb/issues"
            ]
        );
        assert_eq!(f.repo_path(), "a%2Fb");
    }
}
