//! The board admission limits over loopback HTTPS (FDV2-001): a cap on open
//! connections for each source, a header read timeout that also ends an idle
//! keep-alive connection, and an upgraded WebSocket that stays open.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::board::{BoardClient, board};
use common::*;
use fleetdeck_core::Secret;
use fleetdeck_deck::store::AuditResult;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, TcpStream};
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;

const LIMIT: Duration = Duration::from_secs(15);

async fn eventually(what: &str, ok: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn setup() -> (Fixture, BoardClient) {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    (f, c)
}

/// A TLS connection to the board, from `source` when given.
async fn connect(c: &BoardClient, source: Option<IpAddr>) -> std::io::Result<TlsStream<TcpStream>> {
    let tcp = match source {
        None => TcpStream::connect(c.addr).await?,
        Some(ip) => {
            let socket = TcpSocket::new_v4()?;
            socket.bind(SocketAddr::new(ip, 0))?;
            socket.connect(c.addr).await?
        }
    };
    let name = ServerName::try_from("127.0.0.1").unwrap();
    c.tls().connect(name, tcp).await
}

fn request(close: bool) -> String {
    let connection = if close { "Connection: close\r\n" } else { "" };
    format!("GET /api/auth/session HTTP/1.1\r\nHost: 127.0.0.1\r\n{connection}\r\n")
}

/// Reads one response (head and body); returns its status line and body.
async fn read_response(s: &mut TlsStream<TcpStream>) -> (String, String) {
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        let n = s.read(&mut byte).await.unwrap();
        assert_eq!(n, 1, "the response ended inside its head");
        raw.push(byte[0]);
    }
    let head = String::from_utf8(raw).unwrap();
    let length: usize = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length: ")?
                .parse()
                .ok()
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    s.read_exact(&mut body).await.unwrap();
    let status = head.lines().next().unwrap().to_owned();
    (status, String::from_utf8(body).unwrap())
}

/// True when the server closed the stream within `LIMIT` (the end of the
/// stream or a reset); the time it took.
async fn closed_by_server(s: &mut TlsStream<TcpStream>) -> Duration {
    let start = Instant::now();
    let mut buf = [0u8; 256];
    loop {
        let read = tokio::time::timeout(LIMIT, s.read(&mut buf))
            .await
            .expect("the server closes the connection within 15 s");
        match read {
            Ok(0) | Err(_) => return start.elapsed(),
            Ok(_) => {}
        }
    }
}

#[tokio::test]
async fn the_33rd_idle_connection_of_one_ip_is_closed_at_accept() {
    let (_f, c) = setup().await;
    let mut held = Vec::new();
    for _ in 0..32 {
        held.push(connect(&c, None).await.expect("an admitted connection"));
    }
    assert_eq!(c.admitted(), 32);
    // The 33rd gets no TLS: the server drops the TCP stream at accept.
    let refused = tokio::time::timeout(Duration::from_secs(5), connect(&c, None))
        .await
        .expect("the refusal is quick");
    assert!(refused.is_err(), "the 33rd connection must be refused");
    assert_eq!(c.admitted(), 32);

    // Another IP is served while the first one is full (Linux has the whole
    // 127.0.0.0/8 range on loopback).
    if cfg!(target_os = "linux") {
        let other = Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)));
        let mut s = connect(&c, other).await.expect("another IP is admitted");
        s.write_all(request(true).as_bytes()).await.unwrap();
        assert_eq!(read_response(&mut s).await.0, "HTTP/1.1 401 Unauthorized");
    }

    // After the flood the connections are gone and the counts are normal.
    drop(held);
    eventually("the admission guards of the flood end", || {
        c.admitted() == 0
    })
    .await;
    let again = c.get("/api/auth/session", &[]).await;
    assert_eq!(again.status, 401);
}

#[tokio::test]
async fn a_slow_header_client_is_closed_within_15_seconds() {
    let (_f, c) = setup().await;
    let mut s = connect(&c, None).await.unwrap();
    // Half a header, then silence.
    s.write_all(b"GET /api/auth/session HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Slow: a")
        .await
        .unwrap();
    assert_eq!(c.admitted(), 1);
    let took = closed_by_server(&mut s).await;
    assert!(took < LIMIT, "{took:?}");
    eventually("the guard of the slow client ends", || c.admitted() == 0).await;
}

#[tokio::test]
async fn an_idle_keep_alive_connection_is_closed_within_15_seconds() {
    let (_f, c) = setup().await;
    let mut s = connect(&c, None).await.unwrap();
    s.write_all(request(false).as_bytes()).await.unwrap();
    assert_eq!(read_response(&mut s).await.0, "HTTP/1.1 401 Unauthorized");
    // The connection stays open for the next request, then goes silent.
    assert_eq!(c.admitted(), 1);
    let took = closed_by_server(&mut s).await;
    assert!(took < LIMIT, "{took:?}");
    eventually("the guard of the idle connection ends", || {
        c.admitted() == 0
    })
    .await;
}

