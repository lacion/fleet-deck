//! The provider's leader process for the shim (DL-431): the wait that keeps
//! its zombie, and the gate through which every signal to the provider
//! passes. A numeric signal (`kill(-L)`, `kill(L)`) is sent only while the
//! leader is unreaped: Linux keeps its pid number allocated, so no other
//! process can have a group or session with that number.

use std::os::fd::OwnedFd;
use std::sync::Mutex;

use rustix::process::Signal;
use tokio::sync::watch;

/// The result of the wait for the leader.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LeaderWait {
    /// The leader terminated; it stays a zombie (`WNOWAIT`): `L` is pinned.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Ended,
    /// The wait failed; the leader may or may not be reaped. Fail closed.
    Failed(rustix::io::Errno),
}

/// Linux: waits until the leader terminates, without reaping it.
#[cfg(target_os = "linux")]
pub(super) fn wait_leader(pid: u32) -> LeaderWait {
    use rustix::io::Errno;
    use rustix::process::{WaitId, WaitIdOptions, waitid};

    let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return LeaderWait::Failed(Errno::INVAL);
    };
    loop {
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(_)) => return LeaderWait::Ended,
            Ok(None) | Err(Errno::INTR) => {}
            Err(e) => return LeaderWait::Failed(e),
        }
    }
}

/// Not Linux: the shim host refuses outside Linux (DL-182); the code only
/// has to compile.
#[cfg(not(target_os = "linux"))]
pub(super) fn wait_leader(_pid: u32) -> LeaderWait {
    LeaderWait::Failed(rustix::io::Errno::NOSYS)
}

/// Starts the thread `fd-shim-leader-wait` and returns the channel of its
/// result. A thread that cannot start is logged; the closed channel then
/// counts as `Failed(Errno::IO)`.
pub(super) fn start_wait(pid: u32) -> tokio::sync::oneshot::Receiver<LeaderWait> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let started = std::thread::Builder::new()
        .name("fd-shim-leader-wait".to_owned())
        .spawn(move || {
            drop(tx.send(wait_leader(pid)));
        });
    if let Err(e) = started {
        tracing::warn!(error = %e, "cannot start the leader wait thread");
    }
    rx
}

/// Linux: opens a pidfd of `pid`.
#[cfg(target_os = "linux")]
pub(super) fn pidfd_open(pid: rustix::process::Pid) -> Result<OwnedFd, rustix::io::Errno> {
    rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())
}

/// Not Linux: there is no pidfd (DL-182); the code only has to compile.
#[cfg(not(target_os = "linux"))]
pub(super) fn pidfd_open(_pid: rustix::process::Pid) -> Result<OwnedFd, rustix::io::Errno> {
    Err(rustix::io::Errno::NOSYS)
}

/// Linux: sends `sig` to the process of the pidfd `fd`.
#[cfg(target_os = "linux")]
pub(super) fn pidfd_send_signal(fd: &OwnedFd, sig: Signal) -> Result<(), rustix::io::Errno> {
    rustix::process::pidfd_send_signal(fd, sig)
}

/// Not Linux: there is no pidfd (DL-182); the code only has to compile.
#[cfg(not(target_os = "linux"))]
pub(super) fn pidfd_send_signal(_fd: &OwnedFd, _sig: Signal) -> Result<(), rustix::io::Errno> {
    Err(rustix::io::Errno::NOSYS)
}

/// Opens a pidfd of the leader, right after the spawn (the leader is
/// unreaped, so the pidfd names the provider). Any error: no pidfd.
pub(super) fn open_leader_fd(pid: u32) -> Option<OwnedFd> {
    let pid = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)?;
    match pidfd_open(pid) {
        Ok(fd) => Some(fd),
        Err(e) => {
            tracing::debug!(error = %e, "no pidfd of the provider");
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lifecycle {
    /// Not terminated, not reaped.
    Running,
    /// Terminated, a zombie (`LeaderWait::Ended`): the exit sweep runs.
    Ended,
    /// `LeaderWait::Failed`: the leader may still run until tokio reaps it.
    Unpinned,
    /// Reaped (or about to be reaped: the reap starts only after this state).
    Reaped,
}

/// Where a signal goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    /// The process group `L`, with the single-process fallback of the
    /// shim's group signal helper.
    Group(u32),
    /// The leader process only, through its pidfd.
    LeaderFd,
}

/// What `signal` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sent {
    Group,
    Escalated,
    LeaderFd,
    Nothing,
}

type SendFn = Box<dyn Fn(Target, Signal) + Send + Sync>;

/// The only code that signals the provider after the `Shim` exists.
pub(super) struct Leader {
    pid: u32,
    fd: Option<OwnedFd>,
    state: Mutex<Lifecycle>,
    /// A Kill during `Ended`: the sweep skips its graces.
    escalate: watch::Sender<bool>,
    send: SendFn,
}

impl Leader {
    /// The leader `pid` in the state `Running`; signals go to the kernel.
    pub(super) fn new(pid: u32, fd: Option<OwnedFd>) -> Self {
        let for_send = fd.as_ref().and_then(|f| f.try_clone().ok());
        Self::with_sender(
            pid,
            fd,
            Box::new(move |target, sig| match target {
                Target::Group(pid) => super::process::signal_group(pid, sig),
                Target::LeaderFd => {
                    if let Some(fd) = &for_send
                        && let Err(e) = pidfd_send_signal(fd, sig)
                    {
                        tracing::debug!(?sig, error = %e, "cannot signal the provider by pidfd");
                    }
                }
            }),
        )
    }

    /// A leader whose signals go to `send` (tests, a spy).
    pub(super) fn with_sender(pid: u32, fd: Option<OwnedFd>, send: SendFn) -> Self {
        Self {
            pid,
            fd,
            state: Mutex::new(Lifecycle::Running),
            escalate: watch::Sender::new(false),
            send,
        }
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, Lifecycle> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Sends `sig` by the rule table of the design (§3.2.2). `on_running`
    /// runs under the lifecycle lock before the send, only in `Running`.
    pub(super) fn signal(&self, sig: Signal, on_running: impl FnOnce()) -> Sent {
        let state = self.guard();
        match *state {
            Lifecycle::Running => {
                on_running();
                (self.send)(Target::Group(self.pid), sig);
                Sent::Group
            }
            Lifecycle::Ended => {
                if sig == Signal::KILL {
                    self.escalate.send_replace(true);
                    Sent::Escalated
                } else {
                    Sent::Nothing
                }
            }
            Lifecycle::Unpinned => {
                if self.fd.is_some() {
                    (self.send)(Target::LeaderFd, sig);
                    Sent::LeaderFd
                } else {
                    tracing::warn!("the provider cannot be signalled safely");
                    Sent::Nothing
                }
            }
            Lifecycle::Reaped => {
                tracing::debug!(?sig, "the provider is reaped; no signal sent");
                Sent::Nothing
            }
        }
    }

    pub(super) fn set(&self, to: Lifecycle) {
        *self.guard() = to;
    }

    pub(super) fn lifecycle(&self) -> Lifecycle {
        *self.guard()
    }

    pub(super) fn escalated(&self) -> watch::Receiver<bool> {
        self.escalate.subscribe()
    }
}

#[cfg(test)]
#[path = "server_leader_tests.rs"]
mod tests;
