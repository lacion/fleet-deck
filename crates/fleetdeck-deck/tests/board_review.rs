//! Review comments on the Changes diff over loopback HTTPS (DL-179, design
//! `review-comments.md`): the principal matrix, the anchors that the deck
//! reads from the diff of the caller, the turn diff by checkpoint pair and
//! the member deny list. Send and import are in `board_review_send.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod review_support;

use std::time::Duration;

use fleetdeck_proto::link::Scope;
use review_support::*;
use serde_json::{Value, json};

// ---- principals ----------------------------------------------------------

#[tokio::test]
async fn every_route_follows_the_principal_matrix() {
    let t = setup().await;
    let base = t.base();
    // The owner writes and reads.
    let c = t.comment(&t.h, "src/lib.rs", "new", 2, "use a const").await;
    assert_eq!(c.status, 201, "{}", c.body);
    let cid = id(&c);
    assert_eq!(c.body["state"], "draft");
    assert_eq!(c.body["author_kind"], "owner");
    assert!(c.body["short_id"].as_str().unwrap().starts_with("c-"));
    let sent = t.send(&t.h, "k1", &[&cid]).await;
    assert_eq!(sent.status, 200, "{}", sent.body);
    t.wait_follow_ups(1).await;
    // A draft of the owner for the later checks.
    let draft = id(&t.comment(&t.h, "src/lib.rs", "new", 1, "draft").await);

    // A member who may follow up writes and reads; its draft is private.
    let ann = member(&t, "collaborator", "Ann").await;
    let ac = t.comment(&ann, "src/lib.rs", "new", 3, "from Ann").await;
    assert_eq!(ac.status, 201, "{}", ac.body);
    assert_eq!(
        (
            ac.body["author_kind"].as_str(),
            ac.body["author_name"].as_str()
        ),
        (Some("member"), Some("Ann"))
    );
    let seen = ids(&t.list(&ann).await);
    assert!(seen.contains(&cid) && seen.contains(&id(&ac)) && !seen.contains(&draft));
    assert!(!ids(&t.list(&t.h).await).contains(&id(&ac)));

    // A member who cannot follow up (a viewer, a share guest) reads sent
    // comments and writes nothing.
    let guest = member(&t, "viewer", "Gus").await;
    let g = t.list(&guest).await;
    assert_eq!(ids(&g), vec![cid.clone()]);
    expect(
        &t.comment(&guest, "src/lib.rs", "new", 1, "x").await,
        403,
        "forbidden",
    );
    expect(
        &t.patch(
            &guest,
            &format!("{base}/comments/{cid}"),
            json!({"resolved": true}),
        )
        .await,
        403,
        "forbidden",
    );
    expect(
        &t.delete(&guest, &format!("{base}/comments/{cid}")).await,
        403,
        "forbidden",
    );
    expect(&t.send(&guest, "k", &[&cid]).await, 403, "forbidden");
    expect(&t.again(&guest, "r", "a").await, 403, "forbidden");

    // A token with `read` reads sent comments only and never writes.
    let (_, tok) =
        t.f.store
            .create_token("reader".into(), [Scope::Read].into(), 86_400_000)
            .await
            .unwrap();
    let bearer = vec![(
        "Authorization".to_owned(),
        format!("Bearer {}", tok.expose()),
    )];
    assert_eq!(ids(&t.list(&bearer).await), vec![cid.clone()]);
    // A token with `drive` but not `read` fails the scope check; with both it
    // reaches the handler and is refused there.
    let (_, drive) =
        t.f.store
            .create_token("driver".into(), [Scope::Drive].into(), 86_400_000)
            .await
            .unwrap();
    let drive_h = vec![(
        "Authorization".to_owned(),
        format!("Bearer {}", drive.expose()),
    )];
    expect(&t.list(&drive_h).await, 403, "forbidden");
    expect(
        &t.comment(&bearer, "src/lib.rs", "new", 1, "x").await,
        403,
        "forbidden",
    );
    let (_, both) =
        t.f.store
            .create_token(
                "both".into(),
                [Scope::Read, Scope::Drive].into(),
                86_400_000,
            )
            .await
            .unwrap();
    let both_h = vec![(
        "Authorization".to_owned(),
        format!("Bearer {}", both.expose()),
    )];
    expect(
        &t.comment(&both_h, "src/lib.rs", "new", 1, "x").await,
        403,
        "owner_or_member_only",
    );
    expect(
        &t.patch(
            &both_h,
            &format!("{base}/comments/{cid}"),
            json!({"resolved": true}),
        )
        .await,
        403,
        "owner_or_member_only",
    );
    expect(
        &t.delete(&both_h, &format!("{base}/comments/{cid}")).await,
        403,
        "owner_or_member_only",
    );
    expect(
        &t.send(&both_h, "k", &[&cid]).await,
        403,
        "owner_or_member_only",
    );
    expect(
        &t.again(&both_h, "r", "a").await,
        403,
        "owner_or_member_only",
    );

    // No login, and a cookie login without the CSRF header.
    expect(&t.get(&Vec::new(), &base).await, 401, "login_required");
    let cookie_only: Headers = vec![t.h[0].clone()];
    expect(
        &t.comment(&cookie_only, "src/lib.rs", "new", 1, "x").await,
        403,
        "csrf",
    );
    expect(
        &t.patch(
            &cookie_only,
            &format!("{base}/comments/{cid}"),
            json!({"resolved": true}),
        )
        .await,
        403,
        "csrf",
    );
    expect(
        &t.delete(&cookie_only, &format!("{base}/comments/{draft}"))
            .await,
        403,
        "csrf",
    );
    expect(&t.send(&cookie_only, "k", &[&draft]).await, 403, "csrf");
    // The share covers s1 only: another session is unknown to the member.
    let other = format!("/api/sessions/{}/s2/review", t.st());
    expect(&t.get(&ann, &other).await, 404, "unknown_session");
    expect(
        &t.get(&t.h, &format!("/api/sessions/{}/nope/review", t.st()))
            .await,
        404,
        "unknown_session",
    );
    expect(
        &t.get(&t.h, "/api/sessions/nostation/s1/review").await,
        404,
        "unknown_station",
    );
    // The audit log names ids and never a body.
    let log = t.f.store.audit_log(100).await.unwrap();
    let actions: Vec<&str> = log.iter().map(|e| e.action.as_str()).collect();
    assert!(actions.contains(&"review.comment") && actions.contains(&"review.send"));
    assert!(log.iter().all(|e| {
        e.detail
            .as_deref()
            .is_none_or(|d| !d.contains("use a const"))
    }));
}

