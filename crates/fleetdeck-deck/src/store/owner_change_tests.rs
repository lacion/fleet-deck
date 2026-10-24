//! MCP P4b deck test 9 at the store level (the routes are P4b-3c): the change
//! of the classes of an owner grant, its elevation rule, the caps of a newly
//! added class, the kept counters and the pending fleet items that a
//! narrowing fails.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

use fleetdeck_proto::command::DeckCommandState;
use rusqlite::params;

use super::*;
use super::{World, give, grant, new_grant, row_of, set_state, sets, worker_grant, world};

fn classes(list: &[GrantClass]) -> BTreeSet<GrantClass> {
    list.iter().copied().collect()
}

fn change(list: &[GrantClass], widening: bool) -> Change {
    Change {
        classes: classes(list),
        spawn_cap: None,
        max_modes: None,
        answer_cap: None,
        allow_widening: widening,
        now: 0,
    }
}

async fn apply(w: &World, id: &str, ch: Change) -> Result<(Grant, Grant), ChangeError> {
    w.s.change_access(id.to_owned(), ch).await.unwrap()
}

use GrantClass::{Answer, Drive, Mail, Manage, Read, Spawn};

/// A pending fleet item of the grant that needs `required`.
async fn item(w: &World, grant_id: &str, outbox_id: &str, required: Option<&str>) {
    let (grant_id, outbox_id, required) = (
        grant_id.to_owned(),
        outbox_id.to_owned(),
        required.map(str::to_owned),
    );
    let now = w.s.now_ms();
    call(&w.s.inner.db, move |conn| {
        conn.execute(
            "INSERT INTO outbox (outbox_id, station_id, kind, origin, payload, state, next_at,
                 expires_at, created_at, updated_at, authority, required_classes)
             SELECT ?1, station_id, 'command', 'coordinator', '{}', 'queued', ?4, ?5, ?4, ?4,
                 json_object('grant_id', grant_id), ?3
             FROM tokens WHERE grant_id = ?2",
            params![outbox_id, grant_id, required, now, now + 60_000],
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

async fn outbox_state(w: &World, outbox_id: &str) -> (String, Option<String>) {
    let id = outbox_id.to_owned();
    call(&w.s.inner.db, move |conn| {
        Ok(conn.query_row(
            "SELECT state, reason FROM outbox WHERE outbox_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
    .await
    .unwrap()
}

async fn set_counters(w: &World, grant_id: &str, spawn_used: u32, answer_used: u32) {
    let id = grant_id.to_owned();
    call(&w.s.inner.db, move |conn| {
        conn.execute(
            "UPDATE tokens SET spawn_used = ?2, answer_used = ?3 WHERE grant_id = ?1",
            params![id, spawn_used, answer_used],
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

/// MCP P4b test 9, the store part: every added class needs the elevation, a
/// removal does not, the caps come with a newly added class only, the
/// counters stay, a worker grant or a grant without a row is refused, and a
/// narrowing fails the pending fleet items that lost a class.
#[tokio::test]
async fn class_and_cap_changes_need_elevation_for_every_widening() {
    let w = world().await;
    let g = give(&w, "owner-1", new_grant(&w, "owner-1")).await.unwrap();
    let id = g.grant_id.clone();

    // An added class needs the elevation; nothing is written without it.
    let denied = apply(&w, &id, change(&[Read, Mail, Drive], false)).await;
    assert_eq!(denied.unwrap_err(), ChangeError::ElevationRequired);
    assert_eq!(grant(&w, &id).await.classes, classes(&[Read, Mail]));
    assert_eq!(sets(&w, "owner-1").await.len(), 1);
    let (before, after) = apply(&w, &id, change(&[Read, Mail, Drive], true))
        .await
        .unwrap();
    assert_eq!(before.classes, classes(&[Read, Mail]));
    assert_eq!(after.classes, classes(&[Read, Mail, Drive]));
    // The set carries the full state and supersedes the pending give.
    let queued = sets(&w, "owner-1").await;
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0].reason.as_deref(), Some("superseded"));
    assert_eq!(
        (queued[1].seq, queued[1].state),
        (2, DeckCommandState::Pending)
    );
    assert_eq!(
        queued[1].spec.as_ref().unwrap().classes,
        ["read", "mail", "drive"]
    );
    assert_eq!(row_seq(&w, "owner-1").await, 2);

    // A removal needs no elevation.
    let (_, after) = apply(&w, &id, change(&[Read, Mail], false)).await.unwrap();
    assert_eq!(after.classes, classes(&[Read, Mail]));
    assert_eq!(row_seq(&w, "owner-1").await, 3);

    // A newly added `spawn` takes the creation defaults, or the given caps.
    let (_, after) = apply(&w, &id, change(&[Read, Mail, Spawn], true))
        .await
        .unwrap();
    assert_eq!(after.spawn_cap, DEFAULT_SPAWN_CAP);
    assert!(after.max_modes.is_empty());
    apply(&w, &id, change(&[Read, Mail], false)).await.unwrap();
    let mut spawn = change(&[Read, Mail, Spawn], true);
    spawn.spawn_cap = Some(7);
    spawn.max_modes = Some(BTreeMap::from([("claude".to_owned(), "plan".to_owned())]));
    let (_, after) = apply(&w, &id, spawn).await.unwrap();
    assert_eq!(after.spawn_cap, 7);
    assert_eq!(after.max_modes["claude"], "plan");

    // The counters stay across a removal and a re-add.
    set_counters(&w, &id, 2, 0).await;
    let (_, gone) = apply(&w, &id, change(&[Read, Mail], false)).await.unwrap();
    assert_eq!(gone.spawn_used, 2);
    let (_, back) = apply(&w, &id, change(&[Read, Mail, Spawn], true))
        .await
        .unwrap();
    assert_eq!((back.spawn_used, back.spawn_cap), (2, DEFAULT_SPAWN_CAP));

    // The caps of a newly added class follow the creation rules.
    let mut ch = change(&[Read, Mail, Answer], true);
    ch.answer_cap = Some(6000);
    assert_eq!(invalid(&w, &id, ch).await, "answer_cap is 0 to 5000");
    let (_, answers) = apply(&w, &id, change(&[Read, Mail, Spawn, Answer], true))
        .await
        .unwrap();
    assert_eq!(answers.answer_cap, Some(DEFAULT_ANSWER_CAP));
    let mut ch = change(&[Read, Mail, Spawn, Answer, Manage], true);
    ch.max_modes = Some(BTreeMap::from([(
        "codex".to_owned(),
        "never/read-only".to_owned(),
    )]));
    let (_, manages) = apply(&w, &id, ch).await.unwrap();
    assert_eq!(manages.max_modes["codex"], "never/read-only");
    let mut ch = change(&[Read, Mail, Spawn, Answer, Manage, Drive], true);
    ch.spawn_cap = Some(51);
    assert_eq!(
        invalid(&w, &id, ch).await,
        "a cap is changed with the caps route"
    );
    let mut ch = change(&[Read, Mail, Spawn, Answer, Manage, Drive], true);
    ch.max_modes = Some(BTreeMap::from([("claude".to_owned(), "plan".to_owned())]));
    assert_eq!(
        invalid(&w, &id, ch).await,
        "a cap is changed with the caps route"
    );
    let mut ch = change(&[Read, Mail, Spawn, Answer, Manage], true);
    ch.answer_cap = Some(10);
    assert_eq!(
        invalid(&w, &id, ch).await,
        "a cap is changed with the caps route"
    );
    let mut ch = change(&[Read, Mail, Spawn, Answer, Manage], true);
    ch.spawn_cap = Some(4);
    assert_eq!(
        invalid(&w, &id, ch).await,
        "a cap is changed with the caps route"
    );
    // The rules that are not about the elevation come first.
    let mut ch = change(&[Read, Mail, Spawn, Answer, Manage], false);
    ch.spawn_cap = Some(4);
    assert!(matches!(
        apply(&w, &id, ch).await.unwrap_err(),
        ChangeError::Invalid(_)
    ));
    let mut ch = change(&[Read], true);
    ch.spawn_cap = Some(4);
    assert_eq!(
        invalid(&w, &id, ch).await,
        "a cap is changed with the caps route"
    );
    apply(&w, &id, change(&[Read, Mail, Answer], false))
        .await
        .unwrap();
    let mut ch = change(&[Read, Mail, Spawn, Answer], true);
    ch.spawn_cap = Some(51);
    assert_eq!(invalid(&w, &id, ch).await, "spawn_cap is 0 to 50");
    let mut ch = change(&[Read, Mail, Spawn, Answer], true);
    ch.max_modes = Some(BTreeMap::from([("bogus".to_owned(), "plan".to_owned())]));
    assert_eq!(
        invalid(&w, &id, ch).await,
        "max_modes has only the families claude and codex"
    );
    let mut ch = change(&[Read, Mail, Spawn, Answer], true);
    ch.max_modes = Some(BTreeMap::from([(
        "claude".to_owned(),
        "bypassPermissions".to_owned(),
    )]));
    assert_eq!(
        invalid(&w, &id, ch).await,
        "a mode ceiling is not valid for its family"
    );
    assert_eq!(
        invalid(&w, &id, change(&[], true)).await,
        "a coordinator grant needs a class"
    );
    // A removed and re-added `answer` keeps its counter.
    set_counters(&w, &id, 0, 9).await;
    apply(&w, &id, change(&[Read, Mail], false)).await.unwrap();
    let (_, again) = apply(&w, &id, change(&[Read, Mail, Answer], true))
        .await
        .unwrap();
    assert_eq!(again.answer_used, 9);

    // The desktop, screenshot and input access is given only at start.
    let text = "desktop, screenshot and input access is given only at start";
    for class in [GrantClass::Desktop, GrantClass::Capture, GrantClass::Input] {
        for widening in [false, true] {
            assert_eq!(
                invalid(&w, &id, change(&[Read, Mail, class], widening)).await,
                text
            );
        }
    }
}

/// The text of the `Invalid` error of a change.
async fn invalid(w: &World, id: &str, ch: Change) -> &'static str {
    match apply(w, id, ch).await.unwrap_err() {
        ChangeError::Invalid(text) => text,
        other => panic!("{other:?}"),
    }
}

async fn row_seq(w: &World, session: &str) -> u64 {
    row_of(w, session).await.unwrap().seq
}

/// A change refuses what is not a live owner grant with a row.
#[tokio::test]
async fn a_change_needs_a_live_owner_grant_with_a_row() {
    let w = world().await;
    let ch = || change(&[Read], false);
    assert_eq!(
        apply(&w, "nope", ch()).await.unwrap_err(),
        ChangeError::NotFound
    );
    let worker = worker_grant(&w, "w1", None).await;
    assert_eq!(
        apply(&w, &worker.grant_id, ch()).await.unwrap_err(),
        ChangeError::Worker
    );
    let plain = w.s.create_grant(new_grant(&w, "owner-2")).await.unwrap();
    assert_eq!(
        apply(&w, &plain.grant_id, ch()).await.unwrap_err(),
        ChangeError::NoRow
    );
    let g = give(&w, "owner-3", new_grant(&w, "owner-3")).await.unwrap();
    // An idle-expired grant is held: the owner can still change it.
    set_state(&w, &g.grant_id, "expired", Some("idle")).await;
    assert!(apply(&w, &g.grant_id, ch()).await.is_ok());
    set_state(&w, &g.grant_id, "expired", Some("expired")).await;
    assert_eq!(
        apply(&w, &g.grant_id, ch()).await.unwrap_err(),
        ChangeError::NotFound
    );
    assert!(
        w.s.revoke_grant(g.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    assert_eq!(
        apply(&w, &g.grant_id, ch()).await.unwrap_err(),
        ChangeError::NotFound
    );
    // A grant past its lifetime is terminal.
    let late = give(&w, "owner-4", new_grant(&w, "owner-4")).await.unwrap();
    w.det.advance(std::time::Duration::from_millis(
        u64::try_from(late.expires_at - w.s.now_ms()).unwrap() + 1,
    ));
    assert_eq!(
        apply(&w, &late.grant_id, ch()).await.unwrap_err(),
        ChangeError::NotFound
    );
}

/// A narrowing fails the pending fleet items that lost a class, with the code
/// `authority_removed`; the others stay.
#[tokio::test]
async fn a_narrowing_fails_the_pending_fleet_items_of_a_removed_class() {
    let w = world().await;
    let mut ng = new_grant(&w, "owner-1");
    ng.classes = classes(&[Read, Mail, Spawn, Manage]);
    let g = give(&w, "owner-1", ng).await.unwrap();
    item(&w, &g.grant_id, "keep", Some(r#"["read"]"#)).await;
    item(&w, &g.grant_id, "manage", Some(r#"["manage"]"#)).await;
    item(&w, &g.grant_id, "start", Some(r#"["spawn","manage"]"#)).await;
    // An item without a set (admitted before V46) holds.
    item(&w, &g.grant_id, "old", None).await;
    apply(&w, &g.grant_id, change(&[Read, Mail, Spawn], false))
        .await
        .unwrap();
    assert_eq!(outbox_state(&w, "keep").await.0, "queued");
    assert_eq!(outbox_state(&w, "old").await.0, "queued");
    assert_eq!(
        outbox_state(&w, "manage").await,
        ("dead".to_owned(), Some("authority_removed".to_owned()))
    );
    assert_eq!(
        outbox_state(&w, "start").await,
        ("dead".to_owned(), Some("authority_removed".to_owned()))
    );
}
