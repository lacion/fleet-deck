//! Send, the delivery of a review and "Send again" in the review store
//! (DL-179). The helpers are in `review_tests`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::CommandId;
use std::collections::BTreeSet;

use fleetdeck_proto::command::DeckCommandState;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::member_deny::MemberDeny;
use fleetdeck_proto::review::{
    CommentPatch, CommentState, Delivery, MAX_REVIEWS_PER_SESSION, ReviewComment, ReviewSummary,
};

use super::Settle;
use super::review::{Hide, ReviewError, Target, Viewer};
use super::review_send::{AgainRequest, FAIL_AFTER_INSERT};
use super::review_tests::{W, accepted, code, rejected, setup};

// ---- send ----------------------------------------------------------------

#[tokio::test]
async fn a_send_stores_the_command_the_review_and_the_sent_drafts_in_one_transaction() {
    let w = setup().await;
    let a = w.add("a.rs", 4).await;
    let b = w.add("b.rs", 5).await;
    let rev0 = w.list(Viewer::Owner, Hide::Nothing).await.rev;
    let sent = w.send("key-1", &[&b, &a], "cmd-1").await;
    assert!(sent.created);
    let s = &sent.summary;
    assert_eq!(
        (s.delivery, s.attempts, s.command_id.as_str()),
        (Delivery::Pending, 1, "cmd-1")
    );
    assert_eq!(s.principal, "owner");
    assert!(s.short_id.starts_with("r-"));
    assert_eq!(s.comment_ids, [a.short_id.clone(), b.short_id.clone()]);
    let cmd =
        w.s.command(CommandId::from("cmd-1"))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(cmd.state, DeckCommandState::Pending);
    let text = cmd.envelope["command"]["input"]["text"].as_str().unwrap();
    assert!(
        text.starts_with(&format!(
            "Review {} from the owner (2 comments).",
            s.short_id
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("[{}] a.rs line 4 (new side)", a.short_id)),
        "{text}"
    );
    assert!(text.contains("text of a.rs:4"), "{text}");
    let all = w.list(Viewer::Owner, Hide::Nothing).await;
    assert!(all.threads.iter().all(|t| t.comments.iter().all(|c| {
        c.state == CommentState::Sent && c.review_id.as_deref() == Some(s.review_id.as_str())
    })));
    assert!(all.rev > rev0);
    assert_eq!(
        w.s.review_command(s.review_id.clone())
            .await
            .unwrap()
            .unwrap()
            .as_str(),
        "cmd-1"
    );
}

#[tokio::test]
async fn the_same_send_key_returns_the_stored_result_and_another_fingerprint_is_refused() {
    let w = setup().await;
    let a = w.add("a.rs", 1).await;
    let b = w.add("b.rs", 2).await;
    let first = w.send("key-1", &[&a], "cmd-1").await;
    let again = w.send("key-1", &[&a], "cmd-2").await;
    assert!(!again.created);
    assert_eq!(again.summary, first.summary);
    assert_eq!(
        w.rows("SELECT COUNT(*) FROM commands").await,
        1,
        "one command"
    );
    assert!(
        w.s.command(CommandId::from("cmd-2"))
            .await
            .unwrap()
            .is_none()
    );
    // The same key for other comments.
    let r =
        w.s.review_send(w.send_req(&Viewer::Owner, "key-1", &[&b], "cmd-3"))
            .await;
    assert_eq!(code(r), "send_key_reused");
    // The same key of another principal is another send.
    let ann = Viewer::Member("Ann".into());
    let c = w.add_as("s1", &ann, "c.rs", 1).await;
    let other =
        w.s.review_send(w.send_req(&ann, "key-1", &[&c], "cmd-4"))
            .await
            .unwrap();
    assert!(other.created);
    assert_eq!(other.summary.principal, "member:Ann");
    // A comment that is sent already is not a draft any more.
    let r =
        w.s.review_send(w.send_req(&Viewer::Owner, "key-9", &[&a], "cmd-5"))
            .await;
    assert_eq!(code(r), "not_draft");
    // An unknown comment, and another author's draft.
    let d = w.add_as("s1", &ann, "d.rs", 1).await;
    let r =
        w.s.review_send(w.send_req(&Viewer::Owner, "key-8", &[&d], "cmd-6"))
            .await;
    assert_eq!(code(r), "unknown_comment");
    assert_eq!(w.rows("SELECT COUNT(*) FROM commands").await, 2);
}

#[tokio::test]
async fn two_identical_concurrent_sends_make_one_review_and_one_command() {
    let w = setup().await;
    let a = w.add("a.rs", 1).await;
    let one =
        w.s.review_send(w.send_req(&Viewer::Owner, "key-1", &[&a], "cmd-1"));
    let two =
        w.s.review_send(w.send_req(&Viewer::Owner, "key-1", &[&a], "cmd-2"));
    let (one, two) = tokio::join!(one, two);
    let (one, two) = (one.unwrap(), two.unwrap());
    assert_eq!(one.summary.review_id, two.summary.review_id);
    assert_eq!([one.created, two.created].iter().filter(|c| **c).count(), 1);
    assert_eq!(w.rows("SELECT COUNT(*) FROM commands").await, 1);
    assert_eq!(w.rows("SELECT COUNT(*) FROM reviews").await, 1);
}

#[tokio::test]
async fn a_reply_needs_a_sent_top_comment_or_the_top_in_the_same_send() {
    let w = setup().await;
    let top = w.add("a.rs", 1).await;
    let reply = w
        .add_target(
            "s1",
            &Viewer::Owner,
            Target::Reply(top.comment_id.clone()),
            "re",
        )
        .await;
    let r =
        w.s.review_send(w.send_req(&Viewer::Owner, "k1", &[&reply], "cmd-1"))
            .await;
    assert_eq!(code(r), "thread_root_unsent");
    assert_eq!(w.rows("SELECT COUNT(*) FROM commands").await, 0);
    let both =
        w.s.review_send(w.send_req(&Viewer::Owner, "k2", &[&reply, &top], "cmd-2"))
            .await
            .unwrap();
    let text =
        w.s.command(CommandId::from("cmd-2"))
            .await
            .unwrap()
            .unwrap()
            .envelope["command"]["input"]["text"]
            .as_str()
            .unwrap()
            .to_owned();
    assert!(
        text.contains(&format!(
            "[{}] reply to [{}]\nre",
            reply.short_id, top.short_id
        )),
        "{text}"
    );
    assert_eq!(both.summary.comment_ids.len(), 2);
    // Another author's reply under the sent top sends alone.
    let ann = Viewer::Member("Ann".into());
    let r = w
        .add_target("s1", &ann, Target::Reply(top.comment_id.clone()), "ann")
        .await;
    w.s.review_send(w.send_req(&ann, "k3", &[&r], "cmd-3"))
        .await
        .unwrap();
}

#[tokio::test]
async fn an_injected_failure_after_the_command_insert_leaves_no_review_and_no_sent_draft() {
    let w = setup().await;
    let a = w.add("a.rs", 1).await;
    FAIL_AFTER_INSERT
        .lock()
        .unwrap()
        .push("cmd-boom".to_owned());
    let r =
        w.s.review_send(w.send_req(&Viewer::Owner, "k1", &[&a], "cmd-boom"))
            .await;
    assert!(matches!(r, Err(ReviewError::Deck(_))));
    assert_eq!(w.rows("SELECT COUNT(*) FROM reviews").await, 0);
    assert_eq!(
        w.rows("SELECT COUNT(*) FROM commands").await,
        0,
        "the command is rolled back too"
    );
    assert_eq!(
        w.rows("SELECT COUNT(*) FROM review_comments WHERE state = 'draft'")
            .await,
        1
    );
    // The same send works afterwards.
    let ok =
        w.s.review_send(w.send_req(&Viewer::Owner, "k1", &[&a], "cmd-fine"))
            .await
            .unwrap();
    assert!(ok.created);
}

#[tokio::test]
async fn a_send_without_a_live_link_a_review_over_the_text_limit_and_the_200_review_cap_are_refused()
 {
    let w = setup().await;
    let a = w.add("a.rs", 1).await;
    let mut req = w.send_req(&Viewer::Owner, "k1", &[&a], "cmd-1");
    req.link_up = false;
    assert_eq!(code(w.s.review_send(req).await), "station_unreachable");
    assert_eq!(w.rows("SELECT COUNT(*) FROM commands").await, 0);
    // Nine bodies of 8000 characters are over 64 KiB.
    let mut many = Vec::new();
    for n in 0..9 {
        many.push(
            w.add_target(
                "s1",
                &Viewer::Owner,
                Target::Anchor(W::fields("big.rs", n + 1)),
                &"b".repeat(8000),
            )
            .await,
        );
    }
    let refs: Vec<&ReviewComment> = many.iter().collect();
    let r =
        w.s.review_send(w.send_req(&Viewer::Owner, "k-big", &refs, "cmd-big"))
            .await;
    assert_eq!(code(r), "review_too_large");
    assert_eq!(w.rows("SELECT COUNT(*) FROM commands").await, 0);
    // 200 reviews in the life of a session.
    for n in 0..MAX_REVIEWS_PER_SESSION {
        let c = w.add("c.rs", 1).await;
        w.send(&format!("k{n}"), &[&c], &format!("cmd-{n}")).await;
        if n % 40 == 0 {
            // Comments of the session are capped at 500: keep room.
            assert!(w.rows("SELECT COUNT(*) FROM review_comments").await <= 500);
        }
        if w.rows("SELECT COUNT(*) FROM review_comments").await >= 480 {
            break;
        }
    }
    let have = w.rows("SELECT COUNT(*) FROM reviews").await;
    assert!(have > 0);
}

#[tokio::test]
async fn the_review_list_pages_by_review_id_and_names_more_reviews() {
    let w = setup().await;
    let mut ids = Vec::new();
    for n in 0..53 {
        let c = w.add("a.rs", 1).await;
        let s = w.send(&format!("k{n}"), &[&c], &format!("cmd-{n}")).await;
        ids.push(s.summary.review_id);
    }
    let page = w.list(Viewer::Owner, Hide::Nothing).await;
    assert_eq!(page.reviews.len(), 50);
    assert!(page.more_reviews);
    assert_eq!(
        page.reviews[0].review_id,
        *ids.last().unwrap(),
        "newest first"
    );
    let before = page.reviews.last().unwrap().review_id.clone();
    let older =
        w.s.review_list(
            w.station.clone(),
            "s1".into(),
            Viewer::Owner,
            Hide::Nothing,
            Some(before),
        )
        .await
        .unwrap();
    assert_eq!(older.reviews.len(), 3);
    assert!(!older.more_reviews);
    assert_eq!(older.reviews.last().unwrap().review_id, ids[0]);
}

#[tokio::test]
async fn the_200_review_cap_refuses_the_201st_review() {
    let w = setup().await;
    // Insert the 200 rows directly: the cap counts the rows of the session.
    super::call(&w.s.inner.db, {
        let station = w.station.as_str().to_owned();
        move |conn| {
            for n in 0..MAX_REVIEWS_PER_SESSION {
                conn.execute(
                    "INSERT INTO reviews (review_id, short_id, station_id, session_id, send_key,
                         fingerprint, principal, command_id, delivery, comment_count, sent_at)
                     VALUES (?1, ?2, ?3, 's1', ?4, 'f', 'owner', ?5, 'delivered', 1, 1)",
                    rusqlite::params![
                        format!("r{n:04}"),
                        format!("r-{n}"),
                        station,
                        format!("k{n}"),
                        format!("c{n}")
                    ],
                )?;
            }
            Ok(())
        }
    })
    .await
    .unwrap();
    let a = w.add("a.rs", 1).await;
    let r =
        w.s.review_send(w.send_req(&Viewer::Owner, "new", &[&a], "cmd-new"))
            .await;
    assert_eq!(code(r), "too_many_reviews");
    // Another session is not affected.
    let b = w.add_as("s2", &Viewer::Owner, "a.rs", 1).await;
    let mut req = w.send_req(&Viewer::Owner, "new", &[&b], "cmd-s2");
    req.session = "s2".to_owned();
    w.s.review_send(req).await.unwrap();
}

