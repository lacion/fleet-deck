//! Data channel tests over in-memory WebSocket pairs.

use std::time::Duration;

use futures_util::SinkExt;
use tokio::io::DuplexStream;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role as WsRole;

use super::*;
use crate::hello::Channel;
use crate::mux::{MAX_STREAMS, WINDOW};
use crate::tokens::TOKEN_TTL;
use crate::ws::ws_config;

type Raw = WebSocketStream<DuplexStream>;

const T: Duration = Duration::from_secs(10);

async fn ws_pair(buffer: usize) -> (Raw, Raw) {
    let (a, b) = tokio::io::duplex(buffer);
    let cfg = Some(ws_config(Channel::Data));
    let a = WebSocketStream::from_raw_socket(a, WsRole::Client, cfg).await;
    let b = WebSocketStream::from_raw_socket(b, WsRole::Server, cfg).await;
    (a, b)
}

struct End {
    data: Data,
    events: mpsc::UnboundedReceiver<DataEvent>,
    stop: watch::Sender<Option<Closing>>,
    tokens: StreamTokens,
}

fn end(ws: Raw, side: Side, attempt: &str) -> End {
    let tokens = StreamTokens::default();
    let params = DataParams {
        side,
        attempt_id: attempt.to_owned(),
        tokens: tokens.clone(),
        write_limit: Duration::from_secs(10),
    };
    let (stop, rx) = watch::channel(None);
    let (data, events) = spawn(ws, params, rx);
    End {
        data,
        events,
        stop,
        tokens,
    }
}

/// The dialer's and the listener's data channel over one socket.
async fn pair(buffer: usize) -> (End, End) {
    let (a, b) = ws_pair(buffer).await;
    (end(a, Side::Dialer, "att"), end(b, Side::Listener, "att"))
}

async fn next(e: &mut End) -> DataEvent {
    tokio::time::timeout(T, e.events.recv())
        .await
        .expect("no data event")
        .expect("the data channel ended")
}

/// `opener` opens a stream that `server` authorized for `purpose`.
async fn open(opener: &End, server: &mut End, purpose: &str) -> u32 {
    let token = server.data.issue_token(purpose).unwrap();
    let s = opener.data.open(token.as_str()).await.unwrap();
    assert_eq!(
        next(server).await,
        DataEvent::Opened {
            stream: s,
            purpose: purpose.to_owned()
        }
    );
    s
}

