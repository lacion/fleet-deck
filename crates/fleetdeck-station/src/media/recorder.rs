//! The owned recorder process (R13 P5b, design `omarchy-p5b.md` §7.1-§7.3,
//! §8 `RLIMIT_FSIZE`, §9.1, §9.2).
//!
//! One dedicated OS thread starts the child, keeps its `Child` until it is
//! reaped (Linux parent-death signals follow the spawning thread) and runs
//! the timers. The timers do not depend on any async task: commands arrive
//! on a `std::sync::mpsc` channel that the thread polls every
//! [`POLL`]. Signals go to the owned process group only: no global or
//! name-based kill.
//!
//! The child is `<exe> shim-exec --parent <pid> --fsize 268435456
//! --stdin-fd3 -- <recorder> <argv>`: the helper hands the retained output
//! file to the recorder as FD 3 and sets stdin to `/dev/null`.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use rustix::process::{Pid, Signal};
use tokio::sync::oneshot;

use super::clock::{BootInstant, Clocks, SuspendWatch};
use crate::desktop::capture::{Launch, STDERR_KEEP};
use crate::desktop::{Fail, code};

/// `RLIMIT_FSIZE` of the recorder, soft and hard (design §8).
pub const FSIZE_LIMIT: u64 = 268_435_456;
/// The most seconds of one recording.
pub const MAX_SECONDS: u32 = 120;
/// How often the supervisor thread looks at its commands, timers and child.
const POLL: Duration = Duration::from_millis(25);
/// The size of one read of a pipe.
const DRAIN_BUFFER: usize = 4096;
/// The most time that `exited` waits for both drain threads, after the reap.
const DRAIN_WAIT: Duration = Duration::from_secs(2);

/// The `wf-recorder` argv of design §9.1, one element per token. The filter
/// value is built from the integer `seconds`.
///
/// # Errors
/// `invalid_params` when `seconds` is not in 1..=120.
pub fn argv(monitor: &str, seconds: u32) -> Result<Vec<String>, Fail> {
    if !(1..=MAX_SECONDS).contains(&seconds) {
        return Err(Fail::new(
            code::INVALID_PARAMS,
            "`seconds` must be from 1 to 120",
        ));
    }
    let filter = format!("fps=30,trim=duration={seconds}");
    Ok([
        "-o",
        monitor,
        "-r",
        "30",
        "-D",
        "-c",
        "libx264",
        "-x",
        "yuv420p",
        "-b",
        "0",
        "-p",
        "preset=veryfast",
        "-p",
        "crf=23",
        "-F",
        &filter,
        "-m",
        "mp4",
        "-y",
        "-f",
        "/proc/self/fd/3",
    ]
    .iter()
    .map(|t| (*t).to_owned())
    .collect())
}

/// The timers of one recording.
#[derive(Debug, Clone, Copy)]
pub struct Deadlines {
    /// From the start of the supervisor: the save stop (`SIGINT`).
    pub duration: Duration,
    /// From the start of the supervisor: the `Deadline` discard stop.
    pub total: Duration,
    /// The lease expiry on the boot clock; [`Recorder::renew`] moves it.
    pub lease_until: BootInstant,
    /// The time between `SIGINT` and the group `SIGKILL` (3 s).
    pub grace: Duration,
}

/// Why the recorder was stopped. `Duration` and `OwnerStop` request a save;
/// the others discard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    Duration,
    OwnerStop,
    Discard,
    LeaseExpired,
    Suspended,
    Deadline,
}

impl StopCause {
    fn is_save(self) -> bool {
        matches!(self, Self::Duration | Self::OwnerStop)
    }
}

/// How the recorder ended, after the reap and after both drains.
pub struct Exit {
    /// The exit status; `None` when the child could not be waited for.
    pub status: Option<ExitStatus>,
    /// The stop that the supervisor decided; `None` when the child ended on
    /// its own.
    pub cause: Option<StopCause>,
    /// `SIGKILL` was needed after `SIGINT`.
    pub escalated: bool,
    /// The child ended with `SIGXFSZ` (the file-size limit).
    pub xfsz: bool,
    /// The first 16 KiB of stderr. Never shown, logged or returned.
    pub stderr_tail: Vec<u8>,
    /// A drain thread was still reading after `DRAIN_WAIT` (a process outside
    /// the group holds a pipe). The thread ends on its own; it holds only the
    /// pipe read end and a bounded buffer, never the output file.
    pub drain_timed_out: bool,
}

