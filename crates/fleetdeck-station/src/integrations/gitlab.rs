//! GitLab through `glab api` (REST v4). A merge request is a pull request
//! of the wire types.

use std::path::PathBuf;

use fleetdeck_proto::integrations::{
    CHECK_LOG_LINES, CheckFailure, ChecksFailing, ChecksState, IssueComment, IssueDetail, IssueRef,
    IssueSearch, IssueSummary, PrDetail, PrFile, PrGroups, PrState, PrSummary, ReviewEvent,
    ReviewState, code,
};
use fleetdeck_proto::untrusted::fence_cut;
use serde_json::{Value, json};

use super::exec::Fail;
use super::forge::{self, Forge, MAX_COMMENTS};
use super::util::{
    BODY_CHARS, COMMENT_CHARS, LOG_CHARS, image_urls, parse_time, tail_lines, urlencode,
};

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_owned()
}

fn issue_summary(v: &Value) -> IssueSummary {
    let iid = v["iid"].as_u64().unwrap_or(0);
    IssueSummary {
        r#ref: IssueRef {
            system: "gitlab".into(),
            key: format!("#{iid}"),
            url: s(&v["web_url"]),
            title: s(&v["title"]),
        },
        status: s(&v["state"]),
        assignee: v["assignees"][0]["username"].as_str().map(str::to_owned),
        priority: None,
        labels: v["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l.as_str().map(str::to_owned))
            .collect(),
        updated_at: parse_time(v["updated_at"].as_str().unwrap_or_default()),
    }
}

pub async fn issues_search(f: &Forge<'_>, q: &IssueSearch) -> Result<Vec<IssueSummary>, Fail> {
    let mut query = format!("per_page={}&order_by=updated_at&sort=desc", q.limit());
    if q.mine {
        query.push_str("&scope=assigned_to_me");
    } else {
        query.push_str("&scope=all");
    }
    match q.status.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None => {}
        Some("open" | "opened") => query.push_str("&state=opened"),
        Some("closed") => query.push_str("&state=closed"),
        Some(_) => {
            return Err(Fail::new(
                code::INVALID_PARAMS,
                "a GitLab issue status is `open` or `closed`",
            ));
        }
    }
    if let Some(text) = &q.query {
        query.push_str(&format!("&search={}", urlencode(text)));
    }
    let list = f
        .json(
            "GET",
            &format!("projects/{}/issues?{query}", f.repo_path()),
            None,
        )
        .await?;
    Ok(list
        .as_array()
        .into_iter()
        .flatten()
        .map(issue_summary)
        .collect())
}

fn notes(source: &str, list: &Value) -> Vec<IssueComment> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter(|n| n["system"] != true)
        .take(MAX_COMMENTS)
        .map(|n| IssueComment {
            author: n["author"]["username"]
                .as_str()
                .unwrap_or("ghost")
                .to_owned(),
            at: parse_time(n["created_at"].as_str().unwrap_or_default()),
            body_markdown: fence_cut(
                source,
                n["body"].as_str().unwrap_or_default(),
                COMMENT_CHARS,
            ),
        })
        .collect()
}

pub async fn issue_get(f: &Forge<'_>, number: u64) -> Result<IssueDetail, Fail> {
    let project = f.repo_path();
    let issue = f
        .json("GET", &format!("projects/{project}/issues/{number}"), None)
        .await?;
    let source = format!("gitlab:{}#{number}", f.binding.repo);
    let list = f
        .json(
            "GET",
            &format!(
                "projects/{project}/issues/{number}/notes?per_page={MAX_COMMENTS}&sort=asc&order_by=created_at"
            ),
            None,
        )
        .await?;
    let related = match f
        .json(
            "GET",
            &format!("projects/{project}/issues/{number}/related_merge_requests"),
            None,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(code = e.code, "related merge requests: {}", e.message);
            Value::Null
        }
    };
    let linked_prs = related
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["web_url"].as_str().map(str::to_owned))
        .collect();
    let body = issue["description"].as_str().unwrap_or_default();
    Ok(IssueDetail {
        summary: issue_summary(&issue),
        body_markdown: fence_cut(&source, body, BODY_CHARS),
        estimate: issue["time_stats"]["human_time_estimate"]
            .as_str()
            .map(str::to_owned),
        comments: notes(&source, &list),
        linked_prs,
        images: image_urls(body),
    })
}

