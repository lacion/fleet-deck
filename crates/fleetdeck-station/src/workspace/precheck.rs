//! The shell precheck of a schedule (R5, DL-168): `workspace.precheck`.
//!
//! The deck owns the schedule; the station runs the owner's command, as it
//! runs a setup command. The command runs under `/bin/sh -c` in its own
//! process group, with the scrubbed station environment and no input. It has
//! a limit of 60 s. When it ends or times out, the whole process group is
//! stopped (TERM, then KILL), so a background child does not stay.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::link::PrecheckResult;
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::{Instant, sleep, timeout as time_limit};

/// The longest run of a precheck.
pub const TIMEOUT: Duration = Duration::from_secs(60);
/// The longest precheck command (characters), as a setup command.
pub const MAX_CMD_CHARS: usize = 4096;
/// The size of the output tail in an answer.
pub const TAIL_BYTES: usize = 4096;

/// The wait for the process group after each signal.
const GROUP_WAIT: Duration = Duration::from_secs(5);
const GROUP_POLL: Duration = Duration::from_millis(50);
/// The wait for the output readers after the group is gone.
const READER_WAIT: Duration = Duration::from_secs(1);

/// Why a precheck gave no answer about the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrecheckError {
    /// The command is empty, too long, or holds a NUL.
    Invalid(String),
    /// The shell did not start, or a process did not stop.
    Run(String),
}

/// Checks the text of a command.
///
/// # Errors
/// [`PrecheckError::Invalid`].
pub fn check_cmd(cmd: &str) -> Result<(), PrecheckError> {
    if cmd.trim().is_empty() {
        return Err(PrecheckError::Invalid(
            "the precheck command is empty".into(),
        ));
    }
    if cmd.chars().count() > MAX_CMD_CHARS {
        return Err(PrecheckError::Invalid(format!(
            "the precheck command has more than {MAX_CMD_CHARS} characters"
        )));
    }
    if cmd.contains('\0') {
        return Err(PrecheckError::Invalid(
            "the precheck command has a NUL character".into(),
        ));
    }
    Ok(())
}

/// The captured output: the last [`TAIL_BYTES`] bytes of both streams.
#[derive(Default)]
struct Tail(Mutex<VecDeque<u8>>);

impl Tail {
    fn push(&self, chunk: &[u8]) {
        let Ok(mut tail) = self.0.lock() else { return };
        tail.extend(chunk);
        let extra = tail.len().saturating_sub(TAIL_BYTES);
        tail.drain(..extra);
    }

    fn text(&self) -> String {
        let Ok(tail) = self.0.lock() else {
            return String::new();
        };
        let bytes: Vec<u8> = tail.iter().copied().collect();
        super::provision::scrub(&String::from_utf8_lossy(&bytes))
    }
}

async fn read_into(mut stream: impl AsyncRead + Unpin, tail: Arc<Tail>) {
    let mut buf = vec![0_u8; 4096];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => tail.push(&buf[..n]),
        }
    }
}

/// Runs `cmd` in `cwd` and waits for it to end or for `timeout`. When this
/// returns `Ok`, no process of the group is left.
///
/// # Errors
/// [`PrecheckError`].
pub async fn run(
    cmd: &str,
    cwd: &Path,
    env: &ChildEnv,
    timeout: Duration,
) -> Result<PrecheckResult, PrecheckError> {
    check_cmd(cmd)?;
    let mut child = Command::new("/bin/sh")
        .args(["-c", cmd])
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter())
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PrecheckError::Run(format!("the precheck did not start: {e}")))?;
    let Some(pgid) = child
        .id()
        .and_then(|p| i32::try_from(p).ok())
        .and_then(Pid::from_raw)
    else {
        let _ = child.kill().await;
        return Err(PrecheckError::Run("the precheck has no process id".into()));
    };
    let tail = Arc::new(Tail::default());
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|s| tokio::spawn(read_into(s, Arc::clone(&tail)))),
        child
            .stderr
            .take()
            .map(|s| tokio::spawn(read_into(s, Arc::clone(&tail)))),
    ]
    .into_iter()
    .flatten()
    .collect();
    let waited = time_limit(timeout, child.wait()).await;
    let gone = stop_group(pgid, &mut child).await;
    for mut reader in readers {
        if time_limit(READER_WAIT, &mut reader).await.is_err() {
            reader.abort();
        }
    }
    if !gone {
        return Err(PrecheckError::Run("a precheck process did not stop".into()));
    }
    match waited {
        Ok(Ok(status)) => Ok(PrecheckResult {
            exit_code: status.code(),
            timed_out: false,
            output_tail: tail.text(),
        }),
        Ok(Err(e)) => Err(PrecheckError::Run(format!(
            "the precheck status is unknown: {e}"
        ))),
        Err(_) => Ok(PrecheckResult {
            exit_code: None,
            timed_out: true,
            output_tail: tail.text(),
        }),
    }
}