impl std::fmt::Debug for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Exit")
            .field("status", &self.status)
            .field("cause", &self.cause)
            .field("escalated", &self.escalated)
            .field("xfsz", &self.xfsz)
            .field("stderr_bytes", &self.stderr_tail.len())
            .field("drain_timed_out", &self.drain_timed_out)
            .finish()
    }
}

enum Cmd {
    OwnerStop,
    Discard,
    Renew(BootInstant),
}

/// The handle of a running recorder.
pub struct Recorder {
    cmd: Sender<Cmd>,
    exit: oneshot::Receiver<Exit>,
}

impl Recorder {
    /// Save: `SIGINT` to the group, the grace, then `SIGKILL`.
    pub fn owner_stop(&self) {
        // A send error means the supervisor is gone: the child is reaped.
        drop(self.cmd.send(Cmd::OwnerStop));
    }

    /// Discard: `SIGKILL` to the group at once.
    pub fn discard(&self) {
        drop(self.cmd.send(Cmd::Discard));
    }

    /// Moves the lease expiry to `until` when that is later.
    pub fn renew(&self, until: BootInstant) {
        drop(self.cmd.send(Cmd::Renew(until)));
    }

    /// Waits for the end: after the reap and after both drains finished.
    pub async fn exited(self) -> Exit {
        let Self { cmd, exit } = self;
        let result = exit.await;
        // The command channel stays open until the exit arrived: a closed
        // channel would count as a discard.
        drop(cmd);
        result.unwrap_or_else(|_| Exit {
            status: None,
            cause: None,
            escalated: false,
            xfsz: false,
            stderr_tail: Vec::new(),
            drain_timed_out: false,
        })
    }
}

fn failed() -> Fail {
    Fail::new("recording_failed", "The recording tool failed.")
}

/// Starts the recorder through the shim of this executable (`fleetdeck
/// shim-exec`, see [`spawn_with`]). The recorder writes into `output`.
///
/// # Errors
/// `invalid_params` for `seconds` outside 1..=120; `recording_failed` when
/// the executable, the descriptor, a thread or the child cannot be started.
pub fn spawn(
    env: &ChildEnv,
    program: &Path,
    monitor: &str,
    seconds: u32,
    output: &File,
    d: Deadlines,
    clocks: Arc<dyn Clocks>,
) -> Result<Recorder, Fail> {
    let exe = std::env::current_exe().map_err(|e| {
        tracing::warn!(kind = ?e.kind(), "cannot find the station executable");
        failed()
    })?;
    spawn_with(
        &Launch::ShimExec { exe },
        env,
        program,
        monitor,
        seconds,
        output,
        d,
        clocks,
    )
}

/// [`spawn`] with an explicit launch: production passes
/// [`Launch::ShimExec`]; tests of the station crate pass `Launch::Direct`.
///
/// # Errors
/// As [`spawn`].
#[allow(
    clippy::too_many_arguments,
    reason = "the arguments of spawn plus the launch"
)]
pub fn spawn_with(
    launch: &Launch,
    env: &ChildEnv,
    program: &Path,
    monitor: &str,
    seconds: u32,
    output: &File,
    d: Deadlines,
    clocks: Arc<dyn Clocks>,
) -> Result<Recorder, Fail> {
    let args = argv(monitor, seconds)?;
    let stdin = output.try_clone().map_err(|e| {
        tracing::warn!(kind = ?e.kind(), "cannot duplicate the recording file");
        failed()
    })?;
    let cmd = command(launch, env, program, &args, stdin);
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (exit_tx, exit_rx) = oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("record-supervisor".into())
        .spawn(move || {
            let exit = supervise(cmd, d, &*clocks, &cmd_rx, &ready_tx);
            if let Some(exit) = exit {
                // The handle is gone when nobody waits for the exit.
                drop(exit_tx.send(exit));
            }
        });
    if thread.is_err() {
        return Err(failed());
    }
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(Recorder {
            cmd: cmd_tx,
            exit: exit_rx,
        }),
        Ok(Err(fail)) => Err(fail),
        Err(_) => Err(failed()),
    }
}

