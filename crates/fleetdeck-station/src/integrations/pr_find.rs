//! The pull request calls of a land (FG-8, design §11.5 step 6): the exact,
//! uncached lookup and the direct create. They do not use the DL-164 write
//! log (`writes.rs` keeps its deduplication in memory only). A create is
//! classified as `Applied`, `Rejected` or `Unknown` from the HTTP status
//! (design §5.5, revision 3, M2).

use std::time::Duration;

use fleetdeck_proto::integrations::ForgeKind;
use serde_json::Value;

use super::exec::{Fail, scrub};
use super::forge::{Forge, HttpAnswer};
use super::{github, gitlab};

/// The time limit of one forge request of a land (design §2.4).
pub const FORGE_TIMEOUT: Duration = Duration::from_secs(30);

/// A pull request (GitHub) or merge request (GitLab) of a land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRef {
    pub number: u64,
    pub url: String,
}

/// The classified result of [`pr_create_direct`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrCreate {
    /// HTTP 201 with the URL of the pull request.
    Applied(PrRef),
    /// An HTTP 4xx answer with a body: the forge refused this request.
    Rejected(Fail),
    /// A timeout, a transport error, a 5xx answer, or any other answer.
    Unknown(Fail),
}

/// The `PrRef` of one JSON object of the forge.
fn pr_of(kind: ForgeKind, v: &Value) -> Option<PrRef> {
    let (number, url) = match kind {
        ForgeKind::Github => ("number", "html_url"),
        ForgeKind::Gitlab => ("iid", "web_url"),
    };
    Some(PrRef {
        number: v.get(number)?.as_u64()?,
        url: v.get(url)?.as_str()?.to_owned(),
    })
}

fn http_fail(answer: &HttpAnswer) -> Fail {
    let body = String::from_utf8_lossy(&answer.body);
    Fail::upstream(format!("HTTP {}: {}", answer.status, body.trim()))
}

/// The most pages that a lookup reads (100 requests to a page, H16).
pub const MAX_PAGES: u32 = 10;

/// What [`pr_find`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrLookup {
    /// The open requests of this repository from `head` to `base`, after the
    /// last page.
    Found(Vec<PrRef>),
    /// The forge has more than [`MAX_PAGES`] pages of answers: the caller
    /// cannot tell that the match is exact.
    TooManyPages,
}

/// The numeric id of the bound GitLab project, read once per land; `None`
/// for GitHub, which needs no such id (H16).
///
/// # Errors
/// A [`Fail`] when the forge cannot be reached or answers with something
/// that is not a project.
pub async fn pr_project_id(forge: &Forge<'_>, timeout: Duration) -> Result<Option<u64>, Fail> {
    match forge.binding.kind {
        ForgeKind::Github => Ok(None),
        ForgeKind::Gitlab => gitlab::project_id(forge, timeout).await.map(Some),
    }
}

/// The open pull requests from `head` (a branch name) to `base`: an exact
/// lookup, no cache. A request counts only when its head is a branch of the
/// bound repository: GitHub `head.repo.full_name`, GitLab `source_project_id`
/// and `target_project_id` equal to `project` (from [`pr_project_id`]). The
/// pages are read up to [`MAX_PAGES`].
///
/// # Errors
/// A [`Fail`] when the forge cannot be reached, answers with a status other
/// than 200, or answers something that is not a list. The caller then does
/// not know the result.
pub async fn pr_find(
    forge: &Forge<'_>,
    project: Option<u64>,
    head: &str,
    base: &str,
    timeout: Duration,
) -> Result<PrLookup, Fail> {
    if forge.binding.kind == ForgeKind::Gitlab && project.is_none() {
        return Err(Fail::bad_answer("the GitLab project id is not known"));
    }
    let mut found = Vec::new();
    let mut page = 1;
    loop {
        let answer = match forge.binding.kind {
            ForgeKind::Github => github::pulls_find(forge, head, base, page, timeout).await?,
            ForgeKind::Gitlab => gitlab::mrs_find(forge, head, base, page, timeout).await?,
        };
        if answer.status != 200 {
            return Err(http_fail(&answer));
        }
        let list: Value = serde_json::from_slice(&answer.body)
            .map_err(|_| Fail::bad_answer("the forge answer is not JSON"))?;
        let Some(items) = list.as_array() else {
            return Err(Fail::bad_answer("the forge answer is not a list"));
        };
        for item in items {
            let ours = match (forge.binding.kind, project) {
                (ForgeKind::Github, _) => github::pull_is_ours(forge, item),
                (ForgeKind::Gitlab, Some(id)) => gitlab::mr_is_ours(item, id),
                (ForgeKind::Gitlab, None) => false,
            };
            if !ours {
                continue;
            }
            found.push(
                pr_of(forge.binding.kind, item)
                    .ok_or_else(|| Fail::bad_answer("the forge gave no pull request URL"))?,
            );
        }
        let next = match forge.binding.kind {
            ForgeKind::Github => github::next_page(&answer, page),
            ForgeKind::Gitlab => gitlab::next_page(&answer),
        };
        match next {
            None => return Ok(PrLookup::Found(found)),
            Some(_) if page >= MAX_PAGES => return Ok(PrLookup::TooManyPages),
            Some(n) => page = n.max(page + 1),
        }
    }
}

/// Creates the pull request from `head` to `base` and classifies the answer.
pub async fn pr_create_direct(
    forge: &Forge<'_>,
    head: &str,
    base: &str,
    title: &str,
    body: &str,
    timeout: Duration,
) -> PrCreate {
    let sent = match forge.binding.kind {
        ForgeKind::Github => github::pull_create(forge, head, base, title, body, timeout).await,
        ForgeKind::Gitlab => gitlab::mr_create(forge, head, base, title, body, timeout).await,
    };
    let answer = match sent {
        Ok(answer) => answer,
        Err(fail) => return PrCreate::Unknown(fail),
    };
    if answer.status == 201 {
        let made = serde_json::from_slice::<Value>(&answer.body)
            .ok()
            .and_then(|v| pr_of(forge.binding.kind, &v));
        return match made {
            Some(pr) => PrCreate::Applied(pr),
            None => PrCreate::Unknown(Fail::bad_answer("the forge gave no pull request URL")),
        };
    }
    let text = scrub(String::from_utf8_lossy(&answer.body).trim(), &[]);
    if (400..500).contains(&answer.status) && !text.is_empty() {
        return PrCreate::Rejected(http_fail(&answer));
    }
    PrCreate::Unknown(http_fail(&answer))
}

#[cfg(test)]
#[path = "pr_find_tests.rs"]
mod tests;
