//! The watch task of the MCP bridge against a fake station socket (MCP P4b
//! §8.3, §11 test 4).
//!
//! The tests with a socket run in real time: tokio paused clock advances
//! whenever the runtime is idle, also while a real socket delivers its
//! bytes, so it fires the timeouts of the bridge too early. The backoff
//! schedule is tested without a socket, under the paused clock.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};
use tokio::net::{UnixListener, UnixStream};
use tokio::time::Instant;

use super::*;
use crate::coordinator::bridge::relay;

const TOKEN: &str = "fdc1_fake_token";
const NOTE: &str = LIST_CHANGED_LINE;

/// What the fake station does after it wrote its lines to a watch.
#[derive(Clone, Copy)]
enum Then {
    /// Keep the connection until the peer closes.
    Hold,
    /// Close at once.
    Close,
}

/// What the fake station does with one `WATCH` connection.
struct Step {
    bytes: Vec<u8>,
    then: Then,
}

fn step(text: &str, then: Then) -> Step {
    Step {
        bytes: text.as_bytes().to_vec(),
        then,
    }
}

/// An established watch: `OK watch.v1` and the refresh, then held.
fn ok() -> Step {
    step(&format!("OK watch.v1\n{NOTE}\n"), Then::Hold)
}

/// A fake station socket: it answers `AUTH` messages and follows a script for
/// the `WATCH` connections. After the script it answers `ERR unauthorized`,
/// which stops the bridge watch.
struct Station {
    path: PathBuf,
    /// The first line and the arrival time of every `WATCH` connection.
    watches: Arc<StdMutex<Vec<(Instant, String)>>>,
    /// The watch connections that the bridge closed (or that the station
    /// closed after a time).
    ended: Arc<AtomicUsize>,
    /// The `server/discover` answer is a result (true) or an error.
    discover_ok: Arc<AtomicBool>,
    _dir: tempfile::TempDir,
}

impl Station {
    async fn start(script: Vec<Step>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.s");
        let listener = UnixListener::bind(&path).unwrap();
        let station = Self {
            path,
            watches: Arc::default(),
            ended: Arc::default(),
            discover_ok: Arc::new(AtomicBool::new(true)),
            _dir: dir,
        };
        let script = Arc::new(StdMutex::new(VecDeque::from(script)));
        let (watches, ended, discover_ok) = (
            station.watches.clone(),
            station.ended.clone(),
            station.discover_ok.clone(),
        );
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve(
                    stream,
                    script.clone(),
                    watches.clone(),
                    ended.clone(),
                    discover_ok.clone(),
                ));
            }
        });
        station
    }

    fn count(&self) -> usize {
        self.watches.lock().unwrap().len()
    }

    /// Waits until `n` watch connections arrived.
    async fn arrived(&self, n: usize) {
        for _ in 0..10_000 {
            if self.count() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{n} watch connections did not arrive; got {}", self.count());
    }

    /// The time between the arrival of connection `i` and of `i + 1`.
    fn gap(&self, i: usize) -> Duration {
        let watches = self.watches.lock().unwrap();
        watches[i + 1].0 - watches[i].0
    }

    fn first_lines(&self) -> Vec<String> {
        self.watches
            .lock()
            .unwrap()
            .iter()
            .map(|(_, line)| line.clone())
            .collect()
    }
}