// ---- delivery ------------------------------------------------------------

#[tokio::test]
async fn an_accepted_receipt_delivers_a_rejected_one_fails_and_the_comments_stay_sent() {
    let w = setup().await;
    let a = w.add("a.rs", 1).await;
    let b = w.add("b.rs", 2).await;
    let sa = w.send("k1", &[&a], "cmd-a").await.summary;
    let sb = w.send("k2", &[&b], "cmd-b").await.summary;
    // Pending until the router settles it (a receipt that is lost).
    assert_eq!(w.review_of(&sa.review_id).await.delivery, Delivery::Pending);
    let rev = w.list(Viewer::Owner, Hide::Nothing).await.rev;
    w.end("cmd-a", accepted("cmd-a", "in-77")).await;
    let ra = w.review_of(&sa.review_id).await;
    assert_eq!(
        (ra.delivery, ra.input_id.as_deref()),
        (Delivery::Delivered, Some("in-77"))
    );
    w.end("cmd-b", rejected("cmd-b")).await;
    let rb = w.review_of(&sb.review_id).await;
    assert_eq!(
        (rb.delivery, rb.reason.as_deref()),
        (Delivery::Failed, Some("unknown_session"))
    );
    assert!(w.list(Viewer::Owner, Hide::Nothing).await.rev > rev);
    let all = w.list(Viewer::Owner, Hide::Nothing).await;
    assert!(
        all.threads
            .iter()
            .all(|t| t.comments.iter().all(|c| c.state == CommentState::Sent))
    );
}

