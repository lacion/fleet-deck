//! Review comments and the member deny list (DL-179, design
//! `review-comments.md`): the paths that a member cannot read, and the review
//! caches that follow the deny list. The rest is in `board_review.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod review_support;

use fleetdeck_deck::http::review_anchor_cache::cached_entries;
use review_support::*;
use serde_json::json;

// ---- the member deny list ------------------------------------------------

#[tokio::test]
async fn a_member_cannot_comment_on_or_read_a_denied_path_and_the_owner_review_hides_its_excerpt() {
    let t = setup().await;
    let ann = member(&t, "collaborator", "Ann").await;
    // The member diff omits the file: the path is not in the diff.
    expect(
        &t.comment(&ann, ".env", "new", 1, "x").await,
        400,
        "not_in_diff",
    );
    // A rename from a denied file to an allowed name is hidden from the
    // member as well (the station removes the whole section).
    expect(
        &t.comment(&ann, "renamed.txt", "new", 2, "x").await,
        400,
        "not_in_diff",
    );
    let call = t.fake.calls("files.session_diff").pop().unwrap();
    assert_eq!(call["member"], true);
    // The owner can; the member does not read the comments, but reads a plain
    // one.
    let secret = id(&t.comment(&t.h, ".env", "new", 1, "rotate this").await);
    let renamed = id(&t
        .comment(&t.h, "renamed.txt", "new", 2, "why renamed")
        .await);
    let plain = id(&t.comment(&t.h, "src/lib.rs", "new", 2, "plain").await);
    let sent = t.send(&t.h, "k1", &[&secret, &renamed, &plain]).await;
    assert_eq!(sent.status, 200, "{}", sent.body);
    t.wait_follow_ups(1).await;
    let text = &t.fake.follow_ups()[0].1;
    assert!(
        !text.contains("brand-new-secret") && !text.contains("hunter3"),
        "{text}"
    );
    assert!(text.contains("(excerpt hidden: protected file)"), "{text}");
    assert!(
        text.contains("fn two() { 2 }"),
        "the plain excerpt stays: {text}"
    );
    assert!(
        text.contains(".env line 1"),
        "the path and the lines stay: {text}"
    );
    let seen = t.list(&ann).await;
    assert_eq!(ids(&seen), vec![plain.clone()]);
    assert_eq!(
        seen.body["reviews"][0]["comment_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let owner = ids(&t.list(&t.h).await);
    assert_eq!(owner.len(), 3);
    // A reply to a hidden thread is unknown to the member.
    let reply = t
        .post(&ann, &format!("{}/comments", t.base()), json!({ "snapshot": format!("spawn:{BASE}"),
            "path": ".env", "side": "new", "line_start": 1, "line_end": 1, "body": "re", "thread_id": secret }))
        .await;
    expect(&reply, 404, "unknown_comment");
    expect(
        &t.patch(
            &ann,
            &format!("{}/comments/{secret}", t.base()),
            json!({"resolved": true}),
        )
        .await,
        404,
        "unknown_comment",
    );
    // The extra patterns of the station settings count too.
    t.fake.set(|c| c.deny = Some(vec!["src/".to_owned()]));
    assert!(ids(&t.list(&ann).await).is_empty());
}

#[tokio::test]
async fn a_cached_member_diff_does_not_outlive_a_stricter_deny_list() {
    let t = setup().await;
    let ann = member(&t, "collaborator", "Ann").await;
    let secret = "\
diff --git a/secret.txt b/secret.txt
--- a/secret.txt
+++ b/secret.txt
@@ -1 +1,2 @@
 old
+classified line
";
    t.fake.set(|c| {
        c.patch = secret.to_owned();
        c.deny = None;
    });
    // The deny list cannot be established (and is not kept): the member diff
    // is fetched and cached, and the comment is refused.
    expect(
        &t.comment(&ann, "secret.txt", "new", 2, "x").await,
        400,
        "not_in_diff",
    );
    assert_eq!(t.fake.calls("files.session_diff").len(), 1);
    // The station settings now deny the file. The cache of the diff still
    // holds `secret.txt`, and the deny list is read again.
    t.fake.cfg.lock().unwrap().deny = Some(vec!["secret.txt".to_owned()]);
    expect(
        &t.comment(&ann, "secret.txt", "new", 2, "x").await,
        400,
        "not_in_diff",
    );
    assert_eq!(
        t.fake.calls("files.session_diff").len(),
        1,
        "the second answer came from the cache"
    );
    // No comment was stored.
    assert!(ids(&t.list(&t.h).await).is_empty());
    // The owner is not hidden from anything.
    let own = t.comment(&t.h, "secret.txt", "new", 2, "own").await;
    assert_eq!(own.status, 201, "{}", own.body);
}

#[tokio::test]
async fn a_station_settings_save_clears_the_review_caches_of_the_station() {
    let t = setup().await;
    let ann = member(&t, "collaborator", "Ann").await;
    let plain = t.comment(&ann, "src/lib.rs", "new", 2, "plain").await;
    assert_eq!(plain.status, 201, "{}", plain.body);
    // The member diff and the deny list of the station are cached.
    assert_eq!(cached_entries(&t.fake.station), 2);
    // The station applies the new list with the save. `Fake::set` is not
    // used, because it clears the caches.
    t.fake.cfg.lock().unwrap().deny = Some(vec!["src/".to_owned()]);
    let save = t
        .post(
            &t.h,
            "/api/stations/fake/settings",
            json!({ "changes": { "member_deny": ["src/"] }, "revision": "r" }),
        )
        .await;
    assert_eq!(save.status, 200, "{}", save.body);
    assert_eq!(cached_entries(&t.fake.station), 0);
    // The next member comment reads the new list at once.
    expect(
        &t.comment(&ann, "src/lib.rs", "new", 2, "again").await,
        400,
        "not_in_diff",
    );
}

#[tokio::test]
async fn a_deny_list_that_cannot_be_established_hides_every_comment_and_every_excerpt() {
    let t = setup().await;
    let ann = member(&t, "collaborator", "Ann").await;
    let plain = id(&t.comment(&t.h, "src/lib.rs", "new", 2, "plain").await);
    t.send(&t.h, "k0", &[&plain]).await;
    assert_eq!(ids(&t.list(&ann).await), vec![plain.clone()]);
    t.fake.set(|c| c.deny = None);
    let seen = t.list(&ann).await;
    assert!(ids(&seen).is_empty());
    assert!(seen.body["reviews"].as_array().unwrap().is_empty());
    assert!(seen.body["rev"].as_u64().unwrap() > 0);
    // The owner still reads, and a send carries no excerpt.
    let c2 = id(&t.comment(&t.h, "src/lib.rs", "new", 1, "second").await);
    assert_eq!(ids(&t.list(&t.h).await).len(), 2);
    let sent = t.send(&t.h, "k1", &[&c2]).await;
    assert_eq!(sent.status, 200, "{}", sent.body);
    t.wait_follow_ups(2).await;
    let text = &t.fake.follow_ups()[1].1;
    assert!(
        text.contains("(excerpt hidden)") && !text.contains("fn one() {}"),
        "{text}"
    );
    // A member cannot write either: without a list every path may be
    // denied, so the path is not in the diff (NV-33).
    let m = t.comment(&ann, "src/lib.rs", "new", 1, "m").await;
    expect(&m, 400, "not_in_diff");
    assert!(ids(&t.list(&ann).await).is_empty());
}

#[tokio::test]
async fn an_import_replay_follows_the_deny_list_like_every_other_lookup() {
    let t = setup().await;
    let ann = member(&t, "collaborator", "Ann").await;
    let top = id(&t.comment(&t.h, "src/lib.rs", "new", 2, "top").await);
    let sent = t.send(&t.h, "k1", &[&top]).await;
    assert_eq!(sent.status, 200, "{}", sent.body);
    t.wait_follow_ups(1).await;
    // A reply carries an import key and skips the anchor derivation of the
    // route, so the replay reaches the store even when the diff hides the
    // path.
    let url = format!("{}/comments", t.base());
    let reply = |key: &str| {
        json!({ "snapshot": format!("spawn:{BASE}"), "path": "src/lib.rs", "side": "new",
                "line_start": 2, "line_end": 2, "body": "old note", "import_key": key,
                "thread_id": top })
    };
    let (ann_key, owner_key) = ("ab".repeat(32), "cd".repeat(32));
    let made = t.post(&ann, &url, reply(&ann_key)).await;
    assert_eq!(made.status, 201, "{}", made.body);
    let mine = t.post(&t.h, &url, reply(&owner_key)).await;
    assert_eq!(mine.status, 201, "{}", mine.body);
    // The path is visible: the replay returns the stored comment.
    let again = t.post(&ann, &url, reply(&ann_key)).await;
    assert_eq!(again.status, 200, "{}", again.body);
    assert_eq!(again.body["comment_id"], made.body["comment_id"]);
    // The path becomes member-denied: the member replay is unknown and
    // carries no anchor; the owner still gets its comment.
    t.fake.set(|c| c.deny = Some(vec!["src/".to_owned()]));
    let hidden = t.post(&ann, &url, reply(&ann_key)).await;
    expect(&hidden, 404, "unknown_comment");
    assert!(
        !hidden.body.to_string().contains("fn two"),
        "{}",
        hidden.body
    );
    let kept = t.post(&t.h, &url, reply(&owner_key)).await;
    assert_eq!(kept.status, 200, "{}", kept.body);
    assert_eq!(kept.body["comment_id"], mine.body["comment_id"]);
    assert!(kept.body["anchor_text"].is_string());
}
