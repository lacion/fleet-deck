//! The wall remote over loopback HTTPS (B17 §3.1): the owner's control and
//! navigation routes, the relay to a display socket that declared
//! `wall_control.v1`, the remote view and its audit rows.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::collections::BTreeSet;
use std::time::Duration;

use common::Fixture;
use common::board::{BoardClient, Http, Ws};
use displays_support::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::session::RepoInfo;
use fleetdeck_proto::{IncarnationId, StationId};
use serde_json::{Value, json};
use share_support::*;

const FEATURE: &str = "wall_control.v1";

struct Rig {
    f: Fixture,
    c: BoardClient,
    h: Headers,
    st: String,
    other: StationId,
    display: String,
    cookie: String,
}

fn repo_card(id: &str, root: &str) -> fleetdeck_proto::SessionSummary {
    let mut s = card(id);
    s.repo = Some(RepoInfo {
        root: root.to_owned(),
        origin: None,
    });
    s
}

async fn rig() -> Rig {
    let (f, c, h, st) = setup_deck().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![card("s1"), card("s2"), repo_card("r1", "/srv/a/alpha")],
        )
        .await
        .unwrap();
    let rec = c
        .store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "other".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap();
    c.store
        .apply_snapshot(
            rec.station_id.clone(),
            IncarnationId::from("i2"),
            0,
            vec![card("o1")],
        )
        .await
        .unwrap();
    let (display, cookie) = display_login(&c, "Wall", &[&st]).await;
    Rig {
        f,
        c,
        h,
        st,
        other: rec.station_id,
        display,
        cookie,
    }
}

impl Rig {
    fn path(&self, tail: &str) -> String {
        format!("/api/displays/{}/{tail}", self.display)
    }

    async fn control(&self, control: Value) -> Http {
        post(
            &self.c,
            &self.h,
            &self.path("control"),
            json!({ "control": control }),
        )
        .await
    }

    async fn navigate(&self, directive: Value) -> Http {
        post(
            &self.c,
            &self.h,
            &self.path("remote/navigate"),
            json!({ "directive": directive }),
        )
        .await
    }

    async fn remote(&self, after: u64) -> Http {
        get(
            &self.c,
            &self.h,
            &format!("{}?after={after}", self.path("remote")),
        )
        .await
    }

    fn select(&self, session: &str) -> Value {
        json!({ "op": "select", "item": { "station_id": self.st, "session_id": session } })
    }

    /// A display socket. With the feature, it has read the `sync` frame (the
    /// relay counts it); without it, the test waits for the link `older`.
    async fn socket(&self, feature: bool) -> (Ws, Option<Value>) {
        let t = self
            .c
            .post("/api/ws/ticket", &[("Cookie", &self.cookie)], json!({}))
            .await;
        let ticket = t.body["ticket"].as_str().unwrap().to_owned();
        let path = if feature {
            format!("/api/ws?ticket={ticket}&features={FEATURE}")
        } else {
            format!("/api/ws?ticket={ticket}")
        };
        let mut ws = self
            .c
            .ws(&path, &[])
            .await
            .expect("the display socket opens");
        assert_eq!(ws.next().await.unwrap()["type"], "hello");
        assert_eq!(ws.next().await.unwrap()["type"], "snapshot");
        if feature {
            let sync = ws.next_of("wall_control").await;
            (ws, Some(sync))
        } else {
            self.wait_link("older").await;
            (ws, None)
        }
    }

