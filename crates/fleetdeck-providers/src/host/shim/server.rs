//! The shim process (ADR-005 §2–§4): `fleetdeck shim --id <shim_id>`.
//!
//! It runs on a current-thread runtime: Linux sends the provider's
//! parent-death signal when the parent *thread* ends, so the thread that
//! spawned the provider must live as long as the shim.
//!
//! Independent tasks (§4.4): the socket acceptor and one reader per
//! connection, one frame writer per connection, the stdout pump (stops at
//! the buffer size, §4.2), the stdout streamer, the stderr tail, the stdin
//! writer (one active write, §4.3) and the exit waiter. A blocked stdin
//! write never blocks a control frame, a takeover or a status answer.

use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixListener;
use tokio::net::unix::OwnedWriteHalf;
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Notify, mpsc, watch};

use super::identity::identity_of;
use super::journal::{LaunchLock, Record, ShimFiles, check_dir, prepare_dir};
use super::launch::{LaunchReply, LaunchSpec};
use super::output::OutputBuffer;
use super::proof::{AttachDecision, AttachKey, decide_attach, nonce, peer_allowed};
use super::wire::{
    ExitState, FromShim, GapReason, PROTOCOLS, ProviderIdentity, Refusal, StatusAnswer, ToShim,
    WriteState, hex, pick_protocol, read_frame, write_frame,
};
use super::writes::{Admission, WriteTable, write_tracked};
use crate::host::STDERR_TAIL_BYTES;

#[path = "server_conn.rs"]
mod conn;
#[path = "server_proc.rs"]
mod process;

pub use process::exec_provider;
use process::{bind, group_members, reap_orphans, signal_group, spawn_provider};

/// The largest `output` frame payload.
const OUTPUT_CHUNK: usize = 64 * 1024;
/// The frames that may wait for one connection's writer.
const CONNECTION_QUEUE: usize = 256;
/// The time a peer has for its `attach` frame.
const ATTACH_WAIT: Duration = Duration::from_secs(10);
/// The time the shim waits for stdout EOF after the provider ended.
const EOF_WAIT: Duration = Duration::from_secs(5);
/// The default `term_grace` of the stop after an overlong line.
const OVERLONG_TERM_GRACE: Duration = Duration::from_secs(5);
/// The error of a write that `close_stdin` ended.
pub const STDIN_CLOSED: &str = "stdin_closed";

type Out = (FromShim, Vec<u8>);

/// Runs the shim. Returns the process exit code.
#[must_use]
pub fn run(shim_id: &str) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            reply(&LaunchReply::Failed {
                spawn_attempted: false,
                error: format!("runtime: {e}"),
            });
            return 1;
        }
    };
    runtime.block_on(serve(shim_id))
}

/// Writes the one launch reply line on stdout.
fn reply(reply: &LaunchReply) {
    use std::io::Write as _;
    if let Ok(mut line) = serde_json::to_vec(reply) {
        line.push(b'\n');
        let mut out = std::io::stdout().lock();
        drop(out.write_all(&line));
        drop(out.flush());
    }
}

fn failed(spawn_attempted: bool, error: impl std::fmt::Display) -> i32 {
    reply(&LaunchReply::Failed {
        spawn_attempted,
        error: error.to_string(),
    });
    1
}

