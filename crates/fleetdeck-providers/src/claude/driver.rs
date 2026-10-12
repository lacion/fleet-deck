//! The live Claude driver (ADR-001 §2.2): [`ClaudeAdapter`] starts `claude -p`
//! through a [`SessionHost`]; [`ClaudeSession`] is its [`LiveSession`].
//!
//! Three tasks share one [`ClaudeCore`]:
//! - a command (a `LiveSession` method) locks the core, applies itself, and
//!   queues the frames to write and emits the events while it holds the lock;
//! - the reader feeds each stdout line to the core the same way, and reports
//!   the exit to the core at the end of stdout;
//! - the writer writes the queued frames to stdin in order. It is a separate
//!   task, so a child that does not read never blocks the reader or the core.
//!
//! Every frame in both directions goes to the raw-frame log (L12), without
//! token deltas; the events of a frame carry its `raw_ref`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{SessionSpec, StopMode};
use fleetdeck_proto::event::{ProviderKind, RawRef};
use fleetdeck_proto::{Capabilities, Operation, ProviderInstanceId};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinHandle;

use super::core::{ClaudeCore, CoreConfig, Output};
use super::frames::{CliFrame, parse_value};
use super::{Launch, capabilities, cli_args, parse_auth_status, parse_version};
use crate::adapter::{
    Answer, ControlId, EventSink, HostInput, LiveSession, ModelId, NativeSessionId, PermissionMode,
    ProviderAdapter, ProviderError, ProviderProbe, Result, canonical_workspace, require,
};
use crate::host::{ProcessControl, ProcessSpec, SessionHost, StopPolicy};

/// How long a host command waits until its frames are in the stdin pipe.
/// A process that stops reading stdin fills the pipe; the command then
/// fails with an unknown outcome instead of blocking the station.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
use crate::rawlog::{RawDir, RawFrameLog, RawLogLimits};
use crate::stdio::{Write, flush, now_ms, write_failed};

/// Where the raw-frame logs of the sessions go.
#[derive(Debug, Clone)]
pub struct RawLogSettings {
    /// Each session gets its own directory below this one.
    pub root: PathBuf,
    pub limits: RawLogLimits,
}

/// The configuration of one Claude provider instance (L10).
#[derive(Debug, Clone)]
pub struct ClaudeAdapterConfig {
    pub instance: ProviderInstanceId,
    /// `claude`, or the path of the binary (a fake in tests).
    pub program: PathBuf,
    /// The complete child environment (already scrubbed, DL-031).
    pub env: ChildEnv,
    /// Offer `bypassPermissions` (station policy, ADR-001 §2.6).
    pub allow_bypass: bool,
    /// More CLI arguments (`--safe-mode` for recordings, DL-005).
    pub extra_args: Vec<String>,
    pub raw_log: Option<RawLogSettings>,
    pub stop: StopPolicy,
    /// How long `stop` waits, after the process ended, for the reader to
    /// deliver the remaining events. A full sink that nobody drains cannot
    /// hold `stop` longer than this.
    pub delivery_grace: Duration,
    /// The time limit of each probe command.
    pub probe_timeout: Duration,
}

impl ClaudeAdapterConfig {
    /// A configuration with the defaults: no bypass, no raw log, the
    /// ADR-001 stop policy, 5 s event delivery after a stop, 10 s probes.
    #[must_use]
    pub fn new(instance: ProviderInstanceId, program: PathBuf, env: ChildEnv) -> Self {
        Self {
            instance,
            program,
            env,
            allow_bypass: false,
            extra_args: Vec::new(),
            raw_log: None,
            stop: StopPolicy::default(),
            delivery_grace: Duration::from_secs(5),
            probe_timeout: Duration::from_secs(10),
        }
    }
}

/// Starts Claude sessions of one provider instance.
#[derive(Debug, Clone)]
pub struct ClaudeAdapter {
    config: ClaudeAdapterConfig,
    host: Arc<dyn SessionHost>,
}

/// How a session starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    New,
    Resume(NativeSessionId),
    Fork(NativeSessionId),
}