async fn serve(
    stream: UnixStream,
    script: Arc<StdMutex<VecDeque<Step>>>,
    watches: Arc<StdMutex<Vec<(Instant, String)>>>,
    ended: Arc<AtomicUsize>,
    discover_ok: Arc<AtomicBool>,
) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut first = String::new();
    if reader.read_line(&mut first).await.unwrap_or(0) == 0 {
        return;
    }
    let first = first.trim_end().to_owned();
    if first.starts_with("AUTH ") {
        let mut message = String::new();
        let _ = reader.read_line(&mut message).await;
        let message: Value = serde_json::from_str(message.trim_end()).unwrap_or(Value::Null);
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let answer =
            if message["method"] == "server/discover" && !discover_ok.load(Ordering::SeqCst) {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": "no"}})
            } else {
                json!({"jsonrpc": "2.0", "id": id, "result": {}})
            };
        if message.get("id").is_some() {
            let _ = write.write_all(format!("{answer}\n").as_bytes()).await;
        }
        return;
    }
    watches.lock().unwrap().push((Instant::now(), first));
    let next = script.lock().unwrap().pop_front();
    let Step { bytes, then } = next.unwrap_or_else(|| step("ERR unauthorized\n", Then::Close));
    if write.write_all(&bytes).await.is_err() {
        return;
    }
    match then {
        Then::Close => {}
        Then::Hold => {
            let mut sink = [0u8; 64];
            while reader.read(&mut sink).await.unwrap_or(0) > 0 {}
            ended.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// A relay on a duplex stdin and stdout.
struct Bridge {
    stdin: DuplexStream,
    lines: Lines<BufReader<DuplexStream>>,
    done: tokio::task::JoinHandle<()>,
}

impl Bridge {
    fn start(path: &std::path::Path) -> Self {
        let (stdin, stdin_r) = tokio::io::duplex(64 * 1024);
        let (stdout_w, stdout_r) = tokio::io::duplex(64 * 1024);
        let path = path.to_owned();
        let done = tokio::spawn(async move {
            relay(&path, TOKEN, stdin_r, stdout_w).await;
        });
        Self {
            stdin,
            lines: BufReader::new(stdout_r).lines(),
            done,
        }
    }

    async fn send(&mut self, message: &Value) {
        self.stdin
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
    }

    /// The next stdout line.
    async fn line(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(60), self.lines.next_line())
            .await
            .expect("a line in time")
            .unwrap()
            .expect("stdout open")
    }

    /// True when no stdout line arrives in `within`.
    async fn quiet(&mut self, within: Duration) -> bool {
        tokio::time::timeout(within, self.lines.next_line())
            .await
            .is_err()
    }

    /// Ends stdin, waits for the relay and returns the rest of stdout.
    async fn finish(mut self) -> Vec<String> {
        drop(self.stdin);
        tokio::time::timeout(Duration::from_secs(60), self.done)
            .await
            .expect("the relay ended")
            .unwrap();
        let mut rest = Vec::new();
        while let Some(line) = self.lines.next_line().await.unwrap() {
            rest.push(line);
        }
        rest
    }
}

fn discover(id: i64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "server/discover"})
}

fn initialize(id: i64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "initialize",
           "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                      "clientInfo": {"name": "t", "version": "1"}}})
}

fn tools_list(id: i64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/list"})
}

/// Sends `server/discover` and reads its answer line.
async fn discovered(bridge: &mut Bridge) {
    bridge.send(&discover(1)).await;
    let answer: Value = serde_json::from_str(&bridge.line().await).unwrap();
    assert!(answer.get("result").is_some(), "{answer}");
}

/// The longest wait of the first retry: 1 s plus the jitter of 25 %, and room
/// for a slow machine.
const FIRST_RETRY_MAX: Duration = Duration::from_millis(1250 + 750);

/// True when `gap` is a first backoff: 1 s to 1.25 s (and a margin).
fn is_first_backoff(gap: Duration) -> bool {
    gap >= Duration::from_secs(1) && gap <= FIRST_RETRY_MAX
}

