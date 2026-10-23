//! The activity, extras and station resources of a wall display over
//! loopback HTTPS (R13 P2b, §5A item 1, §5B item 1, §5D items 8 to 10 and
//! 17, R9): fixed labels, no mail, no plan, the scope before the page, and
//! zero extras. Sentinel tests: a secret in a tool input, an approval, a
//! plan, a mail, a file and an end reason never reaches a display.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::collections::BTreeSet;

use common::Fixture;
use common::board::BoardClient;
use displays_support::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_deck::store::{NewMail, NewOutboxItem};
use fleetdeck_proto::event::{
    Event, EventBody, FileChange, ProviderRefs, RequestKind, ResolvedBy, ResponseChannel, ToolKind,
    ToolStatus,
};
use fleetdeck_proto::link::{Scope, StationInfo};
use fleetdeck_proto::orchestration::MailDelivery;
use fleetdeck_proto::resources::{DiskSpace, MemoryUse, SessionLoad, StationResources};
use fleetdeck_proto::session::RepoInfo;
use fleetdeck_proto::{EventEnvelope, IncarnationId, SessionId, SessionSummary, StationId};
use serde_json::{Value, json};
use share_support::*;

fn env(seq: u64, session: &str, at: i64, event: Event) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(event),
    }
}

fn message(seq: u64, session: &str, at: i64, text: &str) -> EventEnvelope {
    env(
        seq,
        session,
        at,
        Event::MessageCompleted {
            turn_id: "t".into(),
            parent_node_id: None,
            text: text.to_owned(),
        },
    )
}

fn summary(id: &str, root: &str) -> SessionSummary {
    let mut s = card(id);
    s.title = Some(format!("Title of {id}"));
    s.repo = Some(RepoInfo {
        root: root.to_owned(),
        origin: Some(format!("github.com/{SECRET}/{id}")),
    });
    s
}

struct Rig {
    f: Fixture,
    c: BoardClient,
    h: Headers,
    st: String,
    display: String,
    cookie: String,
}

/// `s1` (repo alpha) and `s2` (repo beta) on the ceiling station, `o1` on a
/// station outside the ceiling with newer rows.
async fn rig() -> Rig {
    let (f, c, h, st) = setup_deck().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![summary("s1", "/srv/a/alpha"), summary("s2", "/srv/b/beta")],
        )
        .await
        .unwrap();
    f.store
        .apply_events(
            f.station.clone(),
            f.inc.clone(),
            vec![
                env(
                    1,
                    "s1",
                    1_000,
                    Event::ToolStarted {
                        node_id: "n1".into(),
                        parent_node_id: None,
                        tool_kind: ToolKind::Shell,
                        title: "Bash".to_owned(),
                        input_summary: format!("curl {SECRET}"),
                        files: vec![format!("/{SECRET}/f")],
                        provider_refs: ProviderRefs::new(),
                    },
                ),
                env(
                    2,
                    "s1",
                    2_000,
                    Event::ToolCompleted {
                        node_id: "n1".into(),
                        status: ToolStatus::Ok,
                        files: Vec::new(),
                        exit_code: Some(0),
                        provider_refs: ProviderRefs::new(),
                    },
                ),
                message(3, "s1", 3_000, "Fixed it.\nSecond line."),
                env(
                    4,
                    "s1",
                    4_000,
                    Event::ApprovalRequested {
                        request_id: "q1".into(),
                        node_id: Some("n1".into()),
                        request_kind: RequestKind::Command,
                        title: format!("Run {SECRET}"),
                        details: format!("rm -rf {SECRET}"),
                        input: Some(json!({ "command": SECRET })),
                        options: Vec::new(),
                        response: ResponseChannel::RpcReply,
                        provider_refs: ProviderRefs::new(),
                    },
                ),
                env(
                    5,
                    "s1",
                    5_000,
                    Event::ApprovalResolved {
                        request_id: "q1".into(),
                        option_id: format!("opt-{SECRET}"),
                        by: ResolvedBy::Board,
                        answered_by: None,
                    },
                ),
                env(
                    6,
                    "s1",
                    6_000,
                    Event::PlanProposed {
                        plan_id: "p1".into(),
                        turn_id: None,
                        markdown: format!("plan {SECRET}"),
                    },
                ),
                env(
                    7,
                    "s1",
                    7_000,
                    Event::FileChanged {
                        path: format!("/{SECRET}/touched.rs"),
                        change: FileChange::Modified,
                        node_id: None,
                    },
                ),
                env(
                    8,
                    "s1",
                    8_000,
                    Event::SessionEnded {
                        reason: format!("crashed {SECRET}"),
                        exit_code: Some(1),
                    },
                ),
                message(9, "s2", 9_000, "Beta text"),
            ],
        )
        .await
        .unwrap();
    f.store
        .send_mail(NewMail {
            mail_id: "m1".to_owned(),
            sender: "owner".to_owned(),
            session_id: SessionId::from("s1".to_owned()),
            delivery: MailDelivery::FollowUp,
            text: format!("mail {SECRET}"),
            item: NewOutboxItem {
                station_id: f.station.clone(),
                origin: "mail".to_owned(),
                payload: json!({ "kind": "session" }),
                expires_at: i64::MAX,
            },
        })
        .await
        .unwrap();
    let (display, cookie) = display_login(&c, "Wall", &[&st]).await;
    Rig {
        f,
        c,
        h,
        st,
        display,
        cookie,
    }
}