impl ClaudeAdapter {
    #[must_use]
    pub fn new(config: ClaudeAdapterConfig, host: Arc<dyn SessionHost>) -> Self {
        Self { config, host }
    }

    /// Starts a session and returns the concrete session type.
    ///
    /// # Errors
    /// The workspace does not resolve (trap 8), the process does not start,
    /// or the raw-frame log cannot be created.
    pub async fn start_session(
        &self,
        spec: SessionSpec,
        sink: EventSink,
        origin: Origin,
    ) -> Result<ClaudeSession> {
        let cwd = canonical_workspace(Path::new(&spec.cwd))?;
        let caps = capabilities(self.config.allow_bypass);
        let (resume, fork) = match &origin {
            Origin::New => (None, false),
            Origin::Resume(id) => (Some(id.clone()), false),
            Origin::Fork(id) => (Some(id.clone()), true),
        };
        let launch = Launch {
            model: spec.model.clone(),
            mode: spec.mode.clone(),
            resume,
            fork,
            extra_args: self.config.extra_args.clone(),
        };
        let args: Vec<OsString> = cli_args(&launch, &caps)
            .into_iter()
            .map(OsString::from)
            .collect();
        let raw = match &self.config.raw_log {
            Some(s) => {
                let dir = s.root.join(ulid::Ulid::new().to_string());
                Some(
                    RawFrameLog::create(&dir, s.limits.clone())
                        .map_err(|source| ProviderError::Workspace { path: dir, source })?,
                )
            }
            None => None,
        };
        let process = self
            .host
            .spawn(ProcessSpec {
                program: self.config.program.clone(),
                args,
                cwd: cwd.clone(),
                env: self.config.env.clone(),
            })
            .await?;
        let mut core = ClaudeCore::new(CoreConfig {
            provider_instance: spec.provider_instance.clone(),
            cwd: cwd.to_string_lossy().into_owned(),
            capabilities: caps.clone(),
            resumed: matches!(origin, Origin::Resume(_)).then_some(true),
        });
        let init = core.initialize()?;
        let raw_dir = raw.as_ref().map(|r| r.dir().to_path_buf());
        let shared = Arc::new(Shared {
            core: Mutex::new(core),
            sink,
            raw: std::sync::Mutex::new(raw),
        });
        let (write_tx, write_rx) = mpsc::unbounded_channel::<Write>();
        let (frames_tx, frames_rx) = watch::channel(0);
        let writer = tokio::spawn(write_loop(
            Arc::clone(&shared),
            Arc::clone(&process.control),
            write_rx,
        ));
        let reader = tokio::spawn(read_loop(
            Arc::clone(&shared),
            Arc::clone(&process.control),
            process.stdout,
            write_tx.clone(),
            frames_tx,
        ));
        let session = ClaudeSession {
            shared,
            caps,
            control: process.control,
            writes: write_tx,
            reader: Some(reader),
            writer: Some(writer),
            frames: frames_rx,
            stop: self.config.stop,
            delivery_grace: self.config.delivery_grace,
            raw_dir,
        };
        for o in init {
            if let Output::Write(v) = o {
                // The writer runs: the send fails only if it already ended.
                let _ = session.writes.send(Write::Frame(v));
            }
        }
        Ok(session)
    }
}