#[tokio::test]
async fn the_bridge_starts_its_watch_after_discovery_and_reconnects_with_backoff() {
    // No `WATCH` before the first discovery answer: not for another request,
    // not for a `server/discover` that got an error.
    let station = Station::start(vec![ok()]).await;
    let mut bridge = Bridge::start(&station.path);
    bridge.send(&tools_list(1)).await;
    assert!(bridge.line().await.contains("\"result\""));
    station.discover_ok.store(false, Ordering::SeqCst);
    bridge.send(&discover(2)).await;
    assert!(bridge.line().await.contains("\"error\""));
    assert!(bridge.quiet(Duration::from_millis(500)).await);
    assert_eq!(station.count(), 0, "no watch yet");

    // The first successful `server/discover` starts it, once. The station's
    // first line after `OK` is written to stdout; the bridge adds none.
    station.discover_ok.store(true, Ordering::SeqCst);
    discovered(&mut bridge).await;
    assert_eq!(bridge.line().await, NOTE);
    assert_eq!(station.first_lines(), [format!("WATCH {TOKEN}")]);
    discovered(&mut bridge).await;
    assert!(bridge.quiet(Duration::from_millis(500)).await);
    assert_eq!(station.count(), 1, "started once");
    assert!(bridge.finish().await.is_empty());

    // `initialize` starts it as well.
    let station = Station::start(vec![ok()]).await;
    let mut bridge = Bridge::start(&station.path);
    bridge.send(&initialize(1)).await;
    assert!(bridge.line().await.contains("\"result\""));
    assert_eq!(bridge.line().await, NOTE);
    assert_eq!(station.count(), 1);
    drop(bridge);

    // A dropped established watch reconnects after the first backoff.
    let station = Station::start(vec![
        step(&format!("OK watch.v1\n{NOTE}\n"), Then::Close),
        ok(),
    ])
    .await;
    let mut bridge = Bridge::start(&station.path);
    discovered(&mut bridge).await;
    assert_eq!(bridge.line().await, NOTE);
    assert_eq!(bridge.line().await, NOTE);
    assert_eq!(station.count(), 2);
    assert!(is_first_backoff(station.gap(0)), "{:?}", station.gap(0));
    drop(bridge);
}

#[tokio::test]
async fn the_station_lines_are_coalesced_while_stdout_is_busy() {
    // Fifty lines in one write while another writer holds stdout: at most one
    // notification is pending, so at most two lines reach stdout (the one
    // that the writer took and the one that came after).
    let burst = format!("OK watch.v1\n{}", format!("{NOTE}\n").repeat(50));
    let station = Station::start(vec![step(&burst, Then::Hold)]).await;
    let (stdout_w, mut stdout_r) = tokio::io::duplex(64 * 1024);
    let out = Arc::new(tokio::sync::Mutex::new(stdout_w));
    let busy = out.clone().lock_owned().await;
    let socket: Arc<std::path::Path> = Arc::from(station.path.as_path());
    let token: Arc<str> = Arc::from(TOKEN);
    let task = tokio::spawn(run(socket, token, out.clone()));
    station.arrived(1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(busy);
    tokio::time::sleep(Duration::from_millis(300)).await;
    task.abort();
    drop(out);
    let mut text = String::new();
    stdout_r.read_to_string(&mut text).await.unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        (1..=2).contains(&lines.len()),
        "coalesced to at most two lines: {lines:?}"
    );
    assert!(lines.iter().all(|line| *line == NOTE));
}

/// A bridge on a station whose first watch line is `first`: it connects once
/// and, after more than the first backoff, has not connected again.
async fn stops_for_good(first: &str) {
    let station = Station::start(vec![step(first, Then::Close)]).await;
    let mut bridge = Bridge::start(&station.path);
    discovered(&mut bridge).await;
    assert!(
        bridge
            .quiet(FIRST_RETRY_MAX + Duration::from_millis(250))
            .await,
        "no line after {first:?}"
    );
    assert_eq!(station.count(), 1, "no retry after {first:?}");
    assert!(bridge.finish().await.is_empty());
}

#[tokio::test]
async fn an_unauthorized_a_bad_request_and_an_unknown_first_line_stop_it_for_good() {
    let oversize = format!("{}\n", "x".repeat(300));
    tokio::join!(
        stops_for_good("ERR unauthorized\n"),
        stops_for_good("ERR bad_request\n"),
        stops_for_good("hello\n"),
        stops_for_good("ERR something_new\n"),
        stops_for_good(&oversize),
    );
}

/// A bridge on a station that follows `script`; the gap between its first
/// two watch connections.
async fn first_gap(script: Vec<Step>) -> Duration {
    let station = Station::start(script).await;
    let mut bridge = Bridge::start(&station.path);
    discovered(&mut bridge).await;
    assert_eq!(bridge.line().await, NOTE);
    assert_eq!(station.count(), 2);
    let gap = station.gap(0);
    drop(bridge);
    gap
}

