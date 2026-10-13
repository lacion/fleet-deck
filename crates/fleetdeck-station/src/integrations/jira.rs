//! Jira Cloud REST v3. Issue bodies and comments are Atlassian Document
//! Format; the station converts them to Markdown ([`super::adf`]).

use fleetdeck_proto::accounts::Container;
use fleetdeck_proto::integrations::{
    IssueComment, IssueDetail, IssueRef, IssueSearch, IssueSummary, code,
};
use fleetdeck_proto::untrusted::fence_cut;
use serde_json::{Value, json};

use super::adf::{self, MediaUrls};
use super::exec::Fail;
use super::tracker::{Tracker, points};
use super::util::{BODY_CHARS, COMMENT_CHARS, parse_time, pr_links};

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_owned()
}

/// `value` as a JQL string literal.
pub fn jql_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' | '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The JQL of a search: the project, the filters and the order.
pub fn build_jql(project: &str, q: &IssueSearch) -> String {
    let mut parts = vec![format!("project = {}", jql_string(project))];
    if q.mine {
        parts.push("assignee = currentUser()".to_owned());
    }
    if let Some(status) = &q.status {
        parts.push(format!("status = {}", jql_string(status)));
    }
    if let Some(text) = &q.query {
        parts.push(format!("text ~ {}", jql_string(text)));
    }
    format!("{} ORDER BY updated DESC", parts.join(" AND "))
}

fn summary(site: &str, v: &Value) -> IssueSummary {
    let key = s(&v["key"]);
    let f = &v["fields"];
    IssueSummary {
        r#ref: IssueRef {
            system: "jira".into(),
            url: format!("{site}/browse/{key}"),
            key,
            title: s(&f["summary"]),
        },
        status: s(&f["status"]["name"]),
        assignee: f["assignee"]["displayName"].as_str().map(str::to_owned),
        priority: f["priority"]["name"].as_str().map(str::to_owned),
        labels: f["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l.as_str().map(str::to_owned))
            .collect(),
        updated_at: parse_time(f["updated"].as_str().unwrap_or_default()),
    }
}

pub async fn search(t: &Tracker<'_>, q: &IssueSearch) -> Result<Vec<IssueSummary>, Fail> {
    let body = json!({
        "jql": build_jql(t.key, q),
        "maxResults": q.limit(),
        "fields": ["summary", "status", "assignee", "priority", "labels", "updated"],
    });
    let answer = t
        .json(
            "POST",
            format!("{}/rest/api/3/search/jql", t.base()),
            Some(&body),
        )
        .await?;
    Ok(answer["issues"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| summary(t.site(), v))
        .collect())
}

fn media_urls(site: &str, attachments: &Value) -> MediaUrls {
    attachments
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let name = a["filename"].as_str()?;
            let id = a["id"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| a["id"].as_u64().map(|n| n.to_string()))?;
            Some((
                name.to_owned(),
                format!("{site}/rest/api/3/attachment/content/{id}"),
            ))
        })
        .collect()
}

/// A body that is ADF (an object) or plain text (an older issue).
fn markdown(body: &Value, media: &MediaUrls) -> String {
    match body {
        Value::Object(_) => adf::to_markdown(body, media),
        Value::String(text) => text.clone(),
        _ => String::new(),
    }
}

