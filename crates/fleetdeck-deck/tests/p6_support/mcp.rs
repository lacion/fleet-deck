//! The MCP client of the P6 proofs: a `tools/call` JSON-RPC request to the
//! loopback coordinator endpoint of the caller station, with the credential
//! of the bound session, exactly as an agent sends it. Also the PNG fixtures
//! and a recorder of the `tracing` events of the process.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The result of one `tools/call`.
#[derive(Debug, Clone)]
pub struct Tool {
    pub status: u16,
    pub is_error: bool,
    pub content: Vec<Value>,
    /// The serialized size of the `result` object.
    pub bytes: usize,
    /// The whole body of the answer.
    pub raw: String,
}

impl Tool {
    /// The first text block, parsed as JSON (`Null` when it is not JSON).
    pub fn json(&self) -> Value {
        let text = self.content[0]["text"].as_str().unwrap_or_default();
        serde_json::from_str(text).unwrap_or(Value::Null)
    }

    /// The `error` code of an error result.
    pub fn code(&self) -> String {
        assert!(self.is_error, "not an error: {}", self.raw_short());
        self.json()["error"].as_str().unwrap_or_default().to_owned()
    }

    /// An answer of success: the JSON of its first text block.
    pub fn ok(&self) -> Value {
        assert!(
            !self.is_error && self.status == 200,
            "an error: {}",
            self.raw_short()
        );
        self.json()
    }

    /// The text of every block, but the base64 of an image.
    pub fn texts(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn raw_short(&self) -> String {
        self.raw.chars().take(600).collect()
    }
}

/// A client of one endpoint.
#[derive(Debug, Clone)]
pub struct Mcp {
    pub url: String,
    pub token: String,
}

impl Mcp {
    pub async fn call(&self, name: &str, arguments: Value) -> Tool {
        let body = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}});
        let (status, raw) = post(&self.url, Some(&self.token), &body.to_string()).await;
        let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let result = v.get("result").cloned().unwrap_or(Value::Null);
        Tool {
            status,
            is_error: result["isError"].as_bool().unwrap_or(true),
            content: result["content"].as_array().cloned().unwrap_or_default(),
            bytes: result.to_string().len(),
            raw,
        }
    }

    /// `tools/list`: the tool names.
    pub async fn names(&self) -> Vec<String> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        let (_, raw) = post(&self.url, Some(&self.token), &body.to_string()).await;
        let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        v["result"]["tools"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// One HTTP POST to the endpoint; the status and the body.
pub async fn post(url: &str, token: Option<&str>, body: &str) -> (u16, String) {
    let addr: SocketAddr = url
        .trim_start_matches("http://")
        .trim_end_matches("/mcp")
        .parse()
        .unwrap();
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        addr.port(),
        body.len()
    );
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(15), s.read_to_end(&mut buf)).await;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, body.to_owned())
}

// ------------------------------------------------------------------- PNG

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// A bitwise CRC-32, independent of the code under test.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                0xedb8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
    out
}

/// A valid PNG (signature, IHDR 640x480, one IDAT with `filler`, IEND)
/// that is exactly `size` bytes long. `filler` repeats to fill the IDAT.
pub fn png_of(size: usize, filler: &[u8]) -> Vec<u8> {
    let mut ihdr = 640_u32.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&480_u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    // The signature, the IHDR chunk (25), the IDAT frame (12), the IEND (12).
    let data_len = size - (8 + 25 + 12 + 12);
    let data: Vec<u8> = filler.iter().copied().cycle().take(data_len).collect();
    let mut out = SIGNATURE.to_vec();
    out.extend(chunk(b"IHDR", &ihdr));
    out.extend(chunk(b"IDAT", &data));
    out.extend(chunk(b"IEND", &[]));
    assert_eq!(out.len(), size);
    out
}

// ------------------------------------------------------------------ logs

fn lines() -> &'static Mutex<Vec<String>> {
    static LINES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    LINES.get_or_init(Mutex::default)
}

struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!(" {}={value:?}", field.name()));
    }
}

/// A subscriber that keeps the events of the process as text lines.
struct Recorder;

impl tracing::Subscriber for Recorder {
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        // The production subscriber denies the MCP SDK targets
        // (fleetdeck_core::logging::subscriber); the recorder mirrors it.
        !fleetdeck_core::logging::is_sdk_target(meta.target())
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields(event.metadata().target().to_owned());
        event.record(&mut fields);
        lines()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(fields.0);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// Starts the recorder (the first call of the process wins).
pub fn record_logs() {
    let _ = tracing::subscriber::set_global_default(Recorder);
}

/// Every line that the recorder kept.
pub fn logged() -> String {
    lines()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .join("\n")
}
