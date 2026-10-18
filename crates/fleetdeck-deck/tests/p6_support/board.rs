//! What the proofs ask of the board: a raw GET (a PNG is not UTF-8, so the
//! shared client cannot read it), the owner screenshot of the P5a route, the
//! revocation of a grant and the elevated session start that makes a grant.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use fleetdeck_proto::capture::CAPTURE_ID_HEADER;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::ServerName;

use crate::common::board::{BoardClient, Http};
use crate::settings_support::{Headers, post, refs};

/// A valid capture ID for the owner screenshot.
pub const OWNER_CAPTURE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

/// The body of a chunked answer.
fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(at) = raw.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&raw[..at]).trim(), 16).unwrap();
        raw = &raw[at + 2..];
        if size == 0 {
            break;
        }
        out.extend_from_slice(&raw[..size]);
        raw = &raw[size + 2..];
    }
    out
}

/// `GET path` on the board with `headers`: the status and the body bytes.
pub async fn raw_get(c: &BoardClient, path: &str, headers: &[(&str, &str)]) -> (u16, Vec<u8>) {
    let tcp = TcpStream::connect(c.addr).await.unwrap();
    let name = ServerName::try_from("127.0.0.1").unwrap();
    let mut s = c.tls().connect(name, tcp).await.unwrap();
    let mut text = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        c.addr
    );
    for (k, v) in headers {
        text.push_str(&format!("{k}: {v}\r\n"));
    }
    text.push_str("\r\n");
    s.write_all(text.as_bytes()).await.unwrap();
    s.flush().await.unwrap();
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(30), s.read_to_end(&mut raw))
        .await
        .expect("a response");
    let at = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a header end");
    let head = String::from_utf8_lossy(&raw[..at]).to_lowercase();
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap();
    let body = &raw[at + 4..];
    if head.contains("transfer-encoding: chunked") {
        (status, dechunk(body))
    } else {
        (status, body.to_vec())
    }
}

/// The board of the proofs lets the owner use the desktop routes.
pub fn enable_desktop(c: &BoardClient) {
    let mut live = (*c.state.live.get()).clone();
    live.desktop_control = true;
    c.state.live.publish(live);
}

/// An owner screenshot of `monitor` of the station `station` (P5a route).
pub async fn owner_capture(c: &BoardClient, h: &Headers, station: &str, monitor: &str) -> Http {
    let origin = format!("https://deck.tail1234.ts.net:{}", c.addr.port());
    let mut headers = refs(h);
    headers.push(("Origin", origin.as_str()));
    headers.push((CAPTURE_ID_HEADER, OWNER_CAPTURE));
    c.post(
        &format!("/api/stations/{station}/desktop/act"),
        &headers,
        json!({"action": "screen_capture", "monitor": monitor}),
    )
    .await
}

/// `session.start` of a coordinator session on `station` with a grant of
/// `classes` over `targets`, as the board sends it with an elevation.
pub async fn start_grant(
    c: &BoardClient,
    h: &Headers,
    elevation: &str,
    station: &str,
    (classes, targets): (&[&str], &[&str]),
) -> Http {
    let body = json!({"station": station, "command_id": "grant-1", "elevation": elevation,
        "command": {"op": "start", "prompt": null, "spec": {
            "provider_instance": "claude", "cwd": "/w", "mode": null, "model": null,
            "title": null, "coordinator": {
                "grant_id": "", "classes": classes, "stations": targets,
                "idle_ttl_ms": 0, "expires_at": 0,
                "control_skill": {"version": "", "text": ""}}}}});
    post(c, h, "/api/commands/session.start", body).await
}

/// The owner revokes `grant_id` on the board.
pub async fn revoke(c: &BoardClient, h: &Headers, grant_id: &str) -> Value {
    let r = post(
        c,
        h,
        &format!("/api/coordinator/grants/{grant_id}/revoke"),
        json!({}),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
}
