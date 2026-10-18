#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use super::*;

fn owner(reg: &Arc<Registry>, hash: &str, addr: &str) -> Ticket {
    reg.register(hash, SessionKind::Owner, SocketKind::Shell, addr)
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[tokio::test]
async fn fence_go_live_is_false_after_a_revoke_while_the_entry_is_pending() {
    let reg = Arc::new(Registry::default());
    let t = owner(&reg, "h1", "10.0.0.1");
    let waiters = reg.revoke(&strings(&["h1"]), &[]);
    assert_eq!(waiters.len(), 1);
    assert!(t.is_revoked());
    // The row still exists, and the entry is still refused.
    assert!(!reg.go_live(&t, || true));
    assert_eq!(reg.entries()[0].state, SocketState::Pending);
    // A pending entry has no relay: its waiter is done at once.
    assert!(wait_all(waiters, Duration::from_millis(50)).await);
    reg.remove(&t);
    assert!(reg.entries().is_empty());
}

#[test]
fn fence_go_live_is_false_when_the_session_row_is_gone() {
    let reg = Arc::new(Registry::default());
    let t = owner(&reg, "h1", "10.0.0.1");
    assert!(!reg.go_live(&t, || false));
    assert!(!t.is_revoked());
    // The row is there now: the same entry goes live.
    assert!(reg.go_live(&t, || true));
    assert_eq!(reg.entries()[0].state, SocketState::Live);
}

#[test]
fn fence_go_live_is_false_for_an_entry_that_was_removed() {
    let reg = Arc::new(Registry::default());
    let t = owner(&reg, "h1", "10.0.0.1");
    reg.remove(&t);
    assert!(!reg.go_live(&t, || true));
}

#[tokio::test]
async fn fence_a_live_relay_ends_on_a_revoke_and_the_waiter_sees_it() {
    let reg = Arc::new(Registry::default());
    let t = owner(&reg, "h1", "10.0.0.1");
    assert!(reg.go_live(&t, || true));
    let relay_ticket = t.clone();
    let relay_reg = Arc::clone(&reg);
    let relay = tokio::spawn(async move {
        relay_ticket.revoked().await;
        // The relay stops forwarding, then it leaves.
        tokio::time::sleep(Duration::from_millis(30)).await;
        relay_reg.remove(&relay_ticket);
    });
    let waiters = reg.revoke(&strings(&["h1"]), &[]);
    assert_eq!(waiters.len(), 1);
    assert!(wait_all(waiters, Duration::from_secs(5)).await);
    relay.await.unwrap();
    assert!(reg.entries().is_empty());
}

#[tokio::test]
async fn fence_wait_all_is_false_when_a_relay_does_not_end_in_time() {
    let reg = Arc::new(Registry::default());
    let t = owner(&reg, "h1", "10.0.0.1");
    assert!(reg.go_live(&t, || true));
    let waiters = reg.revoke(&strings(&["h1"]), &[]);
    assert!(!wait_all(waiters.clone(), Duration::from_millis(50)).await);
    // The relay ends later: a new wait is true.
    reg.remove(&t);
    assert!(wait_all(waiters, Duration::from_millis(50)).await);
}

#[tokio::test]
async fn fence_wait_all_is_true_for_no_waiter() {
    assert!(wait_all(Vec::new(), Duration::from_millis(10)).await);
}

#[test]
fn fence_revoke_takes_the_entries_of_a_session_or_of_an_address_only() {
    let reg = Arc::new(Registry::default());
    let a = owner(&reg, "h1", "10.0.0.1");
    let b = reg.register("h2", SessionKind::Member, SocketKind::Board, "10.0.0.2");
    let c = owner(&reg, "h3", "10.0.0.3");
    assert!(reg.go_live(&a, || true));
    assert!(reg.go_live(&b, || true));
    assert!(reg.go_live(&c, || true));
    let waiters = reg.revoke(&strings(&["h1"]), &strings(&["10.0.0.2"]));
    assert_eq!(waiters.len(), 2);
    assert!(a.is_revoked());
    assert!(b.is_revoked());
    assert!(!c.is_revoked());
    // A revoke of nothing touches nothing.
    assert!(reg.revoke(&[], &[]).is_empty());
}

#[test]
fn fence_two_sockets_of_one_session_are_both_revoked() {
    let reg = Arc::new(Registry::default());
    let a = reg.register("h1", SessionKind::Owner, SocketKind::Board, "10.0.0.1");
    let b = reg.register("h1", SessionKind::Owner, SocketKind::Shell, "10.0.0.1");
    assert_eq!(reg.revoke(&strings(&["h1"]), &[]).len(), 2);
    assert!(a.is_revoked() && b.is_revoked());
}

#[tokio::test]
async fn fence_a_signal_that_fires_before_the_wait_does_not_block() {
    let s = Signal::default();
    s.fire();
    tokio::time::timeout(Duration::from_millis(50), s.wait())
        .await
        .unwrap();
}
