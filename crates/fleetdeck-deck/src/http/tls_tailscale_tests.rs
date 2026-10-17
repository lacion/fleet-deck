#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::MetadataExt;

use rcgen::{BasicConstraints, IsCa, KeyPair, date_time_ymd};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use super::*;
use crate::http::tls::{BoardTls, pinned_client};
use crate::http::tls_test_pki::*;

#[test]
fn a_valid_test_chain_is_admitted() {
    let pki = Pki::new();
    let cert = pki.check(ADMISSION_REMAINING).unwrap();
    assert_eq!(cert.name, NAME);
    assert_eq!(cert.not_before_ms, day(2026, 9, 1) * 1000);
    assert_eq!(cert.not_after_ms, day(2027, 1, 1) * 1000);
    assert_eq!(cert.sha256, fingerprint(&pki.leaf.der));
    assert!(format!("{cert:?}").contains(NAME));
    assert!(cert.certified_key().is_ok());
}

#[test]
fn a_self_signed_leaf_with_the_right_name_and_key_is_refused() {
    let params = leaf_params(NAME, (2027, 1, 1));
    let key = KeyPair::generate().unwrap();
    let der = params.self_signed(&key).unwrap().der().clone();
    let pki = Pki::new();
    let got = check(
        vec![der],
        PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        NAME,
        now(),
        Duration::ZERO,
        &pki.anchors(),
    );
    assert_eq!(got.unwrap_err(), CheckError::BadCertificate);
}

#[test]
fn a_missing_intermediate_a_wrong_name_and_a_ca_leaf_are_refused() {
    let pki = Pki::new();
    let run = |chain, name: &str| {
        check(
            chain,
            pki.leaf.key_der(),
            name,
            now(),
            Duration::ZERO,
            &pki.anchors(),
        )
    };
    assert!(run(pki.chain(), NAME).is_ok());
    let lone = vec![pki.leaf.der.clone()];
    assert_eq!(run(lone, NAME).unwrap_err(), CheckError::BadCertificate);
    let other = "other.tail1.ts.net";
    assert_eq!(
        run(pki.chain(), other).unwrap_err(),
        CheckError::BadCertificate
    );
    let mut params = leaf_params(NAME, (2027, 1, 1));
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_leaf = leaf_with(&pki.inter, &params);
    let got = check(
        vec![ca_leaf.der.clone(), pki.inter.der.clone()],
        ca_leaf.key_der(),
        NAME,
        now(),
        Duration::ZERO,
        &pki.anchors(),
    );
    assert_eq!(got.unwrap_err(), CheckError::BadCertificate);
}

#[test]
fn a_leaf_with_three_days_left_fails_admission_and_passes_use() {
    let pki = Pki::new();
    let leaf = leaf_with(&pki.inter, &leaf_params(NAME, (2026, 10, 18)));
    let run = |remaining| {
        check(
            vec![leaf.der.clone(), pki.inter.der.clone()],
            leaf.key_der(),
            NAME,
            now(),
            remaining,
            &pki.anchors(),
        )
    };
    assert_eq!(
        run(ADMISSION_REMAINING).unwrap_err(),
        CheckError::BadCertificate
    );
    assert!(run(Duration::ZERO).is_ok());
}

#[test]
fn an_intermediate_that_ends_early_fails_admission() {
    let root = root("test root");
    let inter = intermediate(&root, "short intermediate", (2026, 10, 18));
    let leaf = leaf_with(&inter, &leaf_params(NAME, (2027, 1, 1)));
    let run = |remaining| {
        check(
            vec![leaf.der.clone(), inter.der.clone()],
            leaf.key_der(),
            NAME,
            now(),
            remaining,
            &anchors_of(&root),
        )
    };
    assert!(run(Duration::ZERO).is_ok());
    assert!(run(ADMISSION_REMAINING).is_err());
}