/// A station outside the ceiling with newer rows (`o1`).
async fn outside_station(r: &Rig) -> StationId {
    let rec =
        r.c.store
            .pin_station(NewStation {
                key: Identity::generate().unwrap().public_key(),
                name: "other".to_owned(),
                scopes: BTreeSet::from([Scope::Read]),
                dial_url: None,
            })
            .await
            .unwrap();
    r.c.store
        .apply_snapshot(
            rec.station_id.clone(),
            IncarnationId::from("i2"),
            0,
            vec![summary("o1", "/srv/a/alpha")],
        )
        .await
        .unwrap();
    r.c.store
        .apply_events(
            rec.station_id.clone(),
            IncarnationId::from("i2"),
            vec![message(1, "o1", 50_000, "Outside text")],
        )
        .await
        .unwrap();
    rec.station_id
}

impl Rig {
    async fn activity(&self, query: &str) -> common::board::Http {
        get_with(&self.c, &format!("/api/activity{query}"), &self.cookie).await
    }
}

fn texts(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["text"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_display_gets_fixed_labels_and_no_mail_plan_or_input() {
    let r = rig().await;
    outside_station(&r).await;
    let res = r.activity("").await;
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(!res.text.contains(SECRET), "{}", res.text);
    let list = texts(&res.body);
    for want in [
        "Bash (ok)",
        "Fixed it.",
        "Needs you: Approve a command",
        "Answered",
        "Session ended",
        "Beta text",
    ] {
        assert!(list.iter().any(|t| t == want), "{want} in {list:?}");
    }
    // No mail, no plan, nothing of the station outside the ceiling.
    assert!(
        !list
            .iter()
            .any(|t| t.contains("Mail") || t.contains("plan"))
    );
    assert!(!list.iter().any(|t| t.contains("Outside")));
    assert!(
        res.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["kind"] != "mail" && i["station_id"] == r.st.as_str())
    );
    // The repository is the displayed basename, never the origin.
    let s1 = res.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["session_id"] == "s1")
        .unwrap();
    // The request kind of a "Needs you" row is there; the other rows have none.
    for i in res.body["items"].as_array().unwrap() {
        if i["text"] == "Needs you: Approve a command" {
            assert_eq!(i["request_kind"], "command", "{i}");
        } else {
            assert!(i.get("request_kind").is_none(), "{i}");
        }
    }
    assert_eq!(s1["repo"], "alpha");
    assert_eq!(s1["session_title"], "Title of s1");
    // The owner still sees the mail row and the plan.
    let owner_view = get(&r.c, &r.h, "/api/activity").await;
    assert!(owner_view.text.contains(SECRET));
}