#[async_trait]
impl ProviderAdapter for ClaudeAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Claude
    }

    fn capabilities(&self) -> Capabilities {
        capabilities(self.config.allow_bypass)
    }

    /// `claude --version` and `claude auth status` (trap 9). Never a session.
    async fn probe(&self) -> Result<ProviderProbe> {
        let version = self.run_probe(&["--version"]).await?;
        let version = version.and_then(|(_, out)| parse_version(&out));
        let auth = match self.run_probe(&["auth", "status"]).await? {
            Some((ok, out)) => parse_auth_status(ok, &out),
            None => parse_auth_status(false, ""),
        };
        Ok(ProviderProbe {
            kind: ProviderKind::Claude,
            version,
            auth,
        })
    }

    async fn start(&self, spec: SessionSpec, sink: EventSink) -> Result<Box<dyn LiveSession>> {
        Ok(Box::new(self.start_session(spec, sink, Origin::New).await?))
    }

    fn durable_launch(&self, spec: &SessionSpec) -> Result<crate::durable::DurableLaunch> {
        self.launch_durable(spec)
    }

    async fn resume(
        &self,
        native_id: &NativeSessionId,
        spec: SessionSpec,
        sink: EventSink,
    ) -> Result<Box<dyn LiveSession>> {
        let origin = Origin::Resume(native_id.clone());
        Ok(Box::new(self.start_session(spec, sink, origin).await?))
    }

    async fn fork(
        &self,
        native_id: &NativeSessionId,
        spec: SessionSpec,
        sink: EventSink,
    ) -> Result<Box<dyn LiveSession>> {
        let origin = Origin::Fork(native_id.clone());
        Ok(Box::new(self.start_session(spec, sink, origin).await?))
    }
}

impl ClaudeAdapter {
    /// Runs one probe command. `Ok(None)`: it did not start or timed out.
    async fn run_probe(&self, args: &[&str]) -> Result<Option<(bool, String)>> {
        let mut cmd = tokio::process::Command::new(&self.config.program);
        cmd.args(args)
            .env_clear()
            .envs(self.config.env.iter())
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let Ok(run) = tokio::time::timeout(self.config.probe_timeout, cmd.output()).await else {
            return Ok(None);
        };
        Ok(run.ok().map(|o| {
            (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).into_owned(),
            )
        }))
    }
}

/// The state that the session, its reader and its writer share.
#[derive(Debug)]
struct Shared {
    core: Mutex<ClaudeCore>,
    sink: EventSink,
    raw: std::sync::Mutex<Option<RawFrameLog>>,
}

impl Shared {
    /// Logs a raw frame. The log is diagnostic: after its first error the
    /// session continues without it.
    fn log(&self, dir: RawDir, frame: &Value) -> Option<RawRef> {
        let mut guard = self.raw.lock().ok()?;
        let log = guard.as_mut()?;
        match log.record(dir, now_ms(), frame) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!(error = %e, "raw-frame log disabled");
                *guard = None;
                None
            }
        }
    }

    fn note(&self, dir: RawDir, text: &str) {
        if let Ok(mut guard) = self.raw.lock()
            && let Some(log) = guard.as_mut()
            && log.note(dir, now_ms(), text).is_err()
        {
            *guard = None;
        }
    }

    /// Queues the writes and emits the events of `out`. Called with the core
    /// locked, so the events of commands and frames keep one order.
    async fn dispatch(
        &self,
        out: Vec<Output>,
        raw_ref: Option<&RawRef>,
        writes: &mpsc::UnboundedSender<Write>,
    ) -> Result<()> {
        let mut queued = false;
        for o in out {
            match o {
                Output::Write(v) => {
                    if writes.send(Write::Frame(v)).is_err() {
                        // Nothing of this output reached the writer, unless
                        // an earlier frame of it did: then a part may be
                        // written, and the outcome is unknown.
                        return Err(if queued {
                            write_failed("the stdin writer stopped")
                        } else {
                            ProviderError::SessionEnded
                        });
                    }
                    queued = true;
                }
                Output::Emit(e) => self.sink.emit_with(e, raw_ref.cloned()).await?,
            }
        }
        Ok(())
    }
}

/// Token deltas do not go to the raw-frame log (L12).
fn is_token_delta(frame: &Value) -> bool {
    frame.get("type").and_then(Value::as_str) == Some("stream_event")
        && frame
            .get("event")
            .and_then(|e| e.get("type"))
            .and_then(Value::as_str)
            == Some("content_block_delta")
}