fn command(
    launch: &Launch,
    env: &ChildEnv,
    program: &Path,
    args: &[String],
    stdin: File,
) -> Command {
    let mut cmd = match launch {
        Launch::ShimExec { exe } => {
            let mut cmd = Command::new(exe);
            cmd.arg("shim-exec")
                .arg("--parent")
                .arg(std::process::id().to_string())
                .arg("--fsize")
                .arg(FSIZE_LIMIT.to_string())
                .arg("--stdin-fd3")
                .arg("--")
                .arg(program);
            cmd
        }
        #[cfg(test)]
        Launch::Direct => {
            use std::os::unix::process::CommandExt as _;
            let mut cmd = Command::new(program);
            cmd.process_group(0);
            cmd
        }
    };
    cmd.args(args)
        .env_clear()
        .envs(env.iter())
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// A signal to the process group of `pid`. `fallback`: when the group does
/// not exist yet (the shim has not made it), signal the child itself.
/// Returns `true` when a process received the signal. Init is never
/// signalled.
fn signal_group(pid: u32, sig: Signal, fallback: bool) -> bool {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .filter(|p| *p > 1)
        .and_then(Pid::from_raw)
    else {
        return false;
    };
    match rustix::process::kill_process_group(pid, sig) {
        Ok(()) => true,
        Err(rustix::io::Errno::SRCH) if fallback => match rustix::process::kill_process(pid, sig) {
            Ok(()) => true,
            Err(rustix::io::Errno::SRCH) => false,
            Err(e) => {
                tracing::warn!(error = %e, "cannot signal the recorder");
                false
            }
        },
        Err(rustix::io::Errno::SRCH) => false,
        Err(e) => {
            tracing::warn!(error = %e, "cannot signal the recorder group");
            false
        }
    }
}

/// Reads a pipe to EOF and drops the bytes.
fn drain_discard(mut pipe: ChildStdout) {
    let mut buf = [0_u8; DRAIN_BUFFER];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            // A read error ends the drain; the exit status tells the rest.
            Err(_) => break,
        }
    }
}

/// Reads stderr to EOF, keeps the first `STDERR_KEEP` bytes in `kept` and
/// drops the rest.
fn drain_keep(mut pipe: ChildStderr, kept: &Mutex<Vec<u8>>) {
    let mut buf = [0_u8; DRAIN_BUFFER];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
                let take = n.min(STDERR_KEEP - kept.len());
                kept.extend_from_slice(&buf[..take]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

/// The two drain threads of a child. Each sends on its channel when it is
/// done (a closed channel counts as done too).
struct Drains {
    stdout: Receiver<()>,
    stderr: Receiver<()>,
    kept: Arc<Mutex<Vec<u8>>>,
}

impl Drains {
    fn start(stdout: ChildStdout, stderr: ChildStderr) -> std::io::Result<Self> {
        let (out_tx, out_rx) = mpsc::channel();
        let (err_tx, err_rx) = mpsc::channel();
        let kept = Arc::new(Mutex::new(Vec::new()));
        std::thread::Builder::new()
            .name("record-stdout".into())
            .spawn(move || {
                drain_discard(stdout);
                let _ = out_tx.send(());
            })?;
        let shared = Arc::clone(&kept);
        std::thread::Builder::new()
            .name("record-stderr".into())
            .spawn(move || {
                drain_keep(stderr, &shared);
                let _ = err_tx.send(());
            })?;
        Ok(Self {
            stdout: out_rx,
            stderr: err_rx,
            kept,
        })
    }

    /// Waits for both threads, at most `DRAIN_WAIT` in total. The kept
    /// stderr bytes, and `true` when a thread was still reading at the end.
    fn finish(self) -> (Vec<u8>, bool) {
        let end = Instant::now() + DRAIN_WAIT;
        let mut timed_out = false;
        for done in [&self.stdout, &self.stderr] {
            let left = end.saturating_duration_since(Instant::now());
            if done.recv_timeout(left) == Err(RecvTimeoutError::Timeout) {
                timed_out = true;
            }
        }
        let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        (kept.clone(), timed_out)
    }
}

/// The decisions of the supervisor loop.
struct Plan {
    pid: u32,
    cause: Option<StopCause>,
    /// When `SIGINT` was sent (monotonic clock).
    interrupted: Option<std::time::Instant>,
    killed: bool,
    escalated: bool,
}

impl Plan {
    /// A save stop: `SIGINT` to the group. Ignored after any stop.
    fn save(&mut self, cause: StopCause, now: std::time::Instant) {
        if self.cause.is_none() {
            self.cause = Some(cause);
            self.interrupted = Some(now);
            signal_group(self.pid, Signal::INT, true);
        }
    }

    /// A discard stop: `SIGKILL` to the group at once. It replaces a save
    /// cause, never another discard cause.
    fn discard(&mut self, cause: StopCause) {
        if self.cause.is_none_or(StopCause::is_save) {
            self.cause = Some(cause);
        }
        self.kill();
    }

    fn kill(&mut self) {
        if !self.killed {
            self.killed = true;
            // `SIGKILL` after `SIGINT` is an escalation.
            self.escalated = self.interrupted.is_some();
            signal_group(self.pid, Signal::KILL, true);
        }
    }

    fn apply(&mut self, cmd: Cmd, lease: &mut BootInstant, now: std::time::Instant) {
        match cmd {
            Cmd::OwnerStop => self.save(StopCause::OwnerStop, now),
            Cmd::Discard => self.discard(StopCause::Discard),
            Cmd::Renew(until) => *lease = (*lease).max(until),
        }
    }
}

/// Starts the child, runs the timers until it ends, reaps it, kills what is
/// left of its group and finishes the drains. Returns `None` when the child
/// did not start (the failure went to `ready`).
fn supervise(
    mut cmd: Command,
    d: Deadlines,
    clocks: &dyn Clocks,
    rx: &Receiver<Cmd>,
    ready: &Sender<Result<(), Fail>>,
) -> Option<Exit> {
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!(kind = ?e.kind(), "cannot start the recorder");
            drop(ready.send(Err(failed())));
            return None;
        }
    };
    let pid = child.id();
    let drains = match (child.stdout.take(), child.stderr.take()) {
        (Some(out), Some(err)) => Drains::start(out, err).ok(),
        _ => None,
    };
    let Some(drains) = drains else {
        signal_group(pid, Signal::KILL, true);
        drop(child.wait());
        drop(ready.send(Err(failed())));
        return None;
    };
    if ready.send(Ok(())).is_err() {
        // The caller is gone: nobody waits for this recorder.
        signal_group(pid, Signal::KILL, true);
        drop(child.wait());
        drain_ends(drains);
        return None;
    }
    let (status, plan) = run(&mut child, pid, d, clocks, rx);
    // A member that stayed in the group must not keep a pipe or the file
    // open: the group has no other use now.
    let leftover = signal_group(pid, Signal::KILL, false);
    let escalated = plan.escalated || (leftover && plan.interrupted.is_some());
    let (stderr_tail, drain_timed_out) = drains.finish();
    let xfsz = status.is_some_and(|s| {
        std::os::unix::process::ExitStatusExt::signal(&s) == Some(Signal::XFSZ.as_raw())
    });
    Some(Exit {
        status,
        cause: plan.cause,
        escalated,
        xfsz,
        stderr_tail,
        drain_timed_out,
    })
}

