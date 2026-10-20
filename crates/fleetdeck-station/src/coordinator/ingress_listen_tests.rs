//! The `subscriptions/listen` stream of the HTTP ingress and the stall rule
//! of the connection IO (MCP P4b §8.2, §11 test 2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::poll_fn;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::rt::Write as _;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;

use super::*;
use crate::coordinator::mcp_p1_rig_tests::{MAIL, MODERN, Rig, http_request, legacy, modern};
use crate::coordinator::permit_io::{PermitIo, PermitSlot, WRITE_STALL};
use crate::coordinator::{BindingState, ToolClass, digest, mint};

/// An open HTTP response that streams: the head and what the body has
/// delivered so far (with the chunk framing; the tests look for substrings).
struct Sse {
    tcp: TcpStream,
    status: u16,
    head: String,
    text: String,
}

const LIST_CHANGED: &str = "notifications/tools/list_changed";
const ACKNOWLEDGED: &str = "notifications/subscriptions/acknowledged";

fn changes(text: &str) -> usize {
    text.matches(LIST_CHANGED).count()
}

impl Sse {
    /// Sends `body` with `headers` and reads the response head.
    async fn open_with(
        addr: SocketAddr,
        token: &str,
        headers: &[(String, String)],
        body: &Value,
    ) -> Self {
        let mut tcp = TcpStream::connect(addr).await.unwrap();
        let request = http_request(addr, Some(token), headers, &body.to_string());
        tcp.write_all(request.as_bytes()).await.unwrap();
        let mut seen = Vec::new();
        let split = loop {
            if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                break at;
            }
            let mut buf = [0u8; 4096];
            let n = tokio::time::timeout(Duration::from_secs(5), tcp.read(&mut buf))
                .await
                .expect("the response head in time")
                .unwrap();
            assert!(n > 0, "the connection closed before a head");
            seen.extend_from_slice(&buf[..n]);
        };
        let head = String::from_utf8_lossy(&seen[..split]).to_ascii_lowercase();
        let text = String::from_utf8_lossy(&seen[split + 4..]).into_owned();
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        Self {
            tcp,
            status,
            head,
            text,
        }
    }

    /// A modern-era `subscriptions/listen` for `toolsListChanged`.
    async fn listen(addr: SocketAddr, token: &str) -> Self {
        let (body, headers) = modern(
            MODERN,
            1,
            "subscriptions/listen",
            json!({"notifications": {"toolsListChanged": true}}),
        );
        Self::open_with(addr, token, &headers, &body).await
    }

    /// A listen that is accepted: the status, the acknowledgment and the
    /// refresh.
    async fn accepted(addr: SocketAddr, token: &str) -> Self {
        let mut stream = Self::listen(addr, token).await;
        assert_eq!(stream.status, 200, "{}", stream.head);
        stream
            .until(|text| text.contains(ACKNOWLEDGED) && changes(text) >= 1)
            .await;
        stream
    }

    /// Reads until `done` holds for the text so far.
    async fn until(&mut self, done: impl Fn(&str) -> bool) {
        while !done(&self.text) {
            let mut buf = [0u8; 4096];
            let n = tokio::time::timeout(Duration::from_secs(5), self.tcp.read(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("the stream stayed quiet; text so far: {}", self.text))
                .unwrap();
            assert!(n > 0, "the stream ended; text so far: {}", self.text);
            self.text.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    }

    /// Reads to the end of the stream (the server closes the connection).
    async fn read_to_end(&mut self) -> String {
        let mut rest = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), self.tcp.read_to_end(&mut rest))
            .await
            .unwrap_or_else(|_| panic!("the stream did not end; text so far: {}", self.text))
            .unwrap();
        self.text.push_str(&String::from_utf8_lossy(&rest));
        self.text.clone()
    }
}

/// The status of a listen of `token` that is not read further.
async fn listen_status(r: &Rig, token: &str) -> u16 {
    Sse::listen(r.addr(), token).await.status
}

#[test]
fn the_connection_limit_is_512() {
    assert_eq!(Limits::default().connections, 512);
}

