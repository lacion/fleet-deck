//! A test PKI: a root, an intermediate and a leaf for a tailnet name. Only
//! the tests use it.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, date_time_ymd,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, TrustAnchor, UnixTime};

use super::tls_tailscale::{CheckError, TailscaleCert, check};

pub(crate) const NAME: &str = "deck.tail1.ts.net";

/// A date as Unix seconds.
pub(crate) fn day(y: i32, m: u8, d: u8) -> i64 {
    date_time_ymd(y, m, d).unix_timestamp()
}

pub(crate) fn unix(secs: i64) -> UnixTime {
    UnixTime::since_unix_epoch(Duration::from_secs(u64::try_from(secs).unwrap()))
}

/// The test "now": 2026-10-15.
pub(crate) fn now() -> UnixTime {
    unix(day(2026, 10, 15))
}

pub(crate) struct Ca {
    pub(crate) der: CertificateDer<'static>,
    pub(crate) key: KeyPair,
    pub(crate) params: CertificateParams,
}

impl Ca {
    pub(crate) fn issuer(&self) -> Issuer<'_, &KeyPair> {
        Issuer::from_params(&self.params, &self.key)
    }
}

pub(crate) fn ca_params(name: &str, from: (i32, u8, u8), to: (i32, u8, u8)) -> CertificateParams {
    let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
    p.distinguished_name.push(DnType::CommonName, name);
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    p.not_before = date_time_ymd(from.0, from.1, from.2);
    p.not_after = date_time_ymd(to.0, to.1, to.2);
    p
}

pub(crate) fn root(name: &str) -> Ca {
    let params = ca_params(name, (2020, 1, 1), (2040, 1, 1));
    let key = KeyPair::generate().unwrap();
    let der = params.self_signed(&key).unwrap().der().clone();
    Ca { der, key, params }
}

pub(crate) fn intermediate(parent: &Ca, name: &str, to: (i32, u8, u8)) -> Ca {
    let params = ca_params(name, (2024, 1, 1), to);
    let key = KeyPair::generate().unwrap();
    let der = params
        .signed_by(&key, &parent.issuer())
        .unwrap()
        .der()
        .clone();
    Ca { der, key, params }
}

pub(crate) struct Leaf {
    pub(crate) der: CertificateDer<'static>,
    pub(crate) key: KeyPair,
}

impl Leaf {
    pub(crate) fn key_der(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(self.key.serialize_der().into())
    }
}

pub(crate) fn leaf_params(name: &str, to: (i32, u8, u8)) -> CertificateParams {
    let mut p = CertificateParams::new(vec![name.to_owned()]).unwrap();
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    p.not_before = date_time_ymd(2026, 9, 1);
    p.not_after = date_time_ymd(to.0, to.1, to.2);
    p
}

pub(crate) fn leaf_with(issuer: &Ca, params: &CertificateParams) -> Leaf {
    let key = KeyPair::generate().unwrap();
    let der = params
        .signed_by(&key, &issuer.issuer())
        .unwrap()
        .der()
        .clone();
    Leaf { der, key }
}

pub(crate) fn anchors_of(ca: &Ca) -> Vec<TrustAnchor<'static>> {
    vec![
        webpki::anchor_from_trusted_cert(&ca.der)
            .unwrap()
            .to_owned(),
    ]
}

pub(crate) struct Pki {
    pub(crate) root: Ca,
    pub(crate) inter: Ca,
    pub(crate) leaf: Leaf,
}

impl Pki {
    pub(crate) fn new() -> Self {
        let root = root("test root");
        let inter = intermediate(&root, "test intermediate", (2035, 1, 1));
        let leaf = leaf_with(&inter, &leaf_params(NAME, (2027, 1, 1)));
        Self { root, inter, leaf }
    }

    pub(crate) fn chain(&self) -> Vec<CertificateDer<'static>> {
        vec![self.leaf.der.clone(), self.inter.der.clone()]
    }

    pub(crate) fn anchors(&self) -> Vec<TrustAnchor<'static>> {
        anchors_of(&self.root)
    }

    pub(crate) fn check(&self, remaining: Duration) -> Result<TailscaleCert, CheckError> {
        check(
            self.chain(),
            self.leaf.key_der(),
            NAME,
            now(),
            remaining,
            &self.anchors(),
        )
    }
}
