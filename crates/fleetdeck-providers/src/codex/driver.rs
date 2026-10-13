//! The live Codex driver (ADR-002 §2.4): [`CodexAdapter`] starts
//! `codex app-server` through a [`SessionHost`]; [`CodexSession`] is its
//! [`LiveSession`].
//!
//! The task layout is the one of the Claude driver: a command locks the
//! [`CodexCore`] and queues its frames; the reader feeds each stdout line to
//! the core; the writer writes the queued frames in order. A fourth task
//! polls the background terminals (DL-024): after each `turn.completed`, then
//! every `background_poll` while a terminal or a command that outlived its
//! turn is still open.
//!
//! A session is ready when [`CodexAdapter::start_session`] returns: the
//! `initialize` handshake is done and the thread is open, so the first prompt
//! can go out at once.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{SessionSpec, StopMode};
use fleetdeck_proto::event::{ProviderKind, RawRef};
use fleetdeck_proto::{Capabilities, Event, Operation, ProviderInstanceId};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::{Mutex, Notify, mpsc, watch};
use tokio::task::JoinHandle;

use super::core::{CodexCore, Output};
use super::{ClientInfo, CoreConfig, app_server_args, capabilities, rpc};
use crate::adapter::{
    Answer, ControlId, EventSink, HostInput, LiveSession, ModelId, NativeSessionId, PermissionMode,
    ProviderAdapter, ProviderError, ProviderProbe, Result, canonical_workspace, require,
};
use crate::claude::RawLogSettings;
use crate::coordinator::CoordinatorLaunch;
use crate::host::{ProcessControl, ProcessSpec, SessionHost, StopPolicy};
use crate::rawlog::{RawDir, RawFrameLog};
use crate::stdio::{Write, flush, now_ms, write_failed};

use super::probe::parse_version;

/// How a host command waits until its frames are in the stdin pipe.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Notifications that carry token or text deltas. They do not go to the
/// raw-frame log (L12); the completed items carry the same text.
pub const TOKEN_DELTAS: &[&str] = &[
    "item/agentMessage/delta",
    "item/reasoning/summaryTextDelta",
    "item/reasoning/textDelta",
    "item/plan/delta",
];

/// True for a frame of [`TOKEN_DELTAS`].
#[must_use]
pub fn is_token_delta(frame: &Value) -> bool {
    frame
        .get("method")
        .and_then(Value::as_str)
        .is_some_and(|m| TOKEN_DELTAS.contains(&m))
}

/// The configuration of one Codex provider instance (L10).
#[derive(Debug, Clone)]
pub struct CodexAdapterConfig {
    pub instance: ProviderInstanceId,
    /// `codex`, or the path of the binary (a fake in tests).
    pub program: PathBuf,
    /// The complete child environment (already scrubbed, DL-031).
    pub env: ChildEnv,
    /// Offer the `danger-full-access` sandbox (station policy).
    pub allow_full_access: bool,
    /// Reasoning effort of every turn; `None` keeps the Codex default.
    pub effort: Option<String>,
    /// The `clientInfo` of `initialize` (the fixtures name the recorder).
    pub client: ClientInfo,
    /// Ask for the models after the thread opens (`model/list`, DL-151);
    /// replays of recordings without it turn it off.
    pub request_models: bool,
    /// Interval of the background-terminal poll (DL-024); `None` turns the
    /// automatic poll off (replays apply the recorded polls instead).
    pub background_poll: Option<Duration>,
    pub raw_log: Option<RawLogSettings>,
    pub stop: StopPolicy,
    /// How long `stop` waits for the reader to deliver the last events.
    pub delivery_grace: Duration,
    /// How long a `thread/rollback` waits for its answer (default
    /// [`super::rollback::ROLLBACK_TIMEOUT`]).
    pub rollback_timeout: Duration,
    /// The limit of the handshake: `initialize` and the thread reply.
    pub open_timeout: Duration,
    /// The time limit of each probe.
    pub probe_timeout: Duration,
}

