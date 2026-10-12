//! `ShimHost` (ADR-005 §2, slice 3, DL-102): the station side of a durable
//! shim. It launches `fleetdeck shim`, attaches with a generation, streams
//! the provider stdout from a cursor, and implements [`SessionHost`] and
//! [`ProcessControl`] over the shim frames.
//!
//! - Dropping every handle never stops the provider (ADR-005 §2): the
//!   connection closes and the shim keeps the provider and its output.
//! - [`ShimHost::reattach`] takes over with the next generation and replays
//!   from the stored cursor. The caller commits the generation before the
//!   attach (ADR-005 §3), and checks the provider identity of the record.
//! - [`AckMode::Manual`]: the caller acknowledges a cursor only after it
//!   committed the events of the bytes below it (ADR-005 §4.1).
//!   [`AckMode::OnDelivery`] (the [`SessionHost`] path) acknowledges each
//!   complete line once the reader has it; it is not crash-exact, and the
//!   station host setting stays `direct` until the station commits cursors.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncWriteExt as _, DuplexStream};
use tokio::sync::{mpsc, oneshot, watch};

use super::client::{self, AttachResult, Connection, FrameReader, FrameWriter};
use super::identity::{IdentityCheck, check_identity};
use super::journal::{self, ShimFiles, prepare_dir};
use super::launch::{EXIT_PUBLISH_AFTER_MS, EXIT_RETRY_MS, LaunchReply, LaunchSpec};
use super::output::DEFAULT_CAPACITY;
use super::proof::AttachKey;
use super::server::STDIN_CLOSED;
use super::wire::{
    ExitState, FromShim, GapReason, MAX_PAYLOAD, ProviderIdentity, ToShim, WriteState,
};
use crate::host::{
    EndCause, ExitReport, HostError, HostKind, HostedProcess, ProcessControl, ProcessSpec,
    SessionHost, StopPolicy, Termination,
};

/// The in-process pipe between the frame reader and the adapter.
pub const PIPE_BYTES: usize = 256 * 1024;
/// At most this many frames wait for the connection (ADR-005 §4.4: 64
/// queued control frames; stdin writes wait for their answer one by one).
const QUEUE: usize = 64;

/// Where and how a [`ShimHost`] launches shims.
#[derive(Debug, Clone)]
pub struct ShimHostConfig {
    /// The `fleetdeck` binary (it has the hidden `shim` subcommand).
    pub exe: PathBuf,
    /// The persistent station directory; shim files go to `<dir>/shims`.
    pub station_dir: PathBuf,
    /// `$XDG_RUNTIME_DIR`, if set (ADR-005 §5.1).
    pub runtime_dir: Option<PathBuf>,
    /// `${TMPDIR:-/tmp}` for the short-path fallback.
    pub tmp_dir: PathBuf,
    /// The station database incarnation.
    pub incarnation: String,
    /// The shim output buffer (ADR-005 §4.1).
    pub buffer_bytes: usize,
    /// The bound of each launch, connect and attach step.
    pub wait: Duration,
}

impl ShimHostConfig {
    #[must_use]
    pub fn new(exe: PathBuf, station_dir: PathBuf, incarnation: String) -> Self {
        Self {
            exe,
            station_dir,
            runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            tmp_dir: std::env::temp_dir(),
            incarnation,
            buffer_bytes: DEFAULT_CAPACITY,
            wait: Duration::from_secs(10),
        }
    }

    #[must_use]
    pub fn shims_dir(&self) -> PathBuf {
        self.station_dir.join("shims")
    }
}

/// What the station stores about one shim (the `shims` row, ADR-005 §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimRecord {
    pub shim_id: String,
    pub socket_path: PathBuf,
    pub provider_identity: Option<ProviderIdentity>,
    /// The generation of the current attachment.
    pub generation: u64,
    /// The committed stdout cursor (a line boundary).
    pub cursor: u64,
    /// The next stdin write sequence number.
    pub next_write_seq: u64,
}

/// When the host acknowledges stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckMode {
    /// After each complete line reached the reader (not crash-exact).
    OnDelivery,
    /// Only through [`ShimProcess::ack`], after the caller committed.
    Manual,
}

/// An attached shim.
pub struct Attached {
    pub record: ShimRecord,
    pub process: Arc<ShimProcess>,
    pub stdout: DuplexStream,
    /// The lowest offset that the shim still holds.
    pub floor: u64,
}

impl fmt::Debug for Attached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Attached")
            .field("record", &self.record)
            .field("floor", &self.floor)
            .finish_non_exhaustive()
    }
}