fn mr_summary(v: &Value, viewer: &str) -> PrSummary {
    let head = s(&v["source_branch"]);
    let title = s(&v["title"]);
    let author = v["author"]["username"]
        .as_str()
        .unwrap_or("ghost")
        .to_owned();
    let detailed = v["detailed_merge_status"].as_str().unwrap_or_default();
    PrSummary {
        number: v["iid"].as_u64().unwrap_or(0),
        ticket_key: forge::ticket_of(&title, &head),
        title,
        url: s(&v["web_url"]),
        mine: author == viewer,
        author,
        head_branch: head,
        base_branch: s(&v["target_branch"]),
        draft: v["draft"].as_bool().unwrap_or(false),
        state: match v["state"].as_str().unwrap_or_default() {
            "merged" => PrState::Merged,
            "closed" | "locked" => PrState::Closed,
            _ => PrState::Open,
        },
        review_state: match detailed {
            "not_approved" | "approvals_syncing" => ReviewState::ReviewRequired,
            "requested_changes" => ReviewState::ChangesRequested,
            _ => ReviewState::None,
        },
        checks: match v["head_pipeline"]["status"].as_str().unwrap_or_default() {
            "success" => ChecksState::Passing,
            "failed" | "canceled" => ChecksState::Failing,
            "created"
            | "pending"
            | "running"
            | "preparing"
            | "waiting_for_resource"
            | "scheduled" => ChecksState::Pending,
            _ => ChecksState::None,
        },
        mergeable: match detailed {
            "mergeable" => Some(true),
            "conflict" | "need_rebase" => Some(false),
            _ => None,
        },
        updated_at: parse_time(v["updated_at"].as_str().unwrap_or_default()),
        review_requested_from_me: v["reviewers"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|r| r["username"].as_str() == Some(viewer)),
    }
}

pub async fn prs_list(f: &Forge<'_>) -> Result<PrGroups, Fail> {
    let project = f.repo_path();
    let viewer = f.viewer().await?;
    let mut all = Vec::new();
    for (state, per_page) in [("opened", 50), ("merged", 10), ("closed", 10)] {
        let list = f
            .json(
                "GET",
                &format!(
                    "projects/{project}/merge_requests?state={state}&per_page={per_page}&order_by=updated_at&sort=desc"
                ),
                None,
            )
            .await?;
        all.extend(
            list.as_array()
                .into_iter()
                .flatten()
                .map(|v| mr_summary(v, &viewer)),
        );
    }
    Ok(forge::group(all))
}

/// The added and removed lines of a unified diff.
fn count_lines(diff: &str) -> (u64, u64) {
    let (mut add, mut del) = (0, 0);
    for line in diff.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            add += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            del += 1;
        }
    }
    (add, del)
}

pub async fn pr_get(f: &Forge<'_>, number: u64) -> Result<PrDetail, Fail> {
    let project = f.repo_path();
    let viewer = f.viewer().await?;
    let mr = f
        .json(
            "GET",
            &format!("projects/{project}/merge_requests/{number}"),
            None,
        )
        .await?;
    let changes = f
        .json(
            "GET",
            &format!("projects/{project}/merge_requests/{number}/changes"),
            None,
        )
        .await?;
    let list = f
        .json(
            "GET",
            &format!(
                "projects/{project}/merge_requests/{number}/notes?per_page={MAX_COMMENTS}&sort=asc&order_by=created_at"
            ),
            None,
        )
        .await?;
    let source = format!("gitlab:{}!{number}", f.binding.repo);
    Ok(PrDetail {
        summary: mr_summary(&mr, &viewer),
        body_markdown: fence_cut(
            &source,
            mr["description"].as_str().unwrap_or_default(),
            BODY_CHARS,
        ),
        files: changes["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                let (additions, deletions) = count_lines(c["diff"].as_str().unwrap_or_default());
                PrFile {
                    path: c["new_path"]
                        .as_str()
                        .or(c["old_path"].as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    additions,
                    deletions,
                }
            })
            .collect(),
        comments: notes(&source, &list),
    })
}