#[test]
fn an_expired_leaf_and_a_not_yet_valid_leaf_are_refused() {
    let pki = Pki::new();
    let expired = leaf_with(&pki.inter, &leaf_params(NAME, (2026, 10, 1)));
    let mut later = leaf_params(NAME, (2027, 6, 1));
    later.not_before = date_time_ymd(2026, 12, 1);
    let early = leaf_with(&pki.inter, &later);
    for leaf in [expired, early] {
        let got = check(
            vec![leaf.der.clone(), pki.inter.der.clone()],
            leaf.key_der(),
            NAME,
            now(),
            Duration::ZERO,
            &pki.anchors(),
        );
        assert_eq!(got.unwrap_err(), CheckError::BadCertificate);
    }
}

#[test]
fn the_key_of_another_certificate_is_refused() {
    let pki = Pki::new();
    let other = leaf_with(&pki.inter, &leaf_params(NAME, (2027, 1, 1)));
    let got = check(
        pki.chain(),
        other.key_der(),
        NAME,
        now(),
        Duration::ZERO,
        &pki.anchors(),
    );
    assert_eq!(got.unwrap_err(), CheckError::BadCertificate);
}

#[test]
fn a_root_in_the_chain_is_not_an_anchor() {
    let pki = Pki::new();
    let rogue = root("rogue root");
    let leaf = leaf_with(&rogue, &leaf_params(NAME, (2027, 1, 1)));
    let got = check(
        vec![leaf.der.clone(), rogue.der.clone()],
        leaf.key_der(),
        NAME,
        now(),
        Duration::ZERO,
        &pki.anchors(),
    );
    assert_eq!(got.unwrap_err(), CheckError::BadCertificate);
    // The trusted root in the chain is no harm and no need.
    let mut chain = pki.chain();
    chain.push(pki.root.der.clone());
    let got = check(
        chain,
        pki.leaf.key_der(),
        NAME,
        now(),
        Duration::ZERO,
        &pki.anchors(),
    );
    assert!(got.is_ok());
}

fn pem_of(label: &str, der: &[u8]) -> String {
    let mut out = String::new();
    push_block(&mut out, label, der);
    out
}

#[test]
fn the_output_needs_a_chain_and_exactly_one_key() {
    let pki = Pki::new();
    let cert = pem_of("CERTIFICATE", pki.leaf.der.as_ref());
    let inter = pem_of("CERTIFICATE", pki.inter.der.as_ref());
    let key = pem_of("PRIVATE KEY", &pki.leaf.key.serialize_der());
    let ok = format!("{cert}{inter}{key}");
    let (chain, parsed) = parse_pem(ok.as_bytes()).unwrap();
    assert_eq!(chain.len(), 2);
    assert!(matches!(parsed, PrivateKeyDer::Pkcs8(_)));
    // The key may come first.
    assert!(parse_pem(format!("{key}{cert}").as_bytes()).is_ok());
    let public = pem_of("PUBLIC KEY", b"x");
    let bad = [
        format!("{cert}{key}{key}"),
        format!("{cert}{inter}"),
        key.clone(),
        format!("{cert}{key}{public}"),
        "junk".to_owned(),
        String::new(),
        "-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n".to_owned(),
    ];
    for text in bad {
        assert_eq!(
            parse_pem(text.as_bytes()).unwrap_err(),
            CheckError::BadOutput,
            "{text}"
        );
    }
}

#[test]
fn the_embedded_roots_are_the_two_isrg_roots() {
    let expect = [
        (
            ISRG_ROOT_X1,
            "96:BC:EC:06:26:49:76:F3:74:60:77:9A:CF:28:C5:A7:CF:E8:A3:C0:AA:E1:1A:8F:FC:EE:05:C0:BD:DF:08:C6",
        ),
        (
            ISRG_ROOT_X2,
            "69:72:9B:8E:15:A8:6E:FC:17:7A:57:AF:B7:17:1D:FC:64:AD:D2:8C:2F:CA:8C:F1:50:7E:34:45:3C:CB:14:70",
        ),
    ];
    for (pem, want) in expect {
        let (chain, key) = split_pem(pem.as_bytes()).unwrap();
        assert!(key.is_none());
        assert_eq!(chain.len(), 1);
        assert_eq!(fingerprint(&chain[0]), want.to_lowercase());
    }
    assert_eq!(isrg_anchors().len(), 2);
}