    async fn wait_link(&self, want: &str) {
        for _ in 0..100 {
            let r = self.remote(0).await;
            if r.body["link"] == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the link did not become {want}");
    }
}

/// No `wall_control` frame within `wait`; other frames are fine.
async fn no_wall_frame(ws: &mut Ws, wait: Duration) {
    let end = tokio::time::Instant::now() + wait;
    while let Ok(m) = tokio::time::timeout_at(end, ws.next()).await {
        let m = m.expect("an open socket");
        assert_ne!(m["type"], "wall_control", "unexpected frame: {m}");
    }
}

fn audits(r: &Rig, action: &str) -> impl Future<Output = Vec<Option<String>>> {
    r.c.audited(action, AuditResult::Ok)
}

#[tokio::test]
async fn a_declaring_display_socket_first_gets_a_sync_frame() {
    let r = rig().await;
    let (_ws, sync) = r.socket(true).await;
    let frame = &sync.unwrap()["frame"];
    assert_eq!(frame["event"], json!({ "kind": "sync" }));
    assert_eq!(frame["seq"], 0);
    assert_eq!(frame["generation"], 0);
    assert_eq!(
        frame["state"],
        json!({ "selected": null, "frame_tenths": 0 })
    );
    assert_eq!(frame["incarnation"], r.remote(0).await.body["incarnation"]);
    // A second socket, after a control: the sync frame holds the checkpoint
    // of its generation and the current seq, with no new number.
    let sent = r.control(r.select("s2")).await;
    assert_eq!(sent.status, 200, "{}", sent.body);
    let (_ws2, sync2) = r.socket(true).await;
    let frame2 = &sync2.unwrap()["frame"];
    assert_eq!(frame2["event"]["kind"], "sync");
    assert_eq!(frame2["seq"], 1);
    assert_eq!(frame2["state"]["selected"]["session_id"], "s2");
}

#[tokio::test]
async fn owner_control_reaches_a_declaring_display_socket() {
    let r = rig().await;
    let (mut ws, _) = r.socket(true).await;
    let step = json!({ "op": "scroll", "area": "page", "step": "page_down" });
    let ack = r.control(step.clone()).await;
    assert_eq!(ack.status, 200, "{}", ack.body);
    let m = ws.next_of("wall_control").await;
    let frame = &m["frame"];
    assert_eq!(frame["seq"], ack.body["seq"]);
    assert_eq!(frame["generation"], ack.body["generation"]);
    assert_eq!(frame["incarnation"], ack.body["incarnation"]);
    assert_eq!(
        frame["event"],
        json!({ "kind": "control", "control": step })
    );
    assert_eq!(frame["state"]["frame_tenths"], 9);
    // The next control has the next number and the state after it.
    let next = r.control(r.select("s1")).await;
    assert_eq!(next.body["seq"], ack.body["seq"].as_u64().unwrap() + 1);
    let m = ws.next_of("wall_control").await;
    assert_eq!(m["frame"]["state"]["selected"]["session_id"], "s1");
    assert_eq!(m["frame"]["state"]["frame_tenths"], 9);
    // `select` with no item clears the selection.
    r.control(json!({ "op": "select", "item": null })).await;
    let m = ws.next_of("wall_control").await;
    assert_eq!(m["frame"]["state"]["selected"], Value::Null);
}

#[tokio::test]
async fn select_outside_the_scope_is_refused() {
    let r = rig().await;
    let (mut ws, _) = r.socket(true).await;
    let other = r.other.as_str();
    let outside_station = json!({ "op": "select",
        "item": { "station_id": other, "session_id": "o1" } });
    let missing = r.select("does-not-exist");
    for body in [outside_station, missing] {
        let sent = r.control(body.clone()).await;
        assert_eq!(
            (sent.status, sent.code()),
            (400, "invalid_control"),
            "{body} {}",
            sent.body
        );
        assert_eq!(sent.body["message"], "The control is not valid.");
    }
    // A repository filter: `r1` is in `alpha`, and `s1` has no repository.
    let narrowed = r
        .navigate(json!({ "mode": "mission",
            "filter": { "stations": [r.st], "repo": "beta" } }))
        .await;
    assert_eq!(narrowed.status, 200, "{}", narrowed.body);
    for session in ["r1", "s1"] {
        let sent = r.control(r.select(session)).await;
        expect(&sent, 400, "invalid_control");
    }
    let allowed = r
        .navigate(json!({ "mode": "mission",
            "filter": { "stations": [r.st], "repo": "alpha" } }))
        .await;
    assert_eq!(allowed.status, 200, "{}", allowed.body);
    expect(&r.control(r.select("s1")).await, 400, "invalid_control");
    assert_eq!(r.control(r.select("r1")).await.status, 200);
    // A malformed control is refused with the same text.
    let bad = post(
        &r.c,
        &r.h,
        &r.path("control"),
        json!({ "control": { "op": "open" } }),
    )
    .await;
    expect(&bad, 400, "invalid_control");
    // Only the navigations and the one valid control made frames.
    let mut frames = Vec::new();
    while frames.len() < 3 {
        let m = ws.next_of("wall_control").await;
        frames.push(m["frame"]["event"]["kind"].as_str().unwrap().to_owned());
    }
    assert_eq!(frames, ["navigate", "navigate", "control"]);
    no_wall_frame(&mut ws, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn a_display_socket_without_the_feature_gets_no_frame_and_the_post_answers_older() {
    let r = rig().await;
    let (mut ws, none) = r.socket(false).await;
    assert!(none.is_none());
    let sent = r
        .control(json!({ "op": "scroll", "area": "chat", "step": "top" }))
        .await;
    expect(&sent, 409, "wall_controls_unsupported");
    assert_eq!(
        sent.body["message"],
        "This wall runs an older board. Reload the wall to use the remote."
    );
    assert_eq!(r.remote(0).await.body["link"], "older");
    // A navigation still works, and the old socket gets no frame.
    assert_eq!(r.navigate(json!({ "mode": "ambient" })).await.status, 200);
    no_wall_frame(&mut ws, Duration::from_millis(400)).await;
}

#[tokio::test]
async fn no_socket_answers_wall_offline() {
    let r = rig().await;
    let sent = r
        .control(json!({ "op": "scroll", "area": "chat", "step": "top" }))
        .await;
    expect(&sent, 409, "wall_offline");
    assert_eq!(sent.body["message"], "The wall is not connected.");
    assert_eq!(r.remote(0).await.body["link"], "offline");
}

#[tokio::test]
async fn navigate_stores_an_owner_directive_and_pushes_a_navigate_frame() {
    let r = rig().await;
    let (mut ws, _) = r.socket(true).await;
    let body = json!({ "mode": "focus", "pin": true,
        "session": { "station_id": r.st, "session_id": "s1" } });
    let ack = r.navigate(body.clone()).await;
    assert_eq!(ack.status, 200, "{}", ack.body);
    assert_eq!(ack.body["generation"], 1);
    let m = ws.next_of("wall_control").await;
    assert_eq!(m["frame"]["event"], json!({ "kind": "navigate" }));
    assert_eq!(m["frame"]["generation"], 1);
    assert_eq!(m["frame"]["seq"], ack.body["seq"]);
    // The directive route reads the new body and generation.
    let polled = get_with(&r.c, "/api/displays/self/directive", &r.cookie).await;
    assert_eq!(polled.body["generation"], 1);
    assert_eq!(polled.body["directive"]["mode"], "focus");
    // One `display.directive` row, with the identifiers only.
    let rows = audits(&r, "display.directive").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0]
            .as_deref()
            .unwrap()
            .starts_with("mode=focus session=")
    );
}

#[tokio::test]
async fn navigate_works_without_a_socket() {
    let r = rig().await;
    let ack = r
        .navigate(json!({ "mode": "race", "batch_id": "NOBATCH" }))
        .await;
    expect(&ack, 400, "invalid_directive");
    let ack = r.navigate(json!({ "mode": "ambient" })).await;
    assert_eq!(ack.status, 200, "{}", ack.body);
    assert_eq!(ack.body["generation"], 1);
    let polled = get_with(&r.c, "/api/displays/self/directive", &r.cookie).await;
    assert_eq!(polled.body["directive"]["mode"], "ambient");
    assert_eq!(r.remote(0).await.body["link"], "offline");
}

#[tokio::test]
async fn navigate_has_the_checks_of_the_directive_route() {
    let r = rig().await;
    let other = r.other.as_str();
    let outside = r
        .navigate(json!({ "mode": "focus",
            "session": { "station_id": other, "session_id": "o1" } }))
        .await;
    expect(&outside, 403, "outside_ceiling");
    let unknown = r
        .navigate(
            json!({ "mode": "focus", "session": { "station_id": r.st, "session_id": "nope" } }),
        )
        .await;
    expect(&unknown, 400, "invalid_directive");
    // A body that is not a navigation, and a directive with an extra field.
    let bare = post(
        &r.c,
        &r.h,
        &r.path("remote/navigate"),
        json!({ "mode": "mission" }),
    )
    .await;
    expect(&bare, 400, "invalid_directive");
    let url = r
        .navigate(json!({ "mode": "mission", "url": "https://x.example" }))
        .await;
    expect(&url, 400, "invalid_directive");
    assert!(!url.text.contains("example"), "{}", url.text);
    let gone = post(
        &r.c,
        &r.h,
        "/api/displays/nope/remote/navigate",
        json!({ "directive": { "mode": "mission" } }),
    )
    .await;
    expect(&gone, 404, "display_unknown");
    // Nothing was stored.
    let polled = get_with(&r.c, "/api/displays/self/directive", &r.cookie).await;
    assert_eq!(polled.body["generation"], 0);
}

#[tokio::test]
async fn owner_and_member_sockets_never_get_a_control_frame() {
    let r = rig().await;
    let (mut display, _) = r.socket(true).await;
    // The owner declares the feature too.
    let t = post(&r.c, &r.h, "/api/ws/ticket", json!({})).await;
    let ticket = t.body["ticket"].as_str().unwrap().to_owned();
    let mut owner_ws =
        r.c.ws(&format!("/api/ws?ticket={ticket}&features={FEATURE}"), &[])
            .await
            .expect("the owner socket opens");
    // A member of a shared session declares it too.
    let share = json!({ "kind": "session", "target": format!("{}/s1", r.st), "role": "viewer" });
    let (member, _, _) = join(&r.c, &r.h, share, "Alice").await;
    let t = post(&r.c, &member, "/api/ws/ticket", json!({})).await;
    let ticket = t.body["ticket"].as_str().unwrap().to_owned();
    let mut member_ws =
        r.c.ws(&format!("/api/ws?ticket={ticket}&features={FEATURE}"), &[])
            .await
            .expect("the member socket opens");
    assert_eq!(r.control(r.select("s1")).await.status, 200);
    assert_eq!(r.navigate(json!({ "mode": "ambient" })).await.status, 200);
    display.next_of("wall_control").await;
    display.next_of("wall_control").await;
    no_wall_frame(&mut owner_ws, Duration::from_millis(400)).await;
    no_wall_frame(&mut member_ws, Duration::from_millis(400)).await;
}

#[tokio::test]
async fn member_token_and_display_are_refused() {
    let r = rig().await;
    let origin = r.c.origin();
    let share = json!({ "kind": "session", "target": format!("{}/s1", r.st), "role": "viewer" });
    let (member, _, _) = join(&r.c, &r.h, share, "Alice").await;
    let (_, read_token) =
        r.c.store
            .create_token(
                "ops".into(),
                [Scope::Read].into_iter().collect(),
                86_400_000,
            )
            .await
            .unwrap();
    let bearer = format!("Bearer {}", read_token.expose());
    let control = json!({ "control": { "op": "scroll", "area": "chat", "step": "top" } });
    let navigate = json!({ "directive": { "mode": "mission" } });
    for (method, tail, body) in [
        ("POST", "control", Some(control)),
        ("POST", "remote/navigate", Some(navigate)),
        ("GET", "remote", None),
    ] {
        let path = r.path(tail);
        let member_headers = refs(&member);
        let by_member = r.c.req(method, &path, &member_headers, body.clone()).await;
        expect(&by_member, 403, "owner_only");
        let by_display =
            r.c.req(
                method,
                &path,
                &[("Cookie", &r.cookie), ("Origin", &origin)],
                body.clone(),
            )
            .await;
        expect(&by_display, 403, "display_read_only");
        let by_token =
            r.c.req(method, &path, &[("Authorization", &bearer)], body.clone())
                .await;
        if method == "POST" {
            expect(&by_token, 403, "forbidden");
        } else {
            expect(&by_token, 403, "owner_only");
        }
        let anon = r.c.req(method, &path, &[], body).await;
        assert_eq!(anon.status, 401, "{}", anon.body);
    }
    assert_eq!(r.remote(0).await.body["seq"], 0);
}

#[tokio::test]
async fn a_late_remote_sees_the_remote_navigation_and_the_checkpoint() {
    let r = rig().await;
    let (_ws, _) = r.socket(true).await;
    let focus = json!({ "mode": "focus", "pin": true,
        "session": { "station_id": r.st, "session_id": "s1" } });
    assert_eq!(r.navigate(focus).await.status, 200);
    assert_eq!(r.control(r.select("s2")).await.status, 200);
    let live = r.remote(0).await;
    assert_eq!(live.body["complete"], true);
    assert_eq!(live.body["controls"].as_array().unwrap().len(), 2);
    // The controls expire (the hub clock moves on 31 s).
    r.c.state
        .wall_controls
        .advance_clock(Duration::from_secs(31));
    let late = r.remote(0).await;
    assert_eq!(late.status, 200, "{}", late.body);
    assert_eq!(late.body["directive"]["generation"], 1);
    assert_eq!(late.body["directive"]["directive"]["mode"], "focus");
    assert_eq!(late.body["state"]["selected"]["session_id"], "s2");
    assert_eq!(late.body["complete"], false);
    assert!(late.body["controls"].as_array().unwrap().is_empty());
    assert_eq!(late.body["link"], "ready");
    assert_eq!(late.body["seq"], 2);
    let hold = late.body["hold_left_ms"].as_u64().unwrap();
    assert!((80_000..=90_000).contains(&hold), "{hold}");
    // `after = seq` is complete, with no frame.
    let caught_up = r.remote(2).await;
    assert_eq!(caught_up.body["complete"], true);
    assert!(caught_up.body["controls"].as_array().unwrap().is_empty());
    // A directive that another sender changed: the checkpoint is the default.
    let polled = get_with(&r.c, "/api/displays/self/directive", &r.cookie).await;
    assert_eq!(polled.body["generation"], 1);
}

#[tokio::test]
async fn a_restart_gives_a_new_incarnation() {
    let r = rig().await;
    let (_ws, _) = r.socket(true).await;
    assert_eq!(r.control(r.select("s1")).await.status, 200);
    let first = r.remote(0).await;
    assert_eq!(first.body["seq"], 1);
    // A second board on the same database has its own relay.
    let c2 = common::board::board(std::sync::Arc::clone(&r.f.ctx)).await;
    let h2 = owner(&c2).await;
    let second = get(&c2, &h2, &format!("{}?after=0", r.path("remote"))).await;
    assert_eq!(second.status, 200, "{}", second.body);
    assert_ne!(second.body["incarnation"], first.body["incarnation"]);
    assert_eq!(second.body["seq"], 0);
    assert_eq!(second.body["link"], "offline");
    assert_eq!(second.body["complete"], true);
    assert_eq!(
        second.body["state"],
        json!({ "selected": null, "frame_tenths": 0 })
    );
}

#[tokio::test]
async fn revoke_ends_the_relay() {
    let r = rig().await;
    let (mut ws, _) = r.socket(true).await;
    let revoked = post(&r.c, &r.h, &r.path("revoke"), json!({})).await;
    assert_eq!(revoked.status, 204, "{}", revoked.body);
    expect(
        &r.control(json!({ "op": "scroll", "area": "chat", "step": "top" }))
            .await,
        404,
        "display_unknown",
    );
    expect(
        &r.navigate(json!({ "mode": "ambient" })).await,
        404,
        "display_unknown",
    );
    expect(&r.remote(0).await, 404, "display_unknown");
    // The socket ends.
    while ws.next().await.is_ok() {}
}

#[tokio::test]
async fn one_audit_row_for_a_burst() {
    let r = rig().await;
    let (_ws, _) = r.socket(true).await;
    for step in ["line_down", "line_up", "page_down", "top", "bottom"] {
        let sent = r
            .control(json!({ "op": "scroll", "area": "needs", "step": step }))
            .await;
        assert_eq!(sent.status, 200, "{}", sent.body);
    }
    assert_eq!(r.navigate(json!({ "mode": "ambient" })).await.status, 200);
    let rows = audits(&r, "display.remote").await;
    assert_eq!(rows, [Some("start".to_owned())]);
    // A refused control writes no `display.remote` row.
    expect(
        &r.control(json!({ "op": "select", "item": { "station_id": "x", "session_id": "y" } }))
            .await,
        400,
        "invalid_control",
    );
    assert_eq!(audits(&r, "display.remote").await.len(), 1);
    // After ten quiet minutes the next control starts a new session.
    r.c.state
        .wall_controls
        .advance_clock(Duration::from_secs(601));
    let sent = r
        .control(json!({ "op": "scroll", "area": "needs", "step": "top" }))
        .await;
    assert_eq!(sent.status, 200, "{}", sent.body);
    assert_eq!(audits(&r, "display.remote").await.len(), 2);
}

#[tokio::test]
async fn a_control_body_above_512_bytes_is_refused() {
    let r = rig().await;
    let big = json!({ "control": { "op": "select",
        "item": { "station_id": "x".repeat(600), "session_id": "y" } } });
    let sent = post(&r.c, &r.h, &r.path("control"), big).await;
    expect(&sent, 413, "too_large");
}

#[tokio::test]
async fn the_remote_query_must_be_a_number() {
    let r = rig().await;
    let bad = get(&r.c, &r.h, &r.path("remote?after=abc")).await;
    expect(&bad, 400, "invalid_query");
    let none = get(&r.c, &r.h, &r.path("remote")).await;
    assert_eq!(none.status, 200, "{}", none.body);
    assert_eq!(none.body["directive"]["generation"], 0);
    assert_eq!(none.body["directive"]["directive"]["mode"], "mission");
}
