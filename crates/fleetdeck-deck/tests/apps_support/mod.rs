#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
//! The helpers of the MCP App route tests (`board_apps`, `board_apps_more`):
//! the route paths, the JSON of the station answers and a request that the
//! fake station answers.

use std::future::Future;

use crate::common::board::{BoardClient, Http};
use crate::common::*;
use crate::share_support::{Headers, refs};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::link::Outcome;
use ring::digest::{SHA256, digest};
use serde_json::{Value, json};

/// A ULID that the tests use as an app id.
pub const APP: &str = "01HZX3K5M7Q9R2S4T6V8W0Y1Z3";
/// A ULID that the tests use as a call id.
pub const CALL: &str = "01HZX3K5M7Q9R2S4T6V8W0Y1Z4";

pub fn view_path(st: &str, session: &str, app: &str) -> String {
    format!("/api/sessions/{st}/{session}/apps/{app}")
}

pub fn call_path(st: &str, session: &str, app: &str) -> String {
    format!("/api/sessions/{st}/{session}/apps/{app}/call")
}

pub fn calls_path(st: &str, session: &str) -> String {
    format!("/api/sessions/{st}/{session}/app-calls")
}

pub fn release_path(st: &str, session: &str, call: &str) -> String {
    format!("/api/sessions/{st}/{session}/app-calls/{call}/release")
}

/// The lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// A `session.app_meta` answer for the document `doc`.
pub fn meta_json(doc: &[u8], csp: &Value) -> Value {
    json!({
        "app_id": APP, "node_id": "n1", "server": "demo", "tool": "show_counter",
        "resource_uri": "ui://demo/counter", "csp": csp, "prefers_border": true,
        "doc_bytes": doc.len(), "doc_sha256": sha256_hex(doc),
        "tool_input": { "start": 1 }, "tool_result": { "content": [] },
        "created_at": 1700000000000_i64,
    })
}

/// A `session.app_meta` answer with a wrong hash.
pub fn meta_with_sha(doc: &[u8], sha: &str) -> Value {
    let mut meta = meta_json(doc, &json!({}));
    meta["doc_sha256"] = json!(sha);
    meta
}

/// The `session.app_document` answers for `doc` in chunks of `size` bytes.
pub fn document_answers(doc: &[u8], size: usize) -> Vec<Outcome> {
    let chunks: Vec<&[u8]> = doc.chunks(size).collect();
    let last = chunks.len() - 1;
    chunks
        .iter()
        .enumerate()
        .map(|(i, raw)| {
            Outcome::Ok(json!({
                "data_b64": STANDARD.encode(raw),
                "eof": i == last,
                "total": doc.len(),
            }))
        })
        .collect()
}

/// The JSON of one open call of the list answer.
pub fn open_call(call: &str, session: &str, state: &str, cause: Option<&str>) -> Value {
    let mut v = json!({
        "call_id": call, "session_id": session, "app_id": APP, "server": "demo",
        "tool": "bump", "state": state, "created_at": 1700000000000_i64,
    });
    if let Some(cause) = cause {
        v["cause"] = json!(cause);
    }
    v
}

pub fn tools_call(tool: &str, confirmed: bool) -> Value {
    json!({
        "request": { "method": "tools/call", "name": tool, "arguments": { "by": 2 } },
        "confirmed": confirmed,
    })
}

pub fn done() -> Outcome {
    Outcome::Ok(json!({ "outcome": "done", "result": { "content": [] } }))
}

/// Runs `call` while the fake station answers the next requests with
/// `answers`, in order. Returns the answer and the requests.
pub async fn exchange<F: Future<Output = Http>>(
    call: F,
    station: &mut LiveLink,
    answers: Vec<Outcome>,
) -> (Http, Vec<InboundRequest>) {
    let answer = async {
        let mut seen = Vec::new();
        for outcome in answers {
            let req = next_request(station).await;
            station.control.respond(req.id, outcome).await.unwrap();
            seen.push(req);
        }
        seen
    };
    tokio::join!(call, answer)
}

/// One request that the fake station answers with `outcome`.
pub async fn one(
    c: &BoardClient,
    h: &Headers,
    station: &mut LiveLink,
    method: &str,
    path: &str,
    body: Option<Value>,
    outcome: Outcome,
) -> (Http, InboundRequest) {
    let headers = refs(h);
    let call = c.req(method, path, &headers, body);
    let (r, mut seen) = exchange(call, station, vec![outcome]).await;
    (r, seen.remove(0))
}

/// A view of the app that mints a token; returns the view answer.
pub async fn mint(
    c: &BoardClient,
    h: &Headers,
    station: &mut LiveLink,
    st: &str,
    doc: &[u8],
    csp: &Value,
) -> Http {
    let (r, _) = one(
        c,
        h,
        station,
        "GET",
        &view_path(st, "s1", APP),
        None,
        Outcome::Ok(meta_json(doc, csp)),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    r
}