fn drain_ends(drains: Drains) {
    drop(drains.finish());
}

/// The timer loop. Returns the reaped status and the decisions.
fn run(
    child: &mut Child,
    pid: u32,
    d: Deadlines,
    clocks: &dyn Clocks,
    rx: &Receiver<Cmd>,
) -> (Option<ExitStatus>, Plan) {
    let start = clocks.mono();
    let watch = SuspendWatch::start(clocks);
    let mut lease = d.lease_until;
    let mut plan = Plan {
        pid,
        cause: None,
        interrupted: None,
        killed: false,
        escalated: false,
    };
    let mut open = true;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return (Some(status), plan),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(kind = ?e.kind(), "cannot wait for the recorder");
                plan.discard(StopCause::Discard);
                return (child.wait().ok(), plan);
            }
        }
        if open {
            match rx.recv_timeout(POLL) {
                Ok(cmd) => plan.apply(cmd, &mut lease, clocks.mono()),
                Err(RecvTimeoutError::Timeout) => {}
                // The handle was dropped: nobody owns this recording.
                Err(RecvTimeoutError::Disconnected) => {
                    open = false;
                    plan.discard(StopCause::Discard);
                }
            }
            while let Ok(cmd) = rx.try_recv() {
                plan.apply(cmd, &mut lease, clocks.mono());
            }
        } else {
            std::thread::sleep(POLL);
        }
        let now = clocks.mono();
        let elapsed = now.saturating_duration_since(start);
        if !plan.killed {
            if watch.suspended_with(clocks) {
                plan.discard(StopCause::Suspended);
            } else if elapsed >= d.total {
                plan.discard(StopCause::Deadline);
            } else if clocks.boot() >= lease {
                plan.discard(StopCause::LeaseExpired);
            } else if elapsed >= d.duration {
                plan.save(StopCause::Duration, now);
            }
        }
        if !plan.killed
            && plan
                .interrupted
                .is_some_and(|at| now.saturating_duration_since(at) >= d.grace)
        {
            plan.kill();
        }
    }
}

#[cfg(test)]
#[path = "recorder_tests.rs"]
mod tests;