pub async fn get(t: &Tracker<'_>, key: &str) -> Result<IssueDetail, Fail> {
    let site = t.site();
    let api = t.base();
    let issue = t
        .json(
            "GET",
            format!(
                "{api}/rest/api/3/issue/{key}?fields=summary,description,status,assignee,priority,labels,updated,attachment,timetracking,customfield_10016"
            ),
            None,
        )
        .await?;
    let list = t
        .json(
            "GET",
            format!("{api}/rest/api/3/issue/{key}/comment?maxResults=30&orderBy=created"),
            None,
        )
        .await?;
    let remote = match t
        .json(
            "GET",
            format!("{api}/rest/api/3/issue/{key}/remotelink"),
            None,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(code = e.code, "jira remote links: {}", e.message);
            Value::Null
        }
    };
    let fields = &issue["fields"];
    let media = media_urls(&api, &fields["attachment"]);
    let source = format!("jira:{key}");
    let body = markdown(&fields["description"], &media);
    let mut images = adf::image_urls(&fields["description"], &media);
    let mut linked: Vec<String> = remote
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["object"]["url"].as_str())
        .flat_map(pr_links)
        .collect();
    let mut comments = Vec::new();
    for c in list["comments"].as_array().into_iter().flatten() {
        let text = markdown(&c["body"], &media);
        for url in adf::image_urls(&c["body"], &media) {
            if !images.contains(&url) {
                images.push(url);
            }
        }
        linked.extend(pr_links(&text));
        comments.push(IssueComment {
            author: c["author"]["displayName"]
                .as_str()
                .unwrap_or("unknown")
                .to_owned(),
            at: parse_time(c["created"].as_str().unwrap_or_default()),
            body_markdown: fence_cut(&source, &text, COMMENT_CHARS),
        });
    }
    linked.extend(pr_links(&body));
    let mut linked_prs: Vec<String> = Vec::new();
    for url in linked {
        if !linked_prs.contains(&url) {
            linked_prs.push(url);
        }
    }
    let estimate = fields["timetracking"]["originalEstimate"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| fields["customfield_10016"].as_f64().map(points));
    Ok(IssueDetail {
        summary: summary(site, &issue),
        body_markdown: fence_cut(&source, &body, BODY_CHARS),
        estimate,
        comments,
        linked_prs,
        images,
    })
}

pub async fn comment(t: &Tracker<'_>, key: &str, body: &str) -> Result<String, Fail> {
    let site = t.site();
    let made = t
        .json(
            "POST",
            format!("{}/rest/api/3/issue/{key}/comment", t.base()),
            Some(&json!({"body": adf::from_text(body)})),
        )
        .await?;
    let id = s(&made["id"]);
    Ok(if id.is_empty() {
        format!("{site}/browse/{key}")
    } else {
        format!("{site}/browse/{key}?focusedCommentId={id}")
    })
}

/// The id of the transition that leads to the status `to` (compared without
/// letter case, by the target status name, then by the transition name).
fn pick_transition(list: &Value, to: &str) -> Result<String, Fail> {
    let all = list["transitions"].as_array().cloned().unwrap_or_default();
    let found = all
        .iter()
        .find(|t| s(&t["to"]["name"]).eq_ignore_ascii_case(to))
        .or_else(|| all.iter().find(|t| s(&t["name"]).eq_ignore_ascii_case(to)));
    match found {
        Some(t) => Ok(s(&t["id"])),
        None => {
            let names: Vec<String> = all.iter().map(|t| s(&t["to"]["name"])).collect();
            Err(Fail::new(
                code::INVALID_PARAMS,
                format!(
                    "no transition to `{to}`; the issue can move to: {}",
                    names.join(", ")
                ),
            ))
        }
    }
}

pub async fn transition(t: &Tracker<'_>, key: &str, to: &str) -> Result<String, Fail> {
    let site = t.site();
    let api = t.base();
    let list = t
        .json(
            "GET",
            format!("{api}/rest/api/3/issue/{key}/transitions"),
            None,
        )
        .await?;
    let id = pick_transition(&list, to)?;
    t.json(
        "POST",
        format!("{api}/rest/api/3/issue/{key}/transitions"),
        Some(&json!({"transition": {"id": id}})),
    )
    .await?;
    Ok(format!("{site}/browse/{key}"))
}