async fn serve(shim_id: &str) -> i32 {
    let mut stdin = tokio::io::stdin();
    let (spec, key) = match read_frame::<_, LaunchSpec>(&mut stdin).await {
        Ok(Some((spec, payload))) => match AttachKey::from_bytes(&payload) {
            Some(key) => (spec, key),
            None => return failed(false, "the launch frame holds no attach key"),
        },
        Ok(None) => return failed(false, "no launch frame"),
        Err(e) => return failed(false, format!("launch frame: {e}")),
    };
    drop(stdin);
    if spec.shim_id != shim_id {
        return failed(false, "the launch frame is for another shim");
    }
    // The shim leaves the station's session; the provider gets its own
    // (shim-exec). Linux: orphaned descendants come back to the shim.
    if let Err(e) = rustix::process::setsid() {
        tracing::debug!(error = %e, "the shim keeps its session");
    }
    #[cfg(target_os = "linux")]
    if let Err(e) = rustix::process::set_child_subreaper(Some(rustix::process::getpid())) {
        tracing::warn!(error = %e, "cannot become a child subreaper");
    }
    let files = ShimFiles::new(&spec.shims_dir, shim_id);
    if let Err(e) = check_dir(&spec.shims_dir) {
        return failed(false, e);
    }
    let lock = match files.try_lock() {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            reply(&LaunchReply::Locked);
            return 3;
        }
        Err(e) => return failed(false, format!("launch lock: {e}")),
    };
    match files.records() {
        Some(records) if records.contains(&Record::Fenced) => {
            reply(&LaunchReply::Fenced);
            return 0;
        }
        Some(records)
            if matches!(records.first(), Some(Record::Created { shim_id: id, incarnation })
                if id == shim_id && incarnation == &spec.incarnation) => {}
        _ => return failed(false, "the launch journal is missing or foreign"),
    }
    let listener = match bind(&spec) {
        Ok(listener) => listener,
        Err(e) => return failed(false, format!("socket: {e}")),
    };
    if let Err(e) = files.append(&Record::SpawnAttempted) {
        drop(std::fs::remove_file(&spec.socket_path));
        return failed(false, format!("journal: {e}"));
    }
    let mut child = match spawn_provider(&spec) {
        Ok(child) => child,
        Err(e) => {
            drop(std::fs::remove_file(&spec.socket_path));
            return failed(true, format!("provider: {e}"));
        }
    };
    let Some(pid) = child.id() else {
        return failed(true, "the provider has no process id");
    };
    let identity = match identity_of(pid) {
        Ok(identity) => identity,
        Err(e) => {
            signal_group(pid, rustix::process::Signal::KILL);
            drop(child.wait().await);
            return failed(true, format!("provider identity: {e}"));
        }
    };
    reply(&LaunchReply::Ready {
        provider_identity: identity.clone(),
    });
    let shim = Shim::new(spec, key, files, identity, lock);
    shim.run(&mut child, listener).await
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The current attachment.
struct Conn {
    generation: u64,
    tx: mpsc::Sender<Out>,
    /// The next stdout offset to stream; `None` until the first `replay`.
    sent: Option<u64>,
    /// Ends the reader of a superseded attachment.
    closed: Arc<Notify>,
}

struct State {
    buffer: OutputBuffer,
    writes: WriteTable,
    generation: u64,
    conn: Option<Conn>,
    exit: Option<ExitState>,
    cause: Option<&'static str>,
    stderr_tail: VecDeque<u8>,
    /// The discarded overlong line (§4.2); every later replay below its
    /// end reports it.
    overlong: Option<(u64, u64)>,
    done: bool,
}

struct Shim {
    spec: LaunchSpec,
    key: AttachKey,
    files: ShimFiles,
    identity: ProviderIdentity,
    state: Mutex<State>,
    /// Stdout bytes or a replay: the streamer runs.
    streamed: Notify,
    /// An `ack` freed space: the stdout pump reads again.
    acked: Notify,
    /// A write was queued.
    queued: Notify,
    /// `close_stdin`.
    close_stdin: watch::Sender<bool>,
    /// The provider process ended.
    exited: watch::Sender<bool>,
    /// The shim ends (after `ack_exit` or the exit file).
    finished: Notify,
    _lock: LaunchLock,
}

