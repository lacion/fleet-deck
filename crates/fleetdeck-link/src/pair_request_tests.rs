use std::collections::BTreeSet;

use fleetdeck_core::identity::Identity;
use fleetdeck_core::secret::Secret;
use fleetdeck_proto::link::Scope;

use super::*;

fn seeded(byte: u8) -> Identity {
    Identity::from_seed(Secret::new([byte; 32])).unwrap()
}

fn set(scopes: &[Scope]) -> BTreeSet<Scope> {
    scopes.iter().copied().collect()
}

/// The fixed inputs of the ADR-004 §10 test vectors.
fn vector_request() -> CanonicalRequest {
    let mut id = [0u8; 16];
    for (i, b) in id.iter_mut().enumerate() {
        *b = u8::try_from(i).unwrap();
    }
    let mut nonce = [0u8; 32];
    for (i, b) in nonce.iter_mut().enumerate() {
        *b = 0x20 + u8::try_from(i).unwrap();
    }
    CanonicalRequest::new(
        id,
        "laptop".into(),
        nonce,
        set(&[Scope::Read, Scope::Drive]),
    )
    .unwrap()
}

#[test]
fn the_vectors_of_adr_004_hold() {
    let req = vector_request();
    let station = seeded(1);
    let deck = seeded(2);
    let granted = set(&[Scope::Admin]);
    let transcript = b"fleet-deck link transcript v1\0 test vector";
    let t_hash = transcript_hash(transcript);
    let digest = req.digest();
    let pending = pending_digest(
        &digest,
        &station.fingerprint(),
        "100.64.0.7".parse().unwrap(),
        1_800_000_000_123,
    );
    let approval = Approval {
        transcript_hash: t_hash,
        station: station.fingerprint(),
        deck: deck.fingerprint(),
        request: &req,
        granted_scopes: &granted,
    };
    let signature = approval.sign(&deck);
    assert_eq!(hex(&t_hash), VECTOR_TRANSCRIPT_HASH);
    assert_eq!(hex(&digest), VECTOR_REQUEST_DIGEST);
    assert_eq!(hex(&pending), VECTOR_PENDING_DIGEST);
    assert_eq!(hex(&approval.message()), VECTOR_MESSAGE);
    assert_eq!(signature, VECTOR_SIGNATURE);
    assert!(approval.verify(&deck.public_key(), &signature));
    // The station key does not verify the deck's signature.
    assert!(!approval.verify(&station.public_key(), &signature));
}

#[test]
fn the_vector_keys_are_the_documented_ones() {
    assert_eq!(seeded(1).fingerprint().to_string(), VECTOR_STATION_FP);
    assert_eq!(seeded(2).fingerprint().to_string(), VECTOR_DECK_FP);
}

#[test]
fn any_changed_field_changes_the_digest_and_the_signature() {
    let base = vector_request();
    let deck = seeded(2);
    let station = seeded(1);
    let granted = set(&[Scope::Admin]);
    let sign = |req: &CanonicalRequest, hash: [u8; 32], granted: &BTreeSet<Scope>| {
        Approval {
            transcript_hash: hash,
            station: station.fingerprint(),
            deck: deck.fingerprint(),
            request: req,
            granted_scopes: granted,
        }
        .sign(&deck)
    };
    let hash = transcript_hash(b"one");
    let reference = sign(&base, hash, &granted);
    let mut other = base.clone();
    other.name = "laptop2".into();
    assert_ne!(other.digest(), base.digest());
    assert_ne!(sign(&other, hash, &granted), reference);
    let mut other = base.clone();
    other.nonce[0] ^= 1;
    assert_ne!(other.digest(), base.digest());
    let mut other = base.clone();
    other.request_id[0] ^= 1;
    assert_ne!(other.digest(), base.digest());
    let mut other = base.clone();
    other.offered_scopes.insert(Scope::Admin);
    assert_ne!(other.digest(), base.digest());
    // Another TLS session or another grant gives another signature.
    assert_ne!(sign(&base, transcript_hash(b"two"), &granted), reference);
    assert_ne!(
        sign(&base, hash, &set(&[Scope::Admin, Scope::Read])),
        reference
    );
}

