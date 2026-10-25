//! B26 PRE-PR review fixes (design `grants-update.md`, "Build answers"): the
//! link start rebuilds the derived scopes that a rebuild during the gap
//! removed, and the settle epoch of a station moves with each returning link.

use super::*;

/// A scope rebuild during the gap (another grant changed) removes the
/// `coordinate` scope of a grant that is idle by its own clock; the hold at
/// the link start makes the grant in effect again and must put the scope back
/// in the same transaction, so the first call of the returning link passes.
#[tokio::test]
async fn the_link_start_restores_a_scope_that_a_rebuild_in_the_gap_removed() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", 60_000, true).await;
    // Unbound, so never idle: it holds the scope until the owner revokes it.
    let other = grant(&w, &w.a, "coord-2", 24 * 3_600_000, false).await;
    assert!(has_coordinate(&w, &w.a).await);
    advance(&w, 55);
    heard(&w, &w.a, w.s.now_ms()).await;
    advance(&w, 6);
    // An unrelated change rebuilds the scopes: `g` is idle by its own clock
    // (61 s > 60 s) and still `active`, so the scope goes.
    assert!(
        w.s.revoke_grant(other.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    assert!(!has_coordinate(&w, &w.a).await);
    assert_eq!(get(&w, &g).await.state, GrantState::Active);
    advance(&w, 4);
    drop(w.s.take_scope_changes());
    assert!(!w.s.hold_returning(w.a.clone()).await.unwrap());
    assert_eq!(get(&w, &g).await.last_used_at, Some(w.s.now_ms()));
    assert!(has_coordinate(&w, &w.a).await);
    // The change is noted for the link cache.
    assert!(w.s.take_scope_changes().contains(&w.a));
    up(&w, &w.a, 1);
    assert_eq!(query(&w, &g).await, Ok(()));
}

/// The link start does not add a scope that no grant in effect gives.
#[tokio::test]
async fn the_link_start_rebuild_keeps_an_ended_grant_without_a_scope() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", 60_000, true).await;
    heard(&w, &w.a, w.s.now_ms()).await;
    assert!(
        w.s.revoke_grant(g.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    assert!(!has_coordinate(&w, &w.a).await);
    advance(&w, 10);
    w.s.hold_returning(w.a.clone()).await.unwrap();
    assert!(!has_coordinate(&w, &w.a).await);
}

/// The settle epoch: `None` while a link waits; it grows with each returning
/// link and never goes back, so an answer read before a return is stale after
/// that link settled too.
#[tokio::test]
async fn the_settle_epoch_moves_with_each_returning_link() {
    let w = world().await;
    assert_eq!(w.s.settle_epoch(&w.a), Some(0));
    assert!(w.s.settled_since(&w.a, 0));
    w.s.mark_unsettled(&w.a, 1);
    up(&w, &w.a, 1);
    assert_eq!(w.s.settle_epoch(&w.a), None);
    assert!(!w.s.settled_since(&w.a, 0));
    assert!(answer(&w, &w.a, 1, "inc-1", Some("v1")).await.settled);
    assert_eq!(w.s.settle_epoch(&w.a), Some(1));
    assert!(!w.s.settled_since(&w.a, 0));
    assert!(w.s.settled_since(&w.a, 1));
    // A returning link that ends before its check still moves the epoch.
    w.s.mark_unsettled(&w.a, 2);
    w.s.set_link_features(&w.a, 2, vec![]);
    w.s.clear_link_features(&w.a, 2);
    assert_eq!(w.s.settle_epoch(&w.a), Some(2));
    assert!(!w.s.settled_since(&w.a, 1));
    // The epoch of one station does not move another.
    assert_eq!(w.s.settle_epoch(&w.b), Some(0));
}