impl CodexAdapterConfig {
    /// The defaults: no full access, Codex's effort, a 5 s background poll,
    /// no raw log, the ADR stop policy, 30 s to open, 10 s probes.
    #[must_use]
    pub fn new(instance: ProviderInstanceId, program: PathBuf, env: ChildEnv) -> Self {
        Self {
            instance,
            program,
            env,
            allow_full_access: false,
            effort: None,
            client: ClientInfo::fleetdeck(),
            request_models: true,
            background_poll: Some(Duration::from_secs(5)),
            raw_log: None,
            stop: StopPolicy::default(),
            delivery_grace: Duration::from_secs(5),
            rollback_timeout: super::rollback::ROLLBACK_TIMEOUT,
            open_timeout: Duration::from_secs(30),
            probe_timeout: Duration::from_secs(10),
        }
    }
}

/// How a session starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    New,
    /// `thread/resume` of a native thread id.
    Resume(NativeSessionId),
    /// `thread/fork` of a native thread id (a new thread).
    Fork(NativeSessionId),
}

/// Starts Codex sessions of one provider instance.
#[derive(Debug, Clone)]
pub struct CodexAdapter {
    config: CodexAdapterConfig,
    host: Arc<dyn SessionHost>,
}

impl CodexAdapter {
    #[must_use]
    pub fn new(config: CodexAdapterConfig, host: Arc<dyn SessionHost>) -> Self {
        Self { config, host }
    }

    /// Starts `codex app-server`, does the handshake and opens the thread.
    ///
    /// # Errors
    /// The workspace does not resolve, the process does not start, the
    /// handshake fails or times out (the process is then killed), or the
    /// raw-frame log cannot be created.
    pub async fn start_session(
        &self,
        spec: SessionSpec,
        sink: EventSink,
        origin: Origin,
    ) -> Result<CodexSession> {
        self.start_session_with(spec, sink, origin, None).await
    }

    /// [`Self::start_session`] with an optional coordinator handoff
    /// (ADR-006 §5.4): its `-c` overrides follow `app-server`; the
    /// credential goes only into the child environment.
    async fn start_session_with(
        &self,
        spec: SessionSpec,
        sink: EventSink,
        origin: Origin,
        coordinator: Option<&CoordinatorLaunch>,
    ) -> Result<CodexSession> {
        let cwd = canonical_workspace(Path::new(&spec.cwd))?;
        let caps = capabilities(self.config.allow_full_access);
        // A mode that this instance does not offer fails the start before
        // any process (DL-064); the core's safest-mode fallback stays the
        // last guard (L8).
        if let Some(mode) = &spec.mode
            && !caps.modes.iter().any(|m| m == mode)
        {
            return Err(ProviderError::InvalidMode(mode.clone()));
        }
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
                args: app_server_args()
                    .iter()
                    .chain(coordinator.iter().flat_map(|c| c.args.iter()))
                    .map(OsString::from)
                    .collect(),
                cwd: cwd.clone(),
                env: CoordinatorLaunch::env_for(coordinator, &self.config.env, &cwd)?,
            })
            .await?;
        let cwd_text = cwd.to_string_lossy().into_owned();
        let core = CodexCore::new(CoreConfig {
            provider_instance: spec.provider_instance.clone(),
            cwd: cwd_text.clone(),
            capabilities: caps.clone(),
            resumed: matches!(origin, Origin::Resume(_)).then_some(true),
            client: self.config.client.clone(),
            request_models: self.config.request_models,
        });
        let raw_dir = raw.as_ref().map(|r| r.dir().to_path_buf());
        let shared = Arc::new(Shared {
            core: Mutex::new(core),
            sink,
            raw: std::sync::Mutex::new(raw),
            poll: Notify::new(),
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
        let poller = self
            .config
            .background_poll
            .map(|every| tokio::spawn(poll_loop(Arc::clone(&shared), write_tx.clone(), every)));
        let mut session = CodexSession {
            shared,
            caps,
            control: process.control,
            writes: write_tx,
            reader: Some(reader),
            writer: Some(writer),
            poller,
            frames: frames_rx,
            stop: self.config.stop,
            delivery_grace: self.config.delivery_grace,
            rollback_timeout: self.config.rollback_timeout,
            raw_dir,
        };
        let (resume, fork) = match origin {
            Origin::New => (None, false),
            Origin::Resume(id) => (Some(id.0), false),
            Origin::Fork(id) => (Some(id.0), true),
        };
        let thread = super::SessionSpec {
            model: spec.model.clone(),
            effort: self.config.effort.clone(),
            mode: spec.mode.clone(),
            cwd: Some(cwd_text),
            resume,
            fork,
        };
        if let Err(e) = session.open(&thread, self.config.open_timeout).await {
            session.control.kill().await;
            return Err(e);
        }
        Ok(session)
    }
}

