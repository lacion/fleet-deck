//! Mutual TLS 1.3 with pinned keys (PLAN §8.2).
//!
//! No certificate authority, no host name check: each side reads the peer's
//! Ed25519 key from the certificate ([`PeerKey::from_cert`]) and accepts the
//! connection only when the [`PeerPolicy`] accepts that key. The handshake
//! signature is still verified (TLS 1.3 `CertificateVerify`), so the peer
//! proves that it holds the private key of the certificate it shows.
//!
//! Policies:
//! - [`PeerPolicy::Pinned`]: the key is in the [`PinSet`] with the expected
//!   role. Normal links.
//! - [`PeerPolicy::Fingerprint`]: the key has this fingerprint. The invitee of
//!   a pairing dials (or accepts) the inviter that the `fdinv1:` link names.
//! - [`PeerPolicy::PinnedOrInvited`]: the listener of a node with open
//!   invitations. TLS accepts an unpinned key only while an invitation is
//!   open; the link layer then refuses such a key on every channel except the
//!   pair channel, where the secret proof decides (`pairing`).
//! - [`PeerPolicy::AnyKey`]: any key that proves possession (tests only).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, DistinguishedName, PeerIncompatible,
    ServerConfig, SignatureScheme,
};
use serde::{Deserialize, Serialize};

use crate::identity::{Fingerprint, Identity, PeerKey};

/// The TLS server name that dialers send. Verifiers ignore it.
pub const SERVER_NAME: &str = crate::identity::CERT_NAME;

/// The role a pinned key has (PLAN §8.2: the deck checks that the peer key is
/// paired with role station; the station checks that it is its paired deck).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Deck,
    Station,
}

/// Pinned peer keys of one node. Shared by the verifiers, so `unpin`
/// (revocation) applies to every later handshake at once.
#[derive(Default)]
pub struct PinSet {
    pins: RwLock<BTreeMap<PeerKey, Role>>,
}

impl fmt::Debug for PinSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinSet").field("len", &self.len()).finish()
    }
}

impl PinSet {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Pins a key with a role. Returns false when the lock is poisoned.
    pub fn pin(&self, key: PeerKey, role: Role) -> bool {
        self.pins
            .write()
            .map(|mut m| {
                m.insert(key, role);
            })
            .is_ok()
    }

    /// Removes a pin (revocation). Returns true when a pin existed.
    pub fn unpin(&self, key: &PeerKey) -> bool {
        self.pins
            .write()
            .map(|mut m| m.remove(key).is_some())
            .unwrap_or(false)
    }

    /// The role of a pinned key. A poisoned lock fails closed (no role).
    pub fn role_of(&self, key: &PeerKey) -> Option<Role> {
        self.pins.read().ok().and_then(|m| m.get(key).copied())
    }

