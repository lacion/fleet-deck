//! Pairing, revocation, key rotation and reconnect over loopback (ADR-004
//! §2.8, §2.10, §9.5, §9.7; G5, G11, G12).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Node, T, code, link, listen, next_request, node, paired};
use fleetdeck_core::secret::Secret;
use fleetdeck_link::node::{self as link_node, NodeConfig};
use fleetdeck_link::pairing::{DEFAULT_TTL, MAX_ATTEMPTS_PER_IP};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{
    BackoffPolicy, DialerEvent, GrantSource, Invitation, Invitations, LinkError, LinkListener,
    PairLimiter, REVOKED, pair, revoke, rotate, sign_rotation, spawn_dialer, start,
};
use fleetdeck_proto::CommandId;
use fleetdeck_proto::link::{Outcome, Scope};
use serde_json::json;

/// A deck that invites; its listener runs the pair channel.
fn inviting_deck(limiter: PairLimiter) -> (Node, Arc<Invitations>) {
    let mut deck = node(Role::Deck);
    let invitations = Invitations::new();
    deck.config = deck.config.with_invitations(Arc::clone(&invitations));
    deck.config.pair_limiter = limiter;
    (deck, invitations)
}

fn invite(deck: &Node, inv: &Invitations, l: &LinkListener, scopes: &[Scope]) -> Invitation {
    inv.create(
        l.local_addr,
        deck.config.identity.fingerprint(),
        DEFAULT_TTL,
        scopes.iter().copied(),
    )
    .unwrap()
}

/// The same invitation id with a wrong secret.
fn forged(real: &Invitation) -> Invitation {
    Invitation {
        addr: real.addr,
        fingerprint: real.fingerprint,
        id: real.id.clone(),
        secret: Secret::new([7u8; 16]),
        expiry: real.expiry,
    }
}

async fn try_pair(station: &Node, inv: &Invitation) -> Result<(), LinkError> {
    tokio::time::timeout(T, pair(&station.config, inv, [Scope::Read, Scope::Drive]))
        .await
        .expect("pairing timeout")
        .map(|_| ())
}

