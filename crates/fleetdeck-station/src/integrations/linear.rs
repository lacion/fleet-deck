//! Linear GraphQL (`https://api.linear.app/graphql`). Every user value goes
//! in a GraphQL variable, never into the query text.

use fleetdeck_proto::accounts::Container;
use fleetdeck_proto::integrations::{
    IssueComment, IssueDetail, IssueRef, IssueSearch, IssueSummary, code,
};
use fleetdeck_proto::untrusted::fence_cut;
use serde_json::{Value, json};

use super::exec::Fail;
use super::tracker::{Tracker, points};
use super::util::{BODY_CHARS, COMMENT_CHARS, image_urls, parse_time, pr_links};

/// The endpoint of the Linear API.
pub const ENDPOINT: &str = "https://api.linear.app/graphql";

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_owned()
}

const ISSUE_FIELDS: &str = "identifier title url updatedAt priorityLabel \
    state{name} assignee{displayName name} labels{nodes{name}}";

async fn call(t: &Tracker<'_>, query: &str, variables: &Value) -> Result<Value, Fail> {
    let answer = t
        .json(
            "POST",
            ENDPOINT.to_owned(),
            Some(&json!({"query": query, "variables": variables})),
        )
        .await?;
    if let Some(e) = answer["errors"].as_array().and_then(|e| e.first()) {
        return Err(Fail::upstream(
            e["message"].as_str().unwrap_or("the Linear call failed"),
        ));
    }
    Ok(answer["data"].clone())
}

/// The `IssueFilter` of a search.
pub fn build_filter(team: &str, q: &IssueSearch) -> Value {
    let mut filter = json!({"team": {"key": {"eq": team}}});
    if q.mine {
        filter["assignee"] = json!({"isMe": {"eq": true}});
    }
    if let Some(status) = &q.status {
        filter["state"] = json!({"name": {"eqIgnoreCase": status}});
    }
    if let Some(text) = &q.query {
        filter["or"] = json!([
            {"title": {"containsIgnoreCase": text}},
            {"description": {"containsIgnoreCase": text}},
        ]);
    }
    filter
}

fn summary(v: &Value) -> IssueSummary {
    IssueSummary {
        r#ref: IssueRef {
            system: "linear".into(),
            key: s(&v["identifier"]),
            url: s(&v["url"]),
            title: s(&v["title"]),
        },
        status: s(&v["state"]["name"]),
        assignee: v["assignee"]["displayName"]
            .as_str()
            .or(v["assignee"]["name"].as_str())
            .map(str::to_owned),
        priority: v["priorityLabel"]
            .as_str()
            .filter(|p| *p != "No priority")
            .map(str::to_owned),
        labels: v["labels"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["name"].as_str().map(str::to_owned))
            .collect(),
        updated_at: parse_time(v["updatedAt"].as_str().unwrap_or_default()),
    }
}