#[tokio::test]
async fn starting_limited_and_a_lost_first_line_retry_after_the_first_backoff() {
    let (starting, limited, silent, partial) = tokio::join!(
        first_gap(vec![step("ERR starting\n", Then::Close), ok()]),
        first_gap(vec![step("ERR limited\n", Then::Close), ok()]),
        // The station closes before any line.
        first_gap(vec![step("", Then::Close), ok()]),
        // The station closes inside the first line.
        first_gap(vec![step("OK wat", Then::Close), ok()]),
    );
    for gap in [starting, limited, silent, partial] {
        assert!(is_first_backoff(gap), "{gap:?}");
    }
}

/// A station whose first watch sends `bad` after `OK watch.v1`: the bridge
/// closes it and connects again. The gap between the two connections.
async fn closes_on(bad: String) -> Duration {
    let station = Station::start(vec![step(&bad, Then::Hold), ok()]).await;
    let mut bridge = Bridge::start(&station.path);
    discovered(&mut bridge).await;
    // Only the notification of the good watch reaches stdout.
    assert_eq!(bridge.line().await, NOTE);
    assert_eq!(station.count(), 2);
    assert_eq!(
        station.ended.load(Ordering::SeqCst),
        1,
        "closed by the bridge"
    );
    station.gap(0)
}

#[tokio::test]
async fn a_foreign_or_an_oversize_line_closes_the_watch_and_it_retries() {
    let foreign = "OK watch.v1\n{\"jsonrpc\":\"2.0\",\"method\":\"other\"}\n".to_owned();
    let oversize = format!("OK watch.v1\n{}\n", "x".repeat(300));
    let (a, b) = tokio::join!(closes_on(foreign), closes_on(oversize));
    assert!(is_first_backoff(a), "{a:?}");
    assert!(is_first_backoff(b), "{b:?}");
}

#[tokio::test]
async fn a_missing_socket_keeps_the_bridge_answering() {
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = Bridge::start(&dir.path().join("none.s"));
    bridge.send(&discover(1)).await;
    let answer: Value = serde_json::from_str(&bridge.line().await).unwrap();
    assert_eq!(answer["error"]["code"], -32000, "{answer}");
    // The error answer is not a discovery: no watch task ran.
    assert!(bridge.quiet(Duration::from_millis(300)).await);
    assert!(bridge.finish().await.is_empty());
}

#[tokio::test]
async fn the_end_of_stdin_ends_the_watch() {
    let station = Station::start(vec![ok()]).await;
    let mut bridge = Bridge::start(&station.path);
    discovered(&mut bridge).await;
    assert_eq!(bridge.line().await, NOTE);
    assert_eq!(station.ended.load(Ordering::SeqCst), 0);
    // The relay returns at once (it does not wait for the watch) and the
    // watch connection closes.
    assert!(bridge.finish().await.is_empty());
    for _ in 0..100 {
        if station.ended.load(Ordering::SeqCst) == 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the watch connection stayed open after the end of stdin");
}

#[test]
fn the_backoff_doubles_to_30_seconds_and_a_long_connection_resets_it() {
    let secs = Duration::from_secs;
    let mut backoff = Backoff::default();
    let waits: Vec<_> = (0..7).map(|_| backoff.after(Attempt::Failed)).collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30].map(secs));

    // A short connection does not reset it; one of 30 s does.
    assert_eq!(backoff.after(Attempt::Ended(secs(29))), secs(30));
    assert_eq!(backoff.after(Attempt::Ended(secs(30))), secs(1));
    assert_eq!(backoff.after(Attempt::Failed), secs(2));
    assert_eq!(backoff.after(Attempt::Ended(secs(1))), secs(4));
}

#[tokio::test(start_paused = true)]
async fn the_jitter_is_between_zero_and_a_quarter_and_the_wait_runs_on_the_clock() {
    for base in [1u64, 2, 4, 8, 16, 30] {
        let base = Duration::from_secs(base);
        for _ in 0..200 {
            let waited = with_jitter(base);
            assert!(waited >= base && waited <= base + base / 4, "{waited:?}");
        }
    }
    // A backoff sleep of 30 s plus jitter ends at once under the paused
    // clock and takes the clock forward by that time.
    let started = Instant::now();
    tokio::time::sleep(with_jitter(BACKOFF_MAX)).await;
    let elapsed = started.elapsed();
    assert!(elapsed >= BACKOFF_MAX && elapsed <= BACKOFF_MAX + BACKOFF_MAX / 4);
}