// ----------------------------------------------------------------- storage

fn stored(pki: &Pki) -> TailscaleCert {
    pki.check(ADMISSION_REMAINING).unwrap()
}

fn mode(dir: &Path) -> u32 {
    fs::metadata(dir.join(TAILSCALE_PEM)).unwrap().mode() & 0o777
}

#[test]
fn a_write_makes_a_private_file_that_loads_again() {
    let pki = Pki::new();
    let dir = tempfile::tempdir().unwrap();
    write_tailscale(dir.path(), &stored(&pki)).unwrap();
    assert_eq!(mode(dir.path()), 0o600);
    let text = fs::read_to_string(dir.path().join(TAILSCALE_PEM)).unwrap();
    let cert_at = text.find("BEGIN CERTIFICATE").unwrap();
    assert!(cert_at < text.find("BEGIN PRIVATE KEY").unwrap());
    let back = load_tailscale(dir.path(), NAME, now(), &pki.anchors()).unwrap();
    assert_eq!(back.sha256, fingerprint(&pki.leaf.der));
    assert_eq!(back.not_after_ms, day(2027, 1, 1) * 1000);
    assert!(!dir.path().join("board-tailscale.pem.tmp").exists());
    remove_tailscale(dir.path()).unwrap();
    remove_tailscale(dir.path()).unwrap();
    assert!(!dir.path().join(TAILSCALE_PEM).exists());
}

#[test]
fn remove_staging_deletes_the_temporary_file_and_a_missing_one_is_no_error() {
    let dir = tempfile::tempdir().unwrap();
    let staging = dir.path().join("board-tailscale.pem.tmp");
    remove_staging(dir.path()).unwrap();
    fs::write(&staging, "orphan").unwrap();
    remove_staging(dir.path()).unwrap();
    assert!(!staging.exists());
    // The removal of the certificate takes the temporary file too.
    let pki = Pki::new();
    write_tailscale(dir.path(), &stored(&pki)).unwrap();
    fs::write(&staging, "orphan").unwrap();
    remove_tailscale(dir.path()).unwrap();
    assert!(!staging.exists() && !dir.path().join(TAILSCALE_PEM).exists());
}

#[test]
fn a_failed_rename_keeps_the_old_file() {
    let pki = Pki::new();
    let dir = tempfile::tempdir().unwrap();
    write_tailscale(dir.path(), &stored(&pki)).unwrap();
    let before = fs::read(dir.path().join(TAILSCALE_PEM)).unwrap();
    let newer = leaf_with(&pki.inter, &leaf_params(NAME, (2027, 2, 1)));
    let second = check(
        vec![newer.der.clone(), pki.inter.der.clone()],
        newer.key_der(),
        NAME,
        now(),
        Duration::ZERO,
        &pki.anchors(),
    )
    .unwrap();
    let failed = write_tailscale_with(dir.path(), &second, |_, _| {
        Err(io::Error::other("rename failed"))
    });
    assert!(failed.is_err());
    assert_eq!(fs::read(dir.path().join(TAILSCALE_PEM)).unwrap(), before);
    assert!(!dir.path().join("board-tailscale.pem.tmp").exists());
    assert_eq!(mode(dir.path()), 0o600);
}

#[test]
fn an_expired_a_foreign_or_a_loose_file_is_no_certificate() {
    let pki = Pki::new();
    let dir = tempfile::tempdir().unwrap();
    write_tailscale(dir.path(), &stored(&pki)).unwrap();
    let anchors = pki.anchors();
    assert!(load_tailscale(dir.path(), NAME, now(), &anchors).is_some());
    let upper = NAME.to_ascii_uppercase();
    assert!(load_tailscale(dir.path(), &upper, now(), &anchors).is_some());
    let past = unix(day(2027, 1, 2));
    assert!(load_tailscale(dir.path(), NAME, past, &anchors).is_none());
    let other = "other.tail1.ts.net";
    assert!(load_tailscale(dir.path(), other, now(), &anchors).is_none());
    assert!(load_tailscale(dir.path(), NAME, now(), &[]).is_none());
    let path = dir.path().join(TAILSCALE_PEM);
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    assert!(load_tailscale(dir.path(), NAME, now(), &anchors).is_none());
    fs::write(&path, "junk").unwrap();
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    assert!(load_tailscale(dir.path(), NAME, now(), &anchors).is_none());
    let missing = tempfile::tempdir().unwrap();
    assert!(load_tailscale(missing.path(), NAME, now(), &anchors).is_none());
}

