//! The station side of a tailnet join over loopback (DL-178, tailnet design
//! 5.4 and 8): the retry file, the resume after a crash or a restart with a
//! renamed station, and the refusals that end a request.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod pair_support;

use std::net::SocketAddr;
use std::time::Duration;

use common::node;
use fleetdeck_link::pair_hub::HubConfig;
use fleetdeck_link::pair_request::CanonicalRequest;
use fleetdeck_link::retry_file::{self, RetryRecord};
use fleetdeck_link::tailnet_join::{JoinError, JoinPlan, drive};
use fleetdeck_link::tls::Role;
use fleetdeck_proto::link::Scope;
use pair_support::{Deck, T, approve_first, deck, open, pending};

fn plan(deck: &Deck, dir: &std::path::Path, name: &str) -> JoinPlan {
    JoinPlan {
        host: "deck.tail.ts.net".into(),
        port: deck.addr().port(),
        deck: deck.fp(),
        name: name.into(),
        scopes: [Scope::Read, Scope::Drive].into(),
        addrs: vec![deck.addr()],
        dir: dir.to_path_buf(),
        wait: Duration::from_secs(30),
    }
}

#[tokio::test]
async fn a_join_writes_the_retry_file_before_the_request_and_the_caller_removes_it() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let dir = tempfile::tempdir().unwrap();
    let station = node(Role::Station);
    let p = plan(&deck, dir.path(), "laptop");
    let config = station.config.clone();
    let task = tokio::spawn(async move { drive(&config, &p).await });
    let view = pending(&deck, 1).await.remove(0);
    // The file exists while the request waits, with the whole request.
    let record = retry_file::read(dir.path())
        .unwrap()
        .expect("the retry file");
    assert_eq!(record.request_id, view.request_id);
    assert_eq!(record.name, "laptop");
    assert_eq!(record.deck_fingerprint, deck.fp().to_string());
    assert_eq!(record.offered(), [Scope::Read, Scope::Drive].into());
    approve_first(&deck).await;
    let joined = tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(joined.approved.deck, deck.node.config.identity.public_key());
    assert_eq!(joined.scopes, [Scope::Read, Scope::Drive].into());
    assert_eq!(joined.addr, deck.addr());
    // The caller stores the pin first and removes the file after it.
    assert!(retry_file::read(dir.path()).unwrap().is_some());
    assert!(retry_file::remove(dir.path()).unwrap());
}

#[tokio::test]
async fn a_station_that_crashed_before_pending_resumes_with_its_saved_request() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let dir = tempfile::tempdir().unwrap();
    let station = node(Role::Station);
    let p = plan(&deck, dir.path(), "laptop");
    let config = station.config.clone();
    let first = {
        let p = p.clone();
        tokio::spawn(async move { drive(&config, &p).await })
    };
    let view = pending(&deck, 1).await.remove(0);
    // The station crashes while the deck commits the approval.
    let abort = first.abort_handle();
    *deck.memory.on_commit.lock().unwrap() = Some(Box::new(move || abort.abort()));
    approve_first(&deck).await;
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(retry_file::read(dir.path()).unwrap().is_some());
    // The restarted station (renamed in the meantime) resumes.
    let renamed = plan(&deck, dir.path(), "renamed-laptop");
    let joined = drive(&station.config, &renamed).await.unwrap();
    assert_eq!(joined.approved.deck, deck.node.config.identity.public_key());
    assert_eq!(joined.scopes, [Scope::Read, Scope::Drive].into());
    let stored = deck
        .memory
        .done
        .lock()
        .unwrap()
        .get(&view.request_id)
        .cloned();
    assert_eq!(stored.unwrap().requested_name, "laptop", "the old request");
    assert_eq!(deck.memory.done.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_saved_request_that_the_deck_never_approved_is_sent_again_with_the_same_id() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let dir = tempfile::tempdir().unwrap();
    let station = node(Role::Station);
    let p = plan(&deck, dir.path(), "laptop");
    let req = CanonicalRequest::generate("laptop", p.scopes.clone()).unwrap();
    retry_file::write(
        dir.path(),
        &RetryRecord::new(&p.host, p.port, &p.deck.to_string(), &req),
    )
    .unwrap();
    let config = station.config.clone();
    let task = tokio::spawn(async move { drive(&config, &p).await });
    let view = pending(&deck, 1).await.remove(0);
    assert_eq!(view.request_id, req.id_hex(), "the saved id is used again");
    approve_first(&deck).await;
    tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_retry_file_of_another_deck_blocks_a_new_join_until_it_is_cancelled() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let dir = tempfile::tempdir().unwrap();
    let station = node(Role::Station);
    let req = CanonicalRequest::generate("laptop", [Scope::Read].into()).unwrap();
    retry_file::write(
        dir.path(),
        &RetryRecord::new("other.tail.ts.net", 7421, &"ab".repeat(32), &req),
    )
    .unwrap();
    let err = drive(&station.config, &plan(&deck, dir.path(), "laptop"))
        .await
        .unwrap_err();
    assert!(matches!(err, JoinError::Pending), "{err:?}");
    assert!(err.to_string().contains("--cancel"));
    assert!(deck.hub.list().await.is_empty(), "nothing was sent");
    // `--cancel` removes the file; then the join goes on.
    retry_file::remove(dir.path()).unwrap();
    let config = station.config.clone();
    let p = plan(&deck, dir.path(), "laptop");
    let task = tokio::spawn(async move { drive(&config, &p).await });
    pending(&deck, 1).await;
    approve_first(&deck).await;
    tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_refusal_of_the_deck_removes_the_file_and_a_network_failure_keeps_it() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let dir = tempfile::tempdir().unwrap();
    let station = node(Role::Station);
    // Denied: the file goes away.
    let config = station.config.clone();
    let p = plan(&deck, dir.path(), "laptop");
    let task = tokio::spawn(async move { drive(&config, &p).await });
    let view = pending(&deck, 1).await.remove(0);
    deck.hub.deny(&view.request_id).await.unwrap();
    let err = tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code(), "denied");
    assert!(retry_file::read(dir.path()).unwrap().is_none());
    // A deck that is gone: the outcome is unknown, so the file stays.
    let dir2 = tempfile::tempdir().unwrap();
    let mut p2 = plan(&deck, dir2.path(), "laptop");
    let req = CanonicalRequest::generate("laptop", p2.scopes.clone()).unwrap();
    retry_file::write(
        dir2.path(),
        &RetryRecord::new(&p2.host, p2.port, &p2.deck.to_string(), &req),
    )
    .unwrap();
    let dead: SocketAddr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    p2.addrs = vec![dead];
    let err = drive(&station.config, &p2).await.unwrap_err();
    assert!(matches!(err, JoinError::Link(_)), "{err:?}");
    assert!(retry_file::read(dir2.path()).unwrap().is_some());
}