    pub fn len(&self) -> usize {
        self.pins.read().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Which peer key a TLS endpoint accepts.
#[derive(Debug, Clone)]
pub enum PeerPolicy {
    Pinned {
        pins: Arc<PinSet>,
        role: Role,
    },
    /// A pinned key of `role`, or any key while the node has an open
    /// invitation (§8.5). The link layer then lets an unpinned key use only
    /// the pair channel.
    PinnedOrInvited {
        pins: Arc<PinSet>,
        role: Role,
        invites: Arc<crate::pairing::Invitations>,
    },
    Fingerprint(Fingerprint),
    AnyKey,
}

impl PeerPolicy {
    pub fn check(&self, key: &PeerKey) -> Result<(), rustls::Error> {
        let ok = match self {
            Self::Pinned { pins, role } => pins.role_of(key) == Some(*role),
            Self::PinnedOrInvited {
                pins,
                role,
                invites,
            } => pins.role_of(key) == Some(*role) || invites.open_count() > 0,
            Self::Fingerprint(fp) => key.fingerprint().matches(fp),
            Self::AnyKey => true,
        };
        if ok {
            Ok(())
        } else {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("a dialer must know the key it dials: AnyKey is only for the inviting listener")]
    AnyKeyForDialer,
    #[error(transparent)]
    Rustls(#[from] rustls::Error),
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn peer_key(cert: &CertificateDer<'_>) -> Result<PeerKey, rustls::Error> {
    PeerKey::from_cert(cert)
        .map_err(|_| rustls::Error::InvalidCertificate(CertificateError::BadEncoding))
}

fn check_chain(intermediates: &[CertificateDer<'_>]) -> Result<(), rustls::Error> {
    // A node certificate is self-signed; a chain is a sign of a different peer.
    if intermediates.is_empty() {
        Ok(())
    } else {
        Err(rustls::Error::InvalidCertificate(
            CertificateError::ApplicationVerificationFailure,
        ))
    }
}

fn verify13(
    provider: &CryptoProvider,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, rustls::Error> {
    if dss.scheme != SignatureScheme::ED25519 {
        return Err(rustls::Error::PeerIncompatible(
            PeerIncompatible::NoSignatureSchemesInCommon,
        ));
    }
    verify_tls13_signature(
        message,
        cert,
        dss,
        &provider.signature_verification_algorithms,
    )
}

fn tls12_refused() -> Result<HandshakeSignatureValid, rustls::Error> {
    Err(rustls::Error::PeerIncompatible(
        PeerIncompatible::Tls13RequiredForQuic,
    ))
}

/// Verifies the listener's certificate on the dialer side.
#[derive(Debug)]
pub struct PinnedServerVerifier {
    policy: PeerPolicy,
    provider: Arc<CryptoProvider>,
}

impl PinnedServerVerifier {
    pub fn new(policy: PeerPolicy) -> Result<Self, TlsError> {
        if matches!(
            policy,
            PeerPolicy::AnyKey | PeerPolicy::PinnedOrInvited { .. }
        ) {
            return Err(TlsError::AnyKeyForDialer);
        }
        Ok(Self {
            policy,
            provider: provider(),
        })
    }
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        check_chain(intermediates)?;
        self.policy.check(&peer_key(end_entity)?)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        tls12_refused()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify13(&self.provider, message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

/// Verifies the dialer's certificate on the listener side. Client
/// certificates are mandatory.
#[derive(Debug)]
pub struct PinnedClientVerifier {
    policy: PeerPolicy,
    provider: Arc<CryptoProvider>,
}

impl PinnedClientVerifier {
    pub fn new(policy: PeerPolicy) -> Self {
        Self {
            policy,
            provider: provider(),
        }
    }
}

impl ClientCertVerifier for PinnedClientVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        check_chain(intermediates)?;
        self.policy.check(&peer_key(end_entity)?)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        tls12_refused()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify13(&self.provider, message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

/// The dialer's TLS configuration: TLS 1.3 only, this node's certificate as
/// the client certificate, the listener checked against `policy`.
pub fn client_config(
    identity: &Identity,
    policy: PeerPolicy,
) -> Result<Arc<ClientConfig>, TlsError> {
    client_config_raw(
        vec![identity.cert().clone()],
        identity.private_key_der(),
        policy,
    )
}

/// [`client_config`] with an explicit certificate chain and key (tests use it
/// to present forged certificates).
pub fn client_config_raw(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    policy: PeerPolicy,
) -> Result<Arc<ClientConfig>, TlsError> {
    let verifier = Arc::new(PinnedServerVerifier::new(policy)?);
    let mut config = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certs, key)?;
    // A resumed TLS 1.3 session skips the certificate check, so a revoked key
    // could reconnect with an old ticket. Every link handshake is a full one.
    config.resumption = rustls::client::Resumption::disabled();
    Ok(Arc::new(config))
}

/// The listener's TLS configuration: TLS 1.3 only, mandatory client
/// certificate checked against `policy`.
pub fn server_config(
    identity: &Identity,
    policy: PeerPolicy,
) -> Result<Arc<ServerConfig>, TlsError> {
    server_config_raw(
        vec![identity.cert().clone()],
        identity.private_key_der(),
        policy,
    )
}

/// [`server_config`] with an explicit certificate chain and key.
pub fn server_config_raw(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    policy: PeerPolicy,
) -> Result<Arc<ServerConfig>, TlsError> {
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(PinnedClientVerifier::new(policy)))
        .with_single_cert(certs, key)?;
    // No session tickets and no session cache: see `client_config_raw`.
    config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    config.send_tls13_tickets = 0;
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies_check_keys() {
        let a = Identity::generate().unwrap().public_key();
        let b = Identity::generate().unwrap().public_key();
        let pins = PinSet::new();
        assert!(pins.pin(a, Role::Station));
        let station = PeerPolicy::Pinned {
            pins: pins.clone(),
            role: Role::Station,
        };
        let deck = PeerPolicy::Pinned {
            pins: pins.clone(),
            role: Role::Deck,
        };
        assert!(station.check(&a).is_ok());
        assert!(station.check(&b).is_err());
        assert!(deck.check(&a).is_err(), "a station key is not a deck");
        assert!(PeerPolicy::Fingerprint(a.fingerprint()).check(&a).is_ok());
        assert!(PeerPolicy::Fingerprint(a.fingerprint()).check(&b).is_err());
        assert!(PeerPolicy::AnyKey.check(&b).is_ok());
        assert!(pins.unpin(&a));
        assert!(station.check(&a).is_err(), "revocation applies at once");
        assert!(!pins.unpin(&a));
    }

    #[test]
    fn a_dialer_cannot_accept_any_key() {
        let id = Identity::generate().unwrap();
        assert!(matches!(
            client_config(&id, PeerPolicy::AnyKey),
            Err(TlsError::AnyKeyForDialer)
        ));
        assert!(server_config(&id, PeerPolicy::AnyKey).is_ok());
    }
}