impl Shim {
    fn new(
        spec: LaunchSpec,
        key: AttachKey,
        files: ShimFiles,
        identity: ProviderIdentity,
        lock: LaunchLock,
    ) -> Arc<Self> {
        let buffer = OutputBuffer::new(spec.buffer_bytes);
        Arc::new(Self {
            spec,
            key,
            files,
            identity,
            state: Mutex::new(State {
                buffer,
                writes: WriteTable::new(),
                generation: 0,
                conn: None,
                exit: None,
                cause: None,
                stderr_tail: VecDeque::new(),
                overlong: None,
                done: false,
            }),
            streamed: Notify::new(),
            acked: Notify::new(),
            queued: Notify::new(),
            close_stdin: watch::Sender::new(false),
            exited: watch::Sender::new(false),
            finished: Notify::new(),
            _lock: lock,
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn pid(&self) -> u32 {
        self.identity.pid
    }

    /// Sends a frame to the current attachment, if any; never waits.
    fn notify_conn(&self, frame: FromShim) {
        let tx = self.lock().conn.as_ref().map(|c| c.tx.clone());
        if let Some(tx) = tx
            && tx.try_send((frame, Vec::new())).is_err()
        {
            tracing::warn!("the attachment queue is full; a frame was not sent");
        }
    }

    async fn run(self: Arc<Self>, child: &mut Child, listener: UnixListener) -> i32 {
        let (Some(stdout), Some(stderr), Some(stdin)) =
            (child.stdout.take(), child.stderr.take(), child.stdin.take())
        else {
            signal_group(self.pid(), rustix::process::Signal::KILL);
            return 1;
        };
        let (eof_tx, eof_rx) = watch::channel(false);
        tokio::spawn(Arc::clone(&self).pump_stdout(stdout, eof_tx));
        tokio::spawn(Arc::clone(&self).stream());
        tokio::spawn(Arc::clone(&self).drain_stderr(stderr));
        tokio::spawn(Arc::clone(&self).write_stdin(stdin));
        tokio::spawn(Arc::clone(&self).accept(listener));
        let status = child.wait().await;
        self.exited.send_replace(true);
        self.on_exit(status, eof_rx).await;
        self.finished.notified().await;
        0
    }

    async fn on_exit(
        self: &Arc<Self>,
        status: std::io::Result<std::process::ExitStatus>,
        mut eof: watch::Receiver<bool>,
    ) {
        use std::os::unix::process::ExitStatusExt as _;
        let termination = match &status {
            Ok(s) => match (s.code(), s.signal()) {
                (Some(code), _) => format!("code:{code}"),
                (None, Some(sig)) => format!("signal:{sig}"),
                (None, None) => "unknown".to_owned(),
            },
            Err(_) => "unknown".to_owned(),
        };
        let descendants_remaining = group_members(self.pid());
        signal_group(self.pid(), rustix::process::Signal::KILL);
        drop(tokio::time::timeout(EOF_WAIT, eof.wait_for(|done| *done)).await);
        reap_orphans();
        let exit = {
            let mut state = self.lock();
            let stderr_tail =
                String::from_utf8_lossy(state.stderr_tail.make_contiguous()).into_owned();
            let exit = ExitState {
                termination,
                cause: state.cause.unwrap_or("exited").to_owned(),
                stderr_tail,
                descendants_remaining,
                final_offset: state.buffer.end(),
            };
            state.exit = Some(exit.clone());
            exit
        };
        self.notify_conn(FromShim::Exited { exit: exit.clone() });
        tokio::spawn(Arc::clone(self).publish_exit(exit));
    }

    /// §4.5: an exit that no station acknowledges is published to the exit
    /// file after the publish delay; a failed publication keeps the report
    /// served and is tried again after the retry delay.
    async fn publish_exit(self: Arc<Self>, exit: ExitState) {
        tokio::time::sleep(Duration::from_millis(self.spec.exit_publish_after_ms)).await;
        loop {
            if self.lock().done {
                return;
            }
            match self.files.write_exit(&exit) {
                Ok(()) => {
                    self.finish();
                    return;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "cannot publish the exit file; the report stays served");
                }
            }
            tokio::time::sleep(Duration::from_millis(self.spec.exit_retry_ms)).await;
        }
    }

    /// Ends the shim: the socket goes; after `ack_exit` the key and the
    /// lock go too.
    fn finish(&self) {
        {
            let mut state = self.lock();
            if state.done {
                return;
            }
            state.done = true;
        }
        drop(std::fs::remove_file(&self.spec.socket_path));
        self.finished.notify_one();
    }

    async fn pump_stdout(
        self: Arc<Self>,
        mut stdout: tokio::process::ChildStdout,
        eof: watch::Sender<bool>,
    ) {
        let mut chunk = vec![0u8; OUTPUT_CHUNK];
        loop {
            let space = loop {
                let acked = self.acked.notified();
                let space = self.lock().buffer.space();
                if space > 0 {
                    break space;
                }
                self.check_overlong();
                acked.await;
            };
            let want = space.min(chunk.len());
            let n = match stdout.read(&mut chunk[..want]).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            self.lock().buffer.push(&chunk[..n]);
            self.streamed.notify_one();
            self.check_overlong();
        }
        eof.send_replace(true);
    }

    /// §4.2: a full buffer without a newline is a line of the buffer size.
    /// The shim reports the range, discards it and stops the provider.
    fn check_overlong(self: &Arc<Self>) {
        let range = {
            let mut state = self.lock();
            if state.overlong.is_some() {
                return;
            }
            let Some(range) = state.buffer.overlong_line() else {
                return;
            };
            state.overlong = Some(range);
            state.cause = Some("stop_forced");
            // The range lies inside the buffer: the ack cannot fail.
            state.buffer.ack(range.1).ok();
            if let Some(conn) = state.conn.as_mut() {
                conn.sent = conn.sent.map(|s| s.max(range.1));
            }
            range
        };
        self.notify_conn(FromShim::Gap {
            from: range.0,
            to: range.1,
            reason: GapReason::OutputFrameTooLarge,
        });
        self.acked.notify_one();
        tokio::spawn(Arc::clone(self).stop(Duration::ZERO, OVERLONG_TERM_GRACE, "stop_forced"));
    }

    async fn stream(self: Arc<Self>) {
        loop {
            self.streamed.notified().await;
            loop {
                let next = {
                    let mut state = self.lock();
                    let end = state.buffer.end();
                    let floor = state.buffer.floor();
                    let Some(conn) = state.conn.as_ref() else {
                        break;
                    };
                    let Some(sent) = conn.sent.filter(|s| *s < end) else {
                        break;
                    };
                    let from = sent.max(floor);
                    let replay = state.buffer.replay(from);
                    let take = replay.bytes.len().min(OUTPUT_CHUNK);
                    let bytes = replay.bytes[..take].to_vec();
                    let tx = conn.tx.clone();
                    if let Some(conn) = state.conn.as_mut() {
                        conn.sent = Some(replay.offset + take as u64);
                    }
                    (tx, replay.offset, bytes)
                };
                let (tx, offset, bytes) = next;
                let frame = FromShim::Output {
                    offset,
                    read_at: now_ms(),
                };
                if tx.send((frame, bytes)).await.is_err() {
                    break;
                }
            }
        }
    }

    async fn drain_stderr(self: Arc<Self>, mut stderr: tokio::process::ChildStderr) {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            let mut state = self.lock();
            state.stderr_tail.extend(&chunk[..n]);
            let extra = state.stderr_tail.len().saturating_sub(STDERR_TAIL_BYTES);
            state.stderr_tail.drain(..extra);
        }
    }

    /// One active write at a time (§4.3). `close_stdin` ends a blocked
    /// write at once; its bytes in the pipe make it `partial`.
    async fn write_stdin(self: Arc<Self>, stdin: ChildStdin) {
        let mut stdin = Some(stdin);
        let mut close = self.close_stdin.subscribe();
        loop {
            let queued = self.queued.notified();
            let next = self.lock().writes.start_next();
            let Some((write_id, bytes)) = next else {
                if *close.borrow() {
                    stdin = None;
                }
                tokio::select! {
                    () = queued => {}
                    _ = close.changed() => {}
                }
                continue;
            };
            let state = match stdin.as_mut() {
                Some(pipe) if !*close.borrow() => {
                    let done = Arc::new(Mutex::new(0u64));
                    let progress = {
                        let shim = Arc::clone(&self);
                        let id = write_id.clone();
                        let done = Arc::clone(&done);
                        move |n: u64| {
                            *done
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = n;
                            shim.lock().writes.progress(&id, n);
                        }
                    };
                    let write = write_tracked(pipe, &bytes, progress);
                    tokio::select! {
                        state = write => state,
                        _ = close.wait_for(|c| *c) => {
                            let n = *done.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            stdin = None;
                            if n == 0 {
                                WriteState::FailedUnwritten { error: STDIN_CLOSED.to_owned() }
                            } else {
                                WriteState::Partial { bytes_done: n, error: STDIN_CLOSED.to_owned() }
                            }
                        }
                    }
                }
                _ => WriteState::FailedUnwritten {
                    error: STDIN_CLOSED.to_owned(),
                },
            };
            if let Some(pipe) = stdin.as_mut()
                && matches!(state, WriteState::Written)
            {
                drop(pipe.flush().await);
            }
            let seq = self.lock().writes.finish(&write_id, state.clone());
            if let Some(write_seq) = seq {
                self.notify_conn(FromShim::WriteDone {
                    write_id,
                    write_seq,
                    state,
                });
            }
        }
    }

    async fn stop(self: Arc<Self>, grace: Duration, term_grace: Duration, forced: &'static str) {
        let mut exited = self.exited.subscribe();
        self.close_stdin.send_replace(true);
        if tokio::time::timeout(grace, exited.wait_for(|e| *e))
            .await
            .is_ok()
        {
            return;
        }
        {
            // The grace passed: the stop escalates. A kill keeps its cause.
            let mut state = self.lock();
            if state.cause != Some("killed") {
                state.cause = Some(forced);
            }
        }
        signal_group(self.pid(), rustix::process::Signal::TERM);
        if tokio::time::timeout(term_grace, exited.wait_for(|e| *e))
            .await
            .is_ok()
        {
            return;
        }
        signal_group(self.pid(), rustix::process::Signal::KILL);
    }
}
