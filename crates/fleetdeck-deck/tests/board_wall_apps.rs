//! Wall apps (B17 §3.4, C1): the policy of the display artifact route, the
//! `allow_apps` setting, and the app feed. The app policy needs all of four
//! conditions; the query `features=` declares the feature but grants nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::collections::BTreeSet;

use common::Fixture;
use common::board::{BoardClient, Http};
use displays_support::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::display_directives::DirectiveSender;
use fleetdeck_deck::store::displays::DisplayChange;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{Artifact, ArtifactKind, ArtifactVersion};
use fleetdeck_proto::display_directive::{ArtifactRef, DirectiveBody};
use fleetdeck_proto::event::{RequestKind, ResponseChannel};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::session::OpenRequest;
use serde_json::{Value, json};
use share_support::*;

const INERT: &str = "sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'; media-src data:; font-src data:";
/// `app_csp::header(&AppCsp::default())`, literally.
const APP: &str = "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; \
style-src 'unsafe-inline'; img-src data: blob:; font-src data:; \
media-src data: blob:; connect-src 'none'; frame-src 'none'; object-src 'none'; \
base-uri 'none'; form-action 'none'; frame-ancestors 'self'";
const PAGE: &str = "<h1>an app</h1>";
const FEATURES: (&str, &str) = ("X-FleetDeck-Features", "wall_apps.v1");
const LEAK: &str = "SENTINEL-LEAK-4711";

struct Rig {
    _f: Fixture,
    c: BoardClient,
    h: Headers,
    st: String,
    display: String,
    cookie: String,
}

async fn rig() -> Rig {
    let (f, c, h, st) = setup_deck().await;
    let (display, cookie) = display_login(&c, "Wall", &[st.as_str()]).await;
    Rig {
        _f: f,
        c,
        h,
        st,
        display,
        cookie,
    }
}

fn artifact(id: &str, session: Option<&str>) -> Artifact {
    Artifact {
        artifact_id: id.to_owned(),
        station_id: String::new(),
        session_id: session.map(Into::into),
        title: format!("Title of {id}"),
        kind: ArtifactKind::Html,
        latest_version: 1,
        created_at: 1,
        updated_at: 1,
        author: "agent".to_owned(),
    }
}

impl Rig {
    fn station_id(&self) -> StationId {
        StationId::new(self.st.clone())
    }

    /// The index row and the cached bytes of the one file of version 1.
    async fn seed(&self, id: &str, session: Option<&str>, file: &str) {
        let store = &self.c.store;
        store
            .note_artifact(self.station_id(), &artifact(id, session))
            .await
            .unwrap();
        store
            .put_artifact_versions(
                self.station_id(),
                vec![ArtifactVersion {
                    artifact_id: id.to_owned(),
                    version: 1,
                    size: PAGE.len() as u64,
                    sha256: String::new(),
                    files: vec![file.to_owned()],
                    created_at: 1,
                    capture: None,
                }],
            )
            .await
            .unwrap();
        store
            .put_artifact_file(
                self.station_id(),
                id.to_owned(),
                1,
                file.to_owned(),
                PAGE.as_bytes().to_vec(),
            )
            .await
            .unwrap();
    }

