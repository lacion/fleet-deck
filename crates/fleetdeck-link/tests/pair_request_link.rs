//! Pairing requests over loopback (DL-178, tailnet design 5 and 8): the
//! window, the pending record, approval and the signed answer, the retry
//! of a lost answer, the limits and the refusal codes.
//!
//! The deck listener is a loopback socket that has the rules of a tailnet
//! listener (`NodeConfig::listener_kind`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod pair_support;

use std::time::Duration;

use common::{eventually, node};
use fleetdeck_link::hello::feature;
use fleetdeck_link::node::ListenerKind;
use fleetdeck_link::pair_hub::{ApproveError, HubConfig};
use fleetdeck_link::pair_request::CanonicalRequest;
use fleetdeck_link::tls::Role;
use fleetdeck_link::{PairMode, REVOKED};
use fleetdeck_proto::link::Scope;
use pair_support::{
    T, approve_first, ask, ask_at, code, deck, deck_with, open, pending, quick, request, result,
};

#[tokio::test]
async fn a_request_waits_then_an_approval_pairs_with_a_signed_answer() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let station = node(Role::Station);
    let req = request("laptop");
    let asked = ask(&station, &deck, &req, PairMode::Request);

    let view = pending(&deck, 1).await.remove(0);
    assert_eq!(view.requested_name, "laptop");
    assert_eq!(view.request_id, req.id_hex());
    assert_eq!(
        view.station_fingerprint,
        station.config.identity.fingerprint().to_string()
    );
    assert_eq!(view.offered_scopes, vec![Scope::Read, Scope::Drive]);
    assert!(!asked.is_finished(), "the station waits for the owner");

    let approved = approve_first(&deck).await;
    assert_eq!(approved.station, station.config.identity.public_key());
    let answer = result(asked).await.unwrap();
    assert_eq!(answer.deck, deck.node.config.identity.public_key());
    assert_eq!(answer.granted_scopes, [Scope::Admin].into());
    // Commit, then answer: the key is pinned and the record is stored.
    assert_eq!(
        deck.node
            .config
            .pins
            .role_of(&station.config.identity.public_key()),
        Some(Role::Station)
    );
    let stored = deck.memory.done.lock().unwrap().get(&req.id_hex()).cloned();
    let stored = stored.expect("the completion record");
    assert_eq!(stored.request_digest, req.digest());
    assert_eq!(stored.requested_name, "laptop");
    assert_eq!(stored.offered_scopes, [Scope::Read, Scope::Drive].into());
}

#[tokio::test]
async fn no_request_outside_the_window_or_on_a_lan_listener() {
    let deck = deck(HubConfig::default()).await;
    let station = node(Role::Station);
    // The window is closed: TLS refuses the unknown key; the station says so.
    let r = result(ask(&station, &deck, &request("a"), PairMode::Request)).await;
    assert_eq!(code(&r), "not_accepted");
    // The window is open, but a LAN listener never accepts a request.
    open(&deck).await;
    let lan = deck.listen_as(ListenerKind::Other).await;
    let r = result(ask_at(
        &station,
        lan.local_addr,
        deck.fp(),
        &request("a"),
        PairMode::Request,
    ))
    .await;
    assert_eq!(code(&r), "not_accepted");
    assert!(deck.hub.list().await.is_empty());
    assert!(deck.node.config.pins.is_empty());
}

#[tokio::test]
async fn the_window_closing_during_a_wait_ends_the_request_with_window_closed() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let station = node(Role::Station);
    let asked = ask(&station, &deck, &request("a"), PairMode::Request);
    pending(&deck, 1).await;
    assert!(deck.hub.close_window().await);
    assert_eq!(code(&result(asked).await), "window_closed");
    assert!(deck.node.config.pins.is_empty());
}

