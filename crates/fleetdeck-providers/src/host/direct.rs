//! [`DirectChildHost`]: the agent is a child process of the station (MVP, A9).
//!
//! It ends with the station: the station's end of the pipes closes, and the
//! CLI sees the end of its input. A [`ProcessControl`] that is dropped while
//! its process runs kills the group.

use std::collections::VecDeque;
use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::launch_dir::LaunchDir;
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::{
    EndCause, ExitReport, HostError, HostKind, HostedProcess, ProcessControl, ProcessSpec,
    STDERR_TAIL_BYTES, SessionHost, StopPolicy, Termination,
};

/// After the child exits, wait this long for the rest of stderr.
const STDERR_SETTLE: Duration = Duration::from_millis(500);

/// The MVP session host.
#[derive(Debug, Default, Clone, Copy)]
pub struct DirectChildHost;

impl DirectChildHost {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SessionHost for DirectChildHost {
    fn kind(&self) -> HostKind {
        HostKind::DirectChild
    }

    fn survives_station_restart(&self) -> bool {
        false
    }

    async fn spawn(&self, spec: ProcessSpec) -> Result<HostedProcess, HostError> {
        // With an identity the child starts in the checked directory object,
        // or not at all; `dir` lives until the spawn returned (B9).
        let dir = spec
            .launch
            .as_ref()
            .map(|id| LaunchDir::reopen(id).map_err(|e| HostError::LaunchDir(e.to_string())))
            .transpose()?;
        let cwd = match &dir {
            Some(dir) => dir.cwd().map_err(|e| HostError::LaunchDir(e.to_string()))?,
            None => spec.cwd.clone(),
        };
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .current_dir(&cwd)
            .env_clear()
            .envs(spec.env.iter())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|source| HostError::Spawn {
            program: spec.program.display().to_string(),
            source,
        })?;
        drop(dir);
        let pid = child.id().ok_or(HostError::NoPipe)?;
        let stdin = child.stdin.take().ok_or(HostError::NoPipe)?;
        let stdout = child.stdout.take().ok_or(HostError::NoPipe)?;
        let stderr = child.stderr.take().ok_or(HostError::NoPipe)?;

        let shared = Arc::new(Shared {
            requested: Mutex::new(EndCause::Exited),
            tail: Mutex::new(VecDeque::with_capacity(STDERR_TAIL_BYTES)),
        });
        let (exit_tx, exit_rx) = watch::channel(None);
        let (closing, _) = watch::channel(false);
        let drain = tokio::spawn(drain_stderr(stderr, Arc::clone(&shared)));
        tokio::spawn(supervise(child, pid, Arc::clone(&shared), drain, exit_tx));
        tracing::debug!(pid, program = %spec.program.display(), "session process started");
        Ok(HostedProcess {
            stdout: Box::new(stdout),
            control: Arc::new(DirectProcess {
                pid,
                stdin: tokio::sync::Mutex::new(Some(stdin)),
                closing,
                shared,
                exit_rx,
            }),
        })
    }
}

/// State shared by the control and the supervisor task.
#[derive(Debug)]
struct Shared {
    /// The strongest host request so far ([`EndCause`] order).
    requested: Mutex<EndCause>,
    tail: Mutex<VecDeque<u8>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Shared {
    fn request(&self, cause: EndCause) {
        let mut requested = lock(&self.requested);
        *requested = (*requested).max(cause);
    }

    fn report(&self, termination: Termination) -> ExitReport {
        let tail = lock(&self.tail);
        let (a, b) = tail.as_slices();
        let bytes = [a, b].concat();
        ExitReport {
            termination,
            cause: *lock(&self.requested),
            stderr_tail: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }
}

async fn drain_stderr(mut stderr: impl AsyncRead + Unpin, shared: Arc<Shared>) {
    let mut buf = vec![0_u8; 4096];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let mut tail = lock(&shared.tail);
                tail.extend(&buf[..n]);
                let extra = tail.len().saturating_sub(STDERR_TAIL_BYTES);
                tail.drain(..extra);
            }
        }
    }
}

