//! Pinned-key mutual TLS over loopback (PLAN §8.2, D13). Every test runs real
//! TCP + TLS 1.3 handshakes between two identities on 127.0.0.1.

#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ClientConfig, ServerConfig};
use s4_link::addr::AddrError;
use s4_link::identity::{Identity, PeerKey};
use s4_link::net::{self, NetError};
use s4_link::tls::{self, PeerPolicy, PinSet, PinnedServerVerifier, Role};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Runs one handshake. The listener writes "ok" after a successful accept,
/// and the dialer counts as connected only after it reads "ok" (in TLS 1.3 a
/// client learns that the server refused its certificate on the first read).
async fn handshake(
    server: Arc<ServerConfig>,
    client: Arc<ClientConfig>,
) -> (Result<PeerKey, String>, Result<PeerKey, String>) {
    let listener = net::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let srv = tokio::spawn(async move {
        match net::accept(&listener, server).await {
            Ok((mut tls, key, from)) => {
                assert!(from.ip().is_loopback());
                tls.write_all(b"ok").await.map_err(|e| e.to_string())?;
                tls.flush().await.map_err(|e| e.to_string())?;
                let mut buf = [0u8; 1];
                let _ = tls.read(&mut buf).await;
                Ok(key)
            }
            Err(e) => Err(e.to_string()),
        }
    });
    let cli = async {
        let (mut tls, key) = net::connect(addr, client)
            .await
            .map_err(|e| e.to_string())?;
        let mut buf = [0u8; 2];
        tls.read_exact(&mut buf).await.map_err(|e| e.to_string())?;
        if &buf != b"ok" {
            return Err("bad greeting".to_owned());
        }
        tls.shutdown().await.map_err(|e| e.to_string())?;
        Ok(key)
    }
    .await;
    (srv.await.unwrap(), cli)
}

fn pinned(pins: &Arc<PinSet>, role: Role) -> PeerPolicy {
    PeerPolicy::Pinned {
        pins: pins.clone(),
        role,
    }
}

struct Pair {
    deck: Identity,
    station: Identity,
    deck_pins: Arc<PinSet>,
    station_pins: Arc<PinSet>,
}

fn paired() -> Pair {
    let deck = Identity::generate().unwrap();
    let station = Identity::generate().unwrap();
    let deck_pins = PinSet::new();
    let station_pins = PinSet::new();
    deck_pins.pin(station.public_key(), Role::Station);
    station_pins.pin(deck.public_key(), Role::Deck);
    Pair {
        deck,
        station,
        deck_pins,
        station_pins,
    }
}

#[tokio::test]
async fn pinned_mtls_works_with_the_station_dialing() {
    let p = paired();
    let server = tls::server_config(&p.deck, pinned(&p.deck_pins, Role::Station)).unwrap();
    let client = tls::client_config(&p.station, pinned(&p.station_pins, Role::Deck)).unwrap();
    let (srv, cli) = handshake(server, client).await;
    assert_eq!(srv.unwrap(), p.station.public_key());
    assert_eq!(cli.unwrap(), p.deck.public_key());
}

#[tokio::test]
async fn pinned_mtls_works_with_the_deck_dialing() {
    let p = paired();
    let server = tls::server_config(&p.station, pinned(&p.station_pins, Role::Deck)).unwrap();
    let client = tls::client_config(&p.deck, pinned(&p.deck_pins, Role::Station)).unwrap();
    let (srv, cli) = handshake(server, client).await;
    assert_eq!(srv.unwrap(), p.deck.public_key());
    assert_eq!(cli.unwrap(), p.station.public_key());
}

#[tokio::test]
async fn an_unpinned_listener_is_refused_by_the_dialer() {
    let p = paired();
    let stranger = Identity::generate().unwrap();
    let server = tls::server_config(&stranger, PeerPolicy::AnyKey).unwrap();
    let client = tls::client_config(&p.station, pinned(&p.station_pins, Role::Deck)).unwrap();
    let (srv, cli) = handshake(server, client).await;
    assert!(cli.is_err(), "{cli:?}");
    assert!(srv.is_err(), "{srv:?}");
}

#[tokio::test]
async fn an_unpinned_dialer_is_refused_by_the_listener() {
    let p = paired();
    let stranger = Identity::generate().unwrap();
    let server = tls::server_config(&p.deck, pinned(&p.deck_pins, Role::Station)).unwrap();
    let client =
        tls::client_config(&stranger, PeerPolicy::Fingerprint(p.deck.fingerprint())).unwrap();
    let (srv, cli) = handshake(server, client).await;
    assert!(srv.is_err(), "{srv:?}");
    assert!(cli.is_err(), "{cli:?}");
}

#[tokio::test]
async fn a_pinned_key_with_the_wrong_role_is_refused() {
    let p = paired();
    // The deck also pins another deck: that key is not a station.
    let other_deck = Identity::generate().unwrap();
    p.deck_pins.pin(other_deck.public_key(), Role::Deck);
    let server = tls::server_config(&p.deck, pinned(&p.deck_pins, Role::Station)).unwrap();
    let client =
        tls::client_config(&other_deck, PeerPolicy::Fingerprint(p.deck.fingerprint())).unwrap();
    let (srv, _) = handshake(server, client).await;
    assert!(srv.is_err(), "{srv:?}");
}