    async fn allow_apps(&self, on: bool) {
        self.c
            .store
            .display_update(
                self.display.clone(),
                DisplayChange {
                    allow_apps: Some(on),
                    ..DisplayChange::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
    }

    async fn direct(&self, id: &str, version: u32, app: bool) {
        let body = DirectiveBody::Artifact {
            artifact: ArtifactRef {
                station_id: self.station_id(),
                artifact_id: id.to_owned(),
                version,
            },
            app,
            pin: false,
            filter: None,
        };
        self.c
            .store
            .display_directive_set(self.display.clone(), DirectiveSender::Owner, body)
            .await
            .unwrap();
    }

    async fn file(&self, id: &str, file: &str, query: &str, headers: &[(&str, &str)]) -> Http {
        let path = format!(
            "/api/displays/self/artifact/{}/{id}/1/{file}{query}",
            self.st
        );
        let mut all = vec![("Cookie", self.cookie.as_str())];
        all.extend_from_slice(headers);
        self.c.get(&path, &all).await
    }

    async fn feed(&self, id: &str, version: u32) -> Http {
        let path = format!("/api/displays/self/app-feed/{}/{id}/{version}", self.st);
        get_with(&self.c, &path, &self.cookie).await
    }

    async fn directive_mode(&self) -> String {
        let r = get_with(&self.c, "/api/displays/self/directive", &self.cookie).await;
        assert_eq!(r.status, 200, "{}", r.text);
        r.body["directive"]["mode"].as_str().unwrap().to_owned()
    }
}

fn csp(r: &Http) -> &str {
    assert_eq!(r.status, 200, "{}", r.text);
    r.header("content-security-policy").unwrap()
}

fn miss(r: &Http) {
    assert_eq!(
        (r.status, r.code(), r.body["message"].as_str()),
        (404, "unknown_artifact", Some("No such artifact.")),
        "{}",
        r.text
    );
}

#[tokio::test]
async fn app_policy_only_with_directive_flag_setting_html_and_feature() {
    let r = rig().await;
    r.seed("h1", Some("s1"), "page.html").await;
    r.seed("m1", Some("s1"), "page.md").await;
    for allow in [false, true] {
        // The setting first: a change of it resets the directive.
        r.allow_apps(allow).await;
        for flag in [false, true] {
            for html in [false, true] {
                let (id, file) = if html {
                    ("h1", "page.html")
                } else {
                    ("m1", "page.md")
                };
                r.direct(id, 1, flag).await;
                for declared in [false, true] {
                    let headers: &[(&str, &str)] = if declared { &[FEATURES] } else { &[] };
                    let res = r.file(id, file, "", headers).await;
                    let all_four = allow && flag && html && declared;
                    let want = if all_four { APP } else { INERT };
                    assert_eq!(
                        csp(&res),
                        want,
                        "allow={allow} flag={flag} html={html} declared={declared}"
                    );
                    assert_eq!(res.text, PAGE);
                }
            }
        }
    }
    // The app policy: scripts, no network, no same-origin.
    assert!(APP.starts_with("sandbox allow-scripts;"));
    assert!(APP.contains("connect-src 'none'"));
    assert!(!APP.contains("allow-same-origin"));
}

#[tokio::test]
async fn the_query_declares_wall_apps_without_the_header() {
    let r = rig().await;
    r.seed("h1", Some("s1"), "page.html").await;
    r.allow_apps(true).await;
    r.direct("h1", 1, true).await;
    let by_query = r
        .file("h1", "page.html", "?features=wall_apps.v1", &[])
        .await;
    assert_eq!(csp(&by_query), APP);
    // The one-file name and another token list work the same.
    let one = r.file("h1", "_", "?features=a.v1,wall_apps.v1", &[]).await;
    assert_eq!(csp(&one), APP);
    // Another feature in the query does not declare wall apps.
    let other = r
        .file("h1", "page.html", "?features=wall_pages.v1", &[])
        .await;
    assert_eq!(csp(&other), INERT);
    let none = r.file("h1", "page.html", "", &[]).await;
    assert_eq!(csp(&none), INERT);
}

#[tokio::test]
async fn the_query_alone_grants_nothing() {
    let r = rig().await;
    r.seed("h1", Some("s1"), "page.html").await;
    let q = "?features=wall_apps.v1";
    // `allow_apps` off, `app: true`.
    r.direct("h1", 1, true).await;
    assert_eq!(csp(&r.file("h1", "page.html", q, &[]).await), INERT);
    // `allow_apps` on, `app: false`.
    r.allow_apps(true).await;
    r.direct("h1", 1, false).await;
    assert_eq!(csp(&r.file("h1", "page.html", q, &[]).await), INERT);
    // No directive of this artifact: the same 404 as before.
    r.c.store
        .display_directive_set(
            r.display.clone(),
            DirectiveSender::Owner,
            DirectiveBody::mission(),
        )
        .await
        .unwrap();
    miss(&r.file("h1", "page.html", q, &[]).await);
}

#[tokio::test]
async fn both_answers_are_no_store() {
    let r = rig().await;
    r.seed("h1", Some("s1"), "page.html").await;
    r.allow_apps(true).await;
    r.direct("h1", 1, true).await;
    let app = r
        .file("h1", "page.html", "?features=wall_apps.v1", &[])
        .await;
    assert_eq!(csp(&app), APP);
    let inert = r.file("h1", "page.html", "", &[]).await;
    assert_eq!(csp(&inert), INERT);
    for res in [&app, &inert] {
        assert_eq!(res.header("cache-control"), Some("no-store"));
        assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(res.header("referrer-policy"), Some("no-referrer"));
    }
}

#[tokio::test]
async fn allow_apps_needs_elevation_to_turn_on() {
    let r = rig().await;
    let path = format!("/api/displays/{}/settings", r.display);
    expect(
        &post(&r.c, &r.h, &path, json!({ "allow_apps": true })).await,
        400,
        "elevation_required",
    );
    let view = get(&r.c, &r.h, "/api/displays").await;
    assert_eq!(
        view.body["displays"][0]["allow_apps"], false,
        "{}",
        view.body
    );
    let e = elevate(&r.c, &r.h).await;
    let on = post(
        &r.c,
        &r.h,
        &path,
        json!({ "allow_apps": true, "elevation": e }),
    )
    .await;
    assert_eq!(on.status, 200, "{}", on.body);
    assert_eq!(on.body["allow_apps"], true);
    // Turning it off needs no elevation; the same value again needs none.
    let again = post(&r.c, &r.h, &path, json!({ "allow_apps": true })).await;
    assert_eq!(again.status, 200, "{}", again.body);
    let off = post(&r.c, &r.h, &path, json!({ "allow_apps": false })).await;
    assert_eq!(off.status, 200, "{}", off.body);
    assert_eq!(off.body["allow_apps"], false);
    let keys = r.c.audited("display.settings", AuditResult::Ok).await;
    assert!(
        keys.iter().flatten().any(|d| d.contains("allow_apps")),
        "{keys:?}"
    );
}

#[tokio::test]
async fn allow_apps_change_resets_the_directive() {
    let r = rig().await;
    r.seed("h1", Some("s1"), "page.html").await;
    r.direct("h1", 1, true).await;
    assert_eq!(r.directive_mode().await, "artifact");
    // On: the directive goes.
    r.allow_apps(true).await;
    assert_eq!(r.directive_mode().await, "mission");
    r.direct("h1", 1, true).await;
    assert_eq!(r.directive_mode().await, "artifact");
    // The same value keeps the directive.
    r.allow_apps(true).await;
    assert_eq!(r.directive_mode().await, "artifact");
    // Off: the directive goes too.
    r.allow_apps(false).await;
    assert_eq!(r.directive_mode().await, "mission");
}

/// A summary with a callsign, a title and an open request that holds the
/// sentinel in every field that must not leave.
fn summary(id: &str, request: bool) -> fleetdeck_proto::SessionSummary {
    let mut s = card(id);
    s.callsign = Some(format!("call-{id}"));
    s.title = Some(format!("title-{id}"));
    if request {
        s.open_requests = vec![OpenRequest {
            request_id: "q1".into(),
            node_id: None,
            request_kind: RequestKind::Command,
            title: format!("{LEAK} title"),
            details: format!("{LEAK} details"),
            input: Some(json!({ "command": format!("{LEAK} input") })),
            options: vec![],
            response: ResponseChannel::RpcReply,
            requested_at: 100,
        }];
    }
    s
}

/// Sessions of the lineage of `author`: the sessions `kid` (a grant that
/// is revoked) and `kid2` (named only by the start receipt), a grandchild
/// that `kid` started, an outsider's child, and `far` on a station outside
/// the ceiling.
async fn lineage(r: &Rig, far_station: &str) {
    let st = r.st.clone();
    let far = far_station.to_owned();
    r.c.store
        .db()
        .call(move |conn| {
            let tokens = |grant: &str, session: &str, state: &str| {
                conn.execute(
                    "INSERT INTO tokens (grant_id, station_id, session_id, classes, stations,
                         spawn_cap, max_modes, idle_ttl_ms, created_at, expires_at, state)
                     VALUES (?1, ?2, ?3, '[\"read\"]', '[]', 3, '{}', 60000, 1, 2, ?4)",
                    rusqlite::params![grant, st, session, state],
                )
            };
            tokens("g-author", "author", "active")?;
            tokens("g-author-old", "author", "revoked")?;
            tokens("g-kid", "kid", "active")?;
            tokens("g-outsider", "outsider", "active")?;
            let line = |command: &str, grant: &str, station: &str, session: Option<&str>| {
                conn.execute(
                    "INSERT INTO coordinator_lineage
                         (command_id, grant_id, station_id, session_id, created_at)
                     VALUES (?1, ?2, ?3, ?4, 1)",
                    rusqlite::params![command, grant, station, session],
                )
            };
            line("c1", "g-author", &st, Some("kid"))?;
            // The same child again under the old grant: one card only.
            line("c2", "g-author-old", &st, Some("kid"))?;
            line("c3", "g-author", &far, Some("far"))?;
            line("c4", "g-kid", &st, Some("grandkid"))?;
            line("c5", "g-outsider", &st, Some("outsider-kid"))?;
            // A start that the receipt names: no session id in the row.
            conn.execute(
                "INSERT INTO commands (command_id, station_id, method, origin, envelope,
                     retry_until, state, receipt, created_at, updated_at)
                 VALUES ('start1', ?1, 'session.start', 'coordinator', '{}', 1, 'done',
                     '{\"result\":{\"session_id\":\"kid2\"}}', 1, 1)",
                [&st],
            )?;
            conn.execute(
                "INSERT INTO outbox (outbox_id, station_id, kind, origin, payload, state,
                     command_id, next_at, expires_at, created_at, updated_at)
                 VALUES ('ob1', ?1, 'command', 'coordinator', '{}', 'delivered', 'start1',
                     1, 1, 1, 1)",
                [&st],
            )?;
            conn.execute(
                "INSERT INTO coordinator_lineage
                     (command_id, grant_id, station_id, session_id, start_outbox_id, created_at)
                 VALUES ('c6', 'g-author', ?1, NULL, 'ob1', 1)",
                [&st],
            )?;
            // A start that has no session yet: dropped.
            conn.execute(
                "INSERT INTO coordinator_lineage
                     (command_id, grant_id, station_id, session_id, created_at)
                 VALUES ('c7', 'g-author', ?1, NULL, 1)",
                [&st],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn far_station(r: &Rig) -> String {
    let rec =
        r.c.store
            .pin_station(NewStation {
                key: Identity::generate().unwrap().public_key(),
                name: "far".to_owned(),
                scopes: BTreeSet::from([Scope::Read]),
                dial_url: None,
            })
            .await
            .unwrap();
    r.c.store
        .apply_snapshot(
            rec.station_id.clone(),
            fleetdeck_proto::IncarnationId::from("i2"),
            0,
            vec![summary("far", false)],
        )
        .await
        .unwrap();
    rec.station_id.as_str().to_owned()
}

async fn app_rig() -> Rig {
    let r = rig().await;
    let far = far_station(&r).await;
    let station = r._f.station.clone();
    r.c.store
        .apply_snapshot(
            station,
            r._f.inc.clone(),
            0,
            vec![
                summary("author", true),
                summary("kid", false),
                summary("kid2", false),
                summary("grandkid", false),
                summary("outsider", false),
                summary("outsider-kid", false),
            ],
        )
        .await
        .unwrap();
    lineage(&r, &far).await;
    r.seed("app1", Some("author"), "page.html").await;
    r.allow_apps(true).await;
    r.direct("app1", 1, true).await;
    r
}

fn ids(feed: &Http) -> Vec<String> {
    feed.body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["session_id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn feed_holds_only_the_author_lineage_inside_the_scope() {
    let r = app_rig().await;
    let res = r.feed("app1", 1).await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.header("cache-control"), Some("no-store"));
    assert!(res.body["at"].as_i64().unwrap() > 0);
    // The author and its direct children (also one that a revoked grant
    // started, one that the receipt names). Not the grandchild, not an
    // outsider or its child, not a session of a station outside the ceiling.
    let mut got = ids(&res);
    got.sort();
    assert_eq!(got, ["author", "kid", "kid2"], "{}", res.text);
    let author = res.body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["session_id"] == "author")
        .unwrap();
    assert_eq!(author["callsign"], "call-author");
    assert_eq!(author["title"], "title-author");
    assert_eq!(author["needs_you"], "command");
    assert_eq!(author["status"], "needs_you");
    assert!(author["station"].as_str().is_some_and(|n| !n.is_empty()));
    let kid = res.body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["session_id"] == "kid")
        .unwrap();
    assert_eq!(kid["needs_you"], Value::Null);
    assert!(author.get("tool").is_none());
}

#[tokio::test]
async fn feed_follows_the_scope_of_the_display() {
    let r = app_rig().await;
    // A ceiling that holds the far station too shows its session.
    let far = far_station_id(&r).await;
    r.c.store
        .display_update(
            r.display.clone(),
            DisplayChange {
                ceiling: Some(vec![r.station_id(), StationId::new(far.clone())]),
                ..DisplayChange::default()
            },
        )
        .await
        .unwrap();
    // The ceiling change reset the directive: set it again.
    r.direct("app1", 1, true).await;
    let res = r.feed("app1", 1).await;
    let mut got = ids(&res);
    got.sort();
    assert_eq!(got, ["author", "far", "kid", "kid2"], "{}", res.text);
    let far_card = res.body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["session_id"] == "far")
        .unwrap();
    assert_eq!(far_card["station"], "far");
}

