//! Send, "Send again", the delivery of a review, the import of old notes and
//! the live card counter (DL-179, design `review-comments.md` sections 3, 4
//! and 7), over loopback HTTPS against a fake station.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod review_support;

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::member_deny::MemberDeny;
use review_support::*;
use serde_json::{Value, json};

// ---- send ----------------------------------------------------------------

#[tokio::test]
async fn a_send_dispatches_after_the_commit_replays_and_refuses_a_reused_key() {
    let t = setup().await;
    let a = id(&t.comment(&t.h, "src/lib.rs", "new", 1, "first").await);
    let b = id(&t.comment(&t.h, "src/lib.rs", "new", 2, "second").await);
    let sent = t.send(&t.h, "key-1", &[&a]).await;
    assert_eq!(sent.status, 200, "{}", sent.body);
    let review = sent.body["review_id"].as_str().unwrap().to_owned();
    assert_eq!(
        (
            sent.body["delivery"].as_str(),
            sent.body["attempts"].as_u64()
        ),
        (Some("pending"), Some(1))
    );
    t.wait_follow_ups(1).await;
    assert_eq!(t.review_state(&review).await, "delivered");
    let (cmd, text, author) = t.fake.follow_ups().remove(0);
    assert_eq!(cmd, sent.body["command_id"].as_str().unwrap());
    assert_eq!(author, None, "the owner sets no author");
    let short = sent.body["short_id"].as_str().unwrap();
    assert!(
        text.starts_with(&format!("Review {short} from the owner (1 comment).")),
        "{text}"
    );
    let list = t.list(&t.h).await;
    assert_eq!(list.body["reviews"][0]["input_id"], "in-1");
    // The repeat: the same result, no second command.
    let again = t.send(&t.h, "key-1", &[&a]).await;
    assert_eq!(again.status, 200);
    assert_eq!(again.body["review_id"], sent.body["review_id"]);
    assert_eq!(
        again.body["delivery"], "delivered",
        "the stored review as it is now"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(t.fake.follow_ups().len(), 1);
    // Other comments with the same key, a sent comment, an unknown one.
    expect(&t.send(&t.h, "key-1", &[&b]).await, 409, "send_key_reused");
    expect(&t.send(&t.h, "key-2", &[&a]).await, 409, "not_draft");
    expect(
        &t.send(&t.h, "key-3", &["c-nopenope"]).await,
        404,
        "unknown_comment",
    );
    // The shape: no comments, 51, duplicates, a bad key.
    expect(&t.send(&t.h, "key-4", &[]).await, 400, "invalid_body");
    let many: Vec<String> = (0..51).map(|n| format!("x{n}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    expect(&t.send(&t.h, "key-5", &many).await, 400, "invalid_body");
    expect(&t.send(&t.h, "key-6", &[&b, &b]).await, 400, "invalid_body");
    expect(&t.send(&t.h, "bad key!", &[&b]).await, 400, "invalid_body");
    // A reply needs a sent top comment or the top in the same send.
    let top = id(&t.comment(&t.h, "src/lib.rs", "new", 3, "top").await);
    let reply = t
        .post(&t.h, &format!("{}/comments", t.base()), json!({ "snapshot": format!("spawn:{BASE}"),
            "path": "src/lib.rs", "side": "new", "line_start": 3, "line_end": 3, "body": "reply", "thread_id": top }))
        .await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    assert_eq!(reply.body["thread_id"], top.as_str());
    assert_eq!(reply.body["anchor_text"], "fn three() {}");
    let reply_id = id(&reply);
    expect(
        &t.send(&t.h, "key-7", &[&reply_id]).await,
        409,
        "thread_root_unsent",
    );
    assert_eq!(t.send(&t.h, "key-8", &[&reply_id, &top]).await.status, 200);
    t.wait_follow_ups(2).await;
    // Only the failed attempts left no command at the station.
    assert_eq!(t.fake.follow_ups().len(), 2);
}

#[tokio::test]
async fn a_send_without_a_live_link_changes_nothing() {
    let t = setup().await;
    let a = id(&t.comment(&t.h, "src/lib.rs", "new", 1, "first").await);
    // The link drops.
    t.link.close("test", "the link drops");
    t.f.wait("the station unreachable", |f| {
        let (s, id) = (f.store.clone(), f.station.clone());
        async move { !s.is_reachable(&id) }
    })
    .await;
    let r = t.send(&t.h, "k1", &[&a]).await;
    expect(&r, 409, "station_unreachable");
    assert!(t.f.store.commands().await.unwrap().is_empty());
    assert!(
        t.list(&t.h).await.body["reviews"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let list = t.list(&t.h).await;
    assert_eq!(list.body["threads"][0]["comments"][0]["state"], "draft");
}

#[tokio::test]
async fn a_committed_pending_command_is_sent_by_the_router_without_the_route() {
    let t = setup().await;
    let a = id(&t.comment(&t.h, "src/lib.rs", "new", 1, "first").await);
    // The route would dispatch after the commit; here the store commits and
    // nothing dispatches, as after a crash between the commit and the send.
    let id = t.f.store.new_command_id();
    let command = fleetdeck_deck::store::NewCommand {
        command_id: id.clone(),
        station_id: t.f.station.clone(),
        method: "session.follow_up".to_owned(),
        origin: "board".to_owned(),
        envelope: json!({
            "command_id": id, "target_incarnation": t.f.inc, "origin": "board",
            "retry_until": t.f.store.now_ms() + 900_000,
            "command": { "op": "follow_up", "session_id": "s1", "input": { "text": "" } },
        }),
        retry_until: t.f.store.now_ms() + 900_000,
        author: Some("owner".to_owned()),
    };
    let req = fleetdeck_deck::store::review_send::SendRequest {
        station: t.f.station.clone(),
        session: "s1".to_owned(),
        viewer: fleetdeck_deck::store::review::Viewer::Owner,
        send_key: "crash".to_owned(),
        comment_ids: vec![a],
        excerpts: Some(MemberDeny::default()),
        command,
        link_up: true,
    };
    let sent = t.f.store.review_send(req).await.unwrap();
    assert!(sent.created);
    // The router tick finds the pending command and sends it.
    t.wait_follow_ups(1).await;
    assert_eq!(t.review_state(&sent.summary.review_id).await, "delivered");
}

#[tokio::test]
async fn a_member_send_names_the_member_and_sets_the_input_author() {
    let t = setup().await;
    let ann = member(&t, "collaborator", "Ann").await;
    let c = id(&t.comment(&ann, "src/lib.rs", "new", 2, "from Ann").await);
    let sent = t.send(&ann, "k1", &[&c]).await;
    assert_eq!(sent.status, 200, "{}", sent.body);
    assert_eq!(sent.body["principal"], "member:Ann");
    t.wait_follow_ups(1).await;
    let (_, text, author) = t.fake.follow_ups().remove(0);
    assert_eq!(author.as_deref(), Some("member:Ann"));
    assert!(text.contains("from member Ann (1 comment)"), "{text}");
    assert!(!text.contains("from the owner"), "{text}");
    // The same key of the owner is another send.
    let o = id(&t.comment(&t.h, "src/lib.rs", "new", 1, "owner").await);
    assert_eq!(t.send(&t.h, "k1", &[&o]).await.status, 200);
}

#[tokio::test]
async fn a_rejected_follow_up_fails_the_review_and_send_again_makes_a_new_command_on_it() {
    let t = setup().await;
    t.fake.set(|c| c.follow = Follow::Reject);
    let top = id(&t.comment(&t.h, "src/lib.rs", "new", 2, "top").await);
    let sent = t.send(&t.h, "k1", &[&top]).await;
    let review = sent.body["review_id"].as_str().unwrap().to_owned();
    assert_eq!(t.review_state(&review).await, "failed");
    let list = t.list(&t.h).await;
    assert_eq!(list.body["reviews"][0]["reason"], "unknown_session");
    // The comment stays sent, so another author's reply keeps its root.
    assert_eq!(list.body["threads"][0]["comments"][0]["state"], "sent");
    let ann = member(&t, "collaborator", "Ann").await;
    let reply = t
        .post(&ann, &format!("{}/comments", t.base()), json!({ "snapshot": format!("spawn:{BASE}"),
            "path": "src/lib.rs", "side": "new", "line_start": 2, "line_end": 2, "body": "re", "thread_id": top }))
        .await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    assert_eq!(t.send(&ann, "ka", &[&id(&reply)]).await.status, 200);
    // Send again: the same review, a new command, attempts + 1.
    t.fake.set(|c| c.follow = Follow::Accept);
    let again = t.again(&t.h, &review, "att-1").await;
    assert_eq!(again.status, 200, "{}", again.body);
    assert_eq!(
        (
            again.body["review_id"].as_str(),
            again.body["attempts"].as_u64()
        ),
        (Some(review.as_str()), Some(2))
    );
    assert_ne!(again.body["command_id"], sent.body["command_id"]);
    assert_eq!(t.review_state(&review).await, "delivered");
    // The repeat of the attempt: the stored result and no third command.
    let n = t.fake.follow_ups().len();
    let repeat = t.again(&t.h, &review, "att-1").await;
    assert_eq!(repeat.status, 200, "{}", repeat.body);
    assert_eq!(repeat.body["command_id"], again.body["command_id"]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(t.fake.follow_ups().len(), n);
    // A delivered review is not sent again; the shape; other reviews.
    expect(&t.again(&t.h, &review, "att-2").await, 409, "not_failed");
    expect(
        &t.again(&t.h, "r-nope", "att-3").await,
        404,
        "unknown_review",
    );
    expect(
        &t.again(&t.h, &review, "bad key!").await,
        400,
        "invalid_body",
    );
    let r = t
        .post(
            &t.h,
            &format!("{}/reviews/{review}/send-again", t.base()),
            json!({}),
        )
        .await;
    expect(&r, 400, "invalid_body");
    // Only the sender sends it again.
    expect(&t.again(&ann, &review, "att-4").await, 403, "forbidden");
}

#[tokio::test]
async fn a_failed_review_is_sent_again_at_most_five_times() {
    let t = setup().await;
    t.fake.set(|c| c.follow = Follow::Reject);
    let a = id(&t.comment(&t.h, "src/lib.rs", "new", 1, "a").await);
    let review = t.send(&t.h, "k1", &[&a]).await.body["review_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(t.review_state(&review).await, "failed");
    for n in 2..=5 {
        let r = t.again(&t.h, &review, &format!("att-{n}")).await;
        assert_eq!(r.body["attempts"], n, "{}", r.body);
        assert_eq!(t.review_state(&review).await, "failed");
    }
    expect(
        &t.again(&t.h, &review, "att-6").await,
        409,
        "too_many_attempts",
    );
}

#[tokio::test]
async fn a_member_who_must_ask_to_drive_cannot_send_or_send_again() {
    let t = setup().await;
    let share = json!({ "kind": "session", "target": format!("{}/s1", t.st()),
                        "role": "collaborator", "steering": "mob" });
    let ann = join(&t, share, "Ann").await;
    // Nobody drives: Ann writes comments but does not send.
    let c = id(&t.comment(&ann, "src/lib.rs", "new", 1, "x").await);
    expect(&t.send(&ann, "k1", &[&c]).await, 403, "not_driver");
    expect(&t.again(&ann, "r-x", "att").await, 403, "not_driver");
    let r = t
        .post(
            &ann,
            &format!("/api/sessions/{}/s1/drive/request", t.st()),
            json!({}),
        )
        .await;
    assert_eq!(r.body["driver"], "member:Ann", "{}", r.body);
    let sent = t.send(&ann, "k1", &[&c]).await;
    assert_eq!(sent.status, 200, "{}", sent.body);
    let review = sent.body["review_id"].as_str().unwrap().to_owned();
    t.wait_follow_ups(1).await;
    // The driver hands on; Ann cannot retry any more.
    let owner_grant = t
        .post(
            &t.h,
            &format!("/api/sessions/{}/s1/drive/release", t.st()),
            json!({}),
        )
        .await;
    assert_eq!(owner_grant.status, 200, "{}", owner_grant.body);
    expect(&t.again(&ann, &review, "att").await, 403, "not_driver");
    expect(&t.send(&ann, "k2", &[&c]).await, 403, "not_driver");
}

// ---- import --------------------------------------------------------------

#[tokio::test]
async fn an_imported_note_is_made_once_and_keeps_an_imported_anchor_when_its_line_left_the_diff() {
    let t = setup().await;
    let key = "ab".repeat(32);
    let body = |path: &str, line: u32, key: &str| {
        json!({ "snapshot": format!("spawn:{BASE}"), "path": path, "side": "new",
                "line_start": line, "line_end": line, "body": "old note", "import_key": key })
    };
    let url = format!("{}/comments", t.base());
    let first = t.post(&t.h, &url, body("src/lib.rs", 2, &key)).await;
    assert_eq!(first.status, 201, "{}", first.body);
    assert_eq!(
        (
            first.body["anchor_kind"].as_str(),
            first.body["anchor_text"].as_str()
        ),
        (Some("diff"), Some("fn two() { 2 }"))
    );
    let again = t.post(&t.h, &url, body("src/lib.rs", 2, &key)).await;
    assert_eq!(again.status, 200, "{}", again.body);
    assert_eq!(again.body["comment_id"], first.body["comment_id"]);
    // A note whose line is not in the diff keeps an imported anchor.
    let gone = t
        .post(&t.h, &url, body("src/old.rs", 40, &"cd".repeat(32)))
        .await;
    assert_eq!(gone.status, 201, "{}", gone.body);
    assert_eq!(
        (
            gone.body["anchor_kind"].as_str(),
            gone.body["anchor_text"].as_str()
        ),
        (Some("imported"), Some(""))
    );
    // An import is refused for a denied path of a member.
    let ann = member(&t, "collaborator", "Ann").await;
    expect(
        &t.post(&ann, &url, body(".env", 1, &"ef".repeat(32))).await,
        400,
        "not_in_diff",
    );
    // A note without an import key must be in the diff.
    expect(
        &t.comment(&t.h, "src/old.rs", "new", 40, "x").await,
        400,
        "not_in_diff",
    );
    // The follow-up says that there is no excerpt.
    let sent = t.send(&t.h, "k1", &[&id(&gone)]).await;
    assert_eq!(sent.status, 200, "{}", sent.body);
    t.wait_follow_ups(1).await;
    assert!(
        t.fake.follow_ups()[0]
            .1
            .contains("(no excerpt: imported note)")
    );
    let log = t.f.store.audit_log(100).await.unwrap();
    assert!(
        log.iter()
            .any(|e| e.action == "review.import" && e.result == AuditResult::Ok)
    );
}

// ---- live updates --------------------------------------------------------

#[tokio::test]
async fn the_card_counter_changes_with_each_mutation_and_another_board_gets_the_delta() {
    let t = setup().await;
    let (cookie, _) = t.c.login().await;
    let origin = t.c.origin();
    let mut ws =
        t.c.ws("/api/ws", &[("Cookie", &cookie), ("Origin", &origin)])
            .await
            .expect("the socket opens");
    let snap = ws.next_of("snapshot").await;
    let card = |m: &Value, id: &str| -> Option<u64> {
        m["sessions"]
            .as_array()?
            .iter()
            .find(|c| c["summary"]["session_id"] == id)
            .and_then(|c| c["review_rev"].as_u64())
    };
    assert_eq!(card(&snap, "s1"), None);
    // Another board writes; this one gets the card with the counter.
    let c = id(&t.comment(&t.h, "src/lib.rs", "new", 2, "x").await);
    let mut last = 0;
    let delta = ws.next_of("delta").await;
    let rev = card(&delta, "s1").expect("a card with a review_rev");
    assert!(rev > last);
    last = rev;
    t.patch(
        &t.h,
        &format!("{}/comments/{c}", t.base()),
        json!({ "body": "y" }),
    )
    .await;
    let delta = ws.next_of("delta").await;
    assert!(card(&delta, "s1").unwrap() > last);
    last = card(&delta, "s1").unwrap();
    t.send(&t.h, "k1", &[&c]).await;
    // The send, then the delivery: the counter only grows.
    let mut top = last;
    for _ in 0..4 {
        let Ok(m) = tokio::time::timeout(Duration::from_millis(600), ws.next_of("delta")).await
        else {
            break;
        };
        if let Some(r) = card(&m, "s1") {
            assert!(r >= top);
            top = r;
        }
    }
    assert!(top > last);
    let list = t.list(&t.h).await;
    assert_eq!(list.body["rev"].as_u64(), Some(top));
}

#[tokio::test]
async fn a_station_removal_deletes_the_review_rows_and_the_audit_stays() {
    let t = setup().await;
    let c = id(&t.comment(&t.h, "src/lib.rs", "new", 2, "x").await);
    t.send(&t.h, "k1", &[&c]).await;
    t.wait_follow_ups(1).await;
    assert!(!ids(&t.list(&t.h).await).is_empty());
    t.f.store
        .apply_snapshot(
            t.f.station.clone(),
            t.f.inc.clone(),
            5,
            vec![SessionSummary::new("s2".into())],
        )
        .await
        .unwrap();
    let st = t.st();
    expect(
        &t.get(&t.h, &format!("/api/sessions/{st}/s1/review")).await,
        404,
        "unknown_session",
    );
    assert!(
        t.f.store
            .audit_log(100)
            .await
            .unwrap()
            .iter()
            .any(|e| e.action == "review.send")
    );
}

#[tokio::test]
async fn a_lost_receipt_leaves_the_review_pending_until_the_router_settles_it() {
    let mut t = setup().await;
    t.fake.set(|c| c.follow = Follow::Silent);
    let a = id(&t.comment(&t.h, "src/lib.rs", "new", 2, "x").await);
    let sent = t.send(&t.h, "k1", &[&a]).await;
    let review = sent.body["review_id"].as_str().unwrap().to_owned();
    t.wait_follow_ups(1).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let list = t.list(&t.h).await;
    assert_eq!(list.body["reviews"][0]["delivery"], "pending");
    assert_eq!(list.body["threads"][0]["comments"][0]["state"], "sent");
    // The link drops; the router sends the same command again on the next
    // link, and the receipt settles the review.
    t.link.close("test", "the link drops");
    t.f.wait("the station unreachable", |f| {
        let (s, id) = (f.store.clone(), f.station.clone());
        async move { !s.is_reachable(&id) }
    })
    .await;
    t.fake.set(|c| c.follow = Follow::Accept);
    let station = t.f.connect().await;
    serve(station, Arc::clone(&t.fake.cfg), Arc::clone(&t.fake.log));
    t.wait_follow_ups(2).await;
    assert_eq!(t.review_state(&review).await, "delivered");
    let ups = t.fake.follow_ups();
    assert_eq!(
        (&ups[0].0, &ups[0].1),
        (&ups[1].0, &ups[1].1),
        "the same command"
    );
}

#[tokio::test]
async fn an_unknown_delivery_offers_no_send_again_and_is_never_polled() {
    let t = setup().await;
    t.fake.set(|c| c.follow = Follow::Silent);
    let a = id(&t.comment(&t.h, "src/lib.rs", "new", 2, "x").await);
    let sent = t.send(&t.h, "k1", &[&a]).await;
    let review = sent.body["review_id"].as_str().unwrap().to_owned();
    t.wait_follow_ups(1).await;
    // The command was sent and its time ran out: the station may have it.
    t.f.det.advance(Duration::from_secs(16 * 60));
    assert_eq!(t.review_state(&review).await, "unknown");
    let list = t.list(&t.h).await;
    assert_eq!(list.body["reviews"][0]["reason"], "no_receipt");
    expect(&t.again(&t.h, &review, "att-1").await, 409, "not_failed");
    t.f.det.advance(Duration::from_secs(3600));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(t.fake.follow_ups().len(), 1, "nothing is sent again");
    assert_eq!(t.review_state(&review).await, "unknown");
    // The comments stay sent.
    let list = t.list(&t.h).await;
    assert_eq!(list.body["threads"][0]["comments"][0]["state"], "sent");
}

#[tokio::test]
async fn a_retry_is_rebuilt_under_the_current_deny_list() {
    let t = setup().await;
    t.fake.set(|c| c.follow = Follow::Reject);
    let a = id(&t.comment(&t.h, "src/lib.rs", "new", 2, "x").await);
    let review = t.send(&t.h, "k1", &[&a]).await.body["review_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(t.review_state(&review).await, "failed");
    assert!(t.fake.follow_ups()[0].1.contains("fn two() { 2 }"));
    // The path became denied between the first send and the retry.
    t.fake.set(|c| c.deny = Some(vec!["src/".to_owned()]));
    assert_eq!(t.again(&t.h, &review, "att-1").await.status, 200);
    t.wait_follow_ups(2).await;
    let second = &t.fake.follow_ups()[1].1;
    assert!(!second.contains("fn two() { 2 }"), "{second}");
    assert!(
        second.contains("(excerpt hidden: protected file)"),
        "{second}"
    );
    assert_eq!(t.review_state(&review).await, "failed");
    // The deny list cannot be established: no excerpt at all.
    t.fake.set(|c| c.deny = None);
    assert_eq!(t.again(&t.h, &review, "att-2").await.status, 200);
    t.wait_follow_ups(3).await;
    let third = &t.fake.follow_ups()[2].1;
    assert!(
        third.contains("(excerpt hidden)") && !third.contains("fn two()"),
        "{third}"
    );
    // Each attempt is a new command on the same review.
    let ups = t.fake.follow_ups();
    assert!(ups[0].0 != ups[1].0 && ups[1].0 != ups[2].0);
}
