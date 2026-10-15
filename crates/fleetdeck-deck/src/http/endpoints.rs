//! The live endpoints of the board (DL-190, certificate design 2.6).
//!
//! For each board origin the provider answers from the current snapshot of
//! the resolver: the label, the SHA-256 of the certificate that this origin
//! gets, and how far a browser trusts it. A renewal shows at the next call.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::board::CertTrust;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::sign::CertifiedKey;

use super::guard::origin_label;
use super::tls::fingerprint;
use super::tls_tailscale::{Anchors, BoardResolver, isrg_anchors};

/// One board address with the certificate it presents now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointEntry {
    /// `https://<host>[:<port>]`.
    pub origin: String,
    pub label: String,
    pub certificate_sha256: String,
    pub trust: CertTrust,
}

/// The board origins and the resolver that chooses their certificates.
#[derive(Debug)]
pub struct Endpoints {
    origins: Vec<String>,
    resolver: Arc<BoardResolver>,
    self_signed_fallback: bool,
    anchors: Anchors,
}

impl Endpoints {
    /// `origins` are in the order of `Origins::list`.
    #[must_use]
    pub fn new(
        origins: Vec<String>,
        resolver: Arc<BoardResolver>,
        self_signed_fallback: bool,
    ) -> Self {
        Self {
            origins,
            resolver,
            self_signed_fallback,
            anchors: isrg_anchors(),
        }
    }

    /// The same provider with other trust anchors (a test gives its own).
    #[must_use]
    pub fn with_anchors(mut self, anchors: Anchors) -> Self {
        self.anchors = anchors;
        self
    }

    /// One entry per origin, at `now_ms` (Unix ms).
    #[must_use]
    pub fn entries(&self, now_ms: i64) -> Vec<EndpointEntry> {
        self.origins
            .iter()
            .map(|origin| {
                let host = origin_host(origin);
                // A browser sends no SNI for an IP address.
                let sni = host.parse::<IpAddr>().is_err().then_some(host);
                let pair = self.resolver.pick(sni, now_ms);
                let leaf = pair.cert.first();
                EndpointEntry {
                    origin: origin.clone(),
                    label: origin_label(origin),
                    certificate_sha256: leaf.map(fingerprint).unwrap_or_default(),
                    trust: self.trust(&pair, host, now_ms),
                }
            })
            .collect()
    }

    fn trust(&self, pair: &Arc<CertifiedKey>, host: &str, now_ms: i64) -> CertTrust {
        if !Arc::ptr_eq(pair, &self.resolver.fallback()) {
            return CertTrust::Trusted;
        }
        if self.self_signed_fallback {
            CertTrust::SelfSigned
        } else if self.resolver.owner() && self.chain_trusted(&pair.cert, host, now_ms) {
            CertTrust::Trusted
        } else {
            CertTrust::Unknown
        }
    }

