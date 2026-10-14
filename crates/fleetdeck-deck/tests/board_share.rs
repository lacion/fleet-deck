//! The shared fleet (R11, DL-166) over loopback HTTPS: an owner makes a share
//! with an invite, a person joins as a member, and the member sees and does
//! what its role and its share allow and nothing else. A revoke ends the
//! member at once. Mob steering and the side chat work for the owner and the
//! members.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod share_support;

use std::collections::BTreeSet;

use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::link::Scope;
use serde_json::{Value, json};
use share_support::*;

#[tokio::test]
async fn a_member_joins_with_a_one_use_invite_and_sees_only_its_session() {
    let (f, _station, c, h) = setup().await;
    let st = f.station.0.clone();
    let r = post(
        &c,
        &h,
        "/api/shares",
        json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let url = r.body["url"].as_str().unwrap();
    assert!(url.starts_with("https://"), "{url}");
    let token = url.split("#join=").nth(1).unwrap().to_owned();
    // The deck never stores the token itself.
    assert!(!format!("{:?}", f.store.shares().await.unwrap()).contains(&token));

    // The wrong token, and a bad display name, change nothing.
    let bad = c
        .post(
            "/api/auth/join",
            &[],
            json!({ "token": "nope", "display_name": "Ann" }),
        )
        .await;
    expect(&bad, 401, "invalid_invite");
    let bad = c
        .post(
            "/api/auth/join",
            &[],
            json!({ "token": token, "display_name": "  " }),
        )
        .await;
    expect(&bad, 400, "invalid_display_name");

    let j = c
        .post(
            "/api/auth/join",
            &[],
            json!({ "token": token, "display_name": "Ann" }),
        )
        .await;
    assert_eq!(j.status, 200, "{}", j.body);
    assert_eq!(j.body["actor"], "member:Ann");
    assert_eq!(j.body["role"], "viewer");
    assert_eq!(j.body["scope"]["kind"], "session");
    let cookie = j.header("set-cookie").unwrap();
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("Secure"),
        "{cookie}"
    );
    // One use: the same token does not join again.
    let again = c
        .post(
            "/api/auth/join",
            &[],
            json!({ "token": token, "display_name": "Bob" }),
        )
        .await;
    expect(&again, 401, "invalid_invite");

    let m: Headers = vec![(
        "Cookie".to_owned(),
        cookie.split(';').next().unwrap().to_owned(),
    )];
    let sessions = get(&c, &m, "/api/sessions").await;
    let ids: Vec<&str> = sessions
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["summary"]["session_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["s1"]);
    let me = get(&c, &m, "/api/auth/session").await;
    assert_eq!(me.body["actor"], "member:Ann");
    // The stations of the deck are not shown, only the host of s1, without
    // its key and address.
    let stations = get(&c, &m, "/api/stations").await;
    assert_eq!(stations.body.as_array().unwrap().len(), 1);
    assert_eq!(stations.body[0]["key"], "");
    assert_eq!(c.audited("auth.join", AuditResult::Ok).await.len(), 1);
}

#[tokio::test]
async fn a_member_never_reaches_an_owner_only_route() {
    let (f, _station, c, h) = setup().await;
    let st = f.station.0.clone();
    let (m, _, _) = join(
        &c,
        &h,
        json!({ "kind": "board", "role": "approver" }),
        "Ann",
    )
    .await;
    for (method, path) in [
        ("GET", "/api/shares"),
        ("POST", "/api/shares"),
        ("GET", "/api/spaces"),
        ("POST", "/api/spaces"),
        ("GET", "/api/audit"),
        ("GET", "/api/commands"),
        ("GET", "/api/conflicts"),
        ("GET", "/api/archive"),
        ("GET", "/api/plans"),
        ("GET", "/api/tasks"),
        ("POST", "/api/tasks"),
        ("POST", "/api/assign"),
        ("POST", "/api/stations"),
        ("POST", "/api/stations/invite"),
        ("GET", &format!("/api/stations/{st}/workspaces")),
        ("GET", &format!("/api/stations/{st}/worktrees")),
        ("GET", &format!("/api/stations/{st}/shells")),
        ("POST", &format!("/api/stations/{st}/paste")),
        ("GET", "/api/coordinator/grants"),
        ("POST", "/api/auth/device-link"),
    ] {
        let body = (method == "POST").then(|| json!({}));
        let r = c.req(method, path, &refs(&m), body).await;
        expect(&r, 403, "owner_only");
    }
    // Commands that a member never sends, also as an approver.
    for op in [
        json!({ "op": "set_mode", "session_id": "s1", "mode": "plan" }),
        json!({ "op": "set_model", "session_id": "s1", "model": "x" }),
        json!({ "op": "stop", "session_id": "s1", "how": "graceful" }),
        json!({ "op": "fork", "session_id": "s1" }),
        json!({ "op": "rename", "session_id": "s1", "title": "x" }),
    ] {
        let method = match op["op"].as_str().unwrap() {
            "set_mode" => "session.set_mode",
            "set_model" => "session.set_model",
            "stop" => "session.stop",
            "fork" => "session.fork",
            _ => "session.rename",
        };
        let r = post(
            &c,
            &m,
            &format!("/api/commands/{method}"),
            json!({ "station": st, "command_id": "c-x", "command": op }),
        )
        .await;
        expect(&r, 403, "owner_only");
    }
}

#[tokio::test]
async fn a_collaborator_prompts_with_its_name_and_a_viewer_cannot() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let (m, _, _) = join(
        &c,
        &h,
        json!({ "kind": "session", "target": format!("{st}/s1"), "role": "collaborator" }),
        "Ann",
    )
    .await;
    let (r, req) = send_ok(
        &c,
        &m,
        &mut station,
        "session.prompt",
        prompt(&st, "c1", "s1"),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["author"], "member:Ann");
    assert_eq!(req.method, "session.prompt");
    assert_eq!(req.params["command"]["input"]["author"], "member:Ann");
    // A forged author in the body is replaced.
    let mut forged = prompt(&st, "c2", "s1");
    forged["command"]["input"]["author"] = json!("owner");
    let (r, req) = send_ok(&c, &m, &mut station, "session.prompt", forged).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.params["command"]["input"]["author"], "member:Ann");
    // The owner's input carries no author, whatever the body says.
    let mut owner_body = prompt(&st, "c3", "s1");
    owner_body["command"]["input"]["author"] = json!("member:Zed");
    let (r, req) = send_ok(&c, &h, &mut station, "session.prompt", owner_body).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(req.params["command"]["input"].get("author").is_none());
    // Another session of the deck is unknown to the member.
    let r = post(
        &c,
        &m,
        "/api/commands/session.prompt",
        prompt(&st, "c4", "s2"),
    )
    .await;
    expect(&r, 404, "unknown_session");
    let r = post(
        &c,
        &m,
        "/api/commands/session.prompt",
        prompt("nope", "c5", "s1"),
    )
    .await;
    expect(&r, 404, "unknown_session");
    // A member polls its own command only.
    let own = get(&c, &m, "/api/commands/c1").await;
    assert_eq!(own.status, 200);
    expect(
        &get(&c, &m, "/api/commands/c3").await,
        404,
        "unknown_command",
    );
    // No CSRF token, no command.
    let no_csrf: Headers = vec![m[0].clone()];
    let r = post(
        &c,
        &no_csrf,
        "/api/commands/session.prompt",
        prompt(&st, "c6", "s1"),
    )
    .await;
    expect(&r, 403, "csrf");

    let (v, _, _) = join(
        &c,
        &h,
        json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" }),
        "Vic",
    )
    .await;
    let r = post(
        &c,
        &v,
        "/api/commands/session.prompt",
        prompt(&st, "c7", "s1"),
    )
    .await;
    expect(&r, 403, "forbidden");
    assert_eq!(
        c.audited("session.prompt", AuditResult::Refused)
            .await
            .len(),
        4
    );
}

