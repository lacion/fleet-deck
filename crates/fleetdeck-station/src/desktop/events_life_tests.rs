//! The lifetimes of ordinary windows (R13 P7b §7): the incarnation of every
//! window address that the station sees, the epoch of the event stream and
//! the close hook.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::SessionId;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;

use super::*;

async fn socket(dir: &Path) -> UnixListener {
    let inst = dir.join("hypr/sig");
    std::fs::create_dir_all(&inst).unwrap();
    UnixListener::bind(inst.join(".socket2.sock")).unwrap()
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..300 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timeout: {what}");
}

#[tokio::test]
async fn every_open_gets_a_new_incarnation_and_a_close_removes_the_address() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = socket(tmp.path()).await;
    let events = Events::new();
    events.ensure(tmp.path(), "sig");
    let (mut peer, _) = listener.accept().await.unwrap();
    assert!(events.wait_connected(Duration::from_secs(2)).await);
    assert_eq!(events.incarnation("0xaa"), None);
    // A window that the station did not launch counts too.
    peer.write_all(b"openwindow>>aa,1,firefox,t\n")
        .await
        .unwrap();
    until("the first open", || events.incarnation("0xaa").is_some()).await;
    let first = events.incarnation("0xaa").unwrap();
    peer.write_all(b"closewindow>>aa\n").await.unwrap();
    until("the close", || events.incarnation("0xaa").is_none()).await;
    // The address opens again: a new number, never the old one.
    peer.write_all(b"openwindow>>aa,1,firefox,t\n")
        .await
        .unwrap();
    until("the reopen", || events.incarnation("0xaa").is_some()).await;
    assert!(events.incarnation("0xaa").unwrap() > first);
}

#[test]
fn an_enumeration_gets_the_existing_number_or_a_new_one() {
    let events = Events::new();
    let a = events.observe("0xaa");
    assert_eq!(events.observe("0xaa"), a);
    assert_eq!(events.incarnation("0xaa"), Some(a));
    let b = events.observe("0xbb");
    assert!(b > a);
    // An open event of a seen address is a new window.
    events.inject(Event::Open {
        address: "0xaa".into(),
        class: "c".into(),
    });
    assert!(events.incarnation("0xaa").unwrap() > b);
    events.inject(Event::Close {
        address: "0xaa".into(),
    });
    assert_eq!(events.incarnation("0xaa"), None);
    assert_eq!(events.incarnation("0xbb"), Some(b));
}

#[tokio::test]
async fn the_epoch_grows_at_a_connection_and_at_each_gap() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = socket(tmp.path()).await;
    let events = Events::new();
    assert_eq!(events.epoch(), 0);
    events.ensure(tmp.path(), "sig");
    let (peer, _) = listener.accept().await.unwrap();
    assert!(events.wait_connected(Duration::from_secs(2)).await);
    let connected = events.epoch();
    assert!(connected >= 1);
    // The stream ends: a gap. The incarnations are gone, the epoch grew.
    events.observe("0xaa");
    drop(peer);
    until("the gap", || events.epoch() > connected).await;
    assert_eq!(events.incarnation("0xaa"), None);
    // The task connects again: the epoch grows once more.
    let after_gap = events.epoch();
    let (_again, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("a reconnect")
        .unwrap();
    until("the reconnect", || events.epoch() > after_gap).await;
    assert!(events.wait_connected(Duration::from_secs(2)).await);
    // A new signature restarts the stream: the epoch grows and the numbers go.
    events.observe("0xbb");
    let before = events.epoch();
    events.ensure(tmp.path(), "other");
    assert!(events.epoch() > before);
    assert_eq!(events.incarnation("0xbb"), None);
}

#[test]
fn a_gap_clears_the_incarnations_and_grows_the_epoch() {
    let events = Events::new();
    events.observe("0xaa");
    let before = events.epoch();
    events.inject_gap();
    assert_eq!(events.epoch(), before + 1);
    assert_eq!(events.incarnation("0xaa"), None);
}

#[test]
fn the_close_hook_runs_outside_the_lock_with_the_address() {
    let events = Events::new();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let slot = Arc::clone(&seen);
    let inner = events.clone();
    events.on_close(Arc::new(move |address| {
        // The state is readable from the hook: the lock is released.
        slot.lock()
            .unwrap()
            .push((address.to_owned(), inner.incarnation(address)));
    }));
    events.observe("0xaa");
    events.inject(Event::Close {
        address: "0xaa".into(),
    });
    assert_eq!(*seen.lock().unwrap(), [("0xaa".to_owned(), None)]);
    // The owner path of a preview close runs the hook too.
    events.observe("0xbb");
    events.forget("0xbb");
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[test]
fn the_previews_at_an_address_are_found_by_address() {
    let events = Events::new();
    assert!(events.preview_ids_at("0xaa").is_empty());
    // A record and a preview of the station at `0xaa`.
    let record = Record {
        signature: "sig".into(),
        address: "0xaa".into(),
        class: "c".into(),
        kind: Kind::Preview,
        display_id: None,
        monitor: None,
        opener: Some(SessionId::new("01HZZZZZZZZZZZZZZZZZZZZZZZ")),
        launch_id: "launch".into(),
    };
    events.inner.lock().records.insert("0xaa".into(), record);
    let owner = PreviewOwner {
        deck_fp: "deck".into(),
        grant_id: "g".into(),
        caller_station: "s".into(),
        caller_session: "x".into(),
    };
    events
        .register_preview("handle", owner, "label", "0xaa")
        .unwrap();
    assert_eq!(events.preview_ids_at("0xaa"), ["handle"]);
    assert!(events.preview_ids_at("0xbb").is_empty());
}