    /// The chain is valid for `host` at `now_ms` under the anchors.
    fn chain_trusted(&self, chain: &[CertificateDer<'static>], host: &str, now_ms: i64) -> bool {
        let Some((leaf, rest)) = chain.split_first() else {
            return false;
        };
        let Ok(end) = webpki::EndEntityCert::try_from(leaf) else {
            return false;
        };
        let Ok(name) = ServerName::try_from(host) else {
            return false;
        };
        let secs = u64::try_from(now_ms / 1000).unwrap_or(0);
        end.verify_is_valid_for_subject_name(&name).is_ok()
            && end
                .verify_for_usage(
                    webpki::ALL_VERIFICATION_ALGS,
                    &self.anchors,
                    rest,
                    UnixTime::since_unix_epoch(Duration::from_secs(secs)),
                    webpki::KeyUsage::server_auth(),
                    None,
                    None,
                )
                .is_ok()
    }
}

/// The host of `https://host[:port]` without brackets and port.
fn origin_host(origin: &str) -> &str {
    let rest = origin.strip_prefix("https://").unwrap_or(origin);
    if let Some(v6) = rest.strip_prefix('[') {
        return v6.split(']').next().unwrap_or(v6);
    }
    rest.split(':').next().unwrap_or(rest)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::http::tls::BoardTls;
    use crate::http::tls_test_pki::{NAME, Pki, day};

    const TS_ORIGIN: &str = "https://deck.tail1.ts.net:7420";
    const IP_ORIGIN: &str = "https://100.64.0.1:7420";
    const FAR: i64 = i64::MAX;

    fn now_ms() -> i64 {
        day(2026, 10, 15) * 1000
    }

    fn self_signed() -> BoardTls {
        let (c, k) = BoardTls::generate(&["100.64.0.1".to_owned(), NAME.to_owned()]).unwrap();
        BoardTls::from_pem(&c, &k, true)
            .unwrap()
            .with_name(Some(NAME))
            .unwrap()
    }

    fn provider(tls: &BoardTls) -> Endpoints {
        let origins = vec![TS_ORIGIN.to_owned(), IP_ORIGIN.to_owned()];
        Endpoints::new(origins, Arc::clone(&tls.resolver), tls.self_signed)
    }

    #[test]
    fn the_tailscale_name_is_trusted_and_the_address_stays_self_signed() {
        let tls = self_signed();
        let pki = Pki::new();
        let cert = pki.check(Duration::ZERO).unwrap();
        tls.resolver
            .set_tailscale(Some((cert.certified_key().unwrap(), FAR)));
        let list = provider(&tls).entries(now_ms());
        assert_eq!(list[0].trust, CertTrust::Trusted);
        assert_eq!(list[0].certificate_sha256, cert.sha256);
        assert_eq!(list[0].label, "Tailnet deck.tail1.ts.net");
        assert_eq!(list[1].trust, CertTrust::SelfSigned);
        assert_eq!(list[1].certificate_sha256, tls.sha256);
        // The next call sees a removal.
        tls.resolver.set_tailscale(None);
        let list = provider(&tls).entries(now_ms());
        assert_eq!(list[0].trust, CertTrust::SelfSigned);
        assert_eq!(list[0].certificate_sha256, tls.sha256);
    }

    #[test]
    fn an_owner_pair_is_trusted_only_when_it_chains_to_the_anchors() {
        let pki = Pki::new();
        let owner = BoardTls::new(pki.chain(), pki.leaf.key_der(), false)
            .unwrap()
            .with_name(Some(NAME))
            .unwrap();
        let anchors: Anchors = Arc::from(pki.anchors());
        let list = provider(&owner)
            .with_anchors(Arc::clone(&anchors))
            .entries(now_ms());
        // The leaf names the tailnet host only: the IP origin is unknown.
        assert_eq!(list[0].trust, CertTrust::Trusted);
        assert_eq!(list[1].trust, CertTrust::Unknown);
        // Without the test anchors the real ones do not know the chain.
        assert_eq!(
            provider(&owner).entries(now_ms())[0].trust,
            CertTrust::Unknown
        );
        // An expired leaf is not trusted.
        let late = day(2027, 6, 1) * 1000;
        let list = provider(&owner).with_anchors(anchors).entries(late);
        assert_eq!(list[0].trust, CertTrust::Unknown);
        // A self-signed owner pair is unknown.
        let (c, k) = BoardTls::generate(&[NAME.to_owned()]).unwrap();
        let own = BoardTls::from_pem(&c, &k, false).unwrap();
        let own = own.with_name(Some(NAME)).unwrap();
        let list = provider(&own).entries(now_ms());
        assert_eq!(list[0].trust, CertTrust::Unknown);
    }

    #[test]
    fn the_host_of_an_origin_loses_brackets_and_port() {
        assert_eq!(origin_host("https://[fd7a::1]:7420"), "fd7a::1");
        assert_eq!(origin_host("https://localhost"), "localhost");
        assert_eq!(origin_host(TS_ORIGIN), "deck.tail1.ts.net");
    }
}