fn reason(r: Result<(), LinkError>) -> String {
    match r {
        Ok(()) => "ok".into(),
        Err(e) => e.reason().unwrap_or("none").to_owned(),
    }
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[tokio::test]
async fn pairing_pins_both_sides_and_the_new_link_uses_the_granted_scopes() {
    let logs = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let (deck, invitations) = inviting_deck(PairLimiter::default());
    let mut l = listen(&deck).await;
    let station = node(Role::Station);
    let invitation = invite(&deck, &invitations, &l, &[Scope::Coordinate]);
    let text = invitation.reveal();
    tracing::info!(?invitation, "an invitation in a log line shows no secret");

    // The station joins: it grants the deck read and drive.
    let parsed = Invitation::parse(&text).unwrap();
    let joined = tokio::time::timeout(
        T,
        pair(&station.config, &parsed, [Scope::Read, Scope::Drive]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(joined.key, deck.config.identity.public_key());
    assert_eq!(joined.role, Role::Deck);
    let at_deck = tokio::time::timeout(T, invitations.next_paired())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(at_deck.key, station.config.identity.public_key());
    assert_eq!(at_deck.role, Role::Station);
    assert_eq!(
        deck.grants
            .scopes(&station.config.identity.fingerprint())
            .into_iter()
            .collect::<Vec<_>>(),
        vec![Scope::Coordinate]
    );
    assert_eq!(invitations.open_count(), 0, "an invitation works once");
    assert_eq!(
        reason(try_pair(&station, &parsed).await),
        "invalid_invitation"
    );

    // The joiner then dials the inviter; the link uses the stored grants.
    let (at_station, at_deck_link) = link(&station, &deck, &mut l).await;
    let call = {
        let c = at_deck_link.control.clone();
        tokio::spawn(async move {
            let id = CommandId::from("c1".to_owned());
            c.request("session.interrupt", Some(&id), json!({"session_id": "s"}))
                .await
        })
    };
    let mut at_station = at_station;
    let r = next_request(&mut at_station).await;
    at_station
        .control
        .respond(r.id, Outcome::Ok(json!("ok")))
        .await
        .unwrap();
    assert_eq!(call.await.unwrap().unwrap(), Outcome::Ok(json!("ok")));
    let id = CommandId::from("c2".to_owned());
    let o = at_deck_link
        .control
        .request("approval.answer", Some(&id), json!({}))
        .await
        .unwrap();
    assert_eq!(code(&o), "forbidden", "approve was not granted");

    // G12: no log line carries the invitation secret.
    let secret_hex: String = parsed
        .secret
        .expose()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let out = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(out.contains("paired"), "the capture works: {out}");
    assert!(!out.contains(&secret_hex), "secret in the logs: {out}");
    assert!(!out.contains(text.expose().as_str()));
}

#[tokio::test]
async fn five_wrong_proofs_cancel_only_that_invitation() {
    let (deck, invitations) = inviting_deck(PairLimiter::default());
    let l = listen(&deck).await;
    let a = invite(&deck, &invitations, &l, &[]);
    let b = invite(&deck, &invitations, &l, &[]);
    let station = node(Role::Station);
    let mut seen = Vec::new();
    for _ in 0..5 {
        seen.push(reason(try_pair(&station, &forged(&a)).await));
    }
    assert_eq!(&seen[..4], ["invalid_invitation"; 4]);
    assert_eq!(seen[4], "too_many_attempts");
    // A is cancelled even for the right secret; B still works.
    assert_eq!(reason(try_pair(&station, &a).await), "invalid_invitation");
    assert_eq!(invitations.open_count(), 1);
    assert_eq!(reason(try_pair(&station, &b).await), "ok");
    assert_eq!(
        station
            .config
            .pins
            .role_of(&deck.config.identity.public_key()),
        Some(Role::Deck)
    );
}

#[tokio::test]
async fn the_rate_limit_locks_out_an_ip_and_a_new_invitation_works_after_it() {
    let window = Duration::from_secs(2);
    let (deck, invitations) = inviting_deck(PairLimiter::with_window(window));
    let l = listen(&deck).await;
    let station = node(Role::Station);
    // TLS lets the unpinned station in only while an invitation is open.
    let _keeper = invite(&deck, &invitations, &l, &[]);
    // Ten attempts with proofs for cancelled invitations.
    for _ in 0..MAX_ATTEMPTS_PER_IP {
        let stray = invite(&deck, &invitations, &l, &[]);
        assert!(invitations.cancel(&stray.id));
        assert_eq!(
            reason(try_pair(&station, &stray).await),
            "invalid_invitation"
        );
    }
    let good = invite(&deck, &invitations, &l, &[]);
    assert_eq!(reason(try_pair(&station, &good).await), "too_many_attempts");
    tokio::time::sleep(window + Duration::from_millis(200)).await;
    let fresh = invite(&deck, &invitations, &l, &[]);
    assert_eq!(reason(try_pair(&station, &fresh).await), "ok");
}

#[tokio::test]
async fn an_expired_invitation_never_reaches_the_wire() {
    let (deck, invitations) = inviting_deck(PairLimiter::default());
    let l = listen(&deck).await;
    let mut inv = invite(&deck, &invitations, &l, &[]);
    inv.expiry = 1;
    let station = node(Role::Station);
    assert_eq!(reason(try_pair(&station, &inv).await), "invitation_expired");
}

#[tokio::test]
async fn revocation_closes_the_link_within_one_second_and_the_queued_request_fails() {
    let (deck, station) = paired();
    let mut l = listen(&station).await;
    let (at_deck, mut at_station) = link(&deck, &station, &mut l).await;
    let c = at_deck.control.clone();
    let call = tokio::spawn(async move {
        let id = CommandId::from("queued".to_owned());
        c.request("session.interrupt", Some(&id), json!({"session_id": "s"}))
            .await
    });
    let queued = next_request(&mut at_station).await;

    // `station forget-deck` while the request waits.
    let deck_key = deck.config.identity.public_key();
    let t0 = tokio::time::Instant::now();
    assert!(revoke(&station.config, &deck_key));
    let closing = tokio::time::timeout(Duration::from_secs(1), at_deck.control.closed())
        .await
        .expect("the link closes within 1 s");
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert_eq!(closing.reason, REVOKED);
    let r = tokio::time::timeout(T, call).await.unwrap().unwrap();
    assert!(!matches!(r, Ok(Outcome::Ok(_))), "{r:?}");
    assert!(
        at_station
            .control
            .respond(queued.id, Outcome::Ok(json!("ok")))
            .await
            .is_err(),
        "a revoked link answers nothing"
    );
    assert!(station.grants.scopes(&deck_key.fingerprint()).is_empty());

    // The revoked key is refused at the next handshake.
    let again = tokio::time::timeout(
        T,
        link_node::dial(
            &deck.config,
            l.local_addr,
            station.config.identity.public_key(),
        ),
    )
    .await
    .unwrap();
    assert!(again.is_err());
}

/// A deck with a new key that pins `station` (the deck after a rotation).
fn rotated_deck(station: &Node) -> Node {
    let n = node(Role::Deck);
    n.config
        .pins
        .pin(station.config.identity.public_key(), Role::Station);
    n
}

#[tokio::test]
async fn a_rotated_key_keeps_only_the_old_grants_and_the_old_key_is_refused() {
    let (deck, station) = paired();
    let mut l = listen(&station).await;
    let (at_deck, _at_station) = link(&deck, &station, &mut l).await;
    let new_deck = rotated_deck(&station);
    let cmd = CommandId::from("rot-1".to_owned());
    let rotation = sign_rotation(
        &deck.config.identity,
        &new_deck.config.identity.public_key(),
        &station.config.identity.public_key(),
        &cmd,
    );
    let new_key = rotate(
        &station.config,
        &deck.config.identity.public_key(),
        &rotation,
        &cmd,
    )
    .unwrap();
    assert_eq!(new_key, new_deck.config.identity.public_key());
    let closing = tokio::time::timeout(Duration::from_secs(1), at_deck.control.closed())
        .await
        .expect("the old key's link closes within 1 s");
    assert_eq!(closing.reason, REVOKED);

    let (at_new, _s) = link(&new_deck, &station, &mut l).await;
    let id = CommandId::from("c".to_owned());
    let o = at_new
        .control
        .request("station.configure", Some(&id), json!({}))
        .await
        .unwrap();
    assert_eq!(
        code(&o),
        "forbidden",
        "admin was never granted to the old key"
    );
    let mut scopes: Vec<_> = station
        .grants
        .scopes(&new_key.fingerprint())
        .into_iter()
        .collect();
    scopes.sort();
    assert_eq!(scopes, vec![Scope::Read, Scope::Drive, Scope::Approve]);
    let old = link_node::dial(
        &deck.config,
        l.local_addr,
        station.config.identity.public_key(),
    )
    .await;
    assert!(old.is_err(), "the old key is revoked");
}

async fn next_event(rx: &mut tokio::sync::mpsc::Receiver<DialerEvent>) -> DialerEvent {
    tokio::time::timeout(T, rx.recv())
        .await
        .expect("dialer event timeout")
        .expect("dialer ended")
}

#[tokio::test]
async fn the_dialer_redials_with_a_higher_epoch_and_stops_when_revoked() {
    let (deck, station) = paired();
    let mut l = listen(&station).await;
    let addr = l.local_addr;
    let station_cfg: NodeConfig = station.config.clone();
    let (ctl_tx, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let acceptor = tokio::spawn(async move {
        let mut keep = Vec::new();
        while let Some(open) = l.next_link().await {
            let live = start(&station_cfg, open);
            let _ = ctl_tx.send(live.control.clone());
            keep.push(live);
        }
    });
    let policy = BackoffPolicy {
        base: Duration::from_millis(20),
        cap: Duration::from_millis(200),
        stable: Duration::from_secs(60),
    };
    let station_key = station.config.identity.public_key();
    let (mut events, task) = spawn_dialer(deck.config.clone(), addr, station_key, policy);

    let DialerEvent::Linked(first) = next_event(&mut events).await else {
        panic!("expected a link");
    };
    let first_epoch = first.id.epoch;
    // The station side drops the link (a network drop for the dialer).
    let at_station = tokio::time::timeout(T, controls.recv())
        .await
        .unwrap()
        .unwrap();
    at_station.close("test_drop", "the test dropped the link");
    match next_event(&mut events).await {
        DialerEvent::Lost {
            reason, retry_in, ..
        } => {
            assert_eq!(reason, "test_drop");
            assert!(retry_in >= Duration::from_millis(10) && retry_in <= Duration::from_millis(20));
        }
        other => panic!("expected Lost, got {other:?}"),
    }
    let DialerEvent::Linked(second) = next_event(&mut events).await else {
        panic!("expected a new link");
    };
    assert!(
        second.id.epoch > first_epoch,
        "every attempt uses a higher epoch"
    );

    // The deck removes the station: the link closes and the dialer stops.
    assert!(revoke(&deck.config, &station_key));
    match next_event(&mut events).await {
        DialerEvent::Lost { reason, .. } => assert_eq!(reason, REVOKED),
        other => panic!("expected Lost, got {other:?}"),
    }
    match next_event(&mut events).await {
        DialerEvent::Stopped { reason } => assert_eq!(reason, "not_paired"),
        other => panic!("expected Stopped, got {other:?}"),
    }
    tokio::time::timeout(T, task).await.unwrap().unwrap();
    drop((first, second));
    acceptor.abort();
}

#[tokio::test]
async fn a_failed_dial_is_reported_with_its_reason_and_retried() {
    let (deck, station) = paired();
    // Nobody listens at this address.
    let addr = {
        let l = listen(&station).await;
        l.local_addr
    };
    let policy = BackoffPolicy {
        base: Duration::from_millis(10),
        cap: Duration::from_millis(40),
        stable: Duration::from_secs(60),
    };
    let key = station.config.identity.public_key();
    let (mut events, task) = spawn_dialer(deck.config.clone(), addr, key, policy);
    for expected in 0..3 {
        match next_event(&mut events).await {
            DialerEvent::Failed {
                attempt,
                reason,
                retry_in,
            } => {
                assert_eq!(attempt, expected);
                assert!(!reason.is_empty());
                assert!(retry_in <= Duration::from_millis(40));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
    drop(events);
    tokio::time::timeout(T, task).await.unwrap().unwrap();
}