#[test]
fn scope_lists_are_sorted_by_name_and_joined_with_commas() {
    assert_eq!(
        scopes_text(&set(&[Scope::Read, Scope::Admin, Scope::Drive])),
        "admin,drive,read"
    );
    assert_eq!(scopes_text(&BTreeSet::new()), "");
}

#[test]
fn a_name_has_one_to_64_characters_and_no_control_or_bidi_character() {
    assert!(validate_name("laptop").is_ok());
    assert!(validate_name(&"é".repeat(64)).is_ok());
    assert!(validate_name("").is_err());
    assert!(validate_name(&"a".repeat(65)).is_err());
    for bad in [
        "a\nb",
        "a\0b",
        "a\u{202E}b",
        "a\u{2066}b",
        "a\u{200F}b",
        "\u{7f}",
    ] {
        assert!(validate_name(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_station_never_offers_coordinate() {
    assert!(validate_scopes(&set(&[Scope::Read, Scope::Admin])).is_ok());
    assert!(validate_scopes(&set(&[Scope::Coordinate])).is_err());
    assert!(validate_scopes(&BTreeSet::new()).is_ok());
}

#[test]
fn a_frame_request_needs_lowercase_hex_of_the_right_length() {
    let ok = "00".repeat(16);
    let nonce = "ab".repeat(32);
    assert!(CanonicalRequest::from_frame(&ok, "n", &nonce, &[Scope::Read]).is_ok());
    assert_eq!(
        CanonicalRequest::from_frame("00", "n", &nonce, &[]).unwrap_err(),
        RequestError::BadId
    );
    assert_eq!(
        CanonicalRequest::from_frame(&"AB".repeat(16), "n", &nonce, &[]).unwrap_err(),
        RequestError::BadId
    );
    assert_eq!(
        CanonicalRequest::from_frame(&ok, "n", "zz", &[]).unwrap_err(),
        RequestError::BadNonce
    );
    assert_eq!(
        CanonicalRequest::from_frame(&ok, "", &nonce, &[]).unwrap_err(),
        RequestError::BadName
    );
    assert_eq!(
        CanonicalRequest::from_frame(&ok, "n", &nonce, &[Scope::Coordinate]).unwrap_err(),
        RequestError::BadScope
    );
}

const VECTOR_STATION_FP: &str = "34750f98bd59fcfc946da45aaabe933be154a4b5094e1c4abf42866505f3c97e";
const VECTOR_DECK_FP: &str = "6a3803d5f059902a1c6dafbc9ba4729212f7caac08634cc3ae76b27529f03827";
const VECTOR_TRANSCRIPT_HASH: &str =
    "e09327588059314b411bb09d84d6b410f5a61f0022bbfcf50d1541f286c6e547";
const VECTOR_REQUEST_DIGEST: &str =
    "e79c65b4306975d100484529afbf9911cd036698d4fab06c91fcee53b50e595f";
const VECTOR_PENDING_DIGEST: &str =
    "5e177bae222802c44477051778076b22000a351c4087e6e692a5c423634ab91a";
const VECTOR_MESSAGE: &str = "666c6565746465636b20706169722d617070726f7665642076310000000000000020e09327588059314b411bb09d84d6b410f5a61f0022bbfcf50d1541f286c6e547000000000000002034750f98bd59fcfc946da45aaabe933be154a4b5094e1c4abf42866505f3c97e00000000000000206a3803d5f059902a1c6dafbc9ba4729212f7caac08634cc3ae76b27529f038270000000000000010000102030405060708090a0b0c0d0e0f0000000000000020202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f0000000000000020e79c65b4306975d100484529afbf9911cd036698d4fab06c91fcee53b50e595f000000000000000a64726976652c72656164000000000000000561646d696e";
const VECTOR_SIGNATURE: &str = "a57391c1a69ce4e16613b87450f746786a8b05addfa33a74976dff0fdda2dac6c5f2436c6eba354b1ff700ca981af503cfc977840238b66807c01ed4e0ad7f06";