/// Stops the process group: TERM, wait, KILL, wait. The shell itself is
/// reaped here. Returns whether the group is gone.
async fn stop_group(pgid: Pid, child: &mut tokio::process::Child) -> bool {
    for sig in [Signal::TERM, Signal::KILL] {
        if let Err(e) = kill_process_group(pgid, sig)
            && e != rustix::io::Errno::SRCH
        {
            tracing::warn!(?sig, error = %e, "cannot signal the precheck process group");
        }
        if group_gone(pgid, child).await {
            return true;
        }
    }
    false
}

async fn group_gone(pgid: Pid, child: &mut tokio::process::Child) -> bool {
    let deadline = Instant::now() + GROUP_WAIT;
    loop {
        // `try_wait` reaps the shell when it ended after the signal.
        let _ = child.try_wait();
        if matches!(test_kill_process_group(pgid), Err(rustix::io::Errno::SRCH)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(GROUP_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> ChildEnv {
        ChildEnv::from_process()
    }

    #[tokio::test]
    async fn exit_code_and_output_come_back() {
        let dir = tempfile::tempdir().unwrap();
        let ok = run("echo hello; exit 0", dir.path(), &env(), TIMEOUT)
            .await
            .unwrap();
        assert_eq!(ok.exit_code, Some(0));
        assert!(!ok.timed_out);
        assert_eq!(ok.output_tail.trim(), "hello");
        let bad = run("echo no >&2; exit 3", dir.path(), &env(), TIMEOUT)
            .await
            .unwrap();
        assert_eq!(bad.exit_code, Some(3));
        assert_eq!(bad.output_tail.trim(), "no");
    }

    #[tokio::test]
    async fn the_command_runs_in_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker.txt"), "x").unwrap();
        let r = run("test -f marker.txt", dir.path(), &env(), TIMEOUT)
            .await
            .unwrap();
        assert_eq!(r.exit_code, Some(0));
    }

    #[tokio::test]
    async fn a_timeout_stops_the_group_including_a_background_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        let cmd = format!("sleep 30 & echo $! > {}; wait", pid_file.display());
        let r = run(&cmd, dir.path(), &env(), Duration::from_millis(500))
            .await
            .unwrap();
        assert!(r.timed_out);
        assert_eq!(r.exit_code, None);
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let pid = Pid::from_raw(pid).unwrap();
        assert!(
            matches!(
                rustix::process::test_kill_process(pid),
                Err(rustix::io::Errno::SRCH)
            ),
            "the background child is gone"
        );
    }

    #[tokio::test]
    async fn the_output_keeps_only_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let r = run(
            "i=0; while [ $i -lt 2000 ]; do echo line-$i-padding-padding; i=$((i+1)); done",
            dir.path(),
            &env(),
            TIMEOUT,
        )
        .await
        .unwrap();
        assert!(r.output_tail.len() <= TAIL_BYTES);
        assert!(
            r.output_tail
                .trim_end()
                .ends_with("line-1999-padding-padding")
        );
    }

    #[test]
    fn a_bad_command_is_refused() {
        assert!(matches!(check_cmd(" "), Err(PrecheckError::Invalid(_))));
        assert!(matches!(
            check_cmd(&"x".repeat(MAX_CMD_CHARS + 1)),
            Err(PrecheckError::Invalid(_))
        ));
        assert!(matches!(check_cmd("a\0b"), Err(PrecheckError::Invalid(_))));
        assert!(check_cmd("test -f Cargo.toml").is_ok());
    }
}