pub async fn search(t: &Tracker<'_>, q: &IssueSearch) -> Result<Vec<IssueSummary>, Fail> {
    let query = format!(
        "query($filter: IssueFilter, $first: Int) {{ issues(filter: $filter, first: $first, orderBy: updatedAt) {{ nodes {{ {ISSUE_FIELDS} }} }} }}"
    );
    let data = call(
        t,
        &query,
        &json!({"filter": build_filter(t.key, q), "first": q.limit()}),
    )
    .await?;
    Ok(data["issues"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(summary)
        .collect())
}

pub async fn get(t: &Tracker<'_>, key: &str) -> Result<IssueDetail, Fail> {
    let query = format!(
        "query($id: String!) {{ issue(id: $id) {{ {ISSUE_FIELDS} description estimate \
         comments(first: 30) {{ nodes {{ body createdAt user{{displayName name}} }} }} \
         attachments(first: 50) {{ nodes {{ url }} }} }} }}"
    );
    let data = call(t, &query, &json!({"id": key})).await?;
    let issue = &data["issue"];
    if issue.is_null() {
        return Err(Fail::upstream("the issue was not found"));
    }
    let source = format!("linear:{key}");
    let body = issue["description"].as_str().unwrap_or_default();
    let comments: Vec<IssueComment> = issue["comments"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| IssueComment {
            author: c["user"]["displayName"]
                .as_str()
                .or(c["user"]["name"].as_str())
                .unwrap_or("unknown")
                .to_owned(),
            at: parse_time(c["createdAt"].as_str().unwrap_or_default()),
            body_markdown: fence_cut(
                &source,
                c["body"].as_str().unwrap_or_default(),
                COMMENT_CHARS,
            ),
        })
        .collect();
    let mut linked_prs: Vec<String> = Vec::new();
    let from_attachments = issue["attachments"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["url"].as_str())
        .flat_map(pr_links);
    let from_text = pr_links(body).into_iter();
    for url in from_attachments.chain(from_text) {
        if !linked_prs.contains(&url) {
            linked_prs.push(url);
        }
    }
    Ok(IssueDetail {
        summary: summary(issue),
        body_markdown: fence_cut(&source, body, BODY_CHARS),
        estimate: issue["estimate"].as_f64().map(points),
        comments,
        linked_prs,
        images: image_urls(body),
    })
}

/// The Linear id (a UUID) of an issue, from its identifier.
async fn issue_id(t: &Tracker<'_>, key: &str) -> Result<String, Fail> {
    let data = call(
        t,
        "query($id: String!) { issue(id: $id) { id } }",
        &json!({"id": key}),
    )
    .await?;
    data["issue"]["id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Fail::upstream("the issue was not found"))
}

pub async fn comment(t: &Tracker<'_>, key: &str, body: &str) -> Result<String, Fail> {
    let id = issue_id(t, key).await?;
    let data = call(
        t,
        "mutation($issueId: String!, $body: String!) { commentCreate(input: {issueId: $issueId, body: $body}) { success comment { url } } }",
        &json!({"issueId": id, "body": body}),
    )
    .await?;
    if data["commentCreate"]["success"] != true {
        return Err(Fail::upstream("Linear did not create the comment"));
    }
    data["commentCreate"]["comment"]["url"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Fail::bad_answer("Linear gave no comment URL"))
}

pub async fn transition(t: &Tracker<'_>, key: &str, to: &str) -> Result<String, Fail> {
    let data = call(
        t,
        "query($id: String!) { issue(id: $id) { id url team { states { nodes { id name } } } } }",
        &json!({"id": key}),
    )
    .await?;
    let issue = &data["issue"];
    if issue.is_null() {
        return Err(Fail::upstream("the issue was not found"));
    }
    let states = issue["team"]["states"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let Some(state) = states
        .iter()
        .find(|st| s(&st["name"]).eq_ignore_ascii_case(to))
    else {
        let names: Vec<String> = states.iter().map(|st| s(&st["name"])).collect();
        return Err(Fail::new(
            code::INVALID_PARAMS,
            format!("no status `{to}`; the team has: {}", names.join(", ")),
        ));
    };
    let done = call(
        t,
        "mutation($id: String!, $state: String!) { issueUpdate(id: $id, input: {stateId: $state}) { success issue { url } } }",
        &json!({"id": s(&issue["id"]), "state": s(&state["id"])}),
    )
    .await?;
    if done["issueUpdate"]["success"] != true {
        return Err(Fail::upstream("Linear did not change the status"));
    }
    Ok(done["issueUpdate"]["issue"]["url"]
        .as_str()
        .or(issue["url"].as_str())
        .unwrap_or_default()
        .to_owned())
}

/// The user name and the workspace key of the credential.
pub async fn identity(t: &Tracker<'_>) -> Result<(String, Option<String>), Fail> {
    let data = call(
        t,
        "query { viewer { name email organization { urlKey } } }",
        &json!({}),
    )
    .await?;
    let viewer = &data["viewer"];
    let user = viewer["name"]
        .as_str()
        .or(viewer["email"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| Fail::bad_answer("Linear gave no user name"))?;
    let key = viewer["organization"]["urlKey"]
        .as_str()
        .filter(|k| {
            (1..=64).contains(&k.len())
                && k.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
        .map(str::to_owned);
    Ok((user, key))
}

/// The teams of the account (up to 100).
pub async fn containers(t: &Tracker<'_>) -> Result<Vec<Container>, Fail> {
    let data = call(
        t,
        "query { teams(first: 100) { nodes { key name } } }",
        &json!({}),
    )
    .await?;
    Ok(data["teams"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            Some(Container {
                key: n["key"].as_str()?.to_owned(),
                name: n["name"].as_str().unwrap_or_default().to_owned(),
            })
        })
        .collect())
}

pub async fn whoami(t: &Tracker<'_>) -> Result<String, Fail> {
    let data = call(t, "query { viewer { name email } }", &json!({})).await?;
    data["viewer"]["name"]
        .as_str()
        .or(data["viewer"]["email"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| Fail::bad_answer("Linear gave no user name"))
}

#[cfg(test)]
mod tests {
    use fleetdeck_proto::integrations::{Binding, TrackerBinding, TrackerKind};

    use super::*;

    fn search(query: Option<&str>, mine: bool, status: Option<&str>) -> IssueSearch {
        IssueSearch {
            binding: Binding::Tracker(TrackerBinding {
                tracker: "lin".into(),
                kind: TrackerKind::Linear,
                key: "ENG".into(),
            }),
            query: query.map(str::to_owned),
            mine,
            status: status.map(str::to_owned),
            limit: None,
        }
    }

    #[test]
    fn the_filter_scopes_to_the_team_and_keeps_values_as_data() {
        assert_eq!(
            build_filter("ENG", &search(None, false, None)),
            json!({"team": {"key": {"eq": "ENG"}}})
        );
        let f = build_filter(
            "ENG",
            &search(Some("x\" } } hack"), true, Some("In Progress")),
        );
        assert_eq!(f["assignee"], json!({"isMe": {"eq": true}}));
        assert_eq!(f["state"], json!({"name": {"eqIgnoreCase": "In Progress"}}));
        assert_eq!(f["or"][0]["title"]["containsIgnoreCase"], "x\" } } hack");
    }

    #[test]
    fn an_issue_maps_to_the_summary() {
        let v = json!({"identifier": "ENG-42", "title": "Slow", "url": "https://linear.app/a/issue/ENG-42",
            "updatedAt": "2024-05-01T10:00:00.000Z", "priorityLabel": "Urgent",
            "state": {"name": "Todo"}, "assignee": {"displayName": "ana", "name": "Ana B"},
            "labels": {"nodes": [{"name": "perf"}]}});
        let got = summary(&v);
        assert_eq!(got.r#ref.key, "ENG-42");
        assert_eq!(got.priority.as_deref(), Some("Urgent"));
        assert_eq!(got.assignee.as_deref(), Some("ana"));
        assert_eq!(got.updated_at, 1_714_557_600_000);
        let none = summary(&json!({"priorityLabel": "No priority"}));
        assert_eq!(none.priority, None);
    }
}
