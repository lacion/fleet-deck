//! The review store (DL-179): comments, visibility, send and send again,
//! the delivery of a review, caps, cleanup and the counter. The routes and
//! the principals are tested over HTTP in `tests/board_review.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::CommandResult;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::member_deny::MemberDeny;
use fleetdeck_proto::review::{
    AnchorKind, CommentPatch, MAX_COMMENTS_PER_SESSION, ReviewComment, ReviewSide, ReviewSummary,
};
use fleetdeck_proto::{CommandId, InputId, Receipt, Reject, RejectCode, SessionSummary, StationId};
use fleetdeck_testkit::Det;
use serde_json::json;

use super::review::{
    AnchorFields, Hide, NewReviewComment, ReviewError, Target, Viewer, alloc_short, random_short,
};
use super::review_send::{AgainRequest, SendRequest, Sent};
use super::stations::NewStation;
use super::{DeckStore, NewCommand, Settle};

pub(super) struct W {
    pub(super) s: DeckStore,
    pub(super) det: Det,
    pub(super) station: StationId,
    pub(super) key: fleetdeck_core::identity::PeerKey,
}

pub(super) async fn setup() -> W {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let key = Identity::generate().unwrap().public_key();
    let rec = s
        .pin_station(NewStation {
            key,
            name: "alpha".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap();
    let w = W {
        s,
        det,
        station: rec.station_id,
        key,
    };
    w.snapshot(&["s1", "s2"], 1).await;
    w
}

impl W {
    pub(super) async fn snapshot(&self, sessions: &[&str], watermark: u64) {
        self.s
            .apply_snapshot(
                self.station.clone(),
                "i1".into(),
                watermark,
                sessions
                    .iter()
                    .map(|id| SessionSummary::new((*id).into()))
                    .collect(),
            )
            .await
            .unwrap();
    }

    pub(super) fn fields(path: &str, line: u32) -> AnchorFields {
        AnchorFields {
            snapshot: format!("spawn:{}", "a".repeat(40)),
            path: path.to_owned(),
            other_path: None,
            side: ReviewSide::New,
            line_start: line,
            line_end: line,
            anchor_text: format!("text of {path}:{line}"),
            anchor_kind: AnchorKind::Diff,
        }
    }

    pub(super) async fn add_as(
        &self,
        session: &str,
        viewer: &Viewer,
        path: &str,
        line: u32,
    ) -> ReviewComment {
        self.add_target(
            session,
            viewer,
            Target::Anchor(Self::fields(path, line)),
            "please fix",
        )
        .await
    }

    pub(super) async fn add_target(
        &self,
        session: &str,
        viewer: &Viewer,
        target: Target,
        body: &str,
    ) -> ReviewComment {
        self.s
            .review_add(
                self.station.clone(),
                session.to_owned(),
                viewer.clone(),
                Hide::Nothing,
                NewReviewComment {
                    target,
                    body: body.to_owned(),
                    import_key: None,
                },
            )
            .await
            .unwrap()
            .0
    }

    pub(super) async fn add(&self, path: &str, line: u32) -> ReviewComment {
        self.add_as("s1", &Viewer::Owner, path, line).await
    }

    pub(super) fn command(&self, id: &str) -> NewCommand {
        let retry_until = i64::try_from(self.det.now_ms()).unwrap() + 900_000;
        NewCommand {
            command_id: CommandId::from(id),
            station_id: self.station.clone(),
            method: "session.follow_up".to_owned(),
            origin: "board".to_owned(),
            envelope: json!({
                "command_id": id,
                "command": { "op": "follow_up", "session_id": "s1", "input": { "text": "" } }
            }),
            retry_until,
            author: Some("owner".to_owned()),
        }
    }

    pub(super) fn send_req(
        &self,
        viewer: &Viewer,
        key: &str,
        ids: &[&ReviewComment],
        cmd: &str,
    ) -> SendRequest {
        SendRequest {
            station: self.station.clone(),
            session: "s1".to_owned(),
            viewer: viewer.clone(),
            send_key: key.to_owned(),
            comment_ids: ids.iter().map(|c| c.comment_id.clone()).collect(),
            excerpts: Some(MemberDeny::default()),
            command: self.command(cmd),
            link_up: true,
        }
    }

    pub(super) async fn send(&self, key: &str, ids: &[&ReviewComment], cmd: &str) -> Sent {
        self.s
            .review_send(self.send_req(&Viewer::Owner, key, ids, cmd))
            .await
            .unwrap()
    }

    pub(super) async fn again(
        &self,
        review: &str,
        key: &str,
        cmd: &str,
        excerpts: Option<MemberDeny>,
    ) -> Result<Sent, ReviewError> {
        self.s
            .review_send_again(AgainRequest {
                station: self.station.clone(),
                session: "s1".to_owned(),
                viewer: Viewer::Owner,
                review_id: review.to_owned(),
                attempt_key: key.to_owned(),
                excerpts,
                command: self.command(cmd),
                link_up: true,
            })
            .await
    }

    pub(super) async fn list(
        &self,
        viewer: Viewer,
        hide: Hide,
    ) -> fleetdeck_proto::review::ReviewThreads {
        self.s
            .review_list(self.station.clone(), "s1".to_owned(), viewer, hide, None)
            .await
            .unwrap()
    }

    pub(super) async fn rows(&self, sql: &'static str) -> i64 {
        super::call(&self.s.inner.db, move |conn| {
            Ok(conn.query_row(sql, [], |r| r.get::<_, i64>(0))?)
        })
        .await
        .unwrap()
    }

    pub(super) async fn review_of(&self, id: &str) -> ReviewSummary {
        let all = self.list(Viewer::Owner, Hide::Nothing).await;
        all.reviews.into_iter().find(|r| r.review_id == id).unwrap()
    }

    pub(super) async fn end(&self, cmd: &str, settle: Settle) {
        self.s
            .settle_command(CommandId::from(cmd), settle)
            .await
            .unwrap();
    }
}

pub(super) fn code<T: std::fmt::Debug>(r: Result<T, ReviewError>) -> &'static str {
    match r.unwrap_err() {
        ReviewError::Refused(r) => r.code,
        ReviewError::Deck(e) => panic!("a deck error: {e}"),
    }
}

pub(super) fn accepted(id: &str, input: &str) -> Settle {
    Settle::Receipt(Receipt::accepted(
        CommandId::from(id),
        1,
        CommandResult {
            session_id: None,
            input_id: Some(InputId::from(input)),
            seq: None,
        },
        0,
    ))
}

pub(super) fn rejected(id: &str) -> Settle {
    Settle::Receipt(Receipt::rejected(
        CommandId::from(id),
        Reject::new(RejectCode::UnknownSession, "no"),
        0,
    ))
}

// ---- schema, ids, cleanup ------------------------------------------------

#[tokio::test]
async fn migration_v21_is_valid_and_makes_the_review_tables() {
    super::schema::migrations().validate().unwrap();
    let w = setup().await;
    for table in [
        "review_comments",
        "reviews",
        "review_revs",
        "review_imports",
        "review_attempts",
    ] {
        let n = w
            .rows(Box::leak(
                format!(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '{table}'"
                )
                .into_boxed_str(),
            ))
            .await;
        assert_eq!(n, 1, "{table}");
    }
    // The newest table of the list is the last migration: `project_sessions`
    // of V19 is older and the review tables come after it.
    let order = w
        .rows(
            "SELECT (SELECT rowid FROM sqlite_master WHERE name = 'reviews')
                  > (SELECT rowid FROM sqlite_master WHERE name = 'project_sessions')",
        )
        .await;
    assert_eq!(order, 1);
}

#[tokio::test]
async fn a_short_id_clash_retries_with_a_longer_id_and_a_short_id_never_changes() {
    let w = setup().await;
    let first = w.add("a.rs", 1).await;
    assert!(
        first.short_id.starts_with("c-") && first.short_id.len() == 8,
        "{}",
        first.short_id
    );
    let alphabet = "abcdefghijkmnpqrstuvwxyz23456789";
    for _ in 0..50 {
        let id = random_short("c-", 12).unwrap();
        assert!(id[2..].chars().all(|c| alphabet.contains(c)), "{id}");
        assert_eq!(id.len(), 14);
    }
    let station = w.station.clone();
    let taken = first.short_id.clone();
    let got = super::call(&w.s.inner.db, move |conn| {
        let mut lens = Vec::new();
        let id = alloc_short(conn, "review_comments", &station, "s1", |len| {
            lens.push(len);
            // The first two lengths clash; the third is free.
            Ok(match lens.len() {
                1 => taken.clone(),
                2 => taken.clone(),
                _ => format!("c-{}", "x".repeat(len)),
            })
        })?;
        Ok((id, lens))
    })
    .await
    .unwrap();
    assert_eq!(got.1, [6, 8, 10]);
    assert_eq!(got.0, format!("c-{}", "x".repeat(10)));
    // Every length clashes: the call fails.
    let station = w.station.clone();
    let taken = first.short_id.clone();
    let all = super::call(&w.s.inner.db, move |conn| {
        Ok(alloc_short(conn, "review_comments", &station, "s1", |_| {
            Ok(taken.clone())
        })
        .is_err())
    })
    .await
    .unwrap();
    assert!(all);
    // An edit and a send leave the short id as it is, and the table refuses
    // a change.
    let edited =
        w.s.review_edit(
            w.station.clone(),
            "s1".into(),
            Viewer::Owner,
            Hide::Nothing,
            first.comment_id.clone(),
            CommentPatch {
                body: Some("new body".into()),
                resolved: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(edited.comment.short_id, first.short_id);
    let sent = w.send("k1", &[&first], "cmd-1").await;
    let seen = w.list(Viewer::Owner, Hide::Nothing).await;
    assert_eq!(seen.threads[0].comments[0].short_id, first.short_id);
    assert_eq!(sent.summary.comment_ids, vec![first.short_id.clone()]);
    let changed = super::call(&w.s.inner.db, |conn| {
        Ok(conn
            .execute("UPDATE review_comments SET short_id = 'c-zzzzzz'", [])
            .is_err())
    })
    .await
    .unwrap();
    assert!(changed);
}

#[tokio::test]
async fn a_removed_station_takes_its_review_rows_along() {
    let w = setup().await;
    let c = w.add("a.rs", 1).await;
    w.send("k1", &[&c], "cmd-1").await;
    w.add("b.rs", 2).await;
    assert!(w.rows("SELECT COUNT(*) FROM review_comments").await > 0);
    let removed = w.s.revoke_station(w.key).await.unwrap();
    assert!(removed.is_some());
    for table in [
        "review_comments",
        "reviews",
        "review_revs",
        "review_imports",
        "review_attempts",
    ] {
        let sql: &'static str = Box::leak(format!("SELECT COUNT(*) FROM {table}").into_boxed_str());
        assert_eq!(w.rows(sql).await, 0, "{table}");
    }
}

#[tokio::test]
async fn a_snapshot_without_the_session_deletes_its_review_rows_and_a_snapshot_with_it_keeps_them()
{
    let w = setup().await;
    let a = w.add_as("s1", &Viewer::Owner, "a.rs", 1).await;
    w.send("k1", &[&a], "cmd-1").await;
    w.add_as("s2", &Viewer::Owner, "b.rs", 1).await;
    // The snapshot names both: nothing goes.
    w.snapshot(&["s1", "s2"], 2).await;
    assert_eq!(w.rows("SELECT COUNT(*) FROM review_comments").await, 2);
    assert_eq!(w.rows("SELECT COUNT(*) FROM reviews").await, 1);
    // The snapshot names s2 only: the rows of s1 go, the rows of s2 stay.
    w.snapshot(&["s2"], 3).await;
    assert_eq!(
        w.rows("SELECT COUNT(*) FROM review_comments WHERE session_id = 's1'")
            .await,
        0
    );
    assert_eq!(w.rows("SELECT COUNT(*) FROM reviews").await, 0);
    assert_eq!(
        w.rows("SELECT COUNT(*) FROM review_revs WHERE session_id = 's1'")
            .await,
        0
    );
    assert_eq!(
        w.rows("SELECT COUNT(*) FROM review_comments WHERE session_id = 's2'")
            .await,
        1
    );
    assert_eq!(
        w.rows("SELECT COUNT(*) FROM review_revs WHERE session_id = 's2'")
            .await,
        1
    );
}

// ---- visibility ----------------------------------------------------------

#[tokio::test]
async fn drafts_are_private_to_their_author_and_a_token_sees_sent_comments_only() {
    let w = setup().await;
    let ann = Viewer::Member("Ann".into());
    let bob = Viewer::Member("Bob".into());
    let owner_sent = w.add("a.rs", 1).await;
    w.send("k1", &[&owner_sent], "cmd-1").await;
    let owner_draft = w.add("b.rs", 2).await;
    let ann_draft = w.add_as("s1", &ann, "c.rs", 3).await;
    let ids = |t: &fleetdeck_proto::review::ReviewThreads| -> Vec<String> {
        t.threads
            .iter()
            .flat_map(|t| t.comments.iter().map(|c| c.comment_id.clone()))
            .collect()
    };
    let owner = ids(&w.list(Viewer::Owner, Hide::Nothing).await);
    assert!(owner.contains(&owner_draft.comment_id));
    assert!(
        !owner.contains(&ann_draft.comment_id),
        "not another author's draft"
    );
    let ann_sees = ids(&w.list(ann.clone(), Hide::Nothing).await);
    assert!(ann_sees.contains(&ann_draft.comment_id) && ann_sees.contains(&owner_sent.comment_id));
    assert!(!ann_sees.contains(&owner_draft.comment_id));
    let bob_sees = ids(&w.list(bob, Hide::Nothing).await);
    assert_eq!(bob_sees, vec![owner_sent.comment_id.clone()]);
    let token = ids(&w.list(Viewer::Token, Hide::Nothing).await);
    assert_eq!(token, vec![owner_sent.comment_id.clone()]);
    // Another author's draft is as unknown as a missing comment.
    for id in [&ann_draft.comment_id, &owner_draft.comment_id] {
        let who = if *id == ann_draft.comment_id {
            Viewer::Owner
        } else {
            ann.clone()
        };
        let r =
            w.s.review_edit(
                w.station.clone(),
                "s1".into(),
                who.clone(),
                Hide::Nothing,
                id.clone(),
                CommentPatch {
                    body: None,
                    resolved: Some(true),
                },
            )
            .await;
        assert_eq!(code(r), "unknown_comment");
        let r =
            w.s.review_delete(
                w.station.clone(),
                "s1".into(),
                who,
                Hide::Nothing,
                id.clone(),
            )
            .await;
        assert_eq!(code(r), "unknown_comment");
    }
}

#[tokio::test]
async fn a_comment_on_a_denied_path_or_other_path_is_hidden_from_a_member_only() {
    let w = setup().await;
    let plain = w.add("src/a.rs", 1).await;
    let secret = w.add("app/.env", 2).await;
    let mut renamed = W::fields("ok.txt", 3);
    renamed.other_path = Some("deploy/id_rsa".to_owned());
    let renamed = w
        .add_target("s1", &Viewer::Owner, Target::Anchor(renamed), "x")
        .await;
    w.send("k1", &[&plain, &secret, &renamed], "cmd-1").await;
    let deny = || Hide::Denied(MemberDeny::default());
    let ids = |t: &fleetdeck_proto::review::ReviewThreads| -> Vec<String> {
        t.threads
            .iter()
            .flat_map(|t| t.comments.iter().map(|c| c.short_id.clone()))
            .collect()
    };
    let member = w.list(Viewer::Member("Ann".into()), deny()).await;
    assert_eq!(ids(&member), vec![plain.short_id.clone()]);
    // The review lists the short ids that the member may see.
    assert_eq!(member.reviews[0].comment_ids, vec![plain.short_id.clone()]);
    assert_eq!(ids(&w.list(Viewer::Owner, Hide::Nothing).await).len(), 3);
    assert_eq!(ids(&w.list(Viewer::Token, Hide::Nothing).await).len(), 3);
    // A deny list that cannot be established: a member sees no comment.
    let none = w.list(Viewer::Member("Ann".into()), Hide::Everything).await;
    assert!(none.threads.is_empty() && none.reviews.is_empty());
}

// ---- edit, resolve, delete -----------------------------------------------

#[tokio::test]
async fn an_edit_checks_the_author_then_the_state_and_a_reply_is_not_resolved() {
    let w = setup().await;
    let ann = Viewer::Member("Ann".into());
    let top = w.add("a.rs", 1).await;
    let edit = |viewer: Viewer, id: &str, body: Option<&str>, resolved: Option<bool>| {
        let s = w.s.clone();
        let (station, id, body) = (w.station.clone(), id.to_owned(), body.map(str::to_owned));
        async move {
            s.review_edit(
                station,
                "s1".into(),
                viewer,
                Hide::Nothing,
                id,
                CommentPatch { body, resolved },
            )
            .await
        }
    };
    // The author edits a draft; a resolve of the own draft top works.
    let done = edit(Viewer::Owner, &top.comment_id, Some("changed"), Some(true))
        .await
        .unwrap();
    assert!(done.body_changed && done.resolved_changed);
    assert_eq!(
        (done.comment.body.as_str(), done.comment.resolved),
        ("changed", true)
    );
    w.send("k1", &[&top], "cmd-1").await;
    // Sent: another author resolves it, nobody edits the body.
    let done = edit(ann.clone(), &top.comment_id, None, Some(false))
        .await
        .unwrap();
    assert!(!done.comment.resolved);
    assert_eq!(
        code(edit(ann.clone(), &top.comment_id, Some("x"), None).await),
        "not_author"
    );
    assert_eq!(
        code(edit(Viewer::Owner, &top.comment_id, Some("x"), None).await),
        "not_draft"
    );
    // A reply is never resolved.
    let reply = w
        .add_target("s1", &ann, Target::Reply(top.comment_id.clone()), "re")
        .await;
    assert_eq!(reply.thread_id, top.comment_id);
    assert_eq!(
        code(edit(ann.clone(), &reply.comment_id, None, Some(true)).await),
        "invalid_body"
    );
    // An unknown id.
    assert_eq!(
        code(edit(Viewer::Owner, "nope", Some("x"), None).await),
        "unknown_comment"
    );
}

#[tokio::test]
async fn a_delete_removes_a_draft_and_its_draft_replies_and_never_a_sent_comment() {
    let w = setup().await;
    let del = |viewer: Viewer, id: &str| {
        let s = w.s.clone();
        let (station, id) = (w.station.clone(), id.to_owned());
        async move {
            s.review_delete(station, "s1".into(), viewer, Hide::Nothing, id)
                .await
        }
    };
    let top = w.add("a.rs", 1).await;
    let r1 = w
        .add_target(
            "s1",
            &Viewer::Owner,
            Target::Reply(top.comment_id.clone()),
            "r1",
        )
        .await;
    let r2 = w
        .add_target(
            "s1",
            &Viewer::Owner,
            Target::Reply(top.short_id.clone()),
            "r2",
        )
        .await;
    assert_eq!(
        (r1.snapshot.as_str(), r2.path.as_str()),
        (top.snapshot.as_str(), "a.rs")
    );
    assert_eq!(r1.anchor_text, top.anchor_text);
    del(Viewer::Owner, &top.comment_id).await.unwrap();
    assert_eq!(w.rows("SELECT COUNT(*) FROM review_comments").await, 0);
    // A reply alone goes alone.
    let top = w.add("a.rs", 1).await;
    let reply = w
        .add_target(
            "s1",
            &Viewer::Owner,
            Target::Reply(top.comment_id.clone()),
            "r",
        )
        .await;
    del(Viewer::Owner, &reply.comment_id).await.unwrap();
    assert_eq!(w.rows("SELECT COUNT(*) FROM review_comments").await, 1);
    // A sent comment is never deleted.
    w.send("k1", &[&top], "cmd-1").await;
    assert_eq!(code(del(Viewer::Owner, &top.comment_id).await), "not_draft");
    assert_eq!(
        code(del(Viewer::Member("Ann".into()), &top.comment_id).await),
        "not_author"
    );
    // A draft top with a reply that is not a draft of the same author keeps
    // the thread.
    let top2 = w.add("b.rs", 2).await;
    let r = w
        .add_target(
            "s1",
            &Viewer::Owner,
            Target::Reply(top2.comment_id.clone()),
            "r",
        )
        .await;
    super::call(&w.s.inner.db, {
        let id = r.comment_id.clone();
        move |conn| {
            conn.execute(
                "UPDATE review_comments SET state = 'sent' WHERE comment_id = ?1",
                [id],
            )?;
            Ok(())
        }
    })
    .await
    .unwrap();
    assert_eq!(
        code(del(Viewer::Owner, &top2.comment_id).await),
        "thread_has_replies"
    );
}

#[tokio::test]
async fn an_import_key_makes_one_comment_and_a_session_has_at_most_500_comments() {
    let w = setup().await;
    let add = |key: Option<&str>, n: u32| {
        let s = w.s.clone();
        let station = w.station.clone();
        let key = key.map(str::to_owned);
        async move {
            s.review_add(
                station,
                "s1".into(),
                Viewer::Owner,
                Hide::Nothing,
                NewReviewComment {
                    target: Target::Anchor(W::fields("a.rs", n)),
                    body: "note".into(),
                    import_key: key,
                },
            )
            .await
        }
    };
    let key = "ab".repeat(32);
    let (first, new) = add(Some(&key), 1).await.unwrap();
    assert!(new);
    let (again, new) = add(Some(&key), 1).await.unwrap();
    assert!(!new);
    assert_eq!(again, first);
    assert_eq!(w.rows("SELECT COUNT(*) FROM review_comments").await, 1);
    // Deleting the imported draft frees its key: it can be imported again.
    w.s.review_delete(
        w.station.clone(),
        "s1".into(),
        Viewer::Owner,
        Hide::Nothing,
        first.comment_id,
    )
    .await
    .unwrap();
    let (_, new) = add(Some(&key), 1).await.unwrap();
    assert!(new);
    for n in 1..MAX_COMMENTS_PER_SESSION {
        add(None, u32::try_from(n).unwrap()).await.unwrap();
    }
    assert_eq!(code(add(None, 9).await), "too_many_comments");
    // Another session has its own cap.
    w.add_as("s2", &Viewer::Owner, "a.rs", 1).await;
}