// ---------------------------------------------------------------- resolver

const FAR: i64 = i64::MAX / 2;

/// A pair that is served from the start of time up to `end`.
fn until(key: &Arc<CertifiedKey>, end: i64) -> Served {
    Served {
        key: Arc::clone(key),
        not_before_ms: 0,
        not_after_ms: end,
    }
}

fn board(owner: bool) -> BoardTls {
    let (c, k) = BoardTls::generate(&["127.0.0.1".to_owned()]).unwrap();
    BoardTls::from_pem(&c, &k, !owner)
        .unwrap()
        .with_name(Some("Deck.Tail1.ts.net"))
        .unwrap()
}

fn same(a: &Arc<CertifiedKey>, b: &Arc<CertifiedKey>) -> bool {
    a.cert[0] == b.cert[0]
}

#[test]
fn the_resolver_picks_by_sni_and_time() {
    let pki = Pki::new();
    let tls = board(false);
    let ts = stored(&pki).certified_key().unwrap();
    let fallback = tls.resolver.fallback();
    assert!(same(&tls.resolver.pick(Some(NAME), 1), &fallback));
    tls.resolver.set_tailscale(Some(until(&ts, FAR)));
    let same_as = |sni: Option<&str>, now, want: &Arc<CertifiedKey>| {
        assert!(same(&tls.resolver.pick(sni, now), want), "{sni:?}");
    };
    same_as(Some(NAME), 1, &ts);
    same_as(Some("DECK.TAIL1.TS.NET"), 1, &ts);
    same_as(Some("other.tail1.ts.net"), 1, &fallback);
    same_as(Some("127.0.0.1"), 1, &fallback);
    same_as(None, 1, &fallback);
    // At the end of the validity the certificate is not served.
    same_as(Some(NAME), FAR - 1, &ts);
    same_as(Some(NAME), FAR, &fallback);
    // A swap is seen by the next connection.
    tls.resolver.set_tailscale(None);
    same_as(Some(NAME), 1, &fallback);
    tls.resolver.set_tailscale(Some(until(&ts, 5)));
    same_as(Some(NAME), 6, &fallback);
}

#[test]
fn the_resolver_stops_serving_when_an_intermediate_ends_before_the_leaf() {
    let root = root("test root");
    let inter = intermediate(&root, "short intermediate", (2026, 12, 1));
    let leaf = leaf_with(&inter, &leaf_params(NAME, (2027, 1, 1)));
    let cert = check(
        vec![leaf.der.clone(), inter.der.clone()],
        leaf.key_der(),
        NAME,
        now(),
        Duration::ZERO,
        &anchors_of(&root),
    )
    .unwrap();
    let end = day(2026, 12, 1) * 1000;
    assert_eq!(cert.not_after_ms, end);
    let tls = board(false);
    tls.resolver.set_tailscale(Some(cert.served().unwrap()));
    let ts = cert.certified_key().unwrap();
    let fallback = tls.resolver.fallback();
    assert!(same(&tls.resolver.pick(Some(NAME), end - 1), &ts));
    assert!(same(&tls.resolver.pick(Some(NAME), end), &fallback));
    assert!(same(&tls.resolver.pick(Some(NAME), end + 1), &fallback));
}