#[tokio::test]
async fn a_denied_request_closes_with_denied_and_the_key_waits_ten_minutes() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let station = node(Role::Station);
    let asked = ask(&station, &deck, &request("a"), PairMode::Request);
    let view = pending(&deck, 1).await.remove(0);
    deck.hub.deny(&view.request_id).await.unwrap();
    assert_eq!(code(&result(asked).await), "denied");
    let again = ask(&station, &deck, &request("b"), PairMode::Request);
    assert_eq!(code(&result(again).await), "denied");
    assert!(deck.node.config.pins.is_empty());
}

#[tokio::test(start_paused = false)]
async fn a_request_expires_with_the_code_expired() {
    let cfg = HubConfig {
        request_ttl: Duration::from_millis(300),
        ..HubConfig::default()
    };
    let deck = deck(cfg).await;
    open(&deck).await;
    let station = node(Role::Station);
    let asked = ask(&station, &deck, &request("a"), PairMode::Request);
    assert_eq!(code(&result(asked).await), "expired");
    assert!(deck.hub.list().await.is_empty());
}

#[tokio::test]
async fn the_window_deadline_caps_the_expiry_of_a_request() {
    let cfg = HubConfig {
        window_ttl: Duration::from_millis(400),
        ..HubConfig::default()
    };
    let deck = deck(cfg).await;
    open(&deck).await;
    let station = node(Role::Station);
    let started = std::time::Instant::now();
    let asked = ask(&station, &deck, &request("a"), PairMode::Request);
    assert_eq!(code(&result(asked).await), "expired");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn an_old_peer_without_the_feature_is_refused_and_nothing_is_stored() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let mut station = node(Role::Station);
    station
        .config
        .local
        .features
        .retain(|f| f != feature::PAIR_REQUEST);
    let r = result(ask(&station, &deck, &request("a"), PairMode::Request)).await;
    assert_eq!(code(&r), "unsupported");
    assert!(deck.hub.list().await.is_empty());
    // An old deck: the station refuses before it sends the frame.
    let old = deck_with(HubConfig::default(), |c| {
        c.local.features.retain(|f| f != feature::PAIR_REQUEST);
    })
    .await;
    open(&old).await;
    let station = node(Role::Station);
    let r = result(ask(&station, &old, &request("a"), PairMode::Request)).await;
    assert_eq!(code(&r), "unsupported");
    assert!(old.hub.list().await.is_empty());
}

#[tokio::test]
async fn a_key_that_is_paired_is_refused_with_already_paired() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let station = node(Role::Station);
    deck.node
        .config
        .pins
        .pin(station.config.identity.public_key(), Role::Station);
    let r = result(ask(&station, &deck, &request("a"), PairMode::Request)).await;
    assert_eq!(code(&r), "already_paired");
}

#[tokio::test]
async fn a_second_request_of_the_same_key_is_busy_and_the_old_one_stays() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let station = node(Role::Station);
    let first = ask(&station, &deck, &request("a"), PairMode::Request);
    pending(&deck, 1).await;
    let second = ask(&station, &deck, &request("b"), PairMode::Request);
    assert_eq!(code(&result(second).await), "busy");
    assert_eq!(deck.hub.list().await.len(), 1);
    assert!(!first.is_finished());
    approve_first(&deck).await;
    result(first).await.unwrap();
}

#[tokio::test]
async fn a_request_id_of_another_key_or_another_request_is_refused() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let a = node(Role::Station);
    let req = request("a");
    let first = ask(&a, &deck, &req, PairMode::Request);
    pending(&deck, 1).await;
    // Another key sends the same request.
    let b = node(Role::Station);
    let r = result(ask(&b, &deck, &req, PairMode::Request)).await;
    assert_eq!(code(&r), "request_id_conflict");
    // The same key sends the same id with another name.
    let mut changed = req.clone();
    changed.name = "other".into();
    let r = result(ask(&a, &deck, &changed, PairMode::Request)).await;
    assert_eq!(code(&r), "request_id_conflict");
    approve_first(&deck).await;
    result(first).await.unwrap();
}

