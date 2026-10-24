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
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{SessionSpec, StopMode};
use fleetdeck_proto::event::{ProviderKind, RawRef};
use fleetdeck_proto::{Capabilities, Operation, ProviderInstanceId};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinHandle;

use super::core::{ClaudeCore, CoreConfig, Output};
use super::{Launch, capabilities, cli_args, parse_auth_status, parse_version};
use crate::adapter::{
    Answer, ControlId, EventSink, HostInput, LiveSession, ModelId, NativeSessionId, PermissionMode,
    ProviderAdapter, ProviderError, ProviderProbe, Result, canonical_workspace, require,
};
use crate::coordinator::CoordinatorLaunch;
use crate::host::{
    DEFAULT_IDLE_GRACE, ProcessControl, ProcessSpec, SessionHost, StopPolicy, identity_path,
};

/// How long a host command waits until its frames are in the stdin pipe.
/// A process that stops reading stdin fills the pipe; the command then
/// fails with an unknown outcome instead of blocking the station.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
use crate::rawlog::{RawDir, RawFrameLog, RawLogLimits};
use crate::stdio::{
    MAX_LINE_BYTES, Write, flush, now_ms, release_write, reserve_write, write_failed,
};

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
    /// The grace of a graceful stop while the session is idle (no turn, no
    /// pending input): a user's async hook can keep the CLI alive after
    /// stdin EOF (DL-141).
    pub idle_grace: Duration,
    /// How long `stop` waits, after the process ended, for the reader to
    /// deliver the remaining events. A full sink that nobody drains cannot
    /// hold `stop` longer than this.
    pub delivery_grace: Duration,
    /// The time limit of each probe command.
    pub probe_timeout: Duration,
}

