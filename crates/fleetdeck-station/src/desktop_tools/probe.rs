//! The children of the desktop tool reads: a test seam ([`Exec`]), the real
//! runner on top of `omarchy_admin::child`, and the limits of one call
//! ([`Probes`]): four children at the same time, five seconds for each child
//! and a total time for the whole call (`board-gaps.md` §2.3).
//!
//! Every argv is fixed by the caller. This module runs no program on its own.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use futures_util::future::BoxFuture;
use tokio::sync::Semaphore;
use tokio::time::Instant;

use crate::omarchy_admin::child::{self, RunResult, Spec};

/// The longest run of one child.
pub const PROBE_LIMIT: Duration = Duration::from_secs(5);
/// The longest time of one `station.tools` call.
pub const TOTAL_LIMIT: Duration = Duration::from_secs(20);
/// The most children that run at the same time in one call.
pub const MAX_CHILDREN: usize = 4;
/// The most stdout bytes of a child that the station accepts.
pub const MAX_STDOUT: usize = 16 * 1024;

/// How one child ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeRun {
    Exit { code: i32, stdout: Vec<u8> },
    Timeout,
    Failed,
}

/// Runs the fixed children and checks paths. Tests replace it.
pub trait Exec: Send + Sync {
    /// Runs `program` with `args` and exactly `env`, with stdin on
    /// `/dev/null`, for at most `limit`.
    fn run(
        &self,
        program: &Path,
        args: &[OsString],
        env: &[(OsString, OsString)],
        limit: Duration,
    ) -> BoxFuture<'_, ProbeRun>;

    /// Whether `path` exists.
    fn exists(&self, path: &Path) -> bool;
}

/// The real runner: the process group runner of the Omarchy children.
pub struct RealExec;

impl Exec for RealExec {
    fn run(
        &self,
        program: &Path,
        args: &[OsString],
        env: &[(OsString, OsString)],
        limit: Duration,
    ) -> BoxFuture<'_, ProbeRun> {
        let program = program.to_owned();
        let args = args.to_vec();
        let env = env.to_vec();
        Box::pin(async move {
            let spec = Spec {
                program: &program,
                args: &args,
                env: &env,
                extra: &[],
                stdin: None,
                limit,
            };
            match child::run(&spec).await {
                Ok(RunResult::Exit {
                    code: Some(code),
                    stdout,
                    ..
                }) if stdout.len() <= MAX_STDOUT => ProbeRun::Exit { code, stdout },
                Ok(RunResult::TimedOut) => ProbeRun::Timeout,
                Ok(_) => ProbeRun::Failed,
                Err(e) => {
                    tracing::debug!(error = %e, "a desktop tool probe did not run");
                    ProbeRun::Failed
                }
            }
        })
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

/// The limits of one call: the gate of four children and the end of the
/// total time.
pub struct Probes<'a> {
    exec: &'a dyn Exec,
    gate: Semaphore,
    deadline: Instant,
}

impl<'a> Probes<'a> {
    /// The limits of a call that starts now and lasts `total` at most.
    #[must_use]
    pub fn new(exec: &'a dyn Exec, total: Duration) -> Self {
        Self {
            exec,
            gate: Semaphore::new(MAX_CHILDREN),
            deadline: Instant::now() + total,
        }
    }

    /// Whether `path` exists.
    #[must_use]
    pub fn exists(&self, path: &Path) -> bool {
        self.exec.exists(path)
    }

    /// Runs one child. The limit of the child is the smaller of `cap` and
    /// the time left of the call. A child with no time left is `Timeout`
    /// and is not started.
    pub async fn run(
        &self,
        program: &Path,
        args: &[OsString],
        env: &[(OsString, OsString)],
        cap: Duration,
    ) -> ProbeRun {
        let permit = match tokio::time::timeout_at(self.deadline, self.gate.acquire()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_closed)) => return ProbeRun::Failed,
            Err(_elapsed) => return ProbeRun::Timeout,
        };
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return ProbeRun::Timeout;
        }
        let run = self.exec.run(program, args, env, cap.min(left)).await;
        drop(permit);
        run
    }
}

/// The variables of `env` as the pairs that a child gets.
#[must_use]
pub fn env_pairs(env: &ChildEnv) -> Vec<(OsString, OsString)> {
    env.iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// The fixed environment of the package manager children.
#[must_use]
pub fn pacman_env() -> Vec<(OsString, OsString)> {
    vec![
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (OsString::from("LANG"), OsString::from("C")),
    ]
}