#[tokio::test]
async fn an_upgraded_websocket_stays_open_past_the_header_timeout() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let (_f, c) = setup().await;
    let login = c.login().await;
    let origin = c.origin();
    let mut ws = c
        .ws("/api/ws", &[("Cookie", &login.0), ("Origin", &origin)])
        .await
        .expect("the socket opens");
    tokio::time::sleep(LIMIT + Duration::from_secs(1)).await;
    // The socket still answers a ping, and its connection still holds its
    // admission guard.
    ws.0.send(Message::Ping(vec![7].into())).await.unwrap();
    let pong = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.0.next().await {
                Some(Ok(Message::Pong(p))) => return p,
                Some(Ok(_)) => {}
                other => panic!("the socket closed: {other:?}"),
            }
        }
    })
    .await
    .expect("a pong");
    assert_eq!(pong.as_ref(), [7]);
    assert!(c.admitted() >= 1);
    drop(ws);
    eventually("the guard of the socket ends", || c.admitted() == 0).await;
}

/// `POST path` with a JSON body from the source `ip`; the status code and
/// the error code of the answer.
async fn post_from(c: &BoardClient, ip: IpAddr, path: &str, body: &Value) -> (u16, String) {
    let mut s = connect(c, Some(ip)).await.unwrap();
    let body = body.to_string();
    let text = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(text.as_bytes()).await.unwrap();
    let (status, body) = read_response(&mut s).await;
    let code = status.split(' ').nth(1).unwrap().parse().unwrap();
    let json: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    (code, json["error"].as_str().unwrap_or("").to_owned())
}

const PASSWORD: &str = "correct horse battery";

fn loopback(last: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(127, 0, 0, last))
}

#[tokio::test]
async fn twenty_concurrent_wrong_passwords_start_five_verifications() {
    let (_f, c) = setup().await;
    c.store
        .set_password(Secret::new(PASSWORD.to_owned()))
        .await
        .unwrap();
    let wrong = json!({ "password": "wrong" });
    let answers = futures_util::future::join_all(
        (0..20).map(|_| post_from(&c, loopback(1), "/api/auth/login", &wrong)),
    )
    .await;
    let unauthorized = answers.iter().filter(|a| a.0 == 401).count();
    let limited: Vec<_> = answers.iter().filter(|a| a.0 == 429).collect();
    assert_eq!(unauthorized, 5, "{answers:?}");
    assert_eq!(limited.len(), 15, "{answers:?}");
    assert!(limited.iter().all(|a| a.1 == "too_many_attempts"));
    let limits = &c.state.limiter;
    assert_eq!(limits.verifications_started.load(Ordering::SeqCst), 5);
    assert!(limits.verifications_peak.load(Ordering::SeqCst) <= 2);

    // The same source is locked, even with the right password.
    let good = json!({ "password": PASSWORD });
    let r = post_from(&c, loopback(1), "/api/auth/login", &good).await;
    assert_eq!(r, (429, "too_many_attempts".to_owned()));
    // A second source logs in at once.
    let r = post_from(&c, loopback(2), "/api/auth/login", &good).await;
    assert_eq!(r.0, 200, "{r:?}");
    // The other flows of the locked source still work.
    let token = c.store.create_login_link().await.unwrap();
    let r = post_from(
        &c,
        loopback(1),
        "/api/auth/link",
        &json!({ "token": token.expose() }),
    )
    .await;
    assert_eq!(r.0, 200, "{r:?}");
    let r = post_from(
        &c,
        loopback(1),
        "/api/auth/join",
        &json!({ "token": "nope", "display_name": "Ann" }),
    )
    .await;
    assert_eq!(r, (401, "invalid_invite".to_owned()));
    // The refusals name their flow.
    let rows = c.store.audit_log(100).await.unwrap();
    assert!(rows.iter().any(|e| {
        e.action == "auth.login"
            && e.result == AuditResult::Refused
            && e.detail.as_deref() == Some("too_many_attempts")
            && e.target.as_deref() == Some("password")
    }));
    assert!(
        rows.iter()
            .any(|e| { e.action == "auth.join" && e.target.as_deref() == Some("member_join") })
    );
}

#[tokio::test]
async fn a_busy_deck_answers_429_busy_and_does_not_count_a_failure() {
    let (_f, c) = setup().await;
    c.store
        .set_password(Secret::new(PASSWORD.to_owned()))
        .await
        .unwrap();
    // Both verification slots are taken.
    let slots = c.state.limiter.hash_permits();
    let held = slots.acquire_many_owned(2).await.unwrap();
    let started = Instant::now();
    let r = post_from(
        &c,
        loopback(1),
        "/api/auth/login",
        &json!({ "password": "wrong" }),
    )
    .await;
    assert_eq!(r, (429, "busy".to_owned()));
    assert!(started.elapsed() >= Duration::from_secs(2));
    let now = c.store.now_ms();
    assert_eq!(c.state.limiter.password.counts(loopback(1), now), (0, 0));
    assert_eq!(
        c.state.limiter.verifications_started.load(Ordering::SeqCst),
        0
    );
    assert!(
        c.audited("auth.login", AuditResult::Refused)
            .await
            .contains(&Some("busy".to_owned()))
    );
    // A free slot verifies again.
    drop(held);
    let r = post_from(
        &c,
        loopback(1),
        "/api/auth/login",
        &json!({ "password": PASSWORD }),
    )
    .await;
    assert_eq!(r.0, 200, "{r:?}");
    assert_eq!(
        c.state.limiter.verifications_started.load(Ordering::SeqCst),
        1
    );
}
