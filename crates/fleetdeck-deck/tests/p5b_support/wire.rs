//! A raw HTTP/1.1 client of the board that keeps the bytes of a body (the
//! shared client reads a body as text, and an MP4 is not text), sends HEAD,
//! and can read a body in two steps, so that a test can change the world
//! while a body is in flight.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpSocket;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;

use crate::common::board::BoardClient;

/// How long one read of a response may wait.
const WAIT: Duration = Duration::from_secs(60);

/// One answer: the status, the headers and the body bytes.
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body as JSON; `Null` when it is not JSON.
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The text of the body, for a failure message.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body[..self.body.len().min(300)]).into_owned()
    }

    /// The `error` code of a JSON error body.
    pub fn code(&self) -> String {
        self.json()["error"].as_str().unwrap_or("").to_owned()
    }

    pub fn length(&self) -> usize {
        self.header("content-length").unwrap().parse().unwrap()
    }
}

/// The headers of a request.
pub type Pairs = Vec<(String, String)>;

/// A client of one board: its address and the TLS client that trusts it.
#[derive(Clone)]
pub struct Client {
    addr: SocketAddr,
    tls: TlsConnector,
}

/// An answer whose body is read in steps.
pub struct Partial {
    stream: TlsStream<tokio::net::TcpStream>,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    chunked: bool,
    /// The body bytes read so far, as they came.
    raw: Vec<u8>,
}

impl Client {
    pub fn of(board: &BoardClient) -> Self {
        Self {
            addr: board.addr,
            tls: board.tls().clone(),
        }
    }

    /// One request and its whole answer.
    pub async fn send(
        &self,
        method: &str,
        path: &str,
        headers: &Pairs,
        body: Option<Value>,
    ) -> Reply {
        self.open(method, path, headers, body, false)
            .await
            .finish()
            .await
    }

    pub async fn get(&self, path: &str, headers: &Pairs) -> Reply {
        self.send("GET", path, headers, None).await
    }

    pub async fn post(&self, path: &str, headers: &Pairs, body: Value) -> Reply {
        self.send("POST", path, headers, Some(body)).await
    }

    /// Sends a request and reads the headers. `small`: the socket has a
    /// small receive buffer, so that a server that sends a large body waits
    /// for the reader.
    pub async fn open(
        &self,
        method: &str,
        path: &str,
        headers: &Pairs,
        body: Option<Value>,
        small: bool,
    ) -> Partial {
        let socket = TcpSocket::new_v4().unwrap();
        if small {
            socket.set_recv_buffer_size(65_536).unwrap();
        }
        let tcp = socket.connect(self.addr).await.unwrap();
        let name = ServerName::try_from("127.0.0.1").unwrap();
        let mut stream = self.tls.connect(name, tcp).await.unwrap();
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let mut text = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n",
            self.addr,
            body.len()
        );
        if !body.is_empty() {
            text.push_str("Content-Type: application/json\r\n");
        }
        for (k, v) in headers {
            text.push_str(&format!("{k}: {v}\r\n"));
        }
        text.push_str("\r\n");
        text.push_str(&body);
        stream.write_all(text.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        let mut raw = Vec::new();
        let at = loop {
            if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break at;
            }
            let mut buf = [0u8; 4096];
            let n = tokio::time::timeout(WAIT, stream.read(&mut buf))
                .await
                .expect("response headers")
                .unwrap_or(0);
            assert!(n > 0, "the answer ended before its headers");
            raw.extend_from_slice(&buf[..n]);
        };
        let head = String::from_utf8_lossy(&raw[..at]).into_owned();
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|l| l.split(' ').nth(1))
            .and_then(|s| s.parse().ok())
            .expect("a status line");
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(": "))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let chunked = headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked"));
        let head_only = method == "HEAD";
        let mut partial = Partial {
            stream,
            status,
            headers,
            chunked,
            raw: raw[at + 4..].to_vec(),
        };
        if head_only {
            partial.raw.clear();
        }
        partial
    }
}

impl Partial {
    /// Reads until at least `n` body bytes are here, or the body ends.
    /// Returns how many body bytes are here.
    pub async fn read_at_least(&mut self, n: usize) -> usize {
        while self.raw.len() < n {
            let mut buf = vec![0u8; 16_384];
            let read = tokio::time::timeout(WAIT, self.stream.read(&mut buf))
                .await
                .expect("a body read")
                .unwrap_or(0);
            if read == 0 {
                break;
            }
            self.raw.extend_from_slice(&buf[..read]);
        }
        self.raw.len()
    }

    /// Reads to the end of the stream. A stream that a server cut is the end
    /// of the body too: the caller compares the bytes with `Content-Length`.
    pub async fn finish(mut self) -> Reply {
        loop {
            let mut buf = vec![0u8; 65_536];
            let read = tokio::time::timeout(WAIT, self.stream.read(&mut buf))
                .await
                .expect("the end of a body")
                .unwrap_or(0);
            if read == 0 {
                break;
            }
            self.raw.extend_from_slice(&buf[..read]);
        }
        let body = if self.chunked {
            dechunk(&self.raw)
        } else {
            self.raw
        };
        Reply {
            status: self.status,
            headers: self.headers,
            body,
        }
    }
}

fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(at) = raw.windows(2).position(|w| w == b"\r\n") {
        let Ok(size) = usize::from_str_radix(String::from_utf8_lossy(&raw[..at]).trim(), 16) else {
            break;
        };
        raw = &raw[at + 2..];
        if size == 0 || raw.len() < size {
            break;
        }
        out.extend_from_slice(&raw[..size]);
        raw = &raw[(size + 2).min(raw.len())..];
    }
    out
}