/// Failing jobs whose trace the call fetches.
const MAX_LOGS: usize = 10;

pub async fn pr_checks(f: &Forge<'_>, number: u64) -> Result<ChecksFailing, Fail> {
    let project = f.repo_path();
    let pipelines = f
        .json(
            "GET",
            &format!("projects/{project}/merge_requests/{number}/pipelines"),
            None,
        )
        .await?;
    let Some(pipeline) = pipelines[0]["id"].as_u64() else {
        return Ok(ChecksFailing::default());
    };
    let jobs = f
        .json(
            "GET",
            &format!("projects/{project}/pipelines/{pipeline}/jobs?scope[]=failed&per_page=50"),
            None,
        )
        .await?;
    let mut failures = Vec::new();
    for job in jobs.as_array().into_iter().flatten() {
        let name = s(&job["name"]);
        let trace = if failures.len() < MAX_LOGS
            && let Some(id) = job["id"].as_u64()
        {
            match f.text(&format!("projects/{project}/jobs/{id}/trace")).await {
                Ok(t) => tail_lines(&t, CHECK_LOG_LINES),
                Err(e) => format!("(the log is not available: {})", e.message),
            }
        } else {
            String::new()
        };
        let source = format!("gitlab:{}!{number} job {name}", f.binding.repo);
        failures.push(CheckFailure {
            log_tail: if trace.is_empty() {
                String::new()
            } else {
                fence_cut(&source, &trace, LOG_CHARS)
            },
            name,
            url: s(&job["web_url"]),
        });
    }
    Ok(ChecksFailing { failures })
}

