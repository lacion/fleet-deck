//! The `WATCH` push connection of the bridge socket (MCP P4b §8.3, §11
//! test 3).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use super::*;
use crate::coordinator::socket::tests::rig;
use crate::coordinator::{BindingState, Limits, digest};

type Watcher = (BufReader<OwnedReadHalf>, OwnedWriteHalf);

/// Opens a connection and sends `first` as its first line.
async fn open(path: &Path, first: &str) -> Watcher {
    let stream = UnixStream::connect(path).await.unwrap();
    let (read, mut write) = stream.into_split();
    write
        .write_all(format!("{first}\n").as_bytes())
        .await
        .unwrap();
    (BufReader::new(read), write)
}

async fn watch(path: &Path, token: &str) -> Watcher {
    open(path, &format!("WATCH {token}")).await
}

/// The next line, without its newline; `None` at the end of the stream.
async fn line(reader: &mut BufReader<OwnedReadHalf>) -> Option<String> {
    let mut text = String::new();
    let n = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut text))
        .await
        .expect("a line or the end in time")
        .unwrap();
    (n > 0).then(|| text.trim_end_matches('\n').to_owned())
}

/// A watch that is established: `OK watch.v1` and the refresh were read.
async fn established(path: &Path, token: &str) -> Watcher {
    let (mut reader, write) = watch(path, token).await;
    assert_eq!(line(&mut reader).await.as_deref(), Some("OK watch.v1"));
    assert_eq!(line(&mut reader).await.as_deref(), Some(LIST_CHANGED_LINE));
    (reader, write)
}

/// A refusal: one line, then the end of the stream.
async fn refused(path: &Path, first: &str) -> String {
    let (mut reader, _write) = open(path, first).await;
    let got = line(&mut reader).await.expect("a refusal line");
    assert_eq!(line(&mut reader).await, None, "the connection closes");
    got
}

/// True when nothing arrives for a short time.
async fn quiet(reader: &mut BufReader<OwnedReadHalf>) -> bool {
    let mut text = String::new();
    tokio::time::timeout(Duration::from_millis(300), reader.read_line(&mut text))
        .await
        .is_err()
}

#[test]
fn the_notification_line_is_the_one_that_the_bridge_expects() {
    let value: Value = serde_json::from_str(LIST_CHANGED_LINE).unwrap();
    assert_eq!(value["method"], "notifications/tools/list_changed");
    assert!(value.get("id").is_none());
    assert!(!LIST_CHANGED_LINE.contains('\n'));
}

#[tokio::test]
async fn a_watch_connection_refreshes_pushes_and_closes_with_the_credential() {
    let r = rig().await;
    let key = digest(&r.token);

    // `OK watch.v1` and the refresh at once.
    let (mut reader, _write) = established(&r.path, &r.token).await;
    assert!(quiet(&mut reader).await, "no line without a change");

    // A change gives one line; a burst coalesces into at least one.
    assert!(r.bindings.rewrite(&key, |b| b.generation = 2));
    assert_eq!(line(&mut reader).await.as_deref(), Some(LIST_CHANGED_LINE));
    assert!(quiet(&mut reader).await);
    for generation in 3..8 {
        assert!(r.bindings.rewrite(&key, |b| b.generation = generation));
    }
    assert_eq!(line(&mut reader).await.as_deref(), Some(LIST_CHANGED_LINE));

    // The credential ends: the connection closes.
    assert!(
        r.bindings
            .update("b-1", |b| b.state = BindingState::Revoked)
    );
    let mut rest = Vec::new();
    while let Some(next) = line(&mut reader).await {
        rest.push(next);
    }
    assert!(
        rest.iter().all(|l| l == LIST_CHANGED_LINE),
        "only a pending notification may come before the end: {rest:?}"
    );
}