/// Launches shims and attaches to them.
#[derive(Debug, Clone)]
pub struct ShimHost {
    config: ShimHostConfig,
}

fn shim_error(what: impl fmt::Display) -> HostError {
    HostError::Shim(what.to_string())
}

impl ShimHost {
    #[must_use]
    pub fn new(config: ShimHostConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn config(&self) -> &ShimHostConfig {
        &self.config
    }

    /// Launches a shim for `spec` and attaches with generation 1.
    pub async fn launch(&self, spec: &ProcessSpec, mode: AckMode) -> Result<Attached, HostError> {
        let c = &self.config;
        let spawn_error = |source: io::Error| HostError::Spawn {
            program: spec.program.display().to_string(),
            source,
        };
        let shims = c.shims_dir();
        prepare_dir(&shims).map_err(spawn_error)?;
        let shim_id = ulid::Ulid::new().to_string();
        let files = ShimFiles::new(&shims, &shim_id);
        files.create_journal(&c.incarnation).map_err(spawn_error)?;
        let key = AttachKey::generate()
            .map_err(|_| spawn_error(io::Error::other("no random bytes for the attach key")))?;
        files.write_key(&key).map_err(spawn_error)?;
        let socket_path = journal::socket_path(
            c.runtime_dir.as_deref(),
            &c.station_dir,
            &c.tmp_dir,
            &c.incarnation,
            &shim_id,
        )
        .ok_or_else(|| spawn_error(io::Error::other("socket_path_too_long")))?;
        let launch_spec = LaunchSpec {
            shim_id: shim_id.clone(),
            incarnation: c.incarnation.clone(),
            shims_dir: shims,
            socket_path: socket_path.clone(),
            buffer_bytes: c.buffer_bytes,
            exit_publish_after_ms: EXIT_PUBLISH_AFTER_MS,
            exit_retry_ms: EXIT_RETRY_MS,
            program: Vec::new(),
            args: Vec::new(),
            cwd: Vec::new(),
            env: Vec::new(),
        }
        .provider_from(spec);
        let launched = client::launch(&c.exe, &launch_spec, &key, c.wait)
            .await
            .map_err(|e| spawn_error(io::Error::other(e.to_string())))?;
        let provider_identity = match launched.reply {
            LaunchReply::Ready { provider_identity } => provider_identity,
            other => return Err(spawn_error(io::Error::other(format!("{other:?}")))),
        };
        // The shim outlives this handle: tokio never kills it on drop.
        drop(launched.child);
        let record = ShimRecord {
            shim_id,
            socket_path,
            provider_identity: Some(provider_identity),
            generation: 0,
            cursor: 0,
            next_write_seq: 1,
        };
        self.attach_at(&record, 1, mode).await
    }

    /// Takes over a shim with the next generation and replays from the
    /// record cursor. The caller commits `record.generation + 1` first.
    pub async fn reattach(
        &self,
        record: &ShimRecord,
        mode: AckMode,
    ) -> Result<Attached, HostError> {
        self.attach_at(record, record.generation + 1, mode).await
    }

    async fn attach_at(
        &self,
        record: &ShimRecord,
        generation: u64,
        mode: AckMode,
    ) -> Result<Attached, HostError> {
        let c = &self.config;
        let files = ShimFiles::new(&c.shims_dir(), &record.shim_id);
        let key = files
            .read_key()
            .ok_or_else(|| shim_error("the attach key file is missing"))?;
        let conn = Connection::open(&record.socket_path, c.wait)
            .await
            .map_err(shim_error)?;
        if conn.hello.shim_id != record.shim_id {
            return Err(shim_error("the socket belongs to another shim"));
        }
        let attached = conn
            .attach(&key, &c.incarnation, generation, c.wait)
            .await
            .map_err(shim_error)?;
        let (attachment, frame) = match attached {
            AttachResult::Attached { attachment, frame } => (attachment, frame),
            AttachResult::Refused(reason) => {
                return Err(shim_error(format!("the attach was refused: {reason:?}")));
            }
        };
        let FromShim::Attached {
            provider_identity,
            cursor_floor,
            exit,
            ..
        } = frame
        else {
            return Err(shim_error("the shim sent no attached frame"));
        };
        if check_identity(
            record.provider_identity.as_ref(),
            provider_identity.as_ref(),
        ) != IdentityCheck::Same
        {
            return Err(shim_error("identity_mismatch"));
        }
        let pid = provider_identity.as_ref().map_or(0, |p| p.pid);
        let (reader, mut writer) = attachment.into_split();
        writer
            .send(
                &ToShim::Replay {
                    generation,
                    from: record.cursor,
                },
                &[],
            )
            .await
            .map_err(shim_error)?;
        let (tx, rx) = mpsc::channel(QUEUE);
        tokio::spawn(write_loop(writer, rx));
        let (exit_tx, _) = watch::channel(None);
        let shared = Arc::new(Shared {
            exit: exit_tx,
            pending: StdMutex::new(HashMap::new()),
            acked: AtomicU64::new(record.cursor),
            delivered: AtomicU64::new(record.cursor),
            gaps: StdMutex::new(Vec::new()),
        });
        let (sink, stdout) = tokio::io::duplex(PIPE_BYTES);
        let start = record.cursor.max(cursor_floor);
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        tokio::spawn(sink_loop(Sink {
            sink: Some(sink),
            chunks: out_rx,
            shared: Arc::clone(&shared),
            tx: tx.clone(),
            generation,
            mode,
            delivered: start,
        }));
        tokio::spawn(read_loop(Reader {
            frames: reader,
            out: out_tx,
            shared: Arc::clone(&shared),
            received: start,
            exit,
        }));
        let process = Arc::new(ShimProcess {
            pid,
            generation,
            tx,
            shared,
            next_seq: AtomicU64::new(record.next_write_seq.max(1)),
            stdin_closed: AtomicBool::new(false),
        });
        let mut record = record.clone();
        record.generation = generation;
        Ok(Attached {
            record,
            process,
            stdout,
            floor: cursor_floor,
        })
    }
}

#[async_trait]
impl SessionHost for ShimHost {
    fn kind(&self) -> HostKind {
        HostKind::Shim
    }