#[async_trait]
impl ProviderAdapter for CodexAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    fn capabilities(&self) -> Capabilities {
        capabilities(self.config.allow_full_access)
    }

    /// `codex --version`, then `account/read` of a short app-server process
    /// that opens no thread (trap 9: unknown unless the answer is definite).
    async fn probe(&self) -> Result<ProviderProbe> {
        let version = self
            .run_probe(&["--version"])
            .await
            .and_then(|(ok, out)| ok.then(|| parse_version(&out)).flatten());
        let auth = super::probe::account(
            &self.config.program,
            &self.config.env,
            &self.config.client,
            self.config.probe_timeout,
        )
        .await;
        Ok(ProviderProbe {
            kind: ProviderKind::Codex,
            version,
            auth,
        })
    }

    async fn start(&self, spec: SessionSpec, sink: EventSink) -> Result<Box<dyn LiveSession>> {
        Ok(Box::new(self.start_session(spec, sink, Origin::New).await?))
    }

    async fn start_coordinated(
        &self,
        spec: SessionSpec,
        sink: EventSink,
        launch: CoordinatorLaunch,
    ) -> Result<Box<dyn LiveSession>> {
        let session = self.start_session_with(spec, sink, Origin::New, Some(&launch));
        Ok(Box::new(session.await?))
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

    async fn resume_coordinated(
        &self,
        native_id: &NativeSessionId,
        spec: SessionSpec,
        sink: EventSink,
        launch: CoordinatorLaunch,
    ) -> Result<Box<dyn LiveSession>> {
        let origin = Origin::Resume(native_id.clone());
        let session = self.start_session_with(spec, sink, origin, Some(&launch));
        Ok(Box::new(session.await?))
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

/// The state that the session and its tasks share.
#[derive(Debug)]
struct Shared {
    core: Mutex<CodexCore>,
    sink: EventSink,
    raw: std::sync::Mutex<Option<RawFrameLog>>,
    /// Wakes the background poller.
    poll: Notify,
}

impl Shared {
    /// Logs a raw frame. After its first error the session goes on without
    /// the (diagnostic) log.
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

    /// Queues the writes and emits the events of `out`, with the core locked.
    /// A completed turn, or background work that appeared, wakes the poller.
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
                        return Err(if queued {
                            write_failed("the stdin writer stopped")
                        } else {
                            ProviderError::SessionEnded
                        });
                    }
                    queued = true;
                }
                Output::Emit(e) => {
                    let wake = match &e {
                        Event::TurnCompleted { .. } => true,
                        Event::BackgroundUpdated { tasks } => !tasks.is_empty(),
                        _ => false,
                    };
                    self.sink.emit_with(e, raw_ref.cloned()).await?;
                    if wake {
                        self.poll.notify_one();
                    }
                }
            }
        }
        Ok(())
    }
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
            tracing::debug!(error = %e, "codex stdin closed");
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
                tracing::warn!(error = %e, "codex stdout read failed");
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
    drop(core);
    // The poller sees the ended core and stops.
    shared.poll.notify_one();
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
            (rpc::parse_value(v), raw_ref)
        }
        Err(_) => {
            shared.note(RawDir::Out, line);
            (rpc::Incoming::Unknown(Value::from(line)), None)
        }
    };
    let mut core = shared.core.lock().await;
    let out = core.on_frame(frame);
    !matches!(
        shared.dispatch(out, raw_ref.as_ref(), writes).await,
        Err(ProviderError::SinkClosed)
    )
}

