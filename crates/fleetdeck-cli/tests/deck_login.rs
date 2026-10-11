//! Owner login of the deck process (M2 P3-8; PLAN Phase 3 exit "login
//! required also on loopback", A13): `fleetdeck open` prints a one-time
//! login URL; the board API on loopback refuses requests without a login;
//! `deck password`, `deck token` and `deck audit` work through deck.sock;
//! a public board address is refused.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;

use common::deck::Deck;
use common::fleetdeck;
use serde_json::{Value, json};

/// One HTTP/1.1 request to the deck's board; returns (status, headers, body).
fn http(
    deck: &Deck,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (u16, String, Value) {
    let mut s = TcpStream::connect(("127.0.0.1", deck.board_port)).unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut text = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: application/json\r\n",
        body.len()
    );
    for (k, v) in headers {
        text.push_str(&format!("{k}: {v}\r\n"));
    }
    text.push_str("\r\n");
    text.push_str(&body);
    s.write_all(text.as_bytes()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (
        status,
        head.to_owned(),
        serde_json::from_str(body).unwrap_or(Value::Null),
    )
}

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
fn fleetdeck_open_logs_in_once_and_the_loopback_board_refuses_anonymous_requests() {
    let mut deck = Deck::start();

    // Loopback is no exception: no login, no data.
    let (status, _, body) = http(&deck, "GET", "/api/sessions", &[], None);
    assert_eq!(
        (status, body["error"].as_str()),
        (401, Some("login_required"))
    );

    // `fleetdeck open` prints the URL; the token is in the fragment.
    let url = deck.ok(&["open"]);
    let url = url.trim();
    let prefix = format!("http://127.0.0.1:{}/#login=", deck.board_port);
    assert!(url.starts_with(&prefix), "{url}");
    let token = &url[prefix.len()..];
    let (status, head, body) = http(
        &deck,
        "POST",
        "/api/auth/link",
        &[],
        Some(json!({ "token": token })),
    );
    assert_eq!(status, 200, "{body}");
    let cookie = cookie_of(&head);
    let (status, _, _) = http(&deck, "GET", "/api/sessions", &[("Cookie", &cookie)], None);
    assert_eq!(status, 200);
    let (status, _, _) = http(
        &deck,
        "POST",
        "/api/auth/link",
        &[],
        Some(json!({ "token": token })),
    );
    assert_eq!(status, 401, "a login link works once");

    // The password from stdin; a short one is refused.
    let short = common::run_stdin(&deck.node, &["deck", "password", "--stdin"], "short");
    assert!(!short.status.success());
    assert!(String::from_utf8_lossy(&short.stderr).contains("12"));
    deck.ok_stdin(&["deck", "password", "--stdin"], "correct horse battery");
    let (status, _, _) = http(&deck, "GET", "/api/sessions", &[("Cookie", &cookie)], None);
    assert_eq!(status, 401, "a new password ends every login");
    let (status, _, _) = http(
        &deck,
        "POST",
        "/api/auth/login",
        &[],
        Some(json!({ "password": "correct horse battery" })),
    );
    assert_eq!(status, 200);

    // A read token cannot read the audit log; the admin scope can.
    let read = deck.ok(&["deck", "token", "create", "phone", "--scope", "read"]);
    let bearer = format!("Bearer {}", read.trim());
    let (status, _, _) = http(
        &deck,
        "GET",
        "/api/stations",
        &[("Authorization", &bearer)],
        None,
    );
    assert_eq!(status, 200);
    let (status, _, _) = http(
        &deck,
        "GET",
        "/api/audit",
        &[("Authorization", &bearer)],
        None,
    );
    assert_eq!(status, 403);
    let list = deck.ok(&["deck", "token", "list"]);
    assert!(list.contains("phone  read"), "{list}");
    assert!(!list.contains(read.trim()), "a token value is shown once");
    deck.ok(&["deck", "token", "revoke", "phone"]);
    let (status, _, _) = http(
        &deck,
        "GET",
        "/api/stations",
        &[("Authorization", &bearer)],
        None,
    );
    assert_eq!(status, 401);

    // The audit log names local and board actions.
    let audit: Vec<Value> =
        serde_json::from_str(&deck.ok(&["deck", "audit", "--json", "--limit", "100"])).unwrap();
    let actions: Vec<(&str, &str, &str)> = audit
        .iter()
        .map(|e| {
            (
                e["actor"].as_str().unwrap(),
                e["action"].as_str().unwrap(),
                e["result"].as_str().unwrap(),
            )
        })
        .collect();
    for want in [
        ("local", "auth.login_link", "ok"),
        ("owner", "auth.link", "ok"),
        ("anonymous", "auth.link", "refused"),
        ("local", "auth.set_password", "failed"),
        ("local", "auth.set_password", "ok"),
        ("owner", "auth.login", "ok"),
        ("local", "auth.token_create", "ok"),
        ("local", "auth.token_revoke", "ok"),
    ] {
        assert!(actions.contains(&want), "{want:?} in {actions:?}");
    }
    let raw = serde_json::to_string(&audit).unwrap();
    assert!(
        !raw.contains(token) && !raw.contains("correct horse"),
        "no secret in the audit log"
    );
    deck.stop();
}

#[test]
fn a_public_board_address_is_refused() {
    let deck = Deck::start();
    let out = fleetdeck(&deck.node)
        .args(["deck", "serve", "--listen", "8.8.8.8:7420"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--listen"), "{err}");
    let out = fleetdeck(&deck.node)
        .args(["deck", "serve", "--listen", "0.0.0.0:7420"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}