    fn survives_station_restart(&self) -> bool {
        true
    }

    async fn spawn(&self, spec: ProcessSpec) -> Result<HostedProcess, HostError> {
        let attached = self.launch(&spec, AckMode::OnDelivery).await?;
        Ok(HostedProcess {
            stdout: Box::new(attached.stdout),
            control: attached.process,
        })
    }
}

struct Outgoing {
    frame: ToShim,
    payload: Vec<u8>,
}

#[derive(Debug)]
struct Shared {
    exit: watch::Sender<Option<ExitReport>>,
    pending: StdMutex<HashMap<String, oneshot::Sender<WriteState>>>,
    acked: AtomicU64,
    delivered: AtomicU64,
    gaps: StdMutex<Vec<(u64, u64, GapReason)>>,
}

impl Shared {
    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<WriteState>>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn finish(&self, report: ExitReport) {
        // Unanswered writes end as "stdin closed" (their senders drop).
        self.pending().clear();
        self.exit.send_if_modified(|exit| {
            if exit.is_some() {
                return false;
            }
            *exit = Some(report);
            true
        });
    }
}

async fn write_loop(mut writer: FrameWriter, mut rx: mpsc::Receiver<Outgoing>) {
    while let Some(out) = rx.recv().await {
        if writer.send(&out.frame, &out.payload).await.is_err() {
            break;
        }
    }
}

/// What the frame reader hands to the stdout task. The channel has no
/// bound of its own: the shim sends only bytes that it still holds, and it
/// holds at most its buffer size of unacknowledged bytes (ADR-005 §4.1).
enum Chunk {
    Bytes(u64, Vec<u8>),
    /// The shim reported a gap up to this offset.
    Skip(u64),
    /// No more output; acknowledge the exit after the delivery if true.
    End {
        ack_exit: bool,
    },
}

/// Reads frames. A slow stdout reader never blocks the control answers
/// (write results, the exit report): the bytes go to [`sink_loop`].
struct Reader {
    frames: FrameReader,
    out: mpsc::UnboundedSender<Chunk>,
    shared: Arc<Shared>,
    received: u64,
    exit: Option<ExitState>,
}

/// Writes the stdout bytes for the adapter and acknowledges them.
struct Sink {
    sink: Option<DuplexStream>,
    chunks: mpsc::UnboundedReceiver<Chunk>,
    shared: Arc<Shared>,
    tx: mpsc::Sender<Outgoing>,
    generation: u64,
    mode: AckMode,
    delivered: u64,
}

/// The report of a lost connection: the provider may still run under the
/// shim (a reattach decides), but this attachment ends.
fn lost() -> ExitReport {
    ExitReport {
        termination: Termination::Unknown,
        cause: EndCause::Exited,
        stderr_tail: String::new(),
    }
}

/// Maps the shim's exit state to the host exit report.
#[must_use]
pub fn exit_report(exit: &ExitState) -> ExitReport {
    let number = |prefix: &str| {
        exit.termination
            .strip_prefix(prefix)
            .and_then(|n| n.parse::<i32>().ok())
    };
    let termination = number("code:").map_or_else(
        || number("signal:").map_or(Termination::Unknown, Termination::Signal),
        Termination::Code,
    );
    let cause = match exit.cause.as_str() {
        "stopped" => EndCause::Stopped,
        "stop_forced" => EndCause::StopForced,
        "killed" => EndCause::Killed,
        _ => EndCause::Exited,
    };
    ExitReport {
        termination,
        cause,
        stderr_tail: exit.stderr_tail.clone(),
    }
}

async fn read_loop(mut r: Reader) {
    loop {
        if let Some(exit) = r.exit.as_ref()
            && r.received >= exit.final_offset
        {
            r.shared.finish(exit_report(exit));
            drop(r.out.send(Chunk::End { ack_exit: true }));
            return;
        }
        let Ok(Some((frame, payload))) = r.frames.next().await else {
            r.shared.finish(lost());
            drop(r.out.send(Chunk::End { ack_exit: false }));
            return;
        };
        match frame {
            FromShim::Output { offset, .. } => {
                let skip = usize::try_from(r.received.saturating_sub(offset)).unwrap_or(usize::MAX);
                if skip >= payload.len() {
                    continue;
                }
                let start = offset.max(r.received);
                let bytes = payload[skip..].to_vec();
                r.received = start + bytes.len() as u64;
                drop(r.out.send(Chunk::Bytes(start, bytes)));
            }
            FromShim::Gap { from, to, reason } => {
                r.shared
                    .gaps
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((from, to, reason));
                r.received = r.received.max(to);
                drop(r.out.send(Chunk::Skip(to)));
            }
            FromShim::WriteDone {
                write_id, state, ..
            } => {
                if let Some(done) = r.shared.pending().remove(&write_id) {
                    drop(done.send(state));
                }
            }
            FromShim::Exited { exit } => r.exit = Some(exit),
            FromShim::Hello { .. }
            | FromShim::Refused { .. }
            | FromShim::Attached { .. }
            | FromShim::WriteStatus { .. }
            | FromShim::Status { .. } => {}
        }
    }
}

async fn sink_loop(mut s: Sink) {
    let mut line_end = s.delivered;
    while let Some(chunk) = s.chunks.recv().await {
        match chunk {
            Chunk::Bytes(start, bytes) => {
                if let Some(sink) = s.sink.as_mut()
                    && sink.write_all(&bytes).await.is_err()
                {
                    // The adapter dropped its stdout; the bytes stay
                    // unacknowledged in [`AckMode::Manual`].
                    s.sink = None;
                }
                if let Some(i) = bytes.iter().rposition(|b| *b == b'\n') {
                    line_end = start + i as u64 + 1;
                }
                s.delivered = start + bytes.len() as u64;
                s.shared.delivered.store(s.delivered, Ordering::SeqCst);
                if s.mode == AckMode::OnDelivery
                    && line_end > s.shared.acked.fetch_max(line_end, Ordering::SeqCst)
                {
                    let ack = ToShim::Ack {
                        generation: s.generation,
                        cursor: line_end,
                    };
                    drop(
                        s.tx.send(Outgoing {
                            frame: ack,
                            payload: Vec::new(),
                        })
                        .await,
                    );
                }
            }
            Chunk::Skip(to) => {
                s.delivered = s.delivered.max(to);
                line_end = line_end.max(to);
                s.shared.delivered.store(s.delivered, Ordering::SeqCst);
            }
            Chunk::End { ack_exit } => {
                // EOF for the adapter, then the exit acknowledgment.
                s.sink = None;
                if ack_exit && s.mode == AckMode::OnDelivery {
                    let frame = ToShim::AckExit {
                        generation: s.generation,
                    };
                    drop(
                        s.tx.send(Outgoing {
                            frame,
                            payload: Vec::new(),
                        })
                        .await,
                    );
                }
                return;
            }
        }
    }
}

/// The control of one attached shim. Dropping it never stops the provider.
pub struct ShimProcess {
    pid: u32,
    generation: u64,
    tx: mpsc::Sender<Outgoing>,
    shared: Arc<Shared>,
    next_seq: AtomicU64,
    stdin_closed: AtomicBool,
}

impl fmt::Debug for ShimProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShimProcess")
            .field("pid", &self.pid)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl ShimProcess {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The highest acknowledged cursor.
    #[must_use]
    pub fn acked(&self) -> u64 {
        self.shared.acked.load(Ordering::SeqCst)
    }

    /// The stdout offset that the reader received up to.
    #[must_use]
    pub fn delivered(&self) -> u64 {
        self.shared.delivered.load(Ordering::SeqCst)
    }

    /// The next stdin write sequence number (store it with the record).
    #[must_use]
    pub fn next_write_seq(&self) -> u64 {
        self.next_seq.load(Ordering::SeqCst)
    }

    /// The output gaps that the shim reported.
    #[must_use]
    pub fn gaps(&self) -> Vec<(u64, u64, GapReason)> {
        self.shared
            .gaps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    async fn send(&self, frame: ToShim, payload: Vec<u8>) -> bool {
        self.tx.send(Outgoing { frame, payload }).await.is_ok()
    }

    /// Acknowledges `cursor` after the caller committed the events below it
    /// ([`AckMode::Manual`]).
    pub async fn ack(&self, cursor: u64) -> bool {
        self.shared.acked.fetch_max(cursor, Ordering::SeqCst);
        self.send(
            ToShim::Ack {
                generation: self.generation,
                cursor,
            },
            Vec::new(),
        )
        .await
    }

    /// Acknowledges the exit report after the caller committed it; the shim
    /// then removes its socket, key and lock and ends.
    pub async fn ack_exit(&self) -> bool {
        self.send(
            ToShim::AckExit {
                generation: self.generation,
            },
            Vec::new(),
        )
        .await
    }

    async fn write_one(&self, chunk: &[u8]) -> Result<(), HostError> {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let write_id = format!("host-{}-{seq}", self.generation);
        let (done_tx, done_rx) = oneshot::channel();
        self.shared.pending().insert(write_id.clone(), done_tx);
        let frame = ToShim::Write {
            generation: self.generation,
            write_id: write_id.clone(),
            write_seq: seq,
            sent_at: 0,
        };
        if !self.send(frame, chunk.to_vec()).await || self.shared.exit.borrow().is_some() {
            self.shared.pending().remove(&write_id);
            return Err(HostError::StdinClosed);
        }
        match done_rx.await {
            Ok(WriteState::Written) => Ok(()),
            Ok(WriteState::Partial { error, .. } | WriteState::FailedUnwritten { error })
                if error == STDIN_CLOSED =>
            {
                Err(HostError::StdinClosed)
            }
            Ok(WriteState::ClosedUnwritten) | Err(_) => Err(HostError::StdinClosed),
            // A close, stop or kill ended the write (part may be written).
            Ok(_) if self.stdin_closed.load(Ordering::SeqCst) => Err(HostError::StdinClosed),
            Ok(other) => Err(HostError::Write(io::Error::other(format!(
                "the shim reported {other:?}"
            )))),
        }
    }
}

#[async_trait]
impl ProcessControl for ShimProcess {
    fn pid(&self) -> u32 {
        self.pid
    }

    async fn write(&self, bytes: &[u8]) -> Result<(), HostError> {
        if self.stdin_closed.load(Ordering::SeqCst) {
            return Err(HostError::StdinClosed);
        }
        for chunk in bytes.chunks(MAX_PAYLOAD) {
            self.write_one(chunk).await?;
        }
        Ok(())
    }

    async fn close_stdin(&self) {
        self.stdin_closed.store(true, Ordering::SeqCst);
        let frame = ToShim::CloseStdin {
            generation: self.generation,
        };
        self.send(frame, Vec::new()).await;
    }

    async fn stop(&self, policy: StopPolicy) -> ExitReport {
        self.stdin_closed.store(true, Ordering::SeqCst);
        let millis = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        let frame = ToShim::Stop {
            generation: self.generation,
            grace_ms: millis(policy.grace),
            term_grace_ms: millis(policy.term_grace),
        };
        self.send(frame, Vec::new()).await;
        self.wait().await
    }

    async fn kill(&self) -> ExitReport {
        self.stdin_closed.store(true, Ordering::SeqCst);
        let frame = ToShim::Kill {
            generation: self.generation,
        };
        self.send(frame, Vec::new()).await;
        self.wait().await
    }

    async fn wait(&self) -> ExitReport {
        let mut rx = self.shared.exit.subscribe();
        loop {
            if let Some(report) = rx.borrow_and_update().clone() {
                return report;
            }
            if rx.changed().await.is_err() {
                return rx.borrow().clone().unwrap_or_else(lost);
            }
        }
    }

    fn exit(&self) -> Option<ExitReport> {
        self.shared.exit.borrow().clone()
    }
}
