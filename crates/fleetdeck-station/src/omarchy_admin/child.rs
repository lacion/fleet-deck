//! The runner of the fixed children (design `omarchy-admin.md` §3.1).
//!
//! A child runs in its own process group with stdin on `/dev/null`, unless
//! the spec has stdin bytes (the repair actions, at most 64 KiB): the bytes
//! go into a pipe that is closed after the write. The station reads stdout up to 64 KiB and stderr up to 16 KiB, both at the
//! same time. A longer output or the time limit kills the whole group
//! (`SIGKILL`) and reaps the child. A normal exit and a dropped run kill the
//! group too, so a subprocess that a tool left behind does not stay alive.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// The most stdout bytes that the station reads.
pub const STDOUT_CAP: usize = 64 * 1024;
/// The most stderr bytes that the station reads.
pub const STDERR_CAP: usize = 16 * 1024;
/// The most stdin bytes that the station writes.
pub const STDIN_CAP: usize = 64 * 1024;
/// The longest wait for the reap of a killed child.
const REAP_WAIT: Duration = Duration::from_secs(5);

/// The pause between two checks for the exit of the leader.
const POLL_EXIT: Duration = Duration::from_millis(20);

/// How one child ended.
#[derive(Debug)]
pub enum RunResult {
    Exit {
        /// `None` when a signal ended the child.
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    TimedOut,
    TooLarge,
}

/// What to run: the program, its argv, the environment, the stdin and the
/// limit.
pub struct Spec<'a> {
    pub program: &'a Path,
    pub args: &'a [OsString],
    /// The complete base environment of the child.
    pub env: &'a [(OsString, OsString)],
    /// Variables set after the base environment.
    pub extra: &'a [(&'static str, OsString)],
    /// `None`: stdin is `/dev/null`. `Some`: these bytes (at most
    /// [`STDIN_CAP`]) are written to a pipe, which is then closed.
    pub stdin: Option<Vec<u8>>,
    pub limit: Duration,
}

/// Kills the process group of a child at drop, unless it is disarmed.
struct GroupGuard(Option<Pid>);

impl GroupGuard {
    fn kill(&self) {
        if let Some(pgid) = self.0
            && let Err(e) = kill_process_group(pgid, Signal::KILL)
            && e != rustix::io::Errno::SRCH
        {
            tracing::warn!(error = %e, "cannot kill the Omarchy child group");
        }
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Runs the child of `spec`.
///
/// # Errors
/// The stdin is longer than [`STDIN_CAP`], the child did not start, or it
/// has no process id.
pub async fn run(spec: &Spec<'_>) -> io::Result<RunResult> {
    if spec.stdin.as_ref().is_some_and(|b| b.len() > STDIN_CAP) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the stdin of the child is too long",
        ));
    }
    let mut child = Command::new(spec.program)
        .args(spec.args)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .envs(spec.extra.iter().map(|(k, v)| (*k, v)))
        .process_group(0)
        .kill_on_drop(true)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pgid = child
        .id()
        .and_then(|p| i32::try_from(p).ok())
        .and_then(Pid::from_raw);
    let Some(pgid) = pgid else {
        let _ = child.kill().await;
        return Err(io::Error::other("the child has no process id"));
    };
    let mut guard = GroupGuard(Some(pgid));
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdin = child.stdin.take();
    let work = async {
        let (out, err, ()) = tokio::try_join!(
            read_capped(stdout, STDOUT_CAP),
            read_capped(stderr, STDERR_CAP),
            write_stdin(stdin, spec.stdin.as_deref())
        )?;
        // The leader must stay unreaped here (see the normal-exit branch).
        wait_exited(pgid).await?;
        Ok::<_, Overflow>((out, err))
    };
    match tokio::time::timeout(spec.limit, work).await {
        Ok(Ok((stdout, stderr))) => {
            // A subprocess that the tool left behind must not outlive the
            // action. The kill comes before the reap: an unreaped leader
            // keeps its process id, so the group id is still its own. After
            // the reap another process group could own the number.
            guard.kill();
            let status = child.wait().await;
            guard.0 = None;
            match status {
                Ok(status) => Ok(exit(status, stdout, stderr)),
                Err(_) => Err(io::Error::other("the child could not be awaited")),
            }
        }
        Ok(Err(Overflow::TooLarge)) => {
            stop(&guard, &mut child).await;
            guard.0 = None;
            Ok(RunResult::TooLarge)
        }
        Ok(Err(Overflow::Wait)) => {
            stop(&guard, &mut child).await;
            guard.0 = None;
            Err(io::Error::other("the child could not be awaited"))
        }
        Err(_) => {
            stop(&guard, &mut child).await;
            guard.0 = None;
            Ok(RunResult::TimedOut)
        }
    }
}

fn exit(status: ExitStatus, stdout: Vec<u8>, stderr: Vec<u8>) -> RunResult {
    RunResult::Exit {
        code: status.code(),
        stdout,
        stderr,
    }
}

/// Waits until the leader `pid` has exited, and does not reap it (`NOWAIT`).
/// The state is polled every 20 ms.
async fn wait_exited(pid: Pid) -> Result<(), Overflow> {
    loop {
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG,
        ) {
            Ok(Some(_)) => return Ok(()),
            Ok(None) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Err(Overflow::Wait),
        }
        tokio::time::sleep(POLL_EXIT).await;
    }
}

/// Kills the group and reaps the child.
async fn stop(guard: &GroupGuard, child: &mut tokio::process::Child) {
    guard.kill();
    if tokio::time::timeout(REAP_WAIT, child.wait()).await.is_err() {
        tracing::warn!("an Omarchy child did not end after the group kill");
    }
}

enum Overflow {
    TooLarge,
    Wait,
}

/// Writes `bytes` to the pipe `stdin` and closes it. A child that ends
/// before it reads everything breaks the pipe: the exit code of the child
/// still decides the result, so a write error is only logged.
async fn write_stdin(
    stdin: Option<tokio::process::ChildStdin>,
    bytes: Option<&[u8]>,
) -> Result<(), Overflow> {
    let (Some(mut pipe), Some(bytes)) = (stdin, bytes) else {
        return Ok(());
    };
    if let Err(e) = pipe.write_all(bytes).await {
        tracing::debug!(error = %e, "the Omarchy child did not read its stdin");
    }
    // The drop of `pipe` closes the write end.
    Ok(())
}

/// Reads `reader` to its end. More than `cap` bytes is `TooLarge`.
async fn read_capped<R: AsyncRead + Unpin>(
    reader: Option<R>,
    cap: usize,
) -> Result<Vec<u8>, Overflow> {
    let Some(mut reader) = reader else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut buf = [0_u8; 8192];
    loop {
        // A read error ends the stream like an end of file: the exit code
        // still decides the result.
        let n = reader.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
            return Ok(out);
        }
        if out.len() + n > cap {
            return Err(Overflow::TooLarge);
        }
        out.extend_from_slice(&buf[..n]);
    }
}

/// The text of a stderr output for the `message` of an answer: lossy UTF-8,
/// every control character except the line feed removed, at most 2,000
/// characters. An empty text is `None`.
#[must_use]
pub fn clean_message(stderr: &[u8]) -> Option<String> {
    let text: String = String::from_utf8_lossy(stderr)
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .take(2000)
        .collect();
    (!text.is_empty()).then_some(text)
}
