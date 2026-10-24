//! The binary runner of the screenshot (design §5.1): `grim -t png -s 1 -l 6
//! -o <monitor> -` writes the PNG on its stdout. The runner streams stdout
//! into a retained descriptor under a byte cap, never as text, and never
//! logs it. It never falls back to another tool, a portal, a recorder or a
//! path.
//!
//! Threads: a dedicated OS thread launches the child and stays alive until
//! the child is reaped (Linux parent-death signals follow the spawning
//! thread). It also times the run and kills the process group. A second
//! thread drains stderr. The calling thread copies stdout, so the `room`
//! check and the output descriptor stay on the caller's thread.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::capture::{code, text};
use rustix::process::{Pid, Signal};

use super::{COPY_BUFFER, GRIM_TIMEOUT, STDERR_KEEP};
use crate::desktop::run::resolve;
use crate::desktop::{Fail, code as desktop_code};

/// The size of one stderr read.
const DRAIN_BUFFER: usize = 4096;
/// How often the supervisor checks a child that closed stdout.
const EXIT_POLL: Duration = Duration::from_millis(5);

/// How the runner starts `grim`.
#[derive(Debug, Clone)]
pub enum Launch {
    /// Production: `<exe> shim-exec --parent <station pid> -- <grim> …`. The
    /// shim makes the child a session and group leader and arms the
    /// parent-death `SIGKILL`.
    ShimExec { exe: PathBuf },
    /// Tests of the station crate: `<grim> …` as the leader of its own
    /// process group.
    #[cfg(test)]
    Direct,
}

/// The pipes of the started child.
struct Pipes {
    stdout: ChildStdout,
    stderr: ChildStderr,
}

/// How the child ended.
struct End {
    status: Option<ExitStatus>,
    timed_out: bool,
}

/// Why the copy of stdout stopped before EOF.
enum Stop {
    Overflow,
    Room(Fail),
    Write(std::io::Error),
    Read,
}

fn failed() -> Fail {
    Fail::new(code::CAPTURE_FAILED, text::CAPTURE_FAILED)
}

fn invalid_monitor() -> Fail {
    Fail::new(
        desktop_code::INVALID_PARAMS,
        "`monitor` is not a valid monitor name",
    )
}

/// 1-64 of ASCII letters, digits, `.`, `_`, `-`.
fn monitor_name_ok(monitor: &str) -> bool {
    (1..=64).contains(&monitor.len())
        && monitor
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn command(env: &ChildEnv, launch: &Launch, grim: &Path, monitor: &str) -> Command {
    let grim_args = ["-t", "png", "-s", "1", "-l", "6", "-o", monitor, "-"];
    let mut cmd = match launch {
        Launch::ShimExec { exe } => {
            let mut cmd = Command::new(exe);
            cmd.arg("shim-exec")
                .arg("--parent")
                .arg(std::process::id().to_string())
                .arg("--")
                .arg(grim);
            cmd
        }
        #[cfg(test)]
        Launch::Direct => {
            use std::os::unix::process::CommandExt as _;
            let mut cmd = Command::new(grim);
            cmd.process_group(0);
            cmd
        }
    };
    cmd.args(grim_args)
        .env_clear()
        .envs(env.iter())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// `SIGKILL` to the process group of `pid`. `fallback`: when the group does
/// not exist yet (the shim has not made it), signal the child itself.
fn kill_group(pid: u32, fallback: bool) {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .filter(|p| *p > 1)
        .and_then(Pid::from_raw)
    else {
        return;
    };
    match rustix::process::kill_process_group(pid, Signal::KILL) {
        Ok(()) => {}
        Err(rustix::io::Errno::SRCH) if fallback => {
            if let Err(e) = rustix::process::kill_process(pid, Signal::KILL)
                && e != rustix::io::Errno::SRCH
            {
                tracing::warn!(error = %e, "cannot signal the screenshot tool");
            }
        }
        Err(rustix::io::Errno::SRCH) => {}
        Err(e) => tracing::warn!(error = %e, "cannot signal the screenshot tool group"),
    }
}

/// Starts the child, hands its pipes to the caller, waits for the caller's
/// verdict or the time limit, kills the group when needed and reaps the
/// child. The thread ends after the reap.
fn supervise(
    mut cmd: Command,
    limit: Instant,
    ready: &mpsc::Sender<Result<Pipes, Fail>>,
    done: &mpsc::Receiver<bool>,
) -> End {
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!(kind = ?e.kind(), "cannot start the screenshot tool");
            drop(ready.send(Err(failed())));
            return End {
                status: None,
                timed_out: false,
            };
        }
    };
    let pid = child.id();
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        kill_group(pid, true);
        return reap(&mut child, false);
    };
    if ready.send(Ok(Pipes { stdout, stderr })).is_err() {
        // The caller is gone: nobody reads the pipes.
        kill_group(pid, true);
        return reap(&mut child, false);
    }
    let wait = limit.saturating_duration_since(Instant::now());
    match done.recv_timeout(wait) {
        Ok(true) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            kill_group(pid, true);
            reap(&mut child, false)
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            kill_group(pid, true);
            reap(&mut child, true)
        }
        Ok(false) => loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    // A process that stayed in the group must not keep a
                    // pipe open: the group has no other use now.
                    kill_group(pid, false);
                    return End {
                        status: Some(status),
                        timed_out: false,
                    };
                }
                Ok(None) if Instant::now() < limit => std::thread::sleep(EXIT_POLL),
                Ok(None) => {
                    kill_group(pid, true);
                    return reap(&mut child, true);
                }
                Err(e) => {
                    tracing::warn!(kind = ?e.kind(), "cannot wait for the screenshot tool");
                    kill_group(pid, true);
                    return reap(&mut child, false);
                }
            }
        },
    }
}