/// One running `codex app-server` process with one thread.
#[derive(Debug)]
pub struct CodexSession {
    shared: Arc<Shared>,
    caps: Capabilities,
    control: Arc<dyn ProcessControl>,
    writes: mpsc::UnboundedSender<Write>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    poller: Option<JoinHandle<()>>,
    frames: watch::Receiver<u64>,
    stop: StopPolicy,
    delivery_grace: Duration,
    pub(super) rollback_timeout: Duration,
    raw_dir: Option<PathBuf>,
}

impl CodexSession {
    /// The number of stdout lines that the core processed.
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

    /// The native thread id.
    pub async fn thread_id(&self) -> Option<String> {
        self.shared.core.lock().await.thread_id().map(str::to_owned)
    }

    /// Asks for the background terminals now (the poller does it on its
    /// own; replays apply the recorded polls with this).
    ///
    /// # Errors
    /// An ended session or a failed write.
    pub async fn poll_background(&self) -> Result<()> {
        self.run(CodexCore::list_background).await
    }

    /// Sets the collaboration mode (`plan` or `default`) of the next turns.
    ///
    /// # Errors
    /// [`ProviderError::InvalidMode`] for another name.
    pub async fn set_collaboration(&self, mode: &str) -> Result<()> {
        self.run(|c| c.set_collaboration(mode)).await
    }

    /// `initialize`, then the thread request; returns when the thread is
    /// open.
    async fn open(&mut self, thread: &super::SessionSpec, limit: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + limit;
        // A failed write means that the handshake cannot continue (the
        // stdin writer stopped, usually because the app-server is gone).
        // Report it as the same end as a reader that stops, so no race
        // between the two paths decides the error (DL-064). The caller kills
        // the process on every `open` error.
        let gone = |e: ProviderError| match e {
            ProviderError::Host(crate::host::HostError::Write(_)) => ProviderError::SessionEnded,
            other => other,
        };
        self.run(CodexCore::initialize).await.map_err(gone)?;
        self.wait_until(deadline, CodexCore::is_initialized).await?;
        self.run(|c| c.open_thread(thread)).await.map_err(gone)?;
        self.wait_until(deadline, |c| !c.is_opening()).await?;
        let core = self.shared.core.lock().await;
        if core.thread_id().is_none() {
            return Err(ProviderError::NotReady("thread"));
        }
        Ok(())
    }

    /// Waits until `ready` holds for the core, the process ends, or the
    /// deadline passes.
    pub(super) async fn wait_until(
        &mut self,
        deadline: tokio::time::Instant,
        ready: impl Fn(&CodexCore) -> bool,
    ) -> Result<()> {
        loop {
            self.frames.borrow_and_update();
            {
                let core = self.shared.core.lock().await;
                if ready(&core) {
                    return Ok(());
                }
                if core.has_ended() {
                    return Err(ProviderError::SessionEnded);
                }
            }
            match tokio::time::timeout_at(deadline, self.frames.changed()).await {
                Ok(Ok(())) => {}
                // The reader ended: one last look at the core.
                Ok(Err(_)) => {
                    let core = self.shared.core.lock().await;
                    return if ready(&core) {
                        Ok(())
                    } else {
                        Err(ProviderError::SessionEnded)
                    };
                }
                Err(_) => return Err(ProviderError::NotReady("app-server handshake")),
            }
        }
    }

