//! The runner of desktop programs (R13 §6.4, §5A item 8): every program is
//! a fixed executable name that is resolved on the desktop `PATH`, started
//! with an argv `Vec<String>`. Never a shell, never a Hyprland `exec`, never
//! a batch. Stdin is null; stdout is capped at 1 MiB and stderr at 16 KiB.
//! The shape follows `integrations/exec.rs`.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use super::authority::Start;
use super::{Fail, Gate, code};

/// The time limit of one program.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// The time limit of a launch that waits for a window.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
/// The most bytes of stdout that a program may write.
pub const MAX_STDOUT: usize = 1024 * 1024;
/// The most bytes of stderr that the runner keeps.
pub const MAX_STDERR: usize = 16 * 1024;
/// How long the runner waits for the pipes after the program exited: a
/// program that started a background process keeps them open.
const PIPE_GRACE: Duration = Duration::from_millis(300);

/// What a program returned.
#[derive(Debug)]
pub struct Out {
    /// The exit code; -1 for a signal.
    pub code: i32,
    pub stdout: String,
}

/// The absolute path of the executable `name` on the `PATH` of `env`.
#[must_use]
pub fn resolve(env: &ChildEnv, name: &str) -> Option<PathBuf> {
    let path = env.get("PATH")?;
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(name))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// Whether the executable `name` is on the `PATH` of `env`.
#[must_use]
pub fn on_path(env: &ChildEnv, name: &str) -> bool {
    resolve(env, name).is_some()
}

fn not_installed(name: &str) -> Fail {
    Fail::new(
        code::UNSUPPORTED,
        format!("`{name}` is not installed on this desktop"),
    )
}

/// Runs programs with one environment. A gated runner reads the live policy
/// right before every program run and refuses when `allow_desktop` is off.
/// A runner with a `start` check runs it before every program: the first
/// call checks the authority of the action (R13 §5E item 1).
pub struct Runner<'a> {
    pub env: &'a ChildEnv,
    pub gate: Option<Gate<'a>>,
    pub start: Option<&'a Start>,
}

impl Runner<'_> {
    fn check(&self) -> Result<(), Fail> {
        match self.gate {
            Some(gate) if !gate() => Err(Fail::forbidden()),
            _ => Ok(()),
        }
    }

    async fn command(&self, program: &str, args: &[String]) -> Result<Command, Fail> {
        self.check()?;
        if let Some(start) = self.start {
            start.begin(self.env).await?;
        }
        let path = resolve(self.env, program).ok_or_else(|| not_installed(program))?;
        let mut cmd = Command::new(path);
        cmd.args(args)
            .env_clear()
            .envs(self.env.iter())
            .stdin(Stdio::null());
        Ok(cmd)
    }

    /// Runs `program` and waits for it.
    ///
    /// # Errors
    /// `forbidden`, `unsupported` (not installed), or `failed` (cannot start,
    /// time limit, too much output).
    pub async fn run(&self, program: &str, args: &[String], limit: Duration) -> Result<Out, Fail> {
        let mut cmd = self.command(program, args).await?;
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| spawn_failed(program, &e))?;
        let out_task = tokio::spawn(read_capped(child.stdout.take(), MAX_STDOUT));
        let err_task = tokio::spawn(read_capped(child.stderr.take(), MAX_STDERR));
        let status = match tokio::time::timeout(limit, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(e)) => return Err(spawn_failed(program, &e)),
            Err(_) => {
                return Err(Fail::new(
                    code::FAILED,
                    format!("`{program}` did not answer in {} s", limit.as_secs()),
                ));
            }
        };
        let stdout = grace(out_task).await;
        // The text of stderr is not used: it can name paths and values.
        let _ = grace(err_task).await;
        if stdout.len() > MAX_STDOUT {
            return Err(Fail::new(
                code::FAILED,
                format!("`{program}` returned too much data"),
            ));
        }
        Ok(Out {
            code: status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
        })
    }

    /// Like [`Runner::run`], and an exit code other than 0 is `failed`. The
    /// message holds the program name and the code, never the argv.
    ///
    /// # Errors
    /// As [`Runner::run`], and `failed` for a non-zero exit code.
    pub async fn run_ok(
        &self,
        program: &str,
        args: &[String],
        limit: Duration,
    ) -> Result<Out, Fail> {
        let out = self.run(program, args, limit).await?;
        if out.code == 0 {
            Ok(out)
        } else {
            Err(Fail::new(
                code::FAILED,
                format!("`{program}` failed with exit code {}", out.code),
            ))
        }
    }

    /// Starts `program` detached from the runner: no pipes, and it is not
    /// killed when the handle is dropped. The caller waits for its window.
    ///
    /// # Errors
    /// `forbidden`, `unsupported` (not installed), or `failed`.
    pub async fn spawn(&self, program: &str, args: &[String]) -> Result<Child, Fail> {
        let mut cmd = self.command(program, args).await?;
        cmd.stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(false);
        cmd.spawn().map_err(|e| spawn_failed(program, &e))
    }
}

fn spawn_failed(name: &str, e: &std::io::Error) -> Fail {
    if e.kind() == std::io::ErrorKind::NotFound {
        not_installed(name)
    } else {
        Fail::new(code::FAILED, format!("cannot start `{name}`: {}", e.kind()))
    }
}

async fn read_capped<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>, max: usize) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(pipe) = pipe {
        // One byte more than the limit tells the caller about a cut.
        let mut limited = pipe.take(u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1));
        // A read error ends the output; the exit code tells the rest.
        let _ = limited.read_to_end(&mut out).await;
    }
    out
}

/// The result of a read task, or nothing when the pipe stays open.
async fn grace(mut task: tokio::task::JoinHandle<Vec<u8>>) -> Vec<u8> {
    match tokio::time::timeout(PIPE_GRACE, &mut task).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => Vec::new(),
        Err(_) => {
            task.abort();
            Vec::new()
        }
    }
}