fn reap(child: &mut Child, timed_out: bool) -> End {
    let status = match child.wait() {
        Ok(status) => Some(status),
        Err(e) => {
            tracing::warn!(kind = ?e.kind(), "cannot reap the screenshot tool");
            None
        }
    };
    End { status, timed_out }
}

/// Reads stderr to EOF, keeps at most `STDERR_KEEP` bytes and drops the
/// rest. Returns the kept length; the bytes are never returned or logged.
fn drain(mut stderr: ChildStderr) -> usize {
    let mut kept: Vec<u8> = Vec::new();
    let mut buf = [0_u8; DRAIN_BUFFER];
    loop {
        match stderr.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let take = n.min(STDERR_KEEP - kept.len());
                kept.extend_from_slice(&buf[..take]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            // A read error ends the drain; the exit status tells the rest.
            Err(_) => break,
        }
    }
    kept.len()
}

/// Copies stdout into `output`: reads of at most `COPY_BUFFER`, one byte
/// more than the cap to tell an exact-cap EOF from an overflow, `room`
/// before each write, and no write of a buffer that crosses `byte_limit`.
fn copy(
    mut stdout: ChildStdout,
    output: &mut File,
    byte_limit: u64,
    room: &dyn Fn(u64) -> Result<(), Fail>,
) -> Result<u64, Stop> {
    let mut buf = vec![0_u8; COPY_BUFFER];
    let mut written = 0_u64;
    loop {
        let left = byte_limit.saturating_sub(written);
        let want =
            usize::try_from(left.saturating_add(1)).map_or(COPY_BUFFER, |l| l.min(COPY_BUFFER));
        let n = match stdout.read(&mut buf[..want]) {
            Ok(0) => return Ok(written),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(Stop::Read),
        };
        let n64 = n as u64;
        if n64 > left {
            return Err(Stop::Overflow);
        }
        room(n64).map_err(Stop::Room)?;
        output.write_all(&buf[..n]).map_err(Stop::Write)?;
        written += n64;
    }
}

fn stop_fail(stop: Stop) -> Fail {
    match stop {
        Stop::Overflow => Fail::new(code::CAPTURE_TOO_LARGE, text::CAPTURE_TOO_LARGE),
        Stop::Room(fail) => fail,
        Stop::Write(e) if e.raw_os_error() == Some(rustix::io::Errno::NOSPC.raw_os_error()) => {
            Fail::new(code::DISK_FULL, text::DISK_FULL)
        }
        Stop::Write(_) | Stop::Read => failed(),
    }
}

/// Takes one screenshot of `monitor` into `output` and returns the number of
/// bytes written. Blocking: a caller in an async task uses `spawn_blocking`.
///
/// The run ends at `min(deadline, now + GRIM_TIMEOUT)`. On a timeout, an
/// output overflow, a write failure or a `room` refusal the whole process
/// group gets `SIGKILL`, the child is reaped and the pipes are drained
/// before this returns.
///
/// # Errors
/// `invalid_params` for a `monitor` that breaks the grammar; `unsupported`
/// (`NO_GRIM`) when `grim` is not on the desktop PATH of `env`;
/// `capture_too_large` for an output over `byte_limit`; the `Fail` of `room`;
/// `disk_full` for a write that fails with `ENOSPC`; `capture_failed` for a
/// timeout, a nonzero exit, a signal or any other failure.
pub fn capture_png(
    env: &ChildEnv,
    launch: &Launch,
    monitor: &str,
    output: &mut File,
    byte_limit: u64,
    deadline: Instant,
    room: &dyn Fn(u64) -> Result<(), Fail>,
) -> Result<u64, Fail> {
    if !monitor_name_ok(monitor) {
        return Err(invalid_monitor());
    }
    let grim =
        resolve(env, "grim").ok_or_else(|| Fail::new(desktop_code::UNSUPPORTED, text::NO_GRIM))?;
    let limit = deadline.min(Instant::now() + GRIM_TIMEOUT);
    let cmd = command(env, launch, &grim, monitor);

    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let supervisor = std::thread::Builder::new()
        .name("capture-grim".into())
        .spawn(move || supervise(cmd, limit, &ready_tx, &done_rx))
        .map_err(|_| failed())?;
    let pipes = match ready_rx.recv() {
        Ok(Ok(pipes)) => pipes,
        Ok(Err(fail)) => {
            drop(supervisor.join());
            return Err(fail);
        }
        Err(_) => {
            drop(supervisor.join());
            return Err(failed());
        }
    };
    let Pipes { stdout, stderr } = pipes;
    let drainer = std::thread::Builder::new()
        .name("capture-grim-stderr".into())
        .spawn(move || drain(stderr));
    let copied = match &drainer {
        Ok(_) => copy(stdout, output, byte_limit, room),
        Err(_) => Err(Stop::Read),
    };
    // A failed copy kills the group; a copy to EOF waits for the exit. The
    // supervisor is already gone after a timeout kill: the send then fails
    // and there is nothing left to tell it.
    let _ = done_tx.send(copied.is_err());
    let end = supervisor.join();
    if let Ok(Ok(kept)) = drainer.map(|d| d.join()) {
        tracing::debug!(stderr_bytes = kept, "the screenshot tool wrote to stderr");
    }
    let end = end.map_err(|_| failed())?;
    let written = copied.map_err(stop_fail)?;
    match end.status {
        Some(status) if status.success() && !end.timed_out => Ok(written),
        _ => Err(failed()),
    }
}