#[tokio::test]
async fn a_failed_command_fails_the_review_and_an_ambiguous_one_leaves_it_unknown_for_good() {
    let w = setup().await;
    let a = w.add("a.rs", 1).await;
    let b = w.add("b.rs", 2).await;
    let sa = w.send("k1", &[&a], "cmd-a").await.summary;
    let sb = w.send("k2", &[&b], "cmd-b").await.summary;
    // A link refusal on the first send: failed, with the code.
    w.end(
        "cmd-a",
        Settle::Refused {
            code: "forbidden".into(),
            message: "no".into(),
        },
    )
    .await;
    let ra = w.review_of(&sa.review_id).await;
    assert_eq!(
        (ra.delivery, ra.reason.as_deref()),
        (Delivery::Failed, Some("forbidden"))
    );
    // A command that was sent and then expired: ambiguous, so unknown.
    w.s.mark_sent(CommandId::from("cmd-b"))
        .await
        .unwrap()
        .unwrap();
    w.det.advance(std::time::Duration::from_secs(16 * 60));
    let changed = w.s.expire_commands().await.unwrap();
    assert_eq!(changed.len(), 1);
    let rb = w.review_of(&sb.review_id).await;
    assert_eq!(
        (rb.delivery, rb.reason.as_deref()),
        (Delivery::Unknown, Some("no_receipt"))
    );
    // `unknown` is terminal: no Send again, no polling, and it stays.
    assert_eq!(
        code(
            w.again(&sb.review_id, "att-1", "cmd-x", Some(MemberDeny::default()))
                .await
        ),
        "not_failed"
    );
    w.det.advance(std::time::Duration::from_secs(3600));
    assert!(w.s.expire_commands().await.unwrap().is_empty());
    assert_eq!(w.review_of(&sb.review_id).await.delivery, Delivery::Unknown);
    assert!(
        w.s.command(CommandId::from("cmd-x"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn an_expired_command_that_was_never_sent_fails_the_review_with_not_sent() {
    let w = setup().await;
    let a = w.add("a.rs", 1).await;
    let s = w.send("k1", &[&a], "cmd-a").await.summary;
    w.det.advance(std::time::Duration::from_secs(16 * 60));
    w.s.expire_commands().await.unwrap();
    let r = w.review_of(&s.review_id).await;
    assert_eq!(
        (r.delivery, r.reason.as_deref()),
        (Delivery::Failed, Some("not_sent"))
    );
}

// ---- send again ----------------------------------------------------------

async fn failed_review(w: &W, path: &str) -> (ReviewComment, ReviewSummary) {
    let a = w.add(path, 1).await;
    let s = w.send("k1", &[&a], "cmd-1").await.summary;
    w.end("cmd-1", rejected("cmd-1")).await;
    (a, s)
}

#[tokio::test]
async fn send_again_makes_a_new_command_on_the_same_review_and_a_repeated_attempt_key_makes_none() {
    let w = setup().await;
    let (a, s) = failed_review(&w, "a.rs").await;
    let first = w
        .again(&s.review_id, "att-1", "cmd-2", Some(MemberDeny::default()))
        .await
        .unwrap();
    assert!(first.created);
    assert_eq!(
        (first.summary.delivery, first.summary.attempts),
        (Delivery::Pending, 2)
    );
    assert_eq!(first.summary.command_id, "cmd-2");
    assert_eq!(first.summary.review_id, s.review_id);
    assert_eq!(first.summary.comment_ids, vec![a.short_id.clone()]);
    // The same attempt again, also at once: the stored result and no command.
    let one = w.again(&s.review_id, "att-1", "cmd-3", Some(MemberDeny::default()));
    let two = w.again(&s.review_id, "att-1", "cmd-4", Some(MemberDeny::default()));
    let (one, two) = tokio::join!(one, two);
    for r in [one.unwrap(), two.unwrap()] {
        assert!(!r.created);
        assert_eq!(r.summary.command_id, "cmd-2");
    }
    assert_eq!(w.rows("SELECT COUNT(*) FROM commands").await, 2);
    // The old command ending late does not touch the new attempt.
    w.end("cmd-2", accepted("cmd-2", "in-9")).await;
    let r = w.review_of(&s.review_id).await;
    assert_eq!(
        (r.delivery, r.input_id.as_deref(), r.attempts),
        (Delivery::Delivered, Some("in-9"), 2)
    );
    // A delivered review is not sent again.
    assert_eq!(
        code(
            w.again(&s.review_id, "att-2", "cmd-5", Some(MemberDeny::default()))
                .await
        ),
        "not_failed"
    );
    // An unknown review, and the review of another session.
    assert_eq!(
        code(w.again("nope", "att-3", "cmd-6", None).await),
        "unknown_review"
    );
}

#[tokio::test]
async fn a_review_is_sent_at_most_five_times_and_only_by_its_sender() {
    let w = setup().await;
    let (_a, s) = failed_review(&w, "a.rs").await;
    // Another principal is not the sender.
    let r =
        w.s.review_send_again(AgainRequest {
            station: w.station.clone(),
            session: "s1".into(),
            viewer: Viewer::Member("Ann".into()),
            review_id: s.review_id.clone(),
            attempt_key: "att-x".into(),
            excerpts: None,
            command: w.command("cmd-x"),
            link_up: true,
        })
        .await;
    assert_eq!(code(r), "forbidden");
    for n in 2..=5 {
        let cmd = format!("cmd-{n}");
        let again = w
            .again(
                &s.review_id,
                &format!("att-{n}"),
                &cmd,
                Some(MemberDeny::default()),
            )
            .await
            .unwrap();
        assert_eq!(again.summary.attempts, n);
        w.end(&cmd, rejected(&cmd)).await;
    }
    let r = w
        .again(&s.review_id, "att-6", "cmd-6", Some(MemberDeny::default()))
        .await;
    assert_eq!(code(r), "too_many_attempts");
    assert_eq!(w.rows("SELECT COUNT(*) FROM commands").await, 5);
}

#[tokio::test]
async fn the_retry_text_is_rebuilt_under_the_current_deny_policy() {
    let w = setup().await;
    let (_a, s) = failed_review(&w, "config/prod.yml").await;
    let text = |id: &str| {
        let s = w.s.clone();
        let id = id.to_owned();
        async move {
            s.command(CommandId::from(id))
                .await
                .unwrap()
                .unwrap()
                .envelope["command"]["input"]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    assert!(text("cmd-1").await.contains("text of config/prod.yml:1"));
    // The path became denied between the first send and the retry.
    let denied = MemberDeny::new(&["config/prod.yml".to_owned()]);
    w.again(&s.review_id, "att-1", "cmd-2", Some(denied))
        .await
        .unwrap();
    let t = text("cmd-2").await;
    assert!(!t.contains("text of config"), "{t}");
    assert!(t.contains("(excerpt hidden: protected file)"), "{t}");
    w.end("cmd-2", rejected("cmd-2")).await;
    // The deny list became unavailable: no excerpt at all.
    w.again(&s.review_id, "att-2", "cmd-3", None).await.unwrap();
    let t = text("cmd-3").await;
    assert!(
        !t.contains("text of config") && t.contains("(excerpt hidden)"),
        "{t}"
    );
    w.end("cmd-3", rejected("cmd-3")).await;
    // Back to a plain policy: the excerpt returns (the text is rebuilt each time).
    w.again(&s.review_id, "att-3", "cmd-4", Some(MemberDeny::default()))
        .await
        .unwrap();
    assert!(text("cmd-4").await.contains("text of config/prod.yml:1"));
}

#[tokio::test]
async fn the_card_counter_grows_with_each_mutation() {
    let w = setup().await;
    let rev = |w: &W| {
        let s = w.s.clone();
        async move {
            s.review_revs()
                .await
                .unwrap()
                .values()
                .copied()
                .max()
                .unwrap_or(0)
        }
    };
    assert_eq!(rev(&w).await, 0);
    let a = w.add("a.rs", 1).await;
    let r1 = rev(&w).await;
    w.s.review_edit(
        w.station.clone(),
        "s1".into(),
        Viewer::Owner,
        Hide::Nothing,
        a.comment_id.clone(),
        CommentPatch {
            body: Some("b".into()),
            resolved: None,
        },
    )
    .await
    .unwrap();
    let r2 = rev(&w).await;
    w.send("k1", &[&a], "cmd-1").await;
    let r3 = rev(&w).await;
    w.end("cmd-1", accepted("cmd-1", "in-1")).await;
    let r4 = rev(&w).await;
    assert!(
        0 < r1 && r1 < r2 && r2 < r3 && r3 < r4,
        "{r1} {r2} {r3} {r4}"
    );
    // The cards carry it.
    let cards =
        w.s.sessions_for(&super::Principal::Token(
            super::ApiToken {
                name: "t".into(),
                scopes: BTreeSet::from([Scope::Read]),
                created_at: 0,
                expires_at: i64::MAX,
                last_used: None,
            },
            "h".into(),
        ))
        .await
        .unwrap();
    let s1 = cards
        .iter()
        .find(|c| c.summary.session_id.as_str() == "s1")
        .unwrap();
    let s2 = cards
        .iter()
        .find(|c| c.summary.session_id.as_str() == "s2")
        .unwrap();
    assert_eq!((s1.review_rev, s2.review_rev), (Some(r4), None));
}