#[tokio::test]
async fn the_scope_and_the_queries_apply_before_the_page() {
    let r = rig().await;
    outside_station(&r).await;
    // The newest row of the deck is outside the ceiling: a page of 1 still
    // holds an allowed row.
    let one = r.activity("?limit=1").await;
    assert_eq!(one.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(one.body["items"][0]["text"], "Beta text");
    // `repo=` matches the basename only.
    let alpha = r.activity("?repo=alpha").await;
    assert!(
        alpha.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["session_id"] == "s1")
    );
    assert!(!texts(&alpha.body).is_empty());
    for miss in [
        "/srv/a/alpha".to_owned(),
        format!("github.com/{SECRET}/s1"),
        "ALPHA".to_owned(),
    ] {
        let q = format!("?repo={}", miss.replace('/', "%2F"));
        let res = r.activity(&q).await;
        assert_eq!(res.status, 200, "{}", res.body);
        assert_eq!(res.body["items"], json!([]), "{miss}");
    }
    // A session outside the scope gives an empty answer, as a missing one.
    assert_eq!(r.activity("?session=o1").await.body["items"], json!([]));
    assert_eq!(r.activity("?session=nope").await.body["items"], json!([]));
    assert!(
        !r.activity("?session=s2").await.body["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // `since` and the limit rules.
    assert_eq!(
        r.activity("?since=8500").await.body["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for bad in ["limit=0", "limit=501", "limit=x"] {
        let res = r.activity(&format!("?{bad}")).await;
        assert_eq!(res.status, 400, "{bad}: {}", res.body);
    }
    // The directive filter narrows the activity: `beta` goes.
    let d = post(
        &r.c,
        &r.h,
        &format!("/api/displays/{}/directive", r.display),
        json!({ "mode": "mission", "filter": { "stations": [r.st], "repo": "alpha" } }),
    )
    .await;
    assert_eq!(d.status, 200, "{}", d.body);
    let res = r.activity("").await;
    assert!(!texts(&res.body).iter().any(|t| t.contains("Beta")));
    assert!(!texts(&res.body).is_empty());
    // A `repo=` query cannot widen the directive filter.
    let res = r.activity("?repo=beta").await;
    assert_eq!(res.body["items"], json!([]));
}

#[tokio::test]
async fn a_revoked_display_gets_401_from_the_activity_and_the_extras() {
    let r = rig().await;
    let res = post(
        &r.c,
        &r.h,
        &format!("/api/displays/{}/revoke", r.display),
        json!({}),
    )
    .await;
    assert_eq!(res.status, 204, "{}", res.body);
    expect(&r.activity("").await, 401, "login_required");
    expect(
        &get_with(&r.c, "/api/fleet/extras", &r.cookie).await,
        401,
        "login_required",
    );
}

#[tokio::test]
async fn the_extras_of_a_display_are_zero_rows_for_the_visible_cards() {
    let r = rig().await;
    outside_station(&r).await;
    // The owner sees the pending mail.
    let owner_view = get(&r.c, &r.h, "/api/fleet/extras").await;
    assert!(
        owner_view.body["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["session_id"] == "s1" && s["mail_pending"] == 1)
    );
    // A display gets one zero row for each visible card and nothing else.
    let res = get_with(&r.c, "/api/fleet/extras", &r.cookie).await;
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(!res.text.contains(SECRET) && !res.text.contains("touched.rs"));
    let rows = res.body["sessions"].as_array().unwrap();
    let ids: BTreeSet<&str> = rows
        .iter()
        .map(|s| s["session_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, BTreeSet::from(["s1", "s2"]));
    assert!(
        rows.iter()
            .all(|s| s["mail_pending"] == 0 && s["files"] == json!([]))
    );
}

#[tokio::test]
async fn a_station_shows_reduced_resources_for_the_visible_sessions_only() {
    let r = rig().await;
    let info = StationInfo {
        name: "fake".to_owned(),
        incarnation: "fake-station-db".to_owned().into(),
        head: 0,
        live_sessions: 2,
        version: None,
        max_sessions: Some(4),
        allow_shell: None,
        allow_setup_cmd: None,
        providers: Vec::new(),
        assist_tokens_today: None,
        assist_cap: None,
        resources: Some(StationResources {
            at: 77,
            disks: vec![DiskSpace {
                label: format!("label-{SECRET}"),
                path: format!("/mnt/{SECRET}"),
                total_bytes: 100,
                free_bytes: 40,
            }],
            memory: Some(MemoryUse {
                total_bytes: 8,
                available_bytes: 3,
            }),
            cpu: None,
            sessions: vec![
                SessionLoad {
                    session_id: "s1".into(),
                    cpu_pct: Some(12),
                    rss_bytes: Some(34),
                    procs: 5,
                },
                SessionLoad {
                    session_id: "hidden".into(),
                    cpu_pct: Some(99),
                    rss_bytes: None,
                    procs: 1,
                },
            ],
        }),
        desktop: None,
        traits: None,
        config: None,
        taildrive: None,
        fleet_git: None,
        fleet_summary: None,
    };
    r.f.store.set_station_info(&r.f.station, info);
    // Narrow the display to `alpha`: only `s1` is visible.
    post(
        &r.c,
        &r.h,
        &format!("/api/displays/{}/directive", r.display),
        json!({ "mode": "mission", "filter": { "stations": [r.st], "repo": "alpha" } }),
    )
    .await;
    let res = get_with(&r.c, "/api/stations", &r.cookie).await;
    assert!(!res.text.contains(SECRET), "{}", res.text);
    assert!(!res.text.contains("procs") && !res.text.contains("hidden"));
    let station = &res.body[0];
    assert_eq!(station["info"], Value::Null);
    let shown = &station["display_resources"];
    assert_eq!(shown["at"], 77);
    assert_eq!(
        shown["disks"],
        json!([{ "total_bytes": 100, "free_bytes": 40 }])
    );
    assert_eq!(shown["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(shown["sessions"][0]["session_id"], "s1");
    assert_eq!(shown["sessions"][0]["cpu_pct"], 12);
    // The owner still gets the full info and no reduced block.
    let owner_view = get(&r.c, &r.h, "/api/stations").await;
    assert!(owner_view.text.contains(SECRET));
    assert!(owner_view.body[0]["display_resources"].is_null());
}