#[tokio::test]
async fn a_second_address_is_tried_when_the_first_does_not_connect() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let dir = tempfile::tempdir().unwrap();
    let station = node(Role::Station);
    let dead: SocketAddr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let mut p = plan(&deck, dir.path(), "laptop");
    p.addrs = vec![dead, deck.addr()];
    let config = station.config.clone();
    let task = tokio::spawn(async move { drive(&config, &p).await });
    pending(&deck, 1).await;
    approve_first(&deck).await;
    let joined = tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(joined.addr, deck.addr());
}

#[tokio::test]
async fn a_refused_upgrade_keeps_the_retry_file_and_a_later_run_resumes_it() {
    // The window is closed: the deck ends the connection after TLS.
    let deck = deck(HubConfig::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let station = node(Role::Station);
    let p = plan(&deck, dir.path(), "laptop");
    // A request of an earlier run, whose answer was lost.
    let req = CanonicalRequest::generate("laptop", p.scopes.clone()).unwrap();
    retry_file::write(
        dir.path(),
        &RetryRecord::new(&p.host, p.port, &p.deck.to_string(), &req),
    )
    .unwrap();
    let err = drive(&station.config, &p).await.unwrap_err();
    assert_eq!(err.code(), "not_accepted", "{err:?}");
    assert!(matches!(err, JoinError::Link(_)), "{err:?}");
    let saved = retry_file::read(dir.path())
        .unwrap()
        .expect("the retry file stays");
    assert_eq!(saved.request_id, req.id_hex());
    // The window opens; the same plan sends the stored request again.
    open(&deck).await;
    let config = station.config.clone();
    let task = tokio::spawn(async move { drive(&config, &p).await });
    let view = pending(&deck, 1).await.remove(0);
    assert_eq!(view.request_id, saved.request_id, "the stored request");
    approve_first(&deck).await;
    tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn an_explicit_window_closed_answer_still_removes_the_retry_file() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let dir = tempfile::tempdir().unwrap();
    let station = node(Role::Station);
    let p = plan(&deck, dir.path(), "laptop");
    let config = station.config.clone();
    let task = tokio::spawn(async move { drive(&config, &p).await });
    pending(&deck, 1).await;
    assert!(retry_file::read(dir.path()).unwrap().is_some());
    assert!(deck.hub.close_window().await);
    let err = tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code(), "window_closed");
    assert!(retry_file::read(dir.path()).unwrap().is_none());
}