#[tokio::test]
async fn a_duplicate_name_is_refused_at_approval_and_the_request_stays_pending() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    deck.memory.names.lock().unwrap().insert("laptop".into());
    let station = node(Role::Station);
    let asked = ask(&station, &deck, &request("laptop"), PairMode::Request);
    let view = pending(&deck, 1).await.remove(0);
    let fp = view.station_fingerprint.parse().unwrap();
    let err = deck
        .hub
        .approve(&view.request_id, &fp, &pair_support::digest(&view))
        .await
        .unwrap_err();
    assert_eq!(err, ApproveError::NameTaken("laptop".into()));
    assert_eq!(deck.hub.list().await.len(), 1);
    deck.hub.deny(&view.request_id).await.unwrap();
    assert_eq!(code(&result(asked).await), "denied");
}

#[tokio::test]
async fn approval_of_a_substituted_key_or_a_stale_digest_writes_nothing() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let station = node(Role::Station);
    let asked = ask(&station, &deck, &request("a"), PairMode::Request);
    let view = pending(&deck, 1).await.remove(0);
    let other = node(Role::Station).config.identity.fingerprint();
    let digest = pair_support::digest(&view);
    let err = deck.hub.approve(&view.request_id, &other, &digest).await;
    assert_eq!(err.unwrap_err(), ApproveError::FingerprintMismatch);
    let fp = view.station_fingerprint.parse().unwrap();
    let err = deck.hub.approve(&view.request_id, &fp, &[9u8; 32]).await;
    assert_eq!(err.unwrap_err(), ApproveError::DigestMismatch);
    assert!(deck.memory.done.lock().unwrap().is_empty());
    assert!(deck.node.config.pins.is_empty());
    approve_first(&deck).await;
    result(asked).await.unwrap();
}

#[tokio::test]
async fn a_disconnect_cancels_the_request_and_a_later_approval_finds_nothing() {
    let deck = deck(quick()).await;
    open(&deck).await;
    let station = node(Role::Station);
    let req = request("a");
    let asked = ask(&station, &deck, &req, PairMode::Request);
    let view = pending(&deck, 1).await.remove(0);
    asked.abort();
    // The deck notices the closed socket and removes the record.
    pending(&deck, 0).await;
    let fp = view.station_fingerprint.parse().unwrap();
    let err = deck
        .hub
        .approve(&view.request_id, &fp, &pair_support::digest(&view))
        .await;
    assert_eq!(err.unwrap_err(), ApproveError::UnknownRequest);
    assert!(deck.memory.done.lock().unwrap().is_empty());
    assert!(deck.node.config.pins.is_empty());
}

#[tokio::test]
async fn a_waiting_connection_that_sends_a_message_is_closed() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let station = node(Role::Station);
    let (mut ws, _, _) = fleetdeck_link::open_pair(&station.config, deck.addr(), deck.fp())
        .await
        .unwrap();
    let req = request("a");
    let frame = serde_json::json!({
        "type": "pair_request", "request_id": req.id_hex(), "name": "a",
        "nonce": req.nonce_hex(), "offered_scopes": ["read", "drive"],
    });
    ws.send(Message::text(frame.to_string())).await.unwrap();
    pending(&deck, 1).await;
    ws.send(Message::text("{}")).await.unwrap();
    pending(&deck, 0).await;
}

#[tokio::test]
async fn a_lost_answer_is_resumed_with_the_stored_context_over_a_new_connection() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let station = node(Role::Station);
    let req = request("laptop");
    let asked = ask(&station, &deck, &req, PairMode::Request);
    pending(&deck, 1).await;
    // The station vanishes inside the commit: the deck stores the approval
    // and pins the key, and the answer has nobody to go to.
    let abort = asked.abort_handle();
    *deck.memory.on_commit.lock().unwrap() = Some(Box::new(move || abort.abort()));
    approve_first(&deck).await;
    assert!(asked.await.unwrap_err().is_cancelled());
    assert_eq!(deck.memory.done.lock().unwrap().len(), 1);
    assert_eq!(
        deck.node
            .config
            .pins
            .role_of(&station.config.identity.public_key()),
        Some(Role::Station)
    );
    // A fresh connection resumes with the saved request.
    let resumed = result(ask(&station, &deck, &req, PairMode::Resume))
        .await
        .unwrap();
    assert_eq!(resumed.deck, deck.node.config.identity.public_key());
    assert_eq!(resumed.granted_scopes, [Scope::Admin].into());
    assert_eq!(
        deck.memory.done.lock().unwrap().len(),
        1,
        "never paired twice"
    );
}