    /// Reads the core under its lock.
    pub(super) async fn with_core<T>(&self, read: impl FnOnce(&CodexCore) -> T + Send) -> T {
        read(&*self.shared.core.lock().await)
    }

    /// Runs one core command and dispatches its output.
    pub(super) async fn run(
        &self,
        op: impl FnOnce(&mut CodexCore) -> Result<Vec<Output>> + Send,
    ) -> Result<()> {
        let mut core = self.shared.core.lock().await;
        let out = op(&mut core)?;
        let wrote = out.iter().any(|o| matches!(o, Output::Write(_)));
        self.shared.dispatch(out, None, &self.writes).await?;
        drop(core);
        if wrote {
            flush(&self.writes, WRITE_TIMEOUT).await?;
        }
        Ok(())
    }

    /// Waits until the reader reported the exit, at most `delivery_grace`.
    async fn join(&mut self) -> Result<()> {
        if let Some(writer) = self.writer.take() {
            writer.abort();
        }
        if let Some(poller) = self.poller.take() {
            poller.abort();
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
        tracing::warn!(pid = self.control.pid(), "codex events undelivered at stop");
        Err(ProviderError::EventsUndelivered)
    }
}

#[async_trait]
impl LiveSession for CodexSession {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    async fn prompt(&mut self, input: HostInput) -> Result<()> {
        require(&self.caps, Operation::Prompt)?;
        self.run(move |c| c.prompt(&input)).await
    }

    /// `turn/steer` into the running turn.
    async fn steer(&mut self, input: HostInput) -> Result<()> {
        require(&self.caps, Operation::Steer)?;
        self.run(move |c| c.steer(&input)).await
    }

    /// The core keeps the follow-up and sends it after the running turn
    /// (Codex has no native queue, DL-015).
    async fn follow_up(&mut self, input: HostInput) -> Result<()> {
        require(&self.caps, Operation::FollowUp)?;
        self.run(move |c| c.prompt(&input)).await
    }

    async fn interrupt(&mut self, control_id: Option<ControlId>) -> Result<()> {
        require(&self.caps, Operation::Interrupt)?;
        self.run(|core| core.interrupt(control_id.map(|c| c.0)))
            .await
    }

    async fn answer(&mut self, answer: Answer) -> Result<()> {
        require(&self.caps, Operation::Answer)?;
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

    /// Codex forks with `thread/fork` in a new process
    /// ([`ProviderAdapter::fork`]); this returns the thread to fork.
    async fn fork(&mut self) -> Result<NativeSessionId> {
        require(&self.caps, Operation::Fork)?;
        self.thread_id()
            .await
            .map(NativeSessionId)
            .ok_or(ProviderError::NotReady("thread id"))
    }

    /// `thread/rollback`, then the answer (DL-087).
    async fn rollback(&mut self, turns: u32) -> Result<()> {
        require(&self.caps, Operation::Revert)?;
        self.roll_back(turns).await
    }

    /// Graceful: write the queued frames, close stdin (the app-server then
    /// exits), wait the rest of the grace time, then signal the group.
    async fn stop(&mut self, how: StopMode) -> Result<()> {
        if let Some(poller) = self.poller.take() {
            poller.abort();
        }
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
                self.control.stop(policy).await;
            }
            StopMode::Kill => {
                self.control.kill().await;
            }
        }
        self.join().await
    }
}

impl Drop for CodexSession {
    /// The tasks hold the process control: abort them so that the last
    /// reference drops and the host kills the group.
    fn drop(&mut self) {
        for task in [self.reader.take(), self.writer.take(), self.poller.take()]
            .into_iter()
            .flatten()
        {
            task.abort();
        }
    }
}

#[path = "durable_launch.rs"]
mod durable_launch;
#[path = "driver_poll.rs"]
mod poll;
#[path = "driver_probe.rs"]
mod probe_cmd;
use poll::poll_loop;