#[tokio::test]
async fn a_watch_is_refused_with_one_line_and_closes() {
    let r = rig().await;

    // An unknown and an ended credential.
    let (unknown, _) = crate::coordinator::mint().unwrap();
    assert_eq!(
        refused(&r.path, &format!("WATCH {}", unknown.expose())).await,
        "ERR unauthorized"
    );
    assert_eq!(
        refused(&r.path, "WATCH fdc1_short").await,
        "ERR unauthorized"
    );

    // A held credential.
    let key = digest(&r.token);
    r.bindings.hold([key.clone()]);
    assert_eq!(
        refused(&r.path, &format!("WATCH {}", r.token)).await,
        "ERR starting"
    );
    r.bindings.release([&key]);

    // The third watch of a binding.
    let first = established(&r.path, &r.token).await;
    let second = established(&r.path, &r.token).await;
    assert_eq!(
        refused(&r.path, &format!("WATCH {}", r.token)).await,
        "ERR limited"
    );
    // A closed watch gives its permit back.
    drop(first);
    let mut third = None;
    for _ in 0..50 {
        let (mut reader, write) = watch(&r.path, &r.token).await;
        match line(&mut reader).await.as_deref() {
            Some("OK watch.v1") => {
                third = Some((reader, write));
                break;
            }
            Some("ERR limited") => tokio::time::sleep(Duration::from_millis(50)).await,
            other => panic!("unexpected first line {other:?}"),
        }
    }
    assert!(third.is_some(), "the permit of the closed watch came back");
    drop(second);

    // A first line that is neither `AUTH` nor `WATCH` and an empty token keep
    // today's answer; so does a line that is too long.
    assert_eq!(refused(&r.path, "HELLO x").await, "ERR bad_request");
    assert_eq!(refused(&r.path, "WATCH ").await, "ERR bad_request");
    assert_eq!(
        refused(&r.path, &format!("WATCH {}", "a".repeat(2000))).await,
        "ERR bad_request"
    );
}

#[tokio::test]
async fn a_full_intake_gives_err_limited_to_a_watch() {
    let r = rig().await;
    // 16 connections that sent `AUTH` and wait for their message hold the
    // intake permits.
    let mut waiting = Vec::new();
    for _ in 0..Limits::default().unauthenticated {
        waiting.push(open(&r.path, &format!("AUTH {}", r.token)).await);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        refused(&r.path, &format!("WATCH {}", r.token)).await,
        "ERR limited"
    );
    drop(waiting);
}

#[tokio::test]
async fn a_client_byte_or_the_end_of_the_client_closes_a_watch() {
    let r = rig().await;

    let (mut reader, mut write) = established(&r.path, &r.token).await;
    write.write_all(b"x").await.unwrap();
    assert_eq!(line(&mut reader).await, None, "a client byte closes");

    let (mut reader, mut write) = established(&r.path, &r.token).await;
    write.shutdown().await.unwrap();
    assert_eq!(
        line(&mut reader).await,
        None,
        "the end of the client closes"
    );

    // The permits of the closed watches are free again.
    let (_reader, _write) = established(&r.path, &r.token).await;
    let (_reader2, _write2) = established(&r.path, &r.token).await;
}

#[tokio::test]
async fn a_watch_closes_when_the_socket_stops() {
    let r = rig().await;
    let (mut reader, _write) = established(&r.path, &r.token).await;
    // Dropping the socket aborts its tasks, the connection tasks with them.
    drop(r.socket);
    assert_eq!(line(&mut reader).await, None);
    drop(r.dir);
}

#[tokio::test]
async fn a_listen_message_over_the_socket_gets_32601_and_none_without_an_id() {
    let r = rig().await;
    let exchange = |message: &str| {
        let payload = format!("AUTH {}\n{message}\n", r.token);
        let path = r.path.clone();
        async move {
            let mut stream = UnixStream::connect(&path).await.unwrap();
            stream.write_all(payload.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
            let mut out = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut out))
                .await
                .unwrap()
                .unwrap();
            String::from_utf8(out).unwrap()
        }
    };
    let listen = serde_json::json!({"jsonrpc": "2.0", "id": "listen:0",
        "method": "subscriptions/listen",
        "params": {"notifications": {"toolsListChanged": true}}});
    let answer = exchange(&listen.to_string()).await;
    assert_eq!(answer.matches('\n').count(), 1);
    let value: Value = serde_json::from_str(answer.trim_end()).unwrap();
    assert_eq!(value["id"], "listen:0");
    assert_eq!(value["error"]["code"], -32601);
    assert_eq!(
        value["error"]["message"],
        "subscriptions/listen is not served on this transport"
    );
    let without_id = serde_json::json!({"jsonrpc": "2.0", "method": "subscriptions/listen",
        "params": {"notifications": {"toolsListChanged": true}}});
    assert_eq!(exchange(&without_id.to_string()).await, "");
}

#[tokio::test(start_paused = true)]
async fn a_watch_write_that_stalls_for_30_seconds_ends_the_connection() {
    // A pipe of one byte with no reader: the write cannot complete.
    let (mut write, _read) = tokio::io::duplex(1);
    let started = tokio::time::Instant::now();
    assert!(!send_line(&mut write, LIST_CHANGED_LINE).await);
    assert!(started.elapsed() >= WRITE_STALL);
    assert!(started.elapsed() < WRITE_STALL + Duration::from_secs(1));

    // A pipe with room is fine.
    let (mut write, mut read) = tokio::io::duplex(1024);
    assert!(send_line(&mut write, "x").await);
    let mut got = [0u8; 2];
    read.read_exact(&mut got).await.unwrap();
    assert_eq!(&got, b"x\n");
}