async fn far_station_id(r: &Rig) -> String {
    let list = r.c.store.stations().await.unwrap();
    list.into_iter()
        .find(|s| s.name == "far")
        .unwrap()
        .station_id
        .as_str()
        .to_owned()
}

#[tokio::test]
async fn an_artifact_without_an_author_session_has_an_empty_feed() {
    let r = rig().await;
    r.seed("cap1", None, "page.html").await;
    r.allow_apps(true).await;
    r.direct("cap1", 1, true).await;
    let res = r.feed("cap1", 1).await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body["sessions"], json!([]));
}

#[tokio::test]
async fn feed_is_404_without_an_app_directive() {
    let r = app_rig().await;
    assert_eq!(r.feed("app1", 1).await.status, 200);
    // Another version, another artifact, a bad version.
    miss(&r.feed("app1", 2).await);
    miss(&r.feed("app1", 0).await);
    miss(&r.feed("nope", 1).await);
    // A directive without the flag.
    r.direct("app1", 1, false).await;
    miss(&r.feed("app1", 1).await);
    // The flag back, but the setting off (the setting change also resets).
    r.allow_apps(false).await;
    r.direct("app1", 1, true).await;
    miss(&r.feed("app1", 1).await);
    // The setting on again with no directive of an app.
    r.allow_apps(true).await;
    miss(&r.feed("app1", 1).await);
    r.c.store
        .display_directive_set(
            r.display.clone(),
            DirectiveSender::Owner,
            DirectiveBody::mission(),
        )
        .await
        .unwrap();
    miss(&r.feed("app1", 1).await);
    // The owner login does not reach a display route.
    let path = format!("/api/displays/self/app-feed/{}/app1/1", r.st);
    let owner = get(&r.c, &r.h, &path).await;
    assert!(owner.status == 401 || owner.status == 403, "{}", owner.text);
}

#[tokio::test]
async fn feed_never_holds_tool_input_or_mail() {
    let r = app_rig().await;
    let res = r.feed("app1", 1).await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert!(!res.text.contains(LEAK), "{}", res.text);
    let keys: BTreeSet<&str> = res.body["sessions"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "station",
            "session_id",
            "callsign",
            "title",
            "status",
            "needs_you"
        ])
    );
    assert_eq!(
        res.body.as_object().unwrap().keys().count(),
        2,
        "{}",
        res.text
    );
}

#[tokio::test]
async fn the_feed_route_is_closed_to_a_login_that_ended() {
    let r = app_rig().await;
    assert_eq!(r.feed("app1", 1).await.status, 200);
    r.c.store.display_revoke(r.display.clone()).await.unwrap();
    let res = r.feed("app1", 1).await;
    assert_eq!(res.status, 401, "{}", res.text);
}
