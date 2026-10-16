//! The session host boundary (PLAN §6.1, A9).
//!
//! A [`SessionHost`] starts the provider process of one session and owns its
//! life: stdin, stdout, stderr, the process group, stop, kill, and the exit
//! report. Provider adapters talk to their process only through a
//! [`HostedProcess`], so the Phase 7 `ShimHost` (the agent survives a station
//! restart) can replace [`DirectChildHost`] without adapter changes.
//!
//! The trait lives in this crate because the adapter drivers use it; the
//! station selects the host and re-exports it (DL-031).
//!
//! Rules every host keeps:
//! - The child gets exactly [`ProcessSpec::env`] (an already scrubbed
//!   [`ChildEnv`], PLAN §3 rule 6). Nothing secret goes into `args`.
//! - The child leads its own process group. Stop and kill act on the whole
//!   group, because background commands are grandchildren (ADR-001 §2.9).
//! - When the child ends, the rest of its group is killed: background work of
//!   an ended session has no owner (trap 6).
//! - Stderr is drained all the time (a full pipe would block the child); the
//!   last [`STDERR_TAIL_BYTES`] go into the exit report.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_proto::launch::LaunchIdentity;
use tokio::io::AsyncRead;

mod direct;
pub mod shim;

pub use direct::DirectChildHost;

/// The bytes of stderr kept for the exit report.
pub const STDERR_TAIL_BYTES: usize = 8 * 1024;

/// What to start.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    /// A bare name is looked up on the `PATH` of [`Self::env`].
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    /// The complete environment of the child.
    pub env: ChildEnv,
    /// The checked directory object of the session folder (B9): the host
    /// reopens it and starts the child in that object. `None`: `cwd` by name.
    pub launch: Option<LaunchIdentity>,
}

/// The path of a launch identity: `root` joined with `rel` (B9).
pub(crate) fn identity_path(id: &LaunchIdentity) -> PathBuf {
    if id.rel.is_empty() {
        PathBuf::from(&id.root)
    } else {
        std::path::Path::new(&id.root).join(&id.rel)
    }
}

/// The kind of host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    /// MVP: the agent is a child of the station and ends with it.
    DirectChild,
    /// Phase 7: the agent runs under a shim and survives a station restart.
    Shim,
}

/// How a process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    Code(i32),
    Signal(i32),
    /// The host lost the process before it could read the status.
    Unknown,
}

/// Why a process ended. The order is the escalation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EndCause {
    /// The process ended without a host request (a crash, or its own exit).
    Exited,
    /// Stop: the process ended within the grace period after stdin closed.
    Stopped,
    /// Stop: the grace period passed, and the host signalled the group.
    StopForced,
    /// Kill: the host killed the group.
    Killed,
}

/// The exit report of one process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitReport {
    pub termination: Termination,
    pub cause: EndCause,
    /// The end of stderr (at most [`STDERR_TAIL_BYTES`], lossy UTF-8). It is
    /// diagnostic text: do not put it in events without a size budget.
    pub stderr_tail: String,
}

impl ExitReport {
    /// The exit code, if the process exited on its own terms.
    #[must_use]
    pub fn code(&self) -> Option<i32> {
        match self.termination {
            Termination::Code(code) => Some(code),
            Termination::Signal(_) | Termination::Unknown => None,
        }
    }
}

/// The grace of a graceful stop while the session is idle: a user's async
/// hook can keep the CLI alive after stdin EOF (DL-141).
pub const DEFAULT_IDLE_GRACE: Duration = Duration::from_secs(3);

/// The time bounds of [`ProcessControl::stop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopPolicy {
    /// Wait this long after stdin closes (ADR-001 §2.9: 30 s).
    pub grace: Duration,
    /// Then send SIGTERM to the group and wait this long before SIGKILL.
    pub term_grace: Duration,
}

impl Default for StopPolicy {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(30),
            term_grace: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("cannot start `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    /// The session folder is not the checked object any more (B9).
    #[error("{0}")]
    LaunchDir(String),
    #[error("the process has no stdin pipe")]
    NoPipe,
    #[error("stdin of the process is closed")]
    StdinClosed,
    #[error("write to the process failed: {0}")]
    Write(#[source] std::io::Error),
    /// The shim connection or attach failed (ADR-005).
    #[error("shim: {0}")]
    Shim(String),
    /// The shim did not prove its executable, so it got no token (DL-194).
    #[error("the shim could not be verified: {0}")]
    Untrusted(String),
}

/// A started process: its stdout (read by the adapter driver) and its control.
pub struct HostedProcess {
    pub stdout: Box<dyn AsyncRead + Send + Unpin>,
    pub control: Arc<dyn ProcessControl>,
}

impl fmt::Debug for HostedProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostedProcess")
            .field("control", &self.control)
            .finish_non_exhaustive()
    }
}

/// Starts processes for sessions.
#[async_trait]
pub trait SessionHost: Send + Sync + fmt::Debug {
    fn kind(&self) -> HostKind;
    /// True if a hosted process outlives a station restart.
    fn survives_station_restart(&self) -> bool;
    async fn spawn(&self, spec: ProcessSpec) -> Result<HostedProcess, HostError>;
}

/// The control of one hosted process. Dropping the last reference to a
/// process that still runs kills its group.
#[async_trait]
pub trait ProcessControl: Send + Sync + fmt::Debug {
    /// The process id (also the process group id).
    fn pid(&self) -> u32;
    /// Writes all bytes to stdin and flushes. A child that does not read can
    /// block this call until `close_stdin`, `stop` or `kill`; then it returns
    /// [`HostError::StdinClosed`] (part of the bytes may be written).
    async fn write(&self, bytes: &[u8]) -> Result<(), HostError>;
    /// Closes stdin (the clean end of input). It does not wait for the child,
    /// also not behind a blocked write. Later writes fail.
    async fn close_stdin(&self);
    /// Clean stop: close stdin and wait, together at most `grace`; SIGTERM the
    /// group, wait `term_grace`; SIGKILL the group. Returns the exit report.
    async fn stop(&self, policy: StopPolicy) -> ExitReport;
    /// SIGKILL the group now. Returns the exit report.
    async fn kill(&self) -> ExitReport;
    /// Waits for the exit.
    async fn wait(&self) -> ExitReport;
    /// The exit report, if the process ended.
    fn exit(&self) -> Option<ExitReport>;
}
