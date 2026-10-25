use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_core::secret::Secret;
use fleetdeck_proto::link::Scope;

use super::*;

fn addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 7443))
}

fn fingerprint() -> Fingerprint {
    Identity::generate()
        .expect("identity")
        .public_key()
        .fingerprint()
}

fn open(invites: &Invitations) -> Invitation {
    invites
        .create(addr(), fingerprint(), DEFAULT_TTL, [Scope::Read])
        .expect("invitation")
}

const T: &[u8] = b"transcript";

#[test]
fn an_invitation_round_trips_and_debug_hides_the_secret() {
    let invites = Invitations::new();
    let inv = open(&invites);
    let text = inv.reveal();
    let secret_hex = hex(inv.secret.expose());
    assert!(text.expose().starts_with("fdinv1:wss://127.0.0.1:7443#"));
    assert!(text.expose().contains(&secret_hex));
    let back = Invitation::parse(&text).expect("parse");
    assert_eq!(back.addr, inv.addr);
    assert_eq!(back.fingerprint, inv.fingerprint);
    assert_eq!(back.id, inv.id);
    assert_eq!(back.expiry, inv.expiry);
    assert_eq!(back.secret.expose(), inv.secret.expose());
    assert!(!back.is_expired());
    let debug = format!("{back:?} {text:?}");
    assert!(!debug.contains(&secret_hex), "{debug}");
    assert!(debug.contains(inv.id.as_str()));
}

#[test]
fn bad_invitations_are_refused_without_echoing_the_input() {
    let inv = open(&Invitations::new());
    let good = inv.reveal().expose().clone();
    let secret_hex = hex(inv.secret.expose());
    let public = good.replace("127.0.0.1", "8.8.8.8");
    let cases = [
        good.replace("fdinv1:", "fdinv2:"),
        good.replace("wss://", "https://"),
        good.replacen('.', "", 1),
        good.replace(&secret_hex, &secret_hex[..30]),
        format!("{good}.extra"),
        good.replace(inv.id.as_str(), "XYZ"),
    ];
    for case in cases {
        let err = case.parse::<Invitation>().expect_err(&case);
        assert_eq!(err.reason(), reason::BAD_INVITATION, "{case}");
        assert!(!err.to_string().contains(&secret_hex));
    }
    let err = public.parse::<Invitation>().expect_err("public address");
    assert_eq!(err.reason(), reason::ADDRESS_REFUSED);
}

#[test]
fn a_valid_proof_redeems_the_invitation_once_and_both_proofs_check() {
    let invites = Invitations::new();
    let inv = open(&invites);
    assert!(invites.has_open());
    let redeemed = invites
        .redeem(&inv.id, T, &inv.joiner_proof(T))
        .expect("redeem");
    assert_eq!(redeemed.scopes, [Scope::Read].into_iter().collect());
    inv.check_inviter_proof(T, &redeemed.answer)
        .expect("inviter proof");
    assert_eq!(
        inv.check_inviter_proof(b"other", &redeemed.answer),
        Err(PairError::InviterProof)
    );
    // A joiner proof is not an inviter proof (the tags differ).
    assert!(inv.check_inviter_proof(T, &inv.joiner_proof(T)).is_err());
    assert!(!invites.has_open());
    assert_eq!(
        invites.redeem(&inv.id, T, &inv.joiner_proof(T)).err(),
        Some(PairError::InvalidProof)
    );
}

#[test]
fn five_wrong_proofs_cancel_only_that_invitation() {
    let invites = Invitations::new();
    let a = open(&invites);
    let b = open(&invites);
    let wrong = b.joiner_proof(T);
    for _ in 0..4 {
        assert_eq!(
            invites.redeem(&a.id, T, &wrong).err(),
            Some(PairError::InvalidProof)
        );
    }
    assert_eq!(
        invites.redeem(&a.id, T, &wrong).err(),
        Some(PairError::TooManyFailures)
    );
    assert_eq!(
        invites.redeem(&a.id, T, &a.joiner_proof(T)).err(),
        Some(PairError::InvalidProof),
        "the cancelled invitation stays cancelled"
    );
    assert_eq!(invites.open_count(), 1);
    invites
        .redeem(&b.id, T, &b.joiner_proof(T))
        .expect("invitation B still works");
}

#[test]
fn proofs_for_an_unknown_id_count_against_no_invitation() {
    let invites = Invitations::new();
    let a = open(&invites);
    let other = InvitationId::parse("0123456789abcdef").expect("id");
    for _ in 0..10 {
        assert!(invites.redeem(&other, T, &a.joiner_proof(T)).is_err());
    }
    invites
        .redeem(&a.id, T, &a.joiner_proof(T))
        .expect("A works");
}

#[tokio::test(start_paused = true)]
async fn an_invitation_expires_after_its_ttl() {
    let invites = Invitations::new();
    let inv = invites
        .create(addr(), fingerprint(), Duration::from_secs(60), [])
        .expect("invitation");
    tokio::time::advance(Duration::from_secs(61)).await;
    assert!(!invites.has_open());
    assert_eq!(
        invites.redeem(&inv.id, T, &inv.joiner_proof(T)).err(),
        Some(PairError::Expired)
    );
    assert!(invites.cancel(&open(&invites).id));
}

#[tokio::test(start_paused = true)]
async fn ten_attempts_per_ip_and_minute_and_four_handshakes_at_a_time() {
    let limiter = PairLimiter::default();
    let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
    let other = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 21));
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(limiter.admit(ip).expect("slot"));
    }
    assert_eq!(limiter.admit(other).err(), Some(PairError::Busy));
    held.clear();
    for _ in 0..5 {
        drop(limiter.admit(ip).expect("attempt"));
    }
    // 4 + 5 attempts of `ip` so far; the 10th is allowed, the 11th is not.
    drop(limiter.admit(ip).expect("10th attempt"));
    assert_eq!(limiter.admit(ip).err(), Some(PairError::RateLimited(ip)));
    drop(limiter.admit(other).expect("another IP is not limited"));
    tokio::time::advance(RATE_WINDOW).await;
    drop(limiter.admit(ip).expect("the lockout ends after a minute"));
}

#[test]
fn every_pair_error_has_a_stable_reason() {
    let errors = [
        PairError::Format(String::new()),
        PairError::Expired,
        PairError::InvalidProof,
        PairError::TooManyFailures,
        PairError::RateLimited(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        PairError::Busy,
        PairError::InviterProof,
        PairError::Store(String::new()),
        PairError::Random,
    ];
    let reasons: Vec<_> = errors.iter().map(PairError::reason).collect();
    assert_eq!(
        reasons,
        [
            "bad_invitation",
            "invitation_expired",
            "invalid_invitation",
            "too_many_attempts",
            "too_many_attempts",
            "too_many_attempts",
            "inviter_proof_failed",
            "pairing_failed",
            "pairing_failed",
        ]
    );
    let _ = Secret::new(0u8);
}
