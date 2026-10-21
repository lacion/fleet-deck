//! MCP P4b deck test 12 at the store level: one sequence orders every give,
//! change and end of a session, a newer set supersedes the older pending one,
//! and the reconcile after a reconnect queues the latest desired state.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_proto::command::DeckCommandState;

use super::*;
use super::{World, accept, give, grant, new_grant, row_of, set_state, sets, world};

use DeckCommandState::{Done, Failed, Pending};

async fn change_classes(w: &World, id: &str, list: &[GrantClass]) {
    let ch = Change {
        classes: list.iter().copied().collect::<BTreeSet<_>>(),
        spawn_cap: None,
        max_modes: None,
        answer_cap: None,
        allow_widening: true,
        now: 0,
    };
    w.s.change_access(id.to_owned(), ch).await.unwrap().unwrap();
}

/// The `(seq, state, has a spec)` of the sets of a session.
async fn shape(w: &World, session: &str) -> Vec<(u64, DeckCommandState, bool)> {
    sets(w, session)
        .await
        .iter()
        .map(|c| (c.seq, c.state, c.spec.is_some()))
        .collect()
}

/// A revoke, then a give: the sequence only goes up, also across grants, and
/// a set never restores the older grant.
#[tokio::test]
async fn the_session_sequence_reconciles_after_a_reconnect_and_supersedes_pending_sets() {
    let w = world().await;
    use GrantClass::{Drive, Mail, Read};

    // Revoke A, then give B: seq 1, 2, 3. The pending end of A is superseded
    // by the give of B; nothing sends the older state after the newer one.
    let a = give(&w, "owner-1", new_grant(&w, "owner-1")).await.unwrap();
    accept(&w, &sets(&w, "owner-1").await[0].id, Some(1)).await;
    assert!(
        w.s.revoke_with_row(a.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    assert_eq!(grant(&w, &a.grant_id).await.state, GrantState::Revoked);
    let b = give(&w, "owner-1", new_grant(&w, "owner-1")).await.unwrap();
    assert_eq!(
        shape(&w, "owner-1").await,
        [(1, Done, true), (2, Failed, false), (3, Pending, true)]
    );
    let sets_of_1 = sets(&w, "owner-1").await;
    assert_eq!(sets_of_1[1].reason.as_deref(), Some("superseded"));
    assert_eq!(sets_of_1[2].grant_id, b.grant_id);
    let row = row_of(&w, "owner-1").await.unwrap();
    assert_eq!(
        (row.grant_id.as_str(), row.seq, row.station_seq),
        (b.grant_id.as_str(), 3, 1)
    );
    // A revoke of a grant that the row no longer names uses today's path.
    assert!(
        !w.s.revoke_with_row(a.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    assert_eq!(shape(&w, "owner-1").await.len(), 3);

    // The other order: give B, then change it, then revoke it, all before a
    // receipt. Every set supersedes the one before; the sequence is 1, 2, 3.
    let c = give(&w, "owner-2", new_grant(&w, "owner-2")).await.unwrap();
    change_classes(&w, &c.grant_id, &[Read, Mail, Drive]).await;
    assert!(
        w.s.revoke_with_row(c.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    assert_eq!(
        shape(&w, "owner-2").await,
        [(1, Failed, true), (2, Failed, true), (3, Pending, false)]
    );
    // Revoking again changes nothing and queues nothing.
    assert!(
        w.s.revoke_with_row(c.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    assert_eq!(shape(&w, "owner-2").await.len(), 3);
    assert_eq!(row_of(&w, "owner-2").await.unwrap().seq, 3);
}

/// The reconcile queues the latest desired state of the sessions that the
/// station did not acknowledge, and only when no set is pending.
#[tokio::test]
async fn the_reconcile_queues_the_latest_state_of_an_unacknowledged_session_once() {
    let w = world().await;
    use GrantClass::{Drive, Mail, Read};
    let g = give(&w, "owner-1", new_grant(&w, "owner-1")).await.unwrap();
    let id = g.grant_id.clone();
    accept(&w, &sets(&w, "owner-1").await[0].id, Some(1)).await;
    // The row is acknowledged: nothing to do.
    assert_eq!(w.s.reconcile_owner_grants(w.a.clone()).await.unwrap(), 0);

    // A change that is pending: nothing to do while it can still be sent.
    change_classes(&w, &id, &[Read, Mail, Drive]).await;
    assert_eq!(w.s.reconcile_owner_grants(w.a.clone()).await.unwrap(), 0);
    // The set was never sent and passed its retry window: it fails
    // (`not_sent`); the grant stays, because the station acknowledged a set.
    w.det.advance(Duration::from_millis(16 * 60_000));
    w.s.expire_commands().await.unwrap();
    assert_eq!(grant(&w, &id).await.state, GrantState::Active);
    assert_eq!(w.s.reconcile_owner_grants(w.a.clone()).await.unwrap(), 1);
    let queued = sets(&w, "owner-1").await;
    let last = queued.last().unwrap();
    assert_eq!((last.seq, last.state), (2, Pending));
    let spec = last.spec.as_ref().unwrap();
    assert_eq!(spec.classes, ["read", "mail", "drive"]);
    // A second reconcile finds the pending set.
    assert_eq!(w.s.reconcile_owner_grants(w.a.clone()).await.unwrap(), 0);
    // The receipt closes the gap.
    accept(&w, &last.id.clone(), Some(2)).await;
    assert_eq!(row_of(&w, "owner-1").await.unwrap().station_seq, 2);
    assert_eq!(w.s.reconcile_owner_grants(w.a.clone()).await.unwrap(), 0);

    // A grant that is terminal when the link comes back: the latest state is
    // the end (a set with no spec, the row's sequence).
    assert!(
        w.s.revoke_with_row(id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    w.det.advance(Duration::from_millis(16 * 60_000));
    w.s.expire_commands().await.unwrap();
    assert_eq!(w.s.reconcile_owner_grants(w.a.clone()).await.unwrap(), 1);
    let queued = sets(&w, "owner-1").await;
    let last = queued.last().unwrap();
    assert_eq!(
        (last.seq, last.state, last.spec.is_none()),
        (3, Pending, true)
    );
    assert_eq!(row_of(&w, "owner-1").await.unwrap().seq, 3);
    accept(&w, &last.id.clone(), Some(3)).await;

    // An idle-expired grant is still held: the state is the spec.
    let held = give(&w, "owner-2", new_grant(&w, "owner-2")).await.unwrap();
    accept(&w, &sets(&w, "owner-2").await[0].id, Some(1)).await;
    let b = held.grant_id.clone();
    set_state(&w, &b, "expired", Some("idle")).await;
    change_classes(&w, &b, &[Read, Mail, Drive]).await;
    w.det.advance(Duration::from_millis(16 * 60_000));
    w.s.expire_commands().await.unwrap();
    assert_eq!(w.s.reconcile_owner_grants(w.a.clone()).await.unwrap(), 1);
    let last = sets(&w, "owner-2").await.pop().unwrap();
    assert!(last.spec.is_some());
    assert_eq!((last.seq, last.state), (2, Pending));
}