/// Reads `len` bytes of `stream`, giving credit back as it goes.
async fn read(e: &mut End, stream: u32, len: usize) -> Vec<u8> {
    let mut got = Vec::new();
    while got.len() < len {
        match next(e).await {
            DataEvent::Data { stream: s, bytes } if s == stream => {
                e.data.consumed(stream, bytes.len()).await.unwrap();
                got.extend(bytes);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    got
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

async fn send_raw(raw: &mut Raw, frame: &MuxFrame) {
    raw.send(Message::binary(frame.encode())).await.unwrap();
}

async fn ended(e: &End) -> Closing {
    tokio::time::timeout(T, e.data.closed())
        .await
        .expect("the channel did not end")
}

#[tokio::test]
async fn a_mebibyte_flows_each_way_at_once_through_a_small_socket_buffer() {
    let (mut d, mut l) = pair(8 * 1024).await;
    let up = open(&d, &mut l, "artifact:up").await;
    let down = open(&l, &mut d, "artifact:down").await;
    assert_eq!((up % 2, down % 2), (1, 0), "the dialer opens odd ids");
    let (a, b) = (pattern(1 << 20, 1), pattern(1 << 20, 2));
    let w1 = {
        let (data, a) = (d.data.clone(), a.clone());
        tokio::spawn(async move { data.write(up, a).await })
    };
    let w2 = {
        let (data, b) = (l.data.clone(), b.clone());
        tokio::spawn(async move { data.write(down, b).await })
    };
    let (got_up, got_down) = tokio::join!(read(&mut l, up, 1 << 20), read(&mut d, down, 1 << 20));
    assert!(got_up == a && got_down == b);
    w1.await.unwrap().unwrap();
    w2.await.unwrap().unwrap();
    d.data.close(up).await.unwrap();
    assert_eq!(next(&mut l).await, DataEvent::Closed { stream: up });
}

#[tokio::test]
async fn a_reader_that_gives_no_credit_stops_the_writer_at_exactly_256_kib() {
    let (d, mut l) = pair(1 << 20).await;
    let s = open(&d, &mut l, "p").await;
    let w = {
        let data = d.data.clone();
        tokio::spawn(async move { data.write(s, vec![1; 300 * 1024]).await })
    };
    let mut got = 0;
    while got < WINDOW as usize {
        if let DataEvent::Data { bytes, .. } = next(&mut l).await {
            got += bytes.len();
        }
    }
    assert_eq!(got, WINDOW as usize);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(l.events.try_recv().is_err(), "nothing beyond the window");
    assert!(!w.is_finished());
    l.data.consumed(s, got).await.unwrap();
    tokio::time::timeout(T, w).await.unwrap().unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_unknown_used_foreign_or_expired_token_resets_the_stream() {
    let (mut d, mut l) = pair(1 << 20).await;
    let used = l.data.issue_token("p").unwrap();
    open_with(&d, used.as_str()).await;
    assert!(matches!(next(&mut l).await, DataEvent::Opened { .. }));
    let foreign = l.tokens.issue("another-attempt", "p").unwrap();
    let expired = l.data.issue_token("p").unwrap();
    tokio::time::advance(TOKEN_TTL).await;
    for token in [
        "0123456789abcdef0123456789abcdef",
        used.as_str(),
        foreign.as_str(),
        expired.as_str(),
    ] {
        let s = open_with(&d, token).await;
        assert_eq!(
            next(&mut d).await,
            DataEvent::Reset {
                stream: s,
                reason: "bad_stream_token".into()
            }
        );
    }
    assert!(
        l.events.try_recv().is_err(),
        "the server application saw nothing"
    );
    assert!(d.data.ended().is_none() && l.data.ended().is_none());
}

async fn open_with(e: &End, token: &str) -> u32 {
    e.data.open(token).await.unwrap()
}

#[tokio::test]
async fn a_reset_fails_the_pending_writes_of_its_stream_at_once() {
    let (mut d, mut l) = pair(1 << 20).await;
    let (s1, s2) = (open(&d, &mut l, "a").await, open(&d, &mut l, "b").await);
    let blocked = |s| {
        let data = d.data.clone();
        tokio::spawn(async move { data.write(s, vec![0; 2 * WINDOW as usize]).await })
    };
    let (w1, w2) = (blocked(s1), blocked(s2));
    let mut seen = 0;
    while seen < 2 * WINDOW as usize {
        if let DataEvent::Data { bytes, .. } = next(&mut l).await {
            seen += bytes.len();
        }
    }
    // The peer resets s1; the writer of s2 resets s2 itself.
    l.data.reset(s1, "cancelled").await.unwrap();
    let e = tokio::time::timeout(T, w1)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(&e, LinkError::Closed { reason, .. } if reason == "cancelled"),
        "{e}"
    );
    assert_eq!(
        next(&mut d).await,
        DataEvent::Reset {
            stream: s1,
            reason: "cancelled".into()
        }
    );
    d.data.reset(s2, "gone").await.unwrap();
    let e = tokio::time::timeout(T, w2)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(&e, LinkError::Closed { reason, .. } if reason == "gone"),
        "{e}"
    );
    assert_eq!(
        next(&mut l).await,
        DataEvent::Reset {
            stream: s2,
            reason: "gone".into()
        }
    );
    // Ids are not reused, and writes to a finished stream fail.
    assert!(d.data.write(s1, vec![1]).await.is_err());
    assert_eq!(open(&d, &mut l, "c").await, s2 + 2);
}

/// One data channel (listener side) against a raw peer.
async fn raw_peer() -> (End, Raw) {
    let (a, b) = ws_pair(1 << 20).await;
    (end(a, Side::Listener, "att"), b)
}

#[tokio::test]
async fn peer_violations_end_the_channel_with_their_reason() {
    let empty = |s| {
        let mut f = MuxFrame::Close { stream: s }.encode();
        f[0] = 2;
        f
    };
    // (setup, violating frame, reason)
    for case in 0..4 {
        let (mut l, mut raw) = raw_peer().await;
        let token = l.data.issue_token("p").unwrap();
        let open = MuxFrame::Open {
            stream: 1,
            token: token.as_str().to_owned(),
        };
        send_raw(&mut raw, &open).await;
        assert!(matches!(next(&mut l).await, DataEvent::Opened { .. }));
        let want = match case {
            0 => {
                raw.send(Message::binary(empty(1))).await.unwrap();
                "protocol_error"
            }
            1 => {
                send_raw(
                    &mut raw,
                    &MuxFrame::Data {
                        stream: 9,
                        bytes: vec![1],
                    },
                )
                .await;
                "protocol_error"
            }
            2 => {
                for _ in 0..5 {
                    send_raw(
                        &mut raw,
                        &MuxFrame::Data {
                            stream: 1,
                            bytes: vec![0; 64 * 1024],
                        },
                    )
                    .await;
                }
                "flow_control_violation"
            }
            _ => {
                for i in 1..MAX_STREAMS as u32 {
                    let t = l.data.issue_token("p").unwrap();
                    let open = MuxFrame::Open {
                        stream: 2 * i + 1,
                        token: t.as_str().to_owned(),
                    };
                    send_raw(&mut raw, &open).await;
                }
                let t = l.data.issue_token("p").unwrap();
                send_raw(
                    &mut raw,
                    &MuxFrame::Open {
                        stream: 999,
                        token: t.as_str().to_owned(),
                    },
                )
                .await;
                "flow_control_violation"
            }
        };
        let c = ended(&l).await;
        assert_eq!(c.reason, want, "case {case}: {}", c.detail);
        // The queue ends after the delivered events.
        while let Ok(Some(_)) = tokio::time::timeout(T, l.events.recv()).await {}
    }
}

#[tokio::test]
async fn a_stop_ends_the_channel_with_its_reason_at_once_even_with_a_blocked_socket() {
    // The peer reads nothing: the socket buffer fills and the writer blocks.
    let (a, _b) = ws_pair(4 * 1024).await;
    let mut d = end(a, Side::Dialer, "att");
    let s = d.data.open("t").await.unwrap();
    let w = {
        let data = d.data.clone();
        tokio::spawn(async move { data.write(s, vec![0; 2 * WINDOW as usize]).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let start = tokio::time::Instant::now();
    d.stop.send_replace(Some(Closing {
        reason: "superseded".into(),
        detail: "a newer link".into(),
    }));
    let c = ended(&d).await;
    assert_eq!(c.reason, "superseded");
    assert!(
        start.elapsed() < Duration::from_millis(1500),
        "{:?}",
        start.elapsed()
    );
    let e = w.await.unwrap().unwrap_err();
    assert!(
        matches!(&e, LinkError::Closed { reason, .. } if reason == "superseded"),
        "{e}"
    );
    assert!(d.events.recv().await.is_none());
    let e = d.data.open("t").await.unwrap_err();
    assert!(
        matches!(&e, LinkError::Closed { reason, .. } if reason == "superseded"),
        "{e}"
    );
}

#[tokio::test]
async fn the_end_of_one_side_ends_the_other_with_eof() {
    let (d, l) = pair(1 << 20).await;
    d.stop.send_replace(Some(Closing {
        reason: "shutdown".into(),
        detail: String::new(),
    }));
    assert_eq!(ended(&d).await.reason, "shutdown");
    assert_eq!(ended(&l).await.reason, "eof");
}