#[tokio::test]
async fn a_revoked_key_is_refused_on_the_next_handshake() {
    let p = paired();
    let server = tls::server_config(&p.deck, pinned(&p.deck_pins, Role::Station)).unwrap();
    let client = tls::client_config(&p.station, pinned(&p.station_pins, Role::Deck)).unwrap();
    assert!(handshake(server.clone(), client.clone()).await.0.is_ok());
    assert!(p.deck_pins.unpin(&p.station.public_key()));
    // The same client config: a resumed session must not skip the check.
    let (srv, cli) = handshake(server.clone(), client).await;
    assert!(srv.is_err(), "{srv:?}");
    assert!(cli.is_err(), "{cli:?}");
    // The deck can revoke in the other direction too: a station unpins its deck.
    p.deck_pins.pin(p.station.public_key(), Role::Station);
    let client = tls::client_config(&p.station, pinned(&p.station_pins, Role::Deck)).unwrap();
    assert!(handshake(server.clone(), client.clone()).await.1.is_ok());
    assert!(p.station_pins.unpin(&p.deck.public_key()));
    assert!(handshake(server, client).await.1.is_err());
}

#[test]
fn configs_never_resume_sessions() {
    let id = Identity::generate().unwrap();
    let server = tls::server_config(&id, PeerPolicy::AnyKey).unwrap();
    assert_eq!(server.send_tls13_tickets, 0);
    assert!(!server.session_storage.can_cache());
}

#[tokio::test]
async fn a_dialer_without_a_certificate_is_refused() {
    let p = paired();
    let server = tls::server_config(&p.deck, pinned(&p.deck_pins, Role::Station)).unwrap();
    let verifier =
        Arc::new(PinnedServerVerifier::new(PeerPolicy::Fingerprint(p.deck.fingerprint())).unwrap());
    let client = Arc::new(
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth(),
    );
    let (srv, cli) = handshake(server, client).await;
    assert!(srv.is_err(), "{srv:?}");
    assert!(cli.is_err(), "{cli:?}");
}

#[tokio::test]
async fn the_fingerprint_policy_accepts_only_the_named_key() {
    let p = paired();
    let server = tls::server_config(&p.deck, PeerPolicy::AnyKey).unwrap();
    let good =
        tls::client_config(&p.station, PeerPolicy::Fingerprint(p.deck.fingerprint())).unwrap();
    let (srv, cli) = handshake(server.clone(), good).await;
    assert_eq!(cli.unwrap(), p.deck.public_key());
    assert_eq!(srv.unwrap(), p.station.public_key());
    let wrong = Identity::generate().unwrap().fingerprint();
    let bad = tls::client_config(&p.station, PeerPolicy::Fingerprint(wrong)).unwrap();
    assert!(handshake(server, bad).await.1.is_err());
}

/// A certificate made with an attacker key that carries the pinned public key
/// inside a private extension. The verifier must read the real SPKI only.
fn forged_cert(pinned: &PeerKey) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let mut params = rcgen::CertificateParams::new(vec![tls::SERVER_NAME.to_owned()]).unwrap();
    let mut spki = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    spki.extend_from_slice(pinned.as_bytes());
    params
        .custom_extensions
        .push(rcgen::CustomExtension::from_oid_content(
            &[1, 3, 6, 1, 4, 1, 55555, 1],
            spki,
        ));
    let cert = params.self_signed(&pair).unwrap().der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pair.serialize_der()));
    (cert, key)
}

#[tokio::test]
async fn a_forged_certificate_that_embeds_a_pinned_key_is_refused() {
    let p = paired();
    let (cert, key) = forged_cert(&p.deck.public_key());
    let server = tls::server_config_raw(vec![cert.clone()], key, PeerPolicy::AnyKey).unwrap();
    let client = tls::client_config(&p.station, pinned(&p.station_pins, Role::Deck)).unwrap();
    let (_, cli) = handshake(server, client).await;
    assert!(cli.is_err(), "{cli:?}");
    // The same holds for a dialer that forges the pinned station key.
    let (cert, key) = forged_cert(&p.station.public_key());
    let server = tls::server_config(&p.deck, pinned(&p.deck_pins, Role::Station)).unwrap();
    let client = tls::client_config_raw(
        vec![cert],
        key,
        PeerPolicy::Fingerprint(p.deck.fingerprint()),
    )
    .unwrap();
    let (srv, _) = handshake(server, client).await;
    assert!(srv.is_err(), "{srv:?}");
}

#[tokio::test]
async fn public_and_unspecified_addresses_are_refused_before_any_packet() {
    let started = Instant::now();
    for bad in ["0.0.0.0:0", "[::]:0", "203.0.113.7:7000"] {
        let addr: SocketAddr = bad.parse().unwrap();
        assert!(matches!(
            net::bind(addr).await,
            Err(NetError::Addr(AddrError::Bind { .. }))
        ));
    }
    let id = Identity::generate().unwrap();
    let client = tls::client_config(&id, PeerPolicy::Fingerprint(id.fingerprint())).unwrap();
    for bad in ["8.8.8.8:443", "[2606:4700::1111]:443", "0.0.0.0:443"] {
        let addr: SocketAddr = bad.parse().unwrap();
        assert!(matches!(
            net::connect(addr, client.clone()).await,
            Err(NetError::Addr(AddrError::Dial { .. }))
        ));
    }
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "the checks run before any network i/o"
    );
}
