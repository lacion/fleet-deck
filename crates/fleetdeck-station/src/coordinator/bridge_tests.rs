//! Bridge client tests (DL-194 T2): `relay` against a running socket.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::Value;
use tokio::io::AsyncReadExt;

use super::*;
use crate::coordinator::socket::tests::rig;

/// Runs `relay` on `input` and returns the output lines.
async fn run_relay(socket: &Path, token: &str, input: &str) -> Vec<Value> {
    let (mut stdin_w, stdin_r) = tokio::io::duplex(1024 * 1024);
    let (stdout_w, mut stdout_r) = tokio::io::duplex(1024 * 1024);
    stdin_w.write_all(input.as_bytes()).await.unwrap();
    drop(stdin_w);
    relay(socket, token, stdin_r, stdout_w).await;
    let mut out = String::new();
    stdout_r.read_to_string(&mut out).await.unwrap();
    out.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn a_request_gets_its_answer() {
    let r = rig().await;
    let input = "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/list\"}\n";
    let out = run_relay(&r.path, &r.token, input).await;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["id"], 7);
    assert!(out[0]["result"]["tools"].as_array().is_some());
}

#[tokio::test]
async fn a_request_while_the_socket_does_not_exist_gets_the_restart_error_with_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let input = "{\"jsonrpc\":\"2.0\",\"id\":\"abc\",\"method\":\"tools/list\"}\n";
    let out = run_relay(&dir.path().join("none.s"), "fdc1_x", input).await;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["id"], "abc");
    assert_eq!(out[0]["error"]["code"], -32000);
    assert_eq!(
        out[0]["error"]["message"],
        "Fleet Deck is restarting; try again in a moment."
    );
}

#[tokio::test]
async fn a_notification_gets_no_line_also_when_the_socket_is_down() {
    let r = rig().await;
    let note = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
    assert!(run_relay(&r.path, &r.token, note).await.is_empty());
    let dir = tempfile::tempdir().unwrap();
    assert!(
        run_relay(&dir.path().join("none.s"), "fdc1_x", note)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn three_concurrent_requests_all_get_their_answers() {
    let r = rig().await;
    let input: String = [11, 12, 13]
        .iter()
        .map(|id| format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/list\"}}\n"))
        .collect();
    let out = run_relay(&r.path, &r.token, &input).await;
    let mut ids: Vec<i64> = out.iter().map(|v| v["id"].as_i64().unwrap()).collect();
    ids.sort_unstable();
    assert_eq!(ids, [11, 12, 13]);
    assert!(out.iter().all(|v| v["result"]["tools"].is_array()));
}

#[tokio::test]
async fn station_refusals_become_json_rpc_errors() {
    let r = rig().await;
    let list = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n";
    let out = run_relay(&r.path, "fdc1_wrong", list).await;
    assert_eq!(out[0]["error"]["code"], -32001);
    assert_eq!(
        out[0]["error"]["message"],
        "The session token is not valid."
    );
    assert_eq!(out[0]["id"], 1);
    // A batch is relayed unchanged; the station refuses it.
    let out = run_relay(&r.path, &r.token, "[{\"id\":1}]\n").await;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["id"], Value::Null);
    assert_eq!(out[0]["error"]["code"], -32600);
    assert_eq!(out[0]["error"]["message"], "Invalid request.");
}

#[tokio::test]
async fn bad_input_lines_get_a_parse_error_or_are_skipped() {
    let r = rig().await;
    let long = "a".repeat(Limits::default().body_bytes + 10);
    let input = format!(
        "\n  \nnot json\n{long}\n{{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/list\"}}\n"
    );
    let out = run_relay(&r.path, &r.token, &input).await;
    assert_eq!(out.len(), 2);
    assert_eq!(out[0]["id"], Value::Null);
    assert_eq!(out[0]["error"]["code"], -32700);
    assert_eq!(out[0]["error"]["message"], "Parse error.");
    assert_eq!(out[1]["id"], 5);
    assert!(out[1]["result"].is_object());
}

#[tokio::test]
async fn a_held_token_gets_the_starting_error_with_its_id() {
    let r = rig().await;
    r.bindings.hold([crate::coordinator::digest(&r.token)]);
    let input = "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/list\"}\n";
    let out = run_relay(&r.path, &r.token, input).await;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["id"], 9);
    assert_eq!(out[0]["error"]["code"], -32000);
    assert_eq!(
        out[0]["error"]["message"],
        "Fleet Deck is starting; try again in a moment."
    );
}