impl ClaudeAdapterConfig {
    /// A configuration with the defaults: no bypass, no raw log, the
    /// ADR-001 stop policy, 3 s grace for an idle session, 5 s event
    /// delivery after a stop, 10 s probes.
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
            idle_grace: DEFAULT_IDLE_GRACE,
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
        self.start_session_with(spec, sink, origin, None).await
    }

    /// [`Self::start_session`] with an optional coordinator handoff: its
    /// arguments follow the configured extra arguments, and its credential
    /// goes into the child environment only (ADR-006 §5.4, DL-216).
    async fn start_session_with(
        &self,
        spec: SessionSpec,
        sink: EventSink,
        origin: Origin,
        coordinator: Option<&CoordinatorLaunch>,
    ) -> Result<ClaudeSession> {
        // With a launch identity the path is the checked one, never resolved
        // again by name (B9).
        let (cwd, pinned) = match &spec.launch {
            Some(id) => (identity_path(id), Some(id.as_ref().clone())),
            None => (canonical_workspace(Path::new(&spec.cwd))?, None),
        };
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
            extra_args: self
                .config
                .extra_args
                .iter()
                .cloned()
                .chain(coordinator.into_iter().flat_map(|c| c.claude_args()))
                .collect(),
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
                env: CoordinatorLaunch::env_for(
                    coordinator,
                    &apps::with_apps_host(env_with_extra(
                        &self.config.env,
                        spec.extra_env.as_deref(),
                    )),
                    &cwd,
                )?,
                launch: pinned,
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
        let (candidates, candidate_rx) = mpsc::channel(apps::CANDIDATE_QUEUE);
        let shared = Arc::new(Shared {
            core: Mutex::new(core),
            candidates,
            sink,
            raw: std::sync::Mutex::new(raw),
            queued: AtomicUsize::new(0),
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
            MAX_LINE_BYTES,
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
            idle_grace: self.config.idle_grace,
            delivery_grace: self.config.delivery_grace,
            raw_dir,
            apps: apps::SessionApps::new(candidate_rx),
        };
        for o in init {
            if let Output::Write(v) = o {
                // The writer runs: the send fails only if it already ended.
                // The queue is new, so it has room for the first frames.
                if reserve_write(&session.shared.queued).is_ok()
                    && session.writes.send(Write::Frame(v)).is_err()
                {
                    release_write(&session.shared.queued);
                }
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

    async fn start_coordinated(
        &self,
        spec: SessionSpec,
        sink: EventSink,
        launch: CoordinatorLaunch,
    ) -> Result<Box<dyn LiveSession>> {
        let session = self
            .start_session_with(spec, sink, Origin::New, Some(&launch))
            .await?;
        Ok(Box::new(session))
    }

    fn durable_launch(&self, spec: &SessionSpec) -> Result<crate::durable::DurableLaunch> {
        self.launch_durable(spec)
    }

    fn durable_resume_launch(
        &self,
        spec: &SessionSpec,
        native_id: &NativeSessionId,
    ) -> Result<crate::durable::DurableLaunch> {
        self.launch_durable_as(spec, Some(native_id))
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
        let session = self
            .start_session_with(spec, sink, origin, Some(&launch))
            .await?;
        Ok(Box::new(session))
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
    /// The candidates of completed MCP tool calls for the station (B15).
    candidates: mpsc::Sender<crate::apps::AppCandidate>,
    sink: EventSink,
    raw: std::sync::Mutex<Option<RawFrameLog>>,
    /// The frames in the stdin queue that the writer did not take (NV-19).
    queued: AtomicUsize,
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
                    // The queue is bounded: a process that does not read
                    // stdin must not grow it without limit (NV-19).
                    reserve_write(&self.queued)?;
                    if writes.send(Write::Frame(v)).is_err() {
                        release_write(&self.queued);
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
                // Only the durable (shim) driver uses the identity (DL-194).
                Output::NativeIdentity { .. } => {}
                Output::AppCandidate(candidate) => self.offer_candidate(candidate),
            }
        }
        Ok(())
    }
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
    idle_grace: Duration,
    delivery_grace: Duration,
    raw_dir: Option<PathBuf>,
    apps: apps::SessionApps,
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

    fn pid(&self) -> Option<u32> {
        Some(self.control.pid())
    }

    fn exit_watch(&self) -> Option<crate::host::ExitWatch> {
        self.control.exit_watch()
    }

    fn app_bridge(&self) -> Option<Arc<dyn crate::apps::AppBridge>> {
        Some(self.apps.bridge(&self.shared, &self.writes, &self.frames))
    }

    fn take_app_candidates(&mut self) -> Option<mpsc::Receiver<crate::apps::AppCandidate>> {
        self.apps.take_candidates()
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
    /// includes the queued writes. An idle session gets `idle_grace` (at most
    /// the full grace) instead of the full grace. A busy session keeps the
    /// full grace, but when its turn ends before that, the rest of the grace
    /// becomes `idle_grace` (at most what is left) from that moment. When the
    /// core is busy (locked by the reader), the stop counts the session as
    /// not idle. Returns after the exit events were emitted, or with
    /// [`ProviderError::EventsUndelivered`] when the event sink stays full
    /// for `delivery_grace` after the process ended. The process is stopped
    /// in both cases.
    async fn stop(&mut self, how: StopMode) -> Result<()> {
        match how {
            StopMode::Graceful => {
                let idle = self.shared.core.try_lock().is_ok_and(|core| core.is_idle());
                let grace = if idle {
                    self.idle_grace.min(self.stop.grace)
                } else {
                    self.stop.grace
                };
                let started = tokio::time::Instant::now();
                let deadline = started + grace;
                let _ = self.writes.send(Write::Close);
                if let Some(mut writer) = self.writer.take()
                    && tokio::time::timeout(grace, &mut writer).await.is_err()
                {
                    writer.abort();
                }
                let turn_ended = !idle && self.wait_turn_end(deadline).await;
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                let policy = StopPolicy {
                    grace: if turn_ended {
                        self.idle_grace.min(left)
                    } else {
                        left
                    },
                    ..self.stop
                };
                self.control.stop(policy).await
            }
            StopMode::Kill => self.control.kill().await,
        };
        self.join().await
    }
}

impl ClaudeSession {
    /// Waits until the core is idle, the process ended, or `deadline`. True
    /// only when the core became idle.
    async fn wait_turn_end(&self, deadline: tokio::time::Instant) -> bool {
        let mut frames = self.frames.clone();
        let idle = async {
            loop {
                if self.shared.core.try_lock().is_ok_and(|core| core.is_idle()) {
                    return true;
                }
                if frames.changed().await.is_err() {
                    // The reader ended: the process is gone.
                    return false;
                }
            }
        };
        tokio::select! {
            idle = tokio::time::timeout_at(deadline, idle) => idle.unwrap_or(false),
            _ = self.control.wait() => false,
        }
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

/// The instance environment plus the project's `worktree_env` (FG-4, design
/// §7.3). A name that the instance environment has already keeps its value.
/// A value that `ChildEnv` refuses is dropped with a log line.
pub(crate) fn env_with_extra(
    base: &ChildEnv,
    extra: Option<&std::collections::BTreeMap<String, String>>,
) -> ChildEnv {
    let mut env = base.clone();
    for (name, value) in extra.into_iter().flatten() {
        if env.get(name).is_none()
            && let Err(e) = env.set(name, value)
        {
            tracing::warn!(variable = %name, error = %e, "extra_env: a variable is dropped");
        }
    }
    env
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;

#[path = "driver_apps.rs"]
mod apps;
#[path = "durable_launch.rs"]
mod durable_launch;
#[path = "driver_io.rs"]
mod io;
use io::{read_loop, write_loop};