#[tokio::test]
async fn a_resume_after_both_processes_restarted_re_signs_the_stored_context() {
    let first = deck(HubConfig::default()).await;
    open(&first).await;
    let station = node(Role::Station);
    let req = request("laptop");
    let asked = ask(&station, &first, &req, PairMode::Request);
    pending(&first, 1).await;
    approve_first(&first).await;
    result(asked).await.unwrap();
    // The deck restarts: same key, same store. The station restarts as well
    // and was renamed; it still has the saved request.
    let identity = first.node.config.identity.clone();
    let memory = std::sync::Arc::clone(&first.memory);
    drop(first);
    let restarted = pair_support::restart_deck(identity, memory).await;
    // The deck answers from its stored record; a station that checked the
    // answer against its new name would refuse it. The station verifies
    // against its saved request, so the renamed one fails.
    let renamed = request_with_name(&req, "renamed-laptop");
    let r = result(ask(&station, &restarted, &renamed, PairMode::Resume)).await;
    assert_eq!(code(&r), "bad_signature");
    // The saved request verifies and completes.
    let r = result(ask(&station, &restarted, &req, PairMode::Resume)).await;
    assert!(r.is_ok(), "{r:?}");
}

fn request_with_name(req: &CanonicalRequest, name: &str) -> CanonicalRequest {
    let mut other = req.clone();
    other.name = name.to_owned();
    other
}

#[tokio::test]
async fn a_resume_with_another_nonce_another_key_or_a_revoked_pairing_is_refused() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let station = node(Role::Station);
    let req = request("laptop");
    let asked = ask(&station, &deck, &req, PairMode::Request);
    pending(&deck, 1).await;
    approve_first(&deck).await;
    result(asked).await.unwrap();

    // Another nonce for the same request id.
    let mut nonce = req.clone();
    nonce.nonce[0] ^= 1;
    let r = result(ask(&station, &deck, &nonce, PairMode::Resume)).await;
    assert_eq!(code(&r), "unknown_request");
    // Another key with the same id and nonce (the window is open, so TLS
    // lets it in).
    let thief = node(Role::Station);
    let r = result(ask(&thief, &deck, &req, PairMode::Resume)).await;
    assert_eq!(code(&r), "unknown_request");
    // The pairing was revoked: the key is not pinned any more.
    deck.node
        .config
        .pins
        .unpin(&station.config.identity.public_key());
    let r = result(ask(&station, &deck, &req, PairMode::Resume)).await;
    assert_eq!(code(&r), REVOKED);
    // The deleted record (a removed station): nothing to resume.
    deck.memory.done.lock().unwrap().clear();
    let r = result(ask(&station, &deck, &req, PairMode::Resume)).await;
    assert_eq!(code(&r), "unknown_request");
}

#[tokio::test]
async fn a_resume_is_accepted_on_a_listener_that_is_not_a_tailnet_listener() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let station = node(Role::Station);
    let req = request("laptop");
    let asked = ask(&station, &deck, &req, PairMode::Request);
    pending(&deck, 1).await;
    approve_first(&deck).await;
    result(asked).await.unwrap();
    // The window closes; the key is pinned, so TLS lets it in on any
    // listener.
    deck.hub.close_window().await;
    let lan = deck.listen_as(ListenerKind::Other).await;
    let r = result(ask_at(
        &station,
        lan.local_addr,
        deck.fp(),
        &req,
        PairMode::Resume,
    ))
    .await;
    assert!(r.is_ok(), "{r:?}");
    // A request on that listener is refused.
    let other = node(Role::Station);
    let r = result(ask_at(
        &other,
        lan.local_addr,
        deck.fp(),
        &request("b"),
        PairMode::Request,
    ))
    .await;
    assert_eq!(code(&r), "not_accepted");
}