#[tokio::test]
async fn another_authors_draft_is_unknown_to_edit_delete_resolve_and_absent_from_the_list() {
    let t = setup().await;
    let base = t.base();
    let ann = member(&t, "collaborator", "Ann").await;
    let theirs = id(&t.comment(&ann, "src/lib.rs", "new", 1, "Ann's draft").await);
    let mine = id(&t.comment(&t.h, "src/lib.rs", "new", 2, "my draft").await);
    for (h, other) in [(&t.h, &theirs), (&ann, &mine)] {
        let path = format!("{base}/comments/{other}");
        expect(
            &t.patch(h, &path, json!({ "body": "hijack" })).await,
            404,
            "unknown_comment",
        );
        expect(
            &t.patch(h, &path, json!({ "resolved": true })).await,
            404,
            "unknown_comment",
        );
        expect(&t.delete(h, &path).await, 404, "unknown_comment");
        // A reply to a draft of someone else, and a send of it.
        let reply = t
            .post(
                h,
                &format!("{base}/comments"),
                json!({
                "snapshot": format!("spawn:{BASE}"), "path": "src/lib.rs", "side": "new",
                "line_start": 1, "line_end": 1, "body": "re", "thread_id": other }),
            )
            .await;
        expect(&reply, 404, "unknown_comment");
        expect(&t.send(h, "kk", &[other]).await, 404, "unknown_comment");
    }
    assert_eq!(ids(&t.list(&t.h).await), vec![mine.clone()]);
    assert_eq!(ids(&t.list(&ann).await), vec![theirs.clone()]);
    // The author edits and resolves the own draft, in any of the forms.
    let ok = t
        .patch(
            &t.h,
            &format!("{base}/comments/{mine}"),
            json!({ "body": "better", "resolved": true }),
        )
        .await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert_eq!(
        (ok.body["body"].as_str(), ok.body["resolved"].as_bool()),
        (Some("better"), Some(true))
    );
    // Sent: anyone who writes may resolve; the body is not editable.
    t.send(&t.h, "k1", &[&mine]).await;
    let by_short = ok.body["short_id"].as_str().unwrap().to_owned();
    let r = t
        .patch(
            &ann,
            &format!("{base}/comments/{by_short}"),
            json!({ "resolved": false }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    expect(
        &t.patch(
            &ann,
            &format!("{base}/comments/{mine}"),
            json!({ "body": "x" }),
        )
        .await,
        403,
        "not_author",
    );
    expect(
        &t.patch(
            &t.h,
            &format!("{base}/comments/{mine}"),
            json!({ "body": "x" }),
        )
        .await,
        409,
        "not_draft",
    );
    expect(
        &t.delete(&t.h, &format!("{base}/comments/{mine}")).await,
        409,
        "not_draft",
    );
    expect(
        &t.delete(&ann, &format!("{base}/comments/{mine}")).await,
        403,
        "not_author",
    );
    // An empty patch, an unknown field and a bad body.
    for body in [
        json!({}),
        json!({ "author": "x" }),
        json!({ "body": "" }),
        json!({ "body": "x".repeat(8001) }),
    ] {
        expect(
            &t.patch(&t.h, &format!("{base}/comments/{mine}"), body)
                .await,
            400,
            "invalid_body",
        );
    }
    // A delete answers 204 and the comment is gone.
    let d = t.delete(&ann, &format!("{base}/comments/{theirs}")).await;
    assert_eq!(d.status, 204);
    assert!(ids(&t.list(&ann).await).iter().all(|i| *i != theirs));
}

// ---- anchors -------------------------------------------------------------

#[tokio::test]
async fn the_deck_derives_the_anchor_and_refuses_what_the_diff_does_not_show() {
    let t = setup().await;
    let c = t.comment(&t.h, "src/lib.rs", "new", 2, "x").await;
    assert_eq!(c.body["anchor_text"], "fn two() { 2 }");
    assert_eq!(
        (c.body["anchor_kind"].as_str(), c.body["snapshot"].as_str()),
        (Some("diff"), Some(format!("spawn:{BASE}").as_str()))
    );
    assert!(c.body.get("other_path").is_none());
    // A range, the old side, and a rename with its other path.
    let r = t
        .post(
            &t.h,
            &format!("{}/comments", t.base()),
            json!({
            "snapshot": format!("spawn:{BASE}"), "path": "src/lib.rs", "side": "new",
            "line_start": 2, "line_end": 3, "body": "range" }),
        )
        .await;
    assert_eq!(r.body["anchor_text"], "fn two() { 2 }\nfn three() {}");
    let old = t.comment(&t.h, "src/lib.rs", "old", 2, "old").await;
    assert_eq!(old.body["anchor_text"], "fn two() {}");
    let renamed = t.comment(&t.h, "renamed.txt", "new", 2, "x").await;
    assert_eq!(
        (
            renamed.body["anchor_text"].as_str(),
            renamed.body["other_path"].as_str()
        ),
        (Some("password=hunter3"), Some("secrets.txt"))
    );
    let renamed_old = t.comment(&t.h, "secrets.txt", "old", 2, "x").await;
    assert_eq!(renamed_old.body["other_path"], "renamed.txt");
    // Not in the diff: a path, a line, a side, a range across the cut.
    for (path, side, line) in [
        ("src/other.rs", "new", 1),
        ("src/lib.rs", "new", 9),
        ("src/lib.rs", "old", 4),
        ("src/lib.rs", "new", 5),
    ] {
        expect(
            &t.comment(&t.h, path, side, line, "x").await,
            400,
            "not_in_diff",
        );
    }
    // A client anchor text is an unknown field.
    let forged = t
        .post(
            &t.h,
            &format!("{}/comments", t.base()),
            json!({
            "snapshot": format!("spawn:{BASE}"), "path": "src/lib.rs", "side": "new",
            "line_start": 2, "line_end": 2, "body": "x", "anchor_text": "forged" }),
        )
        .await;
    expect(&forged, 400, "invalid_body");
    // Another base commit than the session has.
    let other = t
        .post(
            &t.h,
            &format!("{}/comments", t.base()),
            json!({
            "snapshot": format!("spawn:{OTHER}"), "path": "src/lib.rs", "side": "new",
            "line_start": 2, "line_end": 2, "body": "x" }),
        )
        .await;
    expect(&other, 400, "not_in_diff");
}

#[tokio::test]
async fn the_shape_of_a_new_comment_is_checked_before_the_diff_is_asked() {
    let t = setup().await;
    let base = format!("{}/comments", t.base());
    let ok = json!({ "snapshot": format!("spawn:{BASE}"), "path": "src/lib.rs", "side": "new",
                     "line_start": 2, "line_end": 2, "body": "x" });
    let with = |k: &str, v: Value| {
        let mut b = ok.clone();
        b[k] = v;
        b
    };
    let bad = [
        with("path", json!("a\nb")),
        with("path", json!("../x")),
        with("path", json!("a/../x")),
        with("path", json!("/abs")),
        with("path", json!("a\u{0}b")),
        with("path", json!("")),
        with("path", json!("x".repeat(1025))),
        with("body", json!("")),
        with("body", json!("x".repeat(8001))),
        with("side", json!("left")),
        with("line_start", json!(0)),
        with("line_start", json!(3)),
        with("line_end", json!(202)),
        with("snapshot", json!("spawn:abc")),
        with("snapshot", json!(format!("turn:{FROM}"))),
        with("snapshot", json!(format!("other:{BASE}"))),
        with("import_key", json!("ABCDEF")),
        with("import_key", json!("A".repeat(64))),
        with("import_key", json!("a".repeat(63))),
    ];
    for body in bad {
        expect(
            &t.post(&t.h, &base, body.clone()).await,
            400,
            "invalid_body",
        );
    }
    // None of them reached the station.
    assert!(t.fake.calls("files.session_diff").is_empty());
    // 8000 characters and a range of 200 lines are the limits that pass the
    // shape check (the range is then not in the diff).
    let long = with("body", json!("é".repeat(8000)));
    assert_eq!(t.post(&t.h, &base, long).await.status, 201);
    let wide = {
        let mut b = ok.clone();
        b["line_end"] = json!(201);
        b
    };
    expect(&t.post(&t.h, &base, wide).await, 400, "not_in_diff");
    // An anchor over 4 KiB.
    t.fake.set(|c| {
        let mut p = String::from("diff --git a/big b/big\n--- a/big\n+++ b/big\n@@ -0,0 +1,6 @@\n");
        for _ in 0..6 {
            p.push_str(&format!("+{}\n", "x".repeat(1000)));
        }
        c.patch = p;
    });
    let big = t
        .post(
            &t.h,
            &base,
            json!({ "snapshot": format!("spawn:{BASE}"), "path": "big",
            "side": "new", "line_start": 1, "line_end": 5, "body": "x" }),
        )
        .await;
    expect(&big, 400, "anchor_too_large");
}

#[tokio::test]
async fn a_station_that_has_no_diff_or_no_link_is_reported() {
    let t = setup().await;
    t.fake.set(|c| c.session_error = Some("no_base"));
    expect(
        &t.comment(&t.h, "src/lib.rs", "new", 2, "x").await,
        400,
        "not_in_diff",
    );
    t.fake.set(|c| c.session_error = Some("unknown_session"));
    expect(
        &t.comment(&t.h, "src/lib.rs", "new", 2, "x").await,
        404,
        "unknown_session",
    );
    t.fake.set(|c| c.session_error = Some("weird"));
    expect(
        &t.comment(&t.h, "src/lib.rs", "new", 2, "x").await,
        502,
        "station_error",
    );
    t.fake.set(|c| c.session_error = None);
    // The diff is cached for 60 s: a second comment asks the station once.
    let before = t.fake.calls("files.session_diff").len();
    t.comment(&t.h, "src/lib.rs", "new", 1, "a").await;
    t.comment(&t.h, "src/lib.rs", "new", 2, "b").await;
    assert_eq!(t.fake.calls("files.session_diff").len(), before + 1);
    // Advance the deck clock past 60 s: the station is asked again.
    t.f.det.advance(Duration::from_secs(61));
    t.comment(&t.h, "src/lib.rs", "new", 3, "c").await;
    assert_eq!(t.fake.calls("files.session_diff").len(), before + 2);
}

// ---- turn diffs and the checkpoint selector ------------------------------

#[tokio::test]
async fn a_turn_comment_uses_the_checkpoint_pair_and_an_unknown_pair_is_unavailable() {
    let t = setup().await;
    let turn = format!("turn:{FROM}..{TO}");
    let r = t
        .post(
            &t.h,
            &format!("{}/comments", t.base()),
            json!({ "snapshot": turn, "path": "src/turn.rs",
            "side": "new", "line_start": 2, "line_end": 2, "body": "turn" }),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(
        (r.body["anchor_text"].as_str(), r.body["snapshot"].as_str()),
        (Some("new line of the turn"), Some(turn.as_str()))
    );
    let call = &t.fake.calls("files.diff")[0];
    assert_eq!(
        call["checkpoint"],
        json!({ "from_commit": FROM, "to_commit": TO })
    );
    assert!(call.get("turn_id").is_none(), "{call}");
    assert!(call.get("member").is_none());
    // Another pair: the station has no such row.
    let none = t
        .post(
            &t.h,
            &format!("{}/comments", t.base()),
            json!({ "snapshot": format!("turn:{FROM}..{OTHER}"),
            "path": "src/turn.rs", "side": "new", "line_start": 2, "line_end": 2, "body": "x" }),
        )
        .await;
    expect(&none, 404, "checkpoint_unavailable");
    // A station that refuses the selector (an older build).
    t.fake.set(|c| c.refuse_selector = true);
    expect(
        &t.post(
            &t.h,
            &format!("{}/comments", t.base()),
            json!({ "snapshot": turn,
            "path": "src/turn.rs", "side": "new", "line_start": 2, "line_end": 2, "body": "x" }),
        )
        .await,
        404,
        "checkpoint_unavailable",
    );
    t.fake.set(|c| c.refuse_selector = false);
    // The route of the pair.
    let st = t.st();
    let path = |from: &str, to: &str| {
        format!("/api/stations/{st}/sessions/s1/turn-diff?from={from}&to={to}")
    };
    let d = t.get(&t.h, &path(FROM, TO)).await;
    assert_eq!(d.status, 200, "{}", d.body);
    assert_eq!(
        (d.body["from_commit"].as_str(), d.body["to_commit"].as_str()),
        (Some(FROM), Some(TO))
    );
    expect(
        &t.get(&t.h, &path(FROM, OTHER)).await,
        404,
        "checkpoint_unavailable",
    );
    expect(&t.get(&t.h, &path("abc", TO)).await, 400, "invalid_body");
    expect(
        &t.get(&t.h, &format!("/api/stations/{st}/sessions/s1/turn-diff"))
            .await,
        400,
        "invalid_body",
    );
    t.fake.set(|c| c.refuse_selector = true);
    expect(
        &t.get(&t.h, &path(FROM, TO)).await,
        404,
        "checkpoint_unavailable",
    );
    // The member rules of the existing turn route: the member flag and the
    // share.
    t.fake.set(|c| c.refuse_selector = false);
    let ann = member(&t, "viewer", "Gus").await;
    assert_eq!(t.get(&ann, &path(FROM, TO)).await.status, 200);
    let last = t.fake.calls("files.diff").pop().unwrap();
    assert_eq!(last["member"], true);
    expect(
        &t.get(
            &ann,
            &format!("/api/stations/{st}/sessions/s2/turn-diff?from={FROM}&to={TO}"),
        )
        .await,
        404,
        "unknown_session",
    );
}
