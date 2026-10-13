//! `fleetdeck open --elevate` (DL-172, design 1.5): the owner-only local
//! socket makes a one-time elevation code that the board accepts once.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::deck::Deck;
use serde_json::json;

fn cookie_of(head: &str) -> String {
    let line = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .unwrap();
    line.split_once(':')
        .unwrap()
        .1
        .trim()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn open_elevate_prints_one_line_and_the_code_works_once() {
    let mut deck = Deck::start();
    let out = deck.cli(&["open", "--elevate"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let rest = text
        .strip_prefix("Elevation code: ")
        .and_then(|r| r.strip_suffix(" (one use, valid for 5 minutes). Type it in the board.\n"))
        .unwrap_or_else(|| panic!("unexpected output: {text:?}"));
    let b = rest.as_bytes();
    assert_eq!(b.len(), 11, "{rest}");
    assert_eq!(b[5], b'-');
    assert!(
        rest.chars()
            .filter(|c| *c != '-')
            .all(|c| "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(c)),
        "{rest}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");

    // The board accepts it once, for a board login.
    let (board, token) = deck.board();
    let r = deck.https(
        board,
        "POST",
        "/api/auth/link",
        &[],
        Some(json!({ "token": token })),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let cookie = cookie_of(&r.head);
    let csrf = r.body["csrf"].as_str().unwrap().to_owned();
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let r = deck.https(
        board,
        "POST",
        "/api/auth/elevate",
        &h,
        Some(json!({ "code": rest })),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body["elevation"].as_str().unwrap().len() >= 43);
    let again = deck.https(
        board,
        "POST",
        "/api/auth/elevate",
        &h,
        Some(json!({ "code": rest })),
    );
    assert_eq!(again.status, 401, "{}", again.body);
    deck.stop();
}
