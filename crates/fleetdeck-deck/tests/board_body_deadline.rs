//! The request body deadline of the board (FDV2-001): a client that sends
//! complete headers and withholds the body is answered `408
//! request_timeout` and its connection ends, so its admission permit is
//! released. A body that arrives in time is served.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::board::{BoardClient, board};
use common::*;
use fleetdeck_core::Secret;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;

const LIMIT: Duration = Duration::from_secs(15);
const PASSWORD: &str = "correct horse battery";

async fn setup() -> (Fixture, BoardClient) {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    (f, c)
}

async fn connect(c: &BoardClient) -> TlsStream<TcpStream> {
    let tcp = TcpStream::connect(c.addr).await.unwrap();
    let name = ServerName::try_from("127.0.0.1").unwrap();
    c.tls().connect(name, tcp).await.unwrap()
}

async fn eventually(what: &str, ok: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Reads until the server closes the stream (the end or a reset) and
/// returns the bytes and the time it took.
async fn read_until_closed(s: &mut TlsStream<TcpStream>) -> (String, Duration) {
    let start = Instant::now();
    let mut raw = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let read = tokio::time::timeout(LIMIT, s.read(&mut buf))
            .await
            .expect("the server closes the connection within 15 s");
        match read {
            Ok(0) | Err(_) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
    }
    (String::from_utf8_lossy(&raw).into_owned(), start.elapsed())
}

#[tokio::test]
async fn an_unfinished_login_body_is_answered_408_and_the_connection_ends() {
    let (_f, c) = setup().await;
    let mut s = connect(&c).await;
    s.write_all(
        b"POST /api/auth/login HTTP/1.1\r\nHost: 127.0.0.1\r\n\
          Content-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"pass",
    )
    .await
    .unwrap();
    assert_eq!(c.admitted(), 1);
    let (answer, took) = read_until_closed(&mut s).await;
    assert!(took < LIMIT, "{took:?}");
    assert!(took >= Duration::from_secs(9), "{took:?}");
    assert!(answer.starts_with("HTTP/1.1 408"), "{answer}");
    assert!(
        answer.to_ascii_lowercase().contains("connection: close"),
        "{answer}"
    );
    assert!(answer.contains("request_timeout"), "{answer}");
    assert!(
        answer.contains("the request body was not received in time"),
        "{answer}"
    );
    eventually("the permit of the stalled client is released", || {
        c.admitted() == 0
    })
    .await;
}

#[tokio::test]
async fn a_normal_login_still_works() {
    let (_f, c) = setup().await;
    c.store
        .set_password(Secret::new(PASSWORD.to_owned()))
        .await
        .unwrap();
    let r = c
        .post("/api/auth/login", &[], json!({ "password": PASSWORD }))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
}

/// A request with a JSON body of `total` bytes that the client sends in
/// `parts` writes, `pause` apart. The status of the answer and its body.
async fn slow_post(
    c: &BoardClient,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
    parts: usize,
    pause: Duration,
) -> (u16, Value) {
    let mut s = connect(c).await;
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n",
        c.addr,
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).await.unwrap();
    let size = body.len().div_ceil(parts);
    for chunk in body.as_bytes().chunks(size) {
        s.write_all(chunk).await.unwrap();
        tokio::time::sleep(pause).await;
    }
    let (answer, _) = read_until_closed(&mut s).await;
    let status = answer.split(' ').nth(1).unwrap().parse().unwrap();
    let json = answer
        .split_once("\r\n\r\n")
        .and_then(|(_, b)| serde_json::from_str(b).ok())
        .unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn a_large_paste_upload_that_finishes_in_time_is_served() {
    let (_f, c) = setup().await;
    let (cookie, csrf) = c.login().await;
    let origin = c.origin();
    // Close to the 1 MiB limit of the paste route (a larger body is
    // refused with 413 by the route, before and after this layer).
    let data = "A".repeat(1_000_000);
    let body = json!({ "upload_id": "upload-0001", "ext": "png", "data_base64": data });
    let (status, json) = slow_post(
        &c,
        "/api/stations/nope/paste",
        &[
            ("Cookie", &cookie),
            ("X-CSRF-Token", &csrf),
            ("Origin", &origin),
        ],
        &body.to_string(),
        10,
        Duration::from_millis(200),
    )
    .await;
    // The whole body was read; the station is unknown.
    assert_eq!(
        (status, json["error"].as_str()),
        (404, Some("unknown_station"))
    );
}