#[tokio::test]
async fn a_listen_stream_refreshes_notifies_and_ends_with_the_credential() {
    let r = Rig::start(BindingState::Active, MAIL).await;
    let key = digest(&r.token);

    // The response is an event stream; the ack and the refresh arrive.
    let mut one = Sse::accepted(r.addr(), &r.token).await;
    assert!(one.head.contains("content-type: text/event-stream"));
    assert!(one.head.contains("cache-control:"));
    assert!(one.head.contains("x-accel-buffering: no"));
    assert!(
        !one.head.contains("mcp-session-id"),
        "only three headers are kept"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(changes(&one.text), 1, "one refresh: {}", one.text);

    // An in-place change gives one more `list_changed`.
    assert!(r.bindings.rewrite(&key, |b| {
        b.classes.insert(ToolClass::Own);
    }));
    one.until(|text| changes(text) >= 2).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(changes(&one.text), 2, "one per change: {}", one.text);

    // Eight held request permits do not block a listen; the third listen of
    // the binding is limited; the request permits are really held.
    let held: Vec<_> = (0..8)
        .map(|_| {
            r.bindings
                .authenticate(Some(&format!("Bearer {}", r.token)))
        })
        .collect();
    assert!(held.iter().all(|a| matches!(a, Auth::Ok { .. })));
    assert_eq!(
        r.http_modern(&r.token, "tools/list", json!({}))
            .await
            .status,
        429,
        "an ordinary request is limited"
    );
    let two = Sse::accepted(r.addr(), &r.token).await;
    assert_eq!(listen_status(&r, &r.token).await, 429, "the third listen");
    drop(held);

    // A closed stream gives its permit back (the server notices the closed
    // connection at its next write).
    drop(one);
    drop(two);
    let mut again = None;
    for _ in 0..100 {
        assert!(r.bindings.rewrite(&key, |b| b.generation += 1));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let stream = Sse::listen(r.addr(), &r.token).await;
        if stream.status == 200 {
            again = Some(stream);
            break;
        }
    }
    assert!(
        again.is_some(),
        "the permits of the closed streams came back"
    );
    drop(again);

    // The credential ends: the stream ends with rmcp's final result.
    let ended = r.add("b-2", BindingState::Active, MAIL);
    let mut stream = Sse::accepted(r.addr(), &ended).await;
    assert!(
        r.bindings
            .update("b-2", |b| b.state = BindingState::Revoked)
    );
    let text = stream.read_to_end().await;
    assert!(text.contains("\"result\""), "the final result: {text}");
    assert_eq!(
        listen_status(&r, &ended).await,
        401,
        "an ended credential gets the 401"
    );

    // The station limit: with every station permit taken, a listen is limited.
    let fresh = r.fresh(BindingState::Active, MAIL);
    let station = r
        .bindings
        .station_streams
        .listens
        .clone()
        .try_acquire_many_owned(256)
        .expect("the station permits are free");
    assert_eq!(listen_status(&r, &fresh).await, 429, "the station limit");
    drop(station);
    assert_eq!(listen_status(&r, &fresh).await, 200);
}

#[tokio::test]
async fn a_legacy_era_listen_gets_the_refusal_of_rmcp_and_holds_no_permit() {
    let r = Rig::start(BindingState::Active, MAIL).await;
    let token = r.fresh(BindingState::Active, MAIL);
    let body = legacy(
        1,
        "subscriptions/listen",
        json!({"notifications": {"toolsListChanged": true}}),
    );
    let reply = r.http(Some(&token), &[], &body).await;
    assert!(
        !reply.head.contains("text/event-stream"),
        "a normal reply: {reply:?}"
    );
    assert!(reply.json().get("error").is_some(), "{reply:?}");
    // No listen permit stayed: both permits of the binding are free.
    let one = Sse::accepted(r.addr(), &token).await;
    let two = Sse::accepted(r.addr(), &token).await;
    drop((one, two));
}

#[tokio::test]
async fn a_listen_is_refused_like_a_request_without_a_valid_credential() {
    let r = Rig::start(BindingState::Active, MAIL).await;
    let (unknown, _) = mint().unwrap();
    assert_eq!(listen_status(&r, unknown.expose()).await, 401);
    assert_eq!(listen_status(&r, "fdc1_short").await, 401);
    r.bindings.hold([digest(&r.token)]);
    assert_eq!(listen_status(&r, &r.token).await, 503, "starting");
    // Listens do not use the unauthenticated permits after the handshake.
    r.bindings.release([&digest(&r.token)]);
    let mut open = Vec::new();
    for i in 0..Limits::default().unauthenticated + 2 {
        let token = r.add(&format!("l-{i}"), BindingState::Active, MAIL);
        open.push(Sse::accepted(r.addr(), &token).await);
    }
    assert_eq!(open.len(), Limits::default().unauthenticated + 2);
}

/// An IO whose writes are `Pending` until `ready` is set.
struct Stuck {
    ready: Arc<AtomicBool>,
}

impl Stuck {
    fn poll(&self, len: usize) -> Poll<io::Result<usize>> {
        if self.ready.load(Ordering::SeqCst) {
            Poll::Ready(Ok(len))
        } else {
            Poll::Pending
        }
    }
}

impl hyper::rt::Write for Stuck {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.poll(buf.len())
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.poll(bufs.iter().map(|b| b.len()).sum())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl hyper::rt::Read for Stuck {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Pending
    }
}

fn stuck() -> (PermitIo<Stuck>, Arc<AtomicBool>) {
    let ready = Arc::new(AtomicBool::new(false));
    let io = PermitIo::new(
        Stuck {
            ready: ready.clone(),
        },
        PermitSlot::default(),
    );
    (io, ready)
}

/// One poll of `poll_write` with the task context.
async fn write_once(io: &mut PermitIo<Stuck>) -> Poll<io::Result<usize>> {
    poll_fn(|cx| Poll::Ready(Pin::new(&mut *io).poll_write(cx, b"x"))).await
}

/// One poll of `poll_write_vectored` with the task context.
async fn vectored_once(io: &mut PermitIo<Stuck>) -> Poll<io::Result<usize>> {
    poll_fn(|cx| Poll::Ready(Pin::new(&mut *io).poll_write_vectored(cx, &[io::IoSlice::new(b"x")])))
        .await
}

fn timed_out(polled: &Poll<io::Result<usize>>) -> bool {
    matches!(polled, Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::TimedOut)
}

#[tokio::test(start_paused = true)]
async fn a_write_that_stays_pending_for_30_seconds_fails_with_timed_out() {
    assert_eq!(WRITE_STALL, Duration::from_secs(30));
    let (mut io, _ready) = stuck();
    let started = Instant::now();
    // Awaiting the write: the paused clock advances to the deadline.
    let result = poll_fn(|cx| Pin::new(&mut io).poll_write(cx, b"x")).await;
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert_eq!(started.elapsed(), WRITE_STALL);
}

#[tokio::test(start_paused = true)]
async fn the_vectored_write_shares_the_deadline_and_a_ready_write_clears_it() {
    let (mut io, ready) = stuck();

    // One deadline for both write paths: it starts at the first `Pending`.
    assert!(write_once(&mut io).await.is_pending());
    tokio::time::advance(Duration::from_secs(20)).await;
    assert!(vectored_once(&mut io).await.is_pending());
    tokio::time::advance(Duration::from_secs(9)).await;
    assert!(vectored_once(&mut io).await.is_pending());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(timed_out(&vectored_once(&mut io).await));

    // A `Ready` write clears the deadline: the next `Pending` arms a new one.
    assert!(write_once(&mut io).await.is_pending());
    tokio::time::advance(Duration::from_secs(20)).await;
    ready.store(true, Ordering::SeqCst);
    assert!(matches!(write_once(&mut io).await, Poll::Ready(Ok(1))));
    ready.store(false, Ordering::SeqCst);
    assert!(write_once(&mut io).await.is_pending());
    tokio::time::advance(Duration::from_secs(29)).await;
    assert!(write_once(&mut io).await.is_pending());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(timed_out(&write_once(&mut io).await));

    // Flush is unchanged: it never times out.
    let flushed = poll_fn(|cx| Poll::Ready(Pin::new(&mut io).poll_flush(cx))).await;
    assert!(matches!(flushed, Poll::Ready(Ok(()))));
}