#[tokio::test]
async fn the_station_checks_the_deck_key_and_the_deck_signature() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let station = node(Role::Station);
    // A wrong confirmed fingerprint: TLS fails, nothing reaches the deck.
    let wrong = node(Role::Deck).config.identity.fingerprint();
    let r = result(ask_at(
        &station,
        deck.addr(),
        wrong,
        &request("a"),
        PairMode::Request,
    ))
    .await;
    assert_eq!(code(&r), "fingerprint_mismatch");
    assert!(deck.hub.list().await.is_empty());
}

#[tokio::test]
async fn handshake_slots_are_released_while_requests_wait() {
    let cfg = HubConfig {
        max_per_source: 100,
        ..HubConfig::default()
    };
    let deck = deck(cfg).await;
    open(&deck).await;
    // The pair limiter has 4 slots: 6 waiting requests prove the release.
    let mut asked = Vec::new();
    let stations: Vec<_> = (0..6).map(|_| node(Role::Station)).collect();
    for (i, s) in stations.iter().enumerate() {
        asked.push(ask(s, &deck, &request(&format!("s{i}")), PairMode::Request));
        pending(&deck, i + 1).await;
    }
    // The listener holds one unauthenticated permit for each waiting
    // connection: the permit moved into the pending slot.
    eventually("6 permits", || deck.listener.admitted() == 6).await;
    deck.hub.close_window().await;
    for a in asked {
        assert_eq!(code(&result(a).await), "window_closed");
    }
    eventually("the permits are released", || deck.listener.admitted() == 0).await;
    let _ = T;
}

#[tokio::test]
async fn the_cap_of_pending_requests_refuses_at_once_without_eviction() {
    let cfg = HubConfig {
        max_pending: 2,
        max_per_source: 100,
        ..HubConfig::default()
    };
    let deck = deck(cfg).await;
    open(&deck).await;
    let a = node(Role::Station);
    let b = node(Role::Station);
    let c = node(Role::Station);
    let first = ask(&a, &deck, &request("a"), PairMode::Request);
    let second = ask(&b, &deck, &request("b"), PairMode::Request);
    pending(&deck, 2).await;
    let third = ask(&c, &deck, &request("c"), PairMode::Request);
    assert_eq!(code(&result(third).await), "busy");
    assert_eq!(deck.hub.list().await.len(), 2);
    deck.hub.cancel_all().await;
    assert_eq!(code(&result(first).await), "cancelled");
    assert_eq!(code(&result(second).await), "cancelled");
}

#[tokio::test]
async fn revoking_a_key_cancels_its_waiting_request() {
    let deck = deck(HubConfig::default()).await;
    open(&deck).await;
    let station = node(Role::Station);
    let asked = ask(&station, &deck, &request("a"), PairMode::Request);
    pending(&deck, 1).await;
    fleetdeck_link::revoke(&deck.node.config, &station.config.identity.public_key()).await;
    assert_eq!(code(&result(asked).await), "cancelled");
}

#[tokio::test]
async fn invitation_pairing_still_works_on_the_same_tailnet_listener() {
    let deck = deck(HubConfig::default()).await;
    let station = node(Role::Station);
    let invitation = deck
        .invitations
        .create(
            deck.addr(),
            deck.fp(),
            fleetdeck_link::pairing::DEFAULT_TTL,
            [Scope::Admin],
        )
        .unwrap();
    // The window is closed; an open invitation is enough, as before.
    let paired = tokio::time::timeout(
        T,
        fleetdeck_link::pair(&station.config, &invitation, [Scope::Read]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(paired.key, deck.node.config.identity.public_key());
}