async fn supervise(
    mut child: Child,
    pid: u32,
    shared: Arc<Shared>,
    drain: JoinHandle<()>,
    exit_tx: watch::Sender<Option<ExitReport>>,
) {
    let status = child.wait().await;
    // Leave nothing open: background work of the ended process dies with it.
    signal_group(pid, Signal::KILL);
    let _ = timeout(STDERR_SETTLE, drain).await;
    let termination = match status {
        Ok(s) => match (s.code(), s.signal()) {
            (Some(code), _) => Termination::Code(code),
            (None, Some(sig)) => Termination::Signal(sig),
            (None, None) => Termination::Unknown,
        },
        Err(e) => {
            tracing::warn!(pid, error = %e, "cannot read the exit status");
            Termination::Unknown
        }
    };
    let report = shared.report(termination);
    tracing::debug!(pid, ?report.termination, ?report.cause, "session process ended");
    exit_tx.send_replace(Some(report));
}

/// Signals the process group `pid`. A group that is already gone is fine.
fn signal_group(pid: u32, sig: Signal) {
    // Never signal init's group or a negative id.
    let Some(pid) = i32::try_from(pid)
        .ok()
        .filter(|p| *p > 1)
        .and_then(Pid::from_raw)
    else {
        return;
    };
    if let Err(e) = kill_process_group(pid, sig)
        && e != rustix::io::Errno::SRCH
    {
        tracing::warn!(pid = pid.as_raw_nonzero().get(), ?sig, error = %e, "cannot signal the process group");
    }
}

/// The control of one child process.
#[derive(Debug)]
struct DirectProcess {
    pid: u32,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    /// Set by `close_stdin`. A write that is blocked (the child does not read
    /// and the pipe is full) gives up the stdin lock when this turns true, so
    /// closing never waits for the child.
    closing: watch::Sender<bool>,
    shared: Arc<Shared>,
    exit_rx: watch::Receiver<Option<ExitReport>>,
}

#[async_trait]
impl ProcessControl for DirectProcess {
    fn pid(&self) -> u32 {
        self.pid
    }

    async fn write(&self, bytes: &[u8]) -> Result<(), HostError> {
        let mut closing = self.closing.subscribe();
        if *closing.borrow_and_update() {
            return Err(HostError::StdinClosed);
        }
        let mut guard = tokio::select! {
            guard = self.stdin.lock() => guard,
            _ = closing.wait_for(|c| *c) => return Err(HostError::StdinClosed),
        };
        let stdin = guard.as_mut().ok_or(HostError::StdinClosed)?;
        let send = async {
            stdin.write_all(bytes).await?;
            stdin.flush().await
        };
        tokio::select! {
            sent = send => sent.map_err(HostError::Write),
            // A partial frame is fine here: the input ends anyway.
            _ = closing.wait_for(|c| *c) => Err(HostError::StdinClosed),
        }
    }

    async fn close_stdin(&self) {
        // First release a blocked writer, then drop the pipe. Dropping closes
        // the write end; no `shutdown` call, which could wait on the child.
        self.closing.send_replace(true);
        drop(self.stdin.lock().await.take());
    }

    async fn stop(&self, policy: StopPolicy) -> ExitReport {
        if let Some(report) = self.exit() {
            return report;
        }
        self.shared.request(EndCause::Stopped);
        // The grace period covers closing stdin too.
        let close_then_wait = async {
            self.close_stdin().await;
            self.wait().await
        };
        if let Ok(report) = timeout(policy.grace, close_then_wait).await {
            return report;
        }
        self.shared.request(EndCause::StopForced);
        signal_group(self.pid, Signal::TERM);
        if let Ok(report) = timeout(policy.term_grace, self.wait()).await {
            return report;
        }
        signal_group(self.pid, Signal::KILL);
        self.wait().await
    }

    async fn kill(&self) -> ExitReport {
        if let Some(report) = self.exit() {
            return report;
        }
        self.shared.request(EndCause::Killed);
        signal_group(self.pid, Signal::KILL);
        self.closing.send_replace(true);
        self.wait().await
    }

    async fn wait(&self) -> ExitReport {
        let mut rx = self.exit_rx.clone();
        if let Ok(report) = rx.wait_for(Option::is_some).await
            && let Some(report) = report.clone()
        {
            return report;
        }
        // The supervisor task is gone (runtime shutdown) without a status.
        self.shared.report(Termination::Unknown)
    }

    fn exit(&self) -> Option<ExitReport> {
        self.exit_rx.borrow().clone()
    }
}

impl Drop for DirectProcess {
    fn drop(&mut self) {
        if self.exit_rx.borrow().is_none() {
            self.shared.request(EndCause::Killed);
            signal_group(self.pid, Signal::KILL);
        }
    }
}

#[cfg(test)]
#[path = "direct_tests.rs"]
mod tests;