#[test]
fn an_extra_certificate_off_the_path_does_not_change_the_validity() {
    let other = root("other root");
    let root = root("test root");
    let inter = intermediate(&root, "short intermediate", (2026, 12, 1));
    let leaf = leaf_with(&inter, &leaf_params(NAME, (2027, 1, 1)));
    // An old cross-sign of another root, expired: not on the verified path.
    let old = intermediate(&other, "old cross-sign", (2025, 1, 1));
    let cert = check(
        vec![leaf.der.clone(), inter.der.clone(), old.der.clone()],
        leaf.key_der(),
        NAME,
        now(),
        Duration::ZERO,
        &anchors_of(&root),
    )
    .unwrap();
    assert_eq!(cert.not_after_ms, day(2026, 12, 1) * 1000);
    assert_eq!(cert.not_before_ms, day(2026, 9, 1) * 1000);
}

#[test]
fn the_resolver_does_not_serve_a_pair_before_its_start() {
    let pki = Pki::new();
    let cert = stored(&pki);
    let start = day(2026, 9, 1) * 1000;
    assert_eq!(cert.not_before_ms, start);
    let tls = board(false);
    tls.resolver.set_tailscale(Some(cert.served().unwrap()));
    let ts = cert.certified_key().unwrap();
    let fallback = tls.resolver.fallback();
    assert!(same(&tls.resolver.pick(Some(NAME), start - 1), &fallback));
    assert!(same(&tls.resolver.pick(Some(NAME), start), &ts));
}

#[test]
fn the_owner_pair_wins_for_every_name() {
    let pki = Pki::new();
    let tls = board(true);
    assert!(tls.resolver.owner());
    let ts = stored(&pki).certified_key().unwrap();
    tls.resolver.set_tailscale(Some(until(&ts, FAR)));
    let fallback = tls.resolver.fallback();
    assert!(same(&tls.resolver.pick(Some(NAME), 1), &fallback));
    assert!(!tls.self_signed);
}

#[test]
fn a_board_without_a_name_or_a_certificate_serves_its_own_pair() {
    let (c, k) = BoardTls::generate(&["127.0.0.1".to_owned()]).unwrap();
    let tls = BoardTls::from_pem(&c, &k, true).unwrap();
    assert!(!tls.resolver.owner());
    let fallback = tls.resolver.fallback();
    assert!(same(&tls.resolver.pick(Some(NAME), 1), &fallback));
    assert_eq!(fingerprint(&fallback.cert[0]), tls.sha256);
    assert_eq!(tls.cert, fallback.cert[0]);
}

/// Connects with `sni` and the client that trusts only `pinned`; true when
/// the handshake works.
async fn handshake(tls: &BoardTls, sni: &str, pinned: &CertificateDer<'static>) -> bool {
    let (client_io, server_io) = tokio::io::duplex(16 * 1024);
    let acceptor = TlsAcceptor::from(Arc::clone(&tls.config));
    let server = tokio::spawn(async move {
        let mut stream = acceptor.accept(server_io).await?;
        stream.write_all(b"ok").await?;
        stream.shutdown().await
    });
    let connector = TlsConnector::from(pinned_client(pinned.clone()));
    let name = ServerName::try_from(sni.to_owned()).unwrap();
    let client = async {
        let mut stream = connector.connect(name, client_io).await?;
        let mut buf = [0_u8; 2];
        stream.read_exact(&mut buf).await?;
        Ok::<_, io::Error>(buf)
    };
    let got = client.await;
    drop(server);
    got.is_ok_and(|b| &b == b"ok")
}

#[tokio::test]
async fn a_real_handshake_gets_the_certificate_of_its_sni() {
    let pki = Pki::new();
    let tls = board(false);
    let cert = stored(&pki);
    let key = cert.certified_key().unwrap();
    tls.resolver.set_tailscale(Some(until(&key, FAR)));
    assert!(handshake(&tls, NAME, &pki.leaf.der).await);
    assert!(!handshake(&tls, NAME, &tls.cert).await);
    assert!(handshake(&tls, "localhost", &tls.cert).await);
    assert!(!handshake(&tls, "localhost", &pki.leaf.der).await);
    tls.resolver.set_tailscale(None);
    assert!(handshake(&tls, NAME, &tls.cert).await);
}