#[tokio::test]
async fn only_an_approver_answers_a_tool_approval() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let (collab, _, _) = join(
        &c,
        &h,
        json!({ "kind": "session", "target": format!("{st}/s1"), "role": "collaborator" }),
        "Ann",
    )
    .await;
    let answer = json!({ "station": st, "command_id": "a1",
        "command": { "op": "answer", "session_id": "s1", "request_id": "r1",
                     "decision": { "option_id": "allow", "updated_input": null, "text": null } } });
    // The deck does not know the request: it counts as a tool approval.
    let r = post(&c, &collab, "/api/commands/approval.answer", answer.clone()).await;
    expect(&r, 403, "forbidden");
    let (approver, _, _) = join(
        &c,
        &h,
        json!({ "kind": "session", "target": format!("{st}/s1"), "role": "approver" }),
        "Bea",
    )
    .await;
    let (r, req) = send_ok(&c, &approver, &mut station, "approval.answer", answer).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["author"], "member:Bea");
    assert_eq!(
        req.params["command"]["decision"]["answered_by"],
        "member:Bea"
    );
}

#[tokio::test]
async fn a_revoke_ends_the_member_at_once() {
    let (f, _station, c, h) = setup().await;
    let st = f.station.0.clone();
    let (m, share_id, _) = join(
        &c,
        &h,
        json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" }),
        "Ann",
    )
    .await;
    assert_eq!(get(&c, &m, "/api/auth/session").await.status, 200);
    let shares = get(&c, &h, "/api/shares").await;
    assert_eq!(shares.body[0]["members"], json!(["Ann"]));
    let r = post(&c, &h, &format!("/api/shares/{share_id}/revoke"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body["revoked_at"].is_number());
    expect(
        &get(&c, &m, "/api/auth/session").await,
        401,
        "login_required",
    );
    expect(
        &post(&c, &h, "/api/shares/nope/revoke", json!({})).await,
        404,
        "unknown_share",
    );
    // The member cannot revoke, and a logout ends a login by itself.
    let (m2, _, _) = join(&c, &h, json!({ "kind": "board", "role": "viewer" }), "Cy").await;
    let out = post(&c, &m2, "/api/auth/logout", json!({})).await;
    assert_eq!(out.status, 204);
    expect(
        &get(&c, &m2, "/api/auth/session").await,
        401,
        "login_required",
    );
}

#[tokio::test]
async fn a_share_has_a_lifetime_and_a_use_count() {
    let (f, _station, c, h) = setup().await;
    let st = f.station.0.clone();
    for (body, code) in [
        (
            json!({ "kind": "board", "role": "viewer", "expires_in_days": 0 }),
            "invalid_share",
        ),
        (
            json!({ "kind": "board", "role": "viewer", "expires_in_days": 91 }),
            "invalid_share",
        ),
        (
            json!({ "kind": "board", "role": "viewer", "uses": 0 }),
            "invalid_share",
        ),
        (
            json!({ "kind": "board", "role": "viewer", "target": "x" }),
            "invalid_target",
        ),
        (
            json!({ "kind": "session", "role": "viewer", "target": "nope" }),
            "invalid_target",
        ),
        (
            json!({ "kind": "session", "role": "viewer", "target": format!("{st}/ghost") }),
            "unknown_session",
        ),
        (
            json!({ "kind": "space", "role": "viewer", "target": "ghost" }),
            "unknown_space",
        ),
        (json!({ "kind": "board", "role": "root" }), "invalid_body"),
    ] {
        let r = post(&c, &h, "/api/shares", body).await;
        assert_eq!(r.code(), code, "{}", r.body);
    }
    // Two uses: two people join, the third is refused. After the lifetime the
    // invite and the member login end.
    let r = post(
        &c,
        &h,
        "/api/shares",
        json!({ "kind": "board", "role": "viewer", "uses": 2, "expires_in_days": 1 }),
    )
    .await;
    let token = r.body["url"]
        .as_str()
        .unwrap()
        .split("#join=")
        .nth(1)
        .unwrap()
        .to_owned();
    let mut cookies = Vec::new();
    for name in ["Ann", "Bob"] {
        let j = c
            .post(
                "/api/auth/join",
                &[],
                json!({ "token": token, "display_name": name }),
            )
            .await;
        assert_eq!(j.status, 200, "{}", j.body);
        cookies.push(
            j.header("set-cookie")
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned(),
        );
    }
    let third = c
        .post(
            "/api/auth/join",
            &[],
            json!({ "token": token, "display_name": "Cy" }),
        )
        .await;
    expect(&third, 401, "invalid_invite");
    f.det
        .advance(std::time::Duration::from_millis(2 * 24 * 60 * 60 * 1000));
    let m: Headers = vec![("Cookie".to_owned(), cookies[0].clone())];
    expect(
        &get(&c, &m, "/api/auth/session").await,
        401,
        "login_required",
    );
}

#[tokio::test]
async fn a_space_share_follows_the_sessions_of_the_space() {
    let (f, _station, c, h) = setup().await;
    let st = f.station.0.clone();
    let space = post(&c, &h, "/api/spaces", json!({ "name": "auth refactor" })).await;
    assert_eq!(space.status, 200, "{}", space.body);
    let space_id = space.body["space_id"].as_str().unwrap().to_owned();
    let (m, _, _) = join(
        &c,
        &h,
        json!({ "kind": "space", "target": space_id, "role": "viewer" }),
        "Ann",
    )
    .await;
    assert_eq!(
        get(&c, &m, "/api/sessions")
            .await
            .body
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let r = post(
        &c,
        &h,
        &format!("/api/spaces/{space_id}/sessions"),
        json!({ "station": st, "session_id": "s2" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["sessions"].as_array().unwrap().len(), 1);
    let sessions = get(&c, &m, "/api/sessions").await;
    assert_eq!(sessions.body[0]["summary"]["session_id"], "s2");
    expect(
        &post(
            &c,
            &h,
            "/api/spaces/ghost/sessions",
            json!({ "station": st, "session_id": "s2" }),
        )
        .await,
        404,
        "unknown_space",
    );
    assert_eq!(
        get(&c, &h, "/api/spaces").await.body[0]["name"],
        "auth refactor"
    );
}

#[tokio::test]
async fn the_side_chat_is_for_people_and_never_reaches_the_agent() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let (m, _, _) = join(
        &c,
        &h,
        json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" }),
        "Ann",
    )
    .await;
    let path = format!("/api/sessions/{st}/s1/chat");
    let r = post(&c, &m, &path, json!({ "text": "  looks good  " })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        (r.body["seq"].as_u64(), r.body["author"].as_str()),
        (Some(1), Some("member:Ann"))
    );
    assert_eq!(r.body["text"], "looks good");
    post(&c, &h, &path, json!({ "text": "thanks" })).await;
    let list = get(&c, &m, &path).await;
    assert_eq!(list.body.as_array().unwrap().len(), 2);
    assert_eq!(list.body[1]["author"], "owner");
    let after = get(&c, &m, &format!("{path}?after=1")).await;
    assert_eq!(after.body.as_array().unwrap().len(), 1);
    expect(
        &post(&c, &m, &path, json!({ "text": " " })).await,
        400,
        "invalid_text",
    );
    expect(
        &get(&c, &m, &format!("/api/sessions/{st}/s2/chat")).await,
        404,
        "unknown_session",
    );
    // Nothing went to the station.
    no_request(&mut station, std::time::Duration::from_millis(300)).await;
}

#[tokio::test]
async fn a_bearer_token_cannot_read_or_post_in_the_side_chat() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let path = format!("/api/sessions/{st}/s1/chat");
    let day = 86_400_000;
    let (_, read) = f
        .store
        .create_token("reader".into(), BTreeSet::from([Scope::Read]), day)
        .await
        .unwrap();
    let (_, drive) = f
        .store
        .create_token(
            "driver".into(),
            BTreeSet::from([Scope::Read, Scope::Drive]),
            day,
        )
        .await
        .unwrap();
    let read_auth = format!("Bearer {}", read.expose());
    let drive_auth = format!("Bearer {}", drive.expose());
    let r = c.get(&path, &[("Authorization", read_auth.as_str())]).await;
    expect(&r, 403, "owner_only");
    let r = c
        .post(
            &path,
            &[("Authorization", drive_auth.as_str())],
            json!({ "text": "hello" }),
        )
        .await;
    expect(&r, 403, "owner_only");
    assert_eq!(
        c.audited("chat.post", AuditResult::Refused).await.len(),
        1,
        "the refusal is audited"
    );
    // The owner still reads and posts.
    let r = post(&c, &h, &path, json!({ "text": "from the owner" })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let list = get(&c, &h, &path).await;
    assert_eq!(list.body.as_array().unwrap().len(), 1);
    no_request(&mut station, std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn mob_steering_lets_the_driver_steer_and_the_owner_hand_on() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let share = json!({ "kind": "session", "target": format!("{st}/s1"),
                        "role": "collaborator", "steering": "mob" });
    let (ann, _, _) = join(&c, &h, share.clone(), "Ann").await;
    let (bob, _, _) = join(&c, &h, share, "Bob").await;
    let base = format!("/api/sessions/{st}/s1/drive");
    // Nobody drives: a prompt of a member is refused.
    let r = post(
        &c,
        &ann,
        "/api/commands/session.prompt",
        prompt(&st, "m1", "s1"),
    )
    .await;
    expect(&r, 403, "not_driver");
    // The first request drives at once, the second waits.
    let r = post(&c, &ann, &format!("{base}/request"), json!({})).await;
    assert_eq!(r.body["driver"], "member:Ann", "{}", r.body);
    let r = post(&c, &bob, &format!("{base}/request"), json!({})).await;
    assert_eq!(r.body["driver"], "member:Ann");
    assert_eq!(r.body["requests"], json!(["member:Bob"]));
    let r = post(
        &c,
        &bob,
        "/api/commands/session.prompt",
        prompt(&st, "m2", "s1"),
    )
    .await;
    expect(&r, 403, "not_driver");
    let (r, _) = send_ok(
        &c,
        &ann,
        &mut station,
        "session.prompt",
        prompt(&st, "m3", "s1"),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // The owner always steers, and a mail steers too, so Bob's mail is refused.
    let (r, _) = send_ok(
        &c,
        &h,
        &mut station,
        "session.prompt",
        prompt(&st, "m4", "s1"),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let mail = json!({ "mail_id": "ml1", "station": st, "session_id": "s1", "text": "x" });
    expect(&post(&c, &bob, "/api/mail", mail).await, 403, "not_driver");
    // Only the driver or the owner hands on; Bob cannot take it.
    let r = post(
        &c,
        &bob,
        &format!("{base}/grant"),
        json!({ "to": "member:Bob" }),
    )
    .await;
    expect(&r, 403, "forbidden");
    let r = post(
        &c,
        &ann,
        &format!("{base}/grant"),
        json!({ "to": "member:Bob" }),
    )
    .await;
    assert_eq!(r.body["driver"], "member:Bob", "{}", r.body);
    assert_eq!(r.body["requests"], json!([]));
    let r = post(
        &c,
        &ann,
        &format!("{base}/grant"),
        json!({ "to": "member:Nobody" }),
    )
    .await;
    expect(&r, 403, "forbidden");
    let r = post(
        &c,
        &h,
        &format!("{base}/grant"),
        json!({ "to": "member:Nobody" }),
    )
    .await;
    expect(&r, 400, "invalid_driver");
    let r = post(&c, &h, &format!("{base}/grant"), json!({ "to": "owner" })).await;
    assert_eq!(r.body["driver"], "owner");
    let r = post(&c, &h, &format!("{base}/release"), json!({})).await;
    assert_eq!(r.body["driver"], Value::Null);
}

#[tokio::test]
async fn a_member_reads_files_of_its_session_only() {
    let (f, _station, c, h) = setup().await;
    let st = f.station.0.clone();
    let (m, _, _) = join(
        &c,
        &h,
        json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" }),
        "Ann",
    )
    .await;
    // A workspace root is not a session root.
    let r = get(
        &c,
        &m,
        &format!("/api/stations/{st}/files/list?workspace=%2Fp%2Fapp"),
    )
    .await;
    expect(&r, 403, "not_shared");
    // Another session of the deck is unknown to the member.
    let r = get(&c, &m, &format!("/api/stations/{st}/files/list?session=s2")).await;
    expect(&r, 404, "unknown_session");
    let r = get(
        &c,
        &m,
        &format!("/api/stations/{st}/sessions/s2/turns/t1/diff"),
    )
    .await;
    expect(&r, 404, "unknown_session");
}

#[tokio::test]
async fn the_member_socket_shows_its_session_and_presence() {
    let (f, _station, c, h) = setup().await;
    let st = f.station.0.clone();
    let (m, _, _) = join(
        &c,
        &h,
        json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" }),
        "Ann",
    )
    .await;
    let origin = c.origin();
    let mut ws = c
        .ws("/api/ws", &[("Cookie", &m[0].1), ("Origin", &origin)])
        .await
        .expect("the member socket opens");
    assert_eq!(ws.next().await.unwrap()["type"], "hello");
    let snap = ws.next().await.unwrap();
    assert_eq!(snap["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(snap["sessions"][0]["summary"]["session_id"], "s1");
    assert_eq!(snap["stations"].as_array().unwrap().len(), 1);
    // A session outside the share cannot be followed.
    ws.send(
        json!({ "type": "subscribe", "station_id": st, "session_id": "s2",
                    "incarnation": null, "after": null }),
    )
    .await;
    assert_eq!(ws.next_of("error").await["code"], "unknown_session");
    // A heartbeat for the shared session shows the member to its viewers.
    ws.send(json!({ "type": "presence", "station_id": st, "session_id": "s1" }))
        .await;
    let p = ws.next_of("presence").await;
    assert_eq!(p["who"], json!(["member:Ann"]));
    // The chat reaches the socket.
    let r = post(
        &c,
        &h,
        &format!("/api/sessions/{st}/s1/chat"),
        json!({ "text": "hi" }),
    )
    .await;
    assert_eq!(r.status, 200);
    assert_eq!(ws.next_of("chat").await["message"]["author"], "owner");
}
