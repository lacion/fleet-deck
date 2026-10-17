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

/// The open pull requests from `head` (a branch name) to `base`: an exact
/// lookup, at most two answers, no cache.
///
/// # Errors
/// A [`Fail`] when the forge cannot be reached, answers with a status other
/// than 200, or answers something that is not a list. The caller then does
/// not know the result.
pub async fn pr_find(
    forge: &Forge<'_>,
    head: &str,
    base: &str,
    timeout: Duration,
) -> Result<Vec<PrRef>, Fail> {
    let answer = match forge.binding.kind {
        ForgeKind::Github => github::pulls_find(forge, head, base, timeout).await?,
        ForgeKind::Gitlab => gitlab::mrs_find(forge, head, base, timeout).await?,
    };
    if answer.status != 200 {
        return Err(http_fail(&answer));
    }
    let list: Value = serde_json::from_slice(&answer.body)
        .map_err(|_| Fail::bad_answer("the forge answer is not JSON"))?;
    let Some(items) = list.as_array() else {
        return Err(Fail::bad_answer("the forge answer is not a list"));
    };
    items
        .iter()
        .map(|item| {
            pr_of(forge.binding.kind, item)
                .ok_or_else(|| Fail::bad_answer("the forge gave no pull request URL"))
        })
        .collect()
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