async fn write_loop(
    shared: Arc<Shared>,
    control: Arc<dyn ProcessControl>,
    mut rx: mpsc::UnboundedReceiver<Write>,
) {
    while let Some(item) = rx.recv().await {
        let frame = match item {
            Write::Frame(frame) => frame,
            Write::Close => {
                control.close_stdin().await;
                break;
            }
            Write::Flush(ack) => {
                let _ = ack.send(());
                continue;
            }
        };
        shared.log(RawDir::In, &frame);
        let mut line = frame.to_string();
        line.push('\n');
        if let Err(e) = control.write(line.as_bytes()).await {
            tracing::debug!(error = %e, "claude stdin closed");
            break;
        }
    }
}

async fn read_loop(
    shared: Arc<Shared>,
    control: Arc<dyn ProcessControl>,
    stdout: Box<dyn AsyncRead + Send + Unpin>,
    writes: mpsc::UnboundedSender<Write>,
    frames: watch::Sender<u64>,
) {
    let mut lines = BufReader::new(stdout).lines();
    let mut sink_open = true;
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(error = %e, "claude stdout read failed");
                break;
            }
        };
        if sink_open && !feed_line(&shared, &line, &writes).await {
            // The station dropped the session's events: nobody owns it.
            sink_open = false;
            control.kill().await;
        }
        frames.send_modify(|n| *n += 1);
    }
    let report = control.wait().await;
    if !report.stderr_tail.is_empty() {
        shared.note(RawDir::Stderr, &report.stderr_tail);
    }
    let mut core = shared.core.lock().await;
    let out = core.on_exit(report.code());
    if sink_open {
        let _ = shared.dispatch(out, None, &writes).await;
    }
}

/// Feeds one stdout line. False when the event sink is closed.
async fn feed_line(shared: &Shared, line: &str, writes: &mpsc::UnboundedSender<Write>) -> bool {
    if line.trim().is_empty() {
        return true;
    }
    let (frame, raw_ref) = match serde_json::from_str::<Value>(line) {
        Ok(v) => {
            let raw_ref = (!is_token_delta(&v))
                .then(|| shared.log(RawDir::Out, &v))
                .flatten();
            let frame = parse_value(v).unwrap_or(CliFrame::Unknown(Value::Null));
            (frame, raw_ref)
        }
        Err(_) => {
            shared.note(RawDir::Out, line);
            (CliFrame::Unknown(Value::from(line)), None)
        }
    };
    let mut core = shared.core.lock().await;
    let out = core.on_frame(frame);
    !matches!(
        shared.dispatch(out, raw_ref.as_ref(), writes).await,
        Err(ProviderError::SinkClosed)
    )
}

/// One running `claude -p` process.
#[derive(Debug)]
pub struct ClaudeSession {
    shared: Arc<Shared>,
    caps: Capabilities,
    control: Arc<dyn ProcessControl>,
    writes: mpsc::UnboundedSender<Write>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    frames: watch::Receiver<u64>,
    stop: StopPolicy,
    delivery_grace: Duration,
    raw_dir: Option<PathBuf>,
}

impl ClaudeSession {
    /// The number of stdout lines that the core processed. Tests use it to
    /// apply a command at the same point as a recording.
    #[must_use]
    pub fn frames_seen(&self) -> watch::Receiver<u64> {
        self.frames.clone()
    }

    /// The raw-frame log directory of this session.
    #[must_use]
    pub fn raw_log_dir(&self) -> Option<&Path> {
        self.raw_dir.as_deref()
    }

    /// The process id (also its process group).
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.control.pid()
    }

    /// Runs one core command and dispatches its output.
    async fn run(
        &self,
        op: impl FnOnce(&mut ClaudeCore) -> Result<Vec<Output>> + Send,
    ) -> Result<()> {
        let mut core = self.shared.core.lock().await;
        let out = op(&mut core)?;
        let wrote = out.iter().any(|o| matches!(o, Output::Write(_)));
        self.shared.dispatch(out, None, &self.writes).await?;
        drop(core);
        if wrote {
            self.flush().await?;
        }
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        flush(&self.writes, WRITE_TIMEOUT).await
    }

    async fn input(&self, op: Operation, input: HostInput) -> Result<()> {
        require(&self.caps, op)?;
        self.run(move |c| c.prompt(&input)).await
    }

    /// Waits until the reader saw the end of stdout and reported the exit,
    /// at most `delivery_grace`. A reader that is still blocked on a full
    /// event sink then is aborted: its remaining events are lost.
    async fn join(&mut self) -> Result<()> {
        if let Some(writer) = self.writer.take() {
            writer.abort();
        }
        let Some(mut reader) = self.reader.take() else {
            return Ok(());
        };
        if tokio::time::timeout(self.delivery_grace, &mut reader)
            .await
            .is_ok()
        {
            return Ok(());
        }
        reader.abort();
        tracing::warn!(
            pid = self.control.pid(),
            "claude events undelivered at stop"
        );
        Err(ProviderError::EventsUndelivered)
    }
}