pub async fn pr_create(
    f: &Forge<'_>,
    cwd: PathBuf,
    head: &str,
    base: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> Result<String, Fail> {
    // The REST API has no draft flag: the title prefix makes a draft.
    let title = if draft && !title.starts_with("Draft:") {
        format!("Draft: {title}")
    } else {
        title.to_owned()
    };
    let payload = json!({
        "source_branch": head, "target_branch": base, "title": title, "description": body
    });
    let made = f
        .json_in(
            "POST",
            &format!("projects/{}/merge_requests", f.repo_path()),
            Some(&payload),
            Some(cwd),
        )
        .await?;
    made["web_url"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Fail::bad_answer("GitLab gave no merge request URL"))
}

/// The web URL of a merge request or an issue.
fn web_url(f: &Forge<'_>, pr: bool, number: u64) -> String {
    let kind = if pr { "merge_requests" } else { "issues" };
    format!(
        "https://{}/{}/-/{kind}/{number}",
        f.binding.host, f.binding.repo
    )
}

pub async fn comment(f: &Forge<'_>, pr: bool, number: u64, body: &str) -> Result<String, Fail> {
    let kind = if pr { "merge_requests" } else { "issues" };
    let made = f
        .json(
            "POST",
            &format!("projects/{}/{kind}/{number}/notes", f.repo_path()),
            Some(&json!({"body": body})),
        )
        .await?;
    let id = made["id"]
        .as_u64()
        .ok_or_else(|| Fail::bad_answer("GitLab gave no note id"))?;
    Ok(format!("{}#note_{id}", web_url(f, pr, number)))
}

pub async fn review(
    f: &Forge<'_>,
    number: u64,
    event: ReviewEvent,
    body: &str,
) -> Result<String, Fail> {
    match event {
        ReviewEvent::RequestChanges => Err(Fail::unavailable(
            "GitLab has no request-changes verb in its REST API; post a comment instead",
        )),
        ReviewEvent::Comment => comment(f, true, number, body).await,
        ReviewEvent::Approve => {
            f.json(
                "POST",
                &format!("projects/{}/merge_requests/{number}/approve", f.repo_path()),
                Some(&json!({})),
            )
            .await?;
            if body.trim().is_empty() {
                Ok(web_url(f, true, number))
            } else {
                comment(f, true, number, body).await
            }
        }
    }
}

pub async fn transition(f: &Forge<'_>, number: u64, open: bool) -> Result<String, Fail> {
    let event = if open { "reopen" } else { "close" };
    let made = f
        .json(
            "PUT",
            &format!("projects/{}/issues/{number}", f.repo_path()),
            Some(&json!({"state_event": event})),
        )
        .await?;
    Ok(made["web_url"]
        .as_str()
        .map_or_else(|| web_url(f, false, number), str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merge_request_maps_to_the_summary() {
        let v = json!({
            "iid": 4, "title": "Draft: ENG-9 speed", "web_url": "https://gitlab.com/g/p/-/merge_requests/4",
            "source_branch": "ENG-9-speed", "target_branch": "main", "draft": true, "state": "opened",
            "detailed_merge_status": "not_approved", "head_pipeline": {"status": "running"},
            "author": {"username": "ana"}, "reviewers": [{"username": "luis"}],
            "updated_at": "2024-05-01T12:00:00+02:00"
        });
        let pr = mr_summary(&v, "luis");
        assert_eq!(pr.number, 4);
        assert_eq!(pr.state, PrState::Open);
        assert_eq!(pr.review_state, ReviewState::ReviewRequired);
        assert_eq!(pr.checks, ChecksState::Pending);
        assert_eq!(pr.mergeable, None);
        assert!(pr.draft && pr.review_requested_from_me && !pr.mine);
        assert_eq!(pr.ticket_key.as_deref(), Some("ENG-9"));
        assert_eq!(pr.updated_at, 1_714_557_600_000);
    }

    #[test]
    fn merged_and_conflicting_states_map() {
        let merged = mr_summary(
            &json!({"iid": 1, "state": "merged", "author": {"username": "a"}}),
            "b",
        );
        assert_eq!(merged.state, PrState::Merged);
        let conflict = mr_summary(
            &json!({"iid": 2, "state": "opened", "detailed_merge_status": "conflict",
                    "head_pipeline": {"status": "failed"}}),
            "b",
        );
        assert_eq!(
            (conflict.mergeable, conflict.checks),
            (Some(false), ChecksState::Failing)
        );
    }

    #[test]
    fn diff_lines_are_counted_without_the_file_headers() {
        let diff = "--- a/x\n+++ b/x\n@@ -1,2 +1,3 @@\n keep\n-old\n+new\n+more\n";
        assert_eq!(count_lines(diff), (2, 1));
    }

    #[test]
    fn system_notes_are_dropped_and_bodies_are_fenced() {
        let list = json!([
            {"system": true, "body": "assigned to @x", "author": {"username": "x"}},
            {"system": false, "body": "do it", "author": {"username": "y"}, "created_at": "2024-05-01T10:00:00Z"}
        ]);
        let got = notes("gitlab:g/p#1", &list);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].author, "y");
        assert!(got[0].body_markdown.contains("do it"));
    }

    #[test]
    fn an_issue_maps_to_the_summary() {
        let v = json!({"iid": 3, "title": "T", "web_url": "https://gitlab.com/g/p/-/issues/3",
            "state": "opened", "assignees": [{"username": "ana"}], "labels": ["a", "b"],
            "updated_at": "2024-05-01T10:00:00Z"});
        let got = issue_summary(&v);
        assert_eq!(
            (got.r#ref.key.as_str(), got.assignee.as_deref()),
            ("#3", Some("ana"))
        );
        assert_eq!(got.labels, ["a", "b"]);
    }
}
