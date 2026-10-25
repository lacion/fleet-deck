//! The terminal binding and the retry authorization file (design
//! `board-gaps.md` 4.2 and 4.3).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::binding::{self, Authorization, Binding};
use super::effects::Effects;
use super::fake::{DECK_FP, Fake, OTHER_FP, STATION_FP, STATION_FP_2};
use super::sha256_hex;
use super::state::Phase;

const ID: &str = "0123456789abcdef0123456789abcdef";
const DIGEST: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";

fn base() -> String {
    binding::digest("pending", Phase::Service, "cfg", DECK_FP, Some(STATION_FP))
}

#[test]
fn the_digest_is_the_sha256_of_the_six_lines() {
    let text = format!("v1\npending\nservice\ncfg\n{DECK_FP}\n{STATION_FP}\n");
    assert_eq!(base(), sha256_hex(text.as_bytes()));
    let none = binding::digest("denied", Phase::Link, "cfg", DECK_FP, None);
    let text = format!("v1\ndenied\nlink\ncfg\n{DECK_FP}\n-\n");
    assert_eq!(none, sha256_hex(text.as_bytes()));
}

#[test]
fn the_digest_changes_with_each_input() {
    let all = [
        base(),
        binding::digest("denied", Phase::Service, "cfg", DECK_FP, Some(STATION_FP)),
        binding::digest("pending", Phase::Paired, "cfg", DECK_FP, Some(STATION_FP)),
        binding::digest("pending", Phase::Service, "cfg2", DECK_FP, Some(STATION_FP)),
        binding::digest("pending", Phase::Service, "cfg", OTHER_FP, Some(STATION_FP)),
        binding::digest(
            "pending",
            Phase::Service,
            "cfg",
            DECK_FP,
            Some(STATION_FP_2),
        ),
        binding::digest("pending", Phase::Service, "cfg", DECK_FP, None),
    ];
    let unique: std::collections::BTreeSet<_> = all.iter().collect();
    assert_eq!(unique.len(), all.len());
    for digest in &all {
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        );
    }
}

fn prepared() -> Fake {
    let fake = Fake::new();
    fake.ensure_station_dir().unwrap();
    fake
}

fn ensure(fake: &Fake, digest: &str) -> Binding {
    binding::ensure(&fake.trust_root(), &fake.station_dir(), digest).unwrap()
}

#[test]
fn a_new_terminal_id_exists_only_when_the_digest_changes() {
    let fake = prepared();
    let first = ensure(&fake, DIGEST);
    assert_eq!(first.state_digest, DIGEST);
    assert_eq!(first.terminal_id.len(), 32);
    assert!(
        first
            .terminal_id
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    );
    // The same digest keeps the id.
    assert_eq!(ensure(&fake, DIGEST), first);
    assert_eq!(
        binding::load(&binding::binding_path(&fake.station_dir())),
        Some(first.clone())
    );
    // Another digest gets another id.
    let other_digest = "12".repeat(32);
    let second = ensure(&fake, &other_digest);
    assert_eq!(second.state_digest, other_digest);
    assert_ne!(second.terminal_id, first.terminal_id);
    // Going back to the first digest is a change again: a new id.
    let third = ensure(&fake, DIGEST);
    assert_ne!(third.terminal_id, first.terminal_id);
    assert_ne!(third.terminal_id, second.terminal_id);
}

#[test]
fn the_binding_file_is_private() {
    let fake = prepared();
    ensure(&fake, DIGEST);
    let path = binding::binding_path(&fake.station_dir());
    assert_eq!(std::fs::metadata(path).unwrap().mode() & 0o777, 0o600);
}

#[test]
fn an_unusable_binding_file_is_no_binding() {
    let fake = prepared();
    let path = binding::binding_path(&fake.station_dir());
    assert_eq!(binding::load(&path), None);
    let good = format!(r#"{{"terminal_id":"{ID}","state_digest":"{DIGEST}"}}"#);
    for (bytes, mode) in [
        (good.clone(), 0o644),
        ("{}".to_owned(), 0o600),
        ("not json".to_owned(), 0o600),
        (good.replace(ID, "ABCDEF0123456789abcdef0123456789"), 0o600),
        (good.replace(DIGEST, "abcd"), 0o600),
        (good.replace('}', r#","extra":1}"#), 0o600),
        (format!("{good}{}", " ".repeat(1100)), 0o600),
    ] {
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(binding::load(&path), None);
    }
    std::fs::write(&path, &good).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(binding::load(&path).is_some());
}

#[test]
fn an_invalid_stored_binding_is_replaced_with_a_new_one() {
    let fake = prepared();
    let path = binding::binding_path(&fake.station_dir());
    std::fs::write(&path, "junk").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let made = ensure(&fake, DIGEST);
    assert_eq!(binding::load(&path), Some(made));
}

fn auth_json(retry: &str, terminal: &str, digest: &str) -> Vec<u8> {
    format!(r#"{{"retry_id":"{retry}","terminal_id":"{terminal}","state_digest":"{digest}"}}"#)
        .into_bytes()
}

#[test]
fn an_authorization_has_exactly_three_fields_of_the_right_form() {
    let good = auth_json(ID, ID, DIGEST);
    assert_eq!(
        Authorization::parse(&good),
        Some(Authorization {
            retry_id: ID.to_owned(),
            terminal_id: ID.to_owned(),
            state_digest: DIGEST.to_owned()
        })
    );
    let upper = ID.to_uppercase();
    for bad in [
        auth_json("abcd", ID, DIGEST),
        auth_json(&upper, ID, DIGEST),
        auth_json(ID, &format!("{ID}0"), DIGEST),
        auth_json(ID, ID, &DIGEST[..63]),
        br#"{"retry_id":"x"}"#.to_vec(),
        b"[]".to_vec(),
        b"not json".to_vec(),
        String::from_utf8(auth_json(ID, ID, DIGEST))
            .unwrap()
            .replace('}', r#","extra":"x"}"#)
            .into_bytes(),
    ] {
        assert_eq!(Authorization::parse(&bad), None);
    }
}

#[test]
fn an_authorization_is_written_privately_and_read_back() {
    let fake = prepared();
    let path = binding::authorization_path(&fake.station_dir());
    let auth = Authorization {
        retry_id: ID.to_owned(),
        terminal_id: ID.to_owned(),
        state_digest: DIGEST.to_owned(),
    };
    auth.write(&fake.trust_root(), &path).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    let bytes = fake.read_authorization(&path).unwrap();
    assert_eq!(Authorization::parse(&bytes), Some(auth));
}