pub async fn whoami(t: &Tracker<'_>) -> Result<String, Fail> {
    let me = t
        .json("GET", format!("{}/rest/api/3/myself", t.base()), None)
        .await?;
    me["displayName"]
        .as_str()
        .or(me["emailAddress"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| Fail::bad_answer("Jira gave no user name"))
}

/// The projects of the account (up to 100).
pub async fn containers(t: &Tracker<'_>) -> Result<Vec<Container>, Fail> {
    let answer = t
        .json(
            "GET",
            format!("{}/rest/api/3/project/search?maxResults=100", t.base()),
            None,
        )
        .await?;
    Ok(answer["values"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(Container {
                key: p["key"].as_str()?.to_owned(),
                name: p["name"].as_str().unwrap_or_default().to_owned(),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use fleetdeck_proto::integrations::{Binding, TrackerBinding, TrackerKind};

    use super::*;

    fn search(query: Option<&str>, mine: bool, status: Option<&str>) -> IssueSearch {
        IssueSearch {
            binding: Binding::Tracker(TrackerBinding {
                tracker: "acme".into(),
                kind: TrackerKind::Jira,
                key: "PROJ".into(),
            }),
            query: query.map(str::to_owned),
            mine,
            status: status.map(str::to_owned),
            limit: None,
        }
    }

    #[test]
    fn the_jql_quotes_every_user_value() {
        assert_eq!(
            build_jql("PROJ", &search(None, false, None)),
            "project = \"PROJ\" ORDER BY updated DESC"
        );
        assert_eq!(
            build_jql(
                "PROJ",
                &search(Some("login \"bug\" \\ x"), true, Some("In Progress"))
            ),
            "project = \"PROJ\" AND assignee = currentUser() AND status = \"In Progress\" AND text ~ \"login \\\"bug\\\" \\\\ x\" ORDER BY updated DESC"
        );
    }

    #[test]
    fn a_hostile_query_cannot_end_its_string() {
        let jql = build_jql("P", &search(Some("x\" OR project = \"SECRET"), false, None));
        assert!(
            jql.contains("text ~ \"x\\\" OR project = \\\"SECRET\""),
            "{jql}"
        );
        assert_eq!(
            jql.matches("project =").count(),
            2,
            "the quoted text is one string"
        );
        assert_eq!(jql_string("a\nb"), "\"a b\"");
    }

    #[test]
    fn an_issue_maps_to_the_summary_with_a_browse_url() {
        let v = json!({"key": "PROJ-7", "fields": {
            "summary": "Login fails", "status": {"name": "In Review"},
            "assignee": {"displayName": "Ana"}, "priority": {"name": "High"},
            "labels": ["ui"], "updated": "2024-05-01T12:00:00.000+0200"}});
        let got = summary("https://acme.atlassian.net", &v);
        assert_eq!(got.r#ref.url, "https://acme.atlassian.net/browse/PROJ-7");
        assert_eq!(got.status, "In Review");
        assert_eq!(got.priority.as_deref(), Some("High"));
        assert_eq!(got.updated_at, 1_714_557_600_000);
    }

    #[test]
    fn a_transition_is_found_by_status_then_by_name() {
        let list = json!({"transitions": [
            {"id": "11", "name": "Start", "to": {"name": "In Progress"}},
            {"id": "21", "name": "Send to review", "to": {"name": "In Review"}}
        ]});
        assert_eq!(pick_transition(&list, "in review").unwrap(), "21");
        assert_eq!(pick_transition(&list, "Start").unwrap(), "11");
        let err = pick_transition(&list, "Done").unwrap_err();
        assert_eq!(err.code, code::INVALID_PARAMS);
        assert!(err.message.contains("In Progress, In Review"));
    }

    #[test]
    fn attachments_become_media_urls_by_file_name() {
        let att = json!([{"id": "10", "filename": "shot.png"}, {"id": 11, "filename": "b.png"}]);
        let urls = media_urls("https://a.test", &att);
        assert_eq!(
            urls["shot.png"],
            "https://a.test/rest/api/3/attachment/content/10"
        );
        assert_eq!(
            urls["b.png"],
            "https://a.test/rest/api/3/attachment/content/11"
        );
    }

    #[test]
    fn a_plain_text_description_passes_and_adf_converts() {
        let media = MediaUrls::new();
        assert_eq!(markdown(&json!("old text"), &media), "old text");
        assert_eq!(markdown(&Value::Null, &media), "");
        let doc = json!({"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "hi"}]}]});
        assert_eq!(markdown(&doc, &media), "hi");
    }
}
