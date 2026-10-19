//! The MCP bridge client, `fleetdeck mcp-bridge --socket <path>` (DL-194
//! §2.2, §9.3).
//!
//! A stdio MCP server for a shim-hosted provider: it reads one JSON-RPC
//! message per line from stdin, relays each to the station socket
//! (`AUTH <token>` line, then the message line; one connection per
//! message) and writes the answer as one line to stdout. Stdout is the MCP
//! channel: nothing else is written there. The token comes from
//! [`TOKEN_ENV`] and goes only to the socket. When the station is down,
//! the bridge answers a JSON-RPC error and keeps running. Wire format
//! version 1: an older bridge and a newer station talk, and the reverse.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_providers::coordinator::TOKEN_ENV;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;

use super::ingress::Limits;
use super::socket::{Line, discard_line, read_bounded};

/// Messages relayed at once.
const IN_FLIGHT: usize = 8;
/// The wait for running calls after the end of stdin.
const DRAIN: Duration = Duration::from_secs(5);

const RESTARTING: &str = "Fleet Deck is restarting; try again in a moment.";
const STARTING: &str = "Fleet Deck is starting; try again in a moment.";
const UNAUTHORIZED: &str = "The session token is not valid.";
const LIMITED: &str = "Too many calls; try again in a moment.";
const INVALID: &str = "Invalid request.";
const PARSE: &str = "Parse error.";

/// Runs the bridge on stdin and stdout until stdin ends; the exit code.
/// Exit 2: no session token in the environment.
#[must_use]
pub fn run(socket: &Path) -> i32 {
    let token = match std::env::var(TOKEN_ENV) {
        Ok(token) if !token.is_empty() => token,
        _ => {
            eprintln!("fleetdeck mcp-bridge: no session token");
            return 2;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("fleetdeck mcp-bridge: cannot start: {error}");
            return 1;
        }
    };
    runtime.block_on(relay(
        socket,
        &token,
        tokio::io::stdin(),
        tokio::io::stdout(),
    ));
    0
}

/// Relays the messages of `input` to the station socket and writes the
/// answers to `output`, one line each, until `input` ends.
pub async fn relay<R, W>(socket: &Path, token: &str, input: R, output: W)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let out = Arc::new(Mutex::new(output));
    let socket: Arc<Path> = Arc::from(socket);
    let token: Arc<str> = Arc::from(token);
    let slots = Arc::new(Semaphore::new(IN_FLIGHT));
    let mut tasks = JoinSet::new();
    let mut reader = BufReader::new(input);
    let line_max = Limits::default().body_bytes;
    loop {
        while let Some(done) = tasks.try_join_next() {
            if let Err(error) = done {
                eprintln!("fleetdeck mcp-bridge: a call failed: {error}");
            }
        }
        let line = match read_bounded(&mut reader, line_max).await {
            Ok(Line::Text(text) | Line::Partial(text)) => text,
            Ok(Line::Eof) => break,
            Ok(Line::TooLong) => {
                eprintln!("fleetdeck mcp-bridge: input line too long");
                if let Err(error) = discard_line(&mut reader).await {
                    eprintln!("fleetdeck mcp-bridge: stdin: {error}");
                    break;
                }
                continue;
            }
            Err(error) => {
                eprintln!("fleetdeck mcp-bridge: stdin: {error}");
                break;
            }
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&line) else {
            let answer = error_line(&Value::Null, -32700, PARSE);
            emit(&out, answer.as_bytes()).await;
            continue;
        };
        let Ok(slot) = slots.clone().acquire_owned().await else {
            break;
        };
        let (out, socket, token) = (out.clone(), socket.clone(), token.clone());
        tasks.spawn(async move {
            let _slot = slot;
            if let Some(answer) = answer_for(&socket, &token, &line, &msg).await {
                emit(&out, &answer).await;
            }
        });
    }
    let drained = tokio::time::timeout(DRAIN, async {
        while let Some(done) = tasks.join_next().await {
            if let Err(error) = done {
                eprintln!("fleetdeck mcp-bridge: a call failed: {error}");
            }
        }
    })
    .await;
    if drained.is_err() {
        eprintln!("fleetdeck mcp-bridge: calls still running at the end of input were dropped");
    }
}

/// The line to write for one relayed message, or `None` for no answer.
async fn answer_for(socket: &Path, token: &str, line: &[u8], msg: &Value) -> Option<Vec<u8>> {
    let Some(reply) = exchange(socket, token, line).await else {
        return error_answer(msg, -32000, RESTARTING);
    };
    let Some(code) = reply.strip_prefix(b"ERR ") else {
        return Some(reply);
    };
    match code {
        b"unauthorized" => error_answer(msg, -32001, UNAUTHORIZED),
        b"limited" => error_answer(msg, -32000, LIMITED),
        b"starting" => error_answer(msg, -32000, STARTING),
        // `bad_request` and any later code: a message that is not an
        // object (a batch) gets an answer with a null id.
        _ if msg.is_object() => error_answer(msg, -32600, INVALID),
        _ => Some(error_line(&Value::Null, -32600, INVALID).into_bytes()),
    }
}

/// One connection: the message out, one line back. `None` for a connect
/// or I/O error, or a close without a line.
async fn exchange(socket: &Path, token: &str, line: &[u8]) -> Option<Vec<u8>> {
    let stream = match UnixStream::connect(socket).await {
        Ok(stream) => stream,
        Err(error) => {
            tracing::debug!(kind = ?error.kind(), "the bridge could not connect");
            return None;
        }
    };
    let (read, mut write) = stream.into_split();
    let mut request = Vec::with_capacity(token.len() + line.len() + 8);
    request.extend_from_slice(b"AUTH ");
    request.extend_from_slice(token.as_bytes());
    request.push(b'\n');
    request.extend_from_slice(line);
    request.push(b'\n');
    if let Err(error) = write.write_all(&request).await {
        tracing::debug!(kind = ?error.kind(), "the bridge could not write");
        return None;
    }
    // The station is a process of this user; its answer has no bound.
    let mut answer = Vec::new();
    match BufReader::new(read).read_until(b'\n', &mut answer).await {
        Ok(_) if answer.pop() == Some(b'\n') => Some(answer),
        Ok(_) => None,
        Err(error) => {
            tracing::debug!(kind = ?error.kind(), "the bridge could not read");
            None
        }
    }
}

fn error_line(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

/// A JSON-RPC error for `msg`; none for a message without an id (a
/// notification never gets an answer line).
fn error_answer(msg: &Value, code: i64, message: &str) -> Option<Vec<u8>> {
    let id = msg.as_object()?.get("id")?;
    Some(error_line(id, code, message).into_bytes())
}

/// Writes one line to the output under the lock, so lines never mix.
async fn emit<W: AsyncWrite + Unpin>(out: &Mutex<W>, line: &[u8]) {
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line);
    bytes.push(b'\n');
    let mut out = out.lock().await;
    let sent = async {
        out.write_all(&bytes).await?;
        out.flush().await
    };
    if let Err(error) = sent.await {
        eprintln!("fleetdeck mcp-bridge: stdout: {error}");
    }
}

#[cfg(test)]
#[path = "bridge_tests.rs"]
mod tests;