#[async_trait]
impl LiveSession for ClaudeSession {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    async fn prompt(&mut self, input: HostInput) -> Result<()> {
        self.input(Operation::Prompt, input).await
    }

    /// A message written during a turn joins it (ADR-001 §2.3, E2).
    async fn steer(&mut self, input: HostInput) -> Result<()> {
        self.input(Operation::Steer, input).await
    }

    /// The CLI queues a message written while a turn runs or waits.
    async fn follow_up(&mut self, input: HostInput) -> Result<()> {
        self.input(Operation::FollowUp, input).await
    }

    async fn interrupt(&mut self, control_id: Option<ControlId>) -> Result<()> {
        require(&self.caps, Operation::Interrupt)?;
        self.run(|core| core.interrupt(control_id.map(|c| c.0)))
            .await
    }

    async fn answer(&mut self, answer: Answer) -> Result<()> {
        let op = if answer.decision.updated_input.is_some() {
            Operation::EditInput
        } else {
            Operation::Answer
        };
        require(&self.caps, op)?;
        self.run(move |c| c.answer(&answer)).await
    }

    async fn set_mode(&mut self, mode: PermissionMode) -> Result<()> {
        require(&self.caps, Operation::SetMode)?;
        self.run(move |c| c.set_mode(&mode.0)).await
    }

    async fn set_model(&mut self, model: ModelId) -> Result<()> {
        require(&self.caps, Operation::SetModel)?;
        self.run(move |c| c.set_model(&model.0)).await
    }

    /// Claude forks by starting a new process with `--resume <id>
    /// --fork-session` ([`ProviderAdapter::fork`]).
    async fn fork(&mut self) -> Result<NativeSessionId> {
        require(&self.caps, Operation::Fork)?;
        let core = self.shared.core.lock().await;
        core.native_session_id()
            .map(|id| NativeSessionId(id.to_owned()))
            .ok_or(ProviderError::NotReady("session id"))
    }

    /// Graceful: write the queued frames, close stdin, wait the rest of the
    /// grace time, then signal the group (ADR-001 §2.9). The grace time
    /// includes the queued writes. Returns after the exit events were emitted,
    /// or with [`ProviderError::EventsUndelivered`] when the event sink stays
    /// full for `delivery_grace` after the process ended. The process is
    /// stopped in both cases.
    async fn stop(&mut self, how: StopMode) -> Result<()> {
        match how {
            StopMode::Graceful => {
                let started = tokio::time::Instant::now();
                let _ = self.writes.send(Write::Close);
                if let Some(mut writer) = self.writer.take()
                    && tokio::time::timeout(self.stop.grace, &mut writer)
                        .await
                        .is_err()
                {
                    writer.abort();
                }
                let policy = StopPolicy {
                    grace: self.stop.grace.saturating_sub(started.elapsed()),
                    ..self.stop
                };
                self.control.stop(policy).await
            }
            StopMode::Kill => self.control.kill().await,
        };
        self.join().await
    }
}

impl Drop for ClaudeSession {
    /// The tasks hold the process control: abort them so that the last
    /// reference drops and the host kills the group.
    fn drop(&mut self) {
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
        if let Some(writer) = self.writer.take() {
            writer.abort();
        }
    }
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;

#[path = "durable_launch.rs"]
mod durable_launch;
