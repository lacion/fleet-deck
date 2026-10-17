//! A bounded `git` runner for the workspace service (PLAN §6.6).
//!
//! Every call has a timeout and a fixed, safe set of options:
//!
//! - the variables that point `git` at another repository, index or object
//!   store (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, ...) are removed
//!   from the inherited environment; a caller sets `GIT_INDEX_FILE` only for
//!   a private index;
//! - `git` never prompts (`GIT_TERMINAL_PROMPT=0`) and never takes optional
//!   locks (`GIT_OPTIONAL_LOCKS=0`), so a read never rewrites the real index;
//! - hooks, the file-system monitor and automatic maintenance are off;
//! - with [`GitOptions::isolated`] the system and user configuration are not
//!   read (checkpoints do not need credentials, signing or user aliases).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Variables that redirect `git` to another repository or index.
const REDIRECTING_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_EXTERNAL_DIFF",
    "GIT_DIFF_OPTS",
];

/// Options that every call passes before the subcommand.
const SAFE_CONFIG: &[&str] = &[
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.quotePath=false",
    "-c",
    "gc.auto=0",
    "-c",
    "maintenance.auto=false",
    "-c",
    "diff.external=",
];

/// The longest stderr text kept in an error.
const STDERR_KEEP: usize = 2048;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("cannot start git: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("git {args} did not end within {secs} s")]
    Timeout { args: String, secs: u64 },
    #[error("git {args} failed ({code}): {stderr}")]
    Failed {
        args: String,
        code: String,
        stderr: String,
    },
    #[error("git output is not UTF-8: {0}")]
    Utf8(String),
    /// `fleetdeck confine-exec` failed before it ran Git (B9, DL-200).
    #[error("the station cannot confine Git (fleetdeck confine-exec failed)")]
    Confine,
}

/// The first stderr line of `fleetdeck confine-exec` starts with this.
const CONFINE_PREFIX: &str = "fleetdeck-confine: ";
/// The exit code of `fleetdeck confine-exec` when it failed itself.
const CONFINE_FAILED: i32 = 125;
/// Logs the missing Landlock once per process.
static LANDLOCK_WARNED: std::sync::Once = std::sync::Once::new();

/// Run Git under `fleetdeck confine-exec` (B9, DL-200): writes only below `roots`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confine {
    pub exe: PathBuf,
    pub roots: Vec<PathBuf>,
}

/// How the runner starts `git`.
#[derive(Debug, Clone)]
pub struct GitOptions {
    /// The `git` executable.
    pub program: PathBuf,
    /// The limit for one call.
    pub timeout: Duration,
    /// Do not read the system and user configuration.
    pub isolated: bool,
    /// Run Git under `fleetdeck confine-exec` (B9).
    pub confine: Option<Confine>,
}

impl GitOptions {
    /// `git` from `PATH`, 60 s per call, no system or user configuration.
    #[must_use]
    pub fn isolated() -> Self {
        Self {
            program: PathBuf::from("git"),
            timeout: Duration::from_secs(60),
            isolated: true,
            confine: None,
        }
    }
}

/// One `git` call in a working directory.
#[derive(Debug, Clone)]
pub struct Git {
    dir: PathBuf,
    options: GitOptions,
    /// With a slot, each call runs in its own process group and the slot
    /// holds the pid (the group id) of the running child, else 0.
    group: Option<Arc<AtomicU32>>,
}

impl Git {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>, options: GitOptions) -> Self {
        Self {
            dir: dir.into(),
            options,
            group: None,
        }
    }

    /// Runs each call in its own process group and writes the pid of the
    /// child (the group id) into `slot` right after the spawn. The slot is 0
    /// before a spawn and after a call that ended; a dropped call leaves its
    /// pid there, so the owner can stop the group.
    #[must_use]
    pub fn with_group(mut self, slot: Arc<AtomicU32>) -> Self {
        self.group = Some(slot);
        self
    }

    /// The command that runs Git: the program itself, or `fleetdeck
    /// confine-exec --write-root <root>... -- <program>` (B9).
    fn command(&self) -> Command {
        let Some(confine) = &self.options.confine else {
            return Command::new(&self.options.program);
        };
        let mut cmd = Command::new(&confine.exe);
        cmd.arg("confine-exec");
        for root in &confine.roots {
            cmd.arg("--write-root").arg(root);
        }
        cmd.arg("--").arg(&self.options.program);
        cmd
    }

    /// The exit code and stderr of a confined call (B9): exit 125 is a
    /// helper failure; the status line of the helper is dropped from the
    /// stderr, and `not_enforced` is logged once.
    fn confined(&self, code: i32, stderr: &[u8]) -> Result<Vec<u8>, GitError> {
        if self.options.confine.is_none() {
            return Ok(stderr.to_vec());
        }
        if code == CONFINE_FAILED {
            return Err(GitError::Confine);
        }
        let end = stderr
            .iter()
            .position(|b| *b == b'\n')
            .map_or(stderr.len(), |i| i + 1);
        let first = String::from_utf8_lossy(&stderr[..end]);
        let Some(status) = first.trim_end().strip_prefix(CONFINE_PREFIX) else {
            return Ok(stderr.to_vec());
        };
        if status == "not_enforced" {
            LANDLOCK_WARNED.call_once(|| {
                tracing::warn!(
                    "Landlock is not available: worktree and clone provisioning is checked after Git, not confined."
                );
            });
        }
        Ok(stderr[end..].to_vec())
    }

    /// Puts the child in its own process group when a slot is set.
    fn grouped(&self, cmd: &mut Command) {
        if self.group.is_some() {
            cmd.process_group(0);
        }
    }

    /// Records the pid of a new child in the slot.
    fn spawned(&self, child: &tokio::process::Child) {
        if let Some(slot) = &self.group {
            slot.store(child.id().unwrap_or(0), Ordering::SeqCst);
        }
    }

    /// Clears the slot: no child runs.
    fn ended(&self) {
        if let Some(slot) = &self.group {
            slot.store(0, Ordering::SeqCst);
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Runs `git <args>` and returns its stdout. A non-zero exit is an error.
    pub async fn run(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        self.call(args, &[], None).await
    }

    /// Runs `git <args>` and returns its stdout as text.
    pub async fn text(&self, args: &[&str]) -> Result<String, GitError> {
        let out = self.run(args).await?;
        String::from_utf8(out).map_err(|e| GitError::Utf8(e.to_string()))
    }

    /// Runs `git <args>` with extra environment variables and optional stdin.
    pub async fn call(
        &self,
        args: &[&str],
        env: &[(&str, &OsStr)],
        stdin: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, GitError> {
        match self.status(args, env, stdin).await? {
            (0, out, _) => Ok(out),
            (code, _, stderr) => Err(GitError::Failed {
                args: args.join(" "),
                code: code.to_string(),
                stderr,
            }),
        }
    }

    /// Runs `git <args>` and returns `(exit code, stdout, stderr)`; a signal
    /// gives the code -1.
    pub async fn status(
        &self,
        args: &[&str],
        env: &[(&str, &OsStr)],
        stdin: Option<Vec<u8>>,
    ) -> Result<(i32, Vec<u8>, String), GitError> {
        let mut cmd = self.command();
        cmd.current_dir(&self.dir)
            .args(SAFE_CONFIG)
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        self.grouped(&mut cmd);
        for name in REDIRECTING_VARS {
            cmd.env_remove(name);
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C");
        if self.options.isolated {
            cmd.env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null");
        }
        for (name, value) in env {
            cmd.env(name, value);
        }
        let mut child = cmd.spawn().map_err(GitError::Spawn)?;
        self.spawned(&child);
        if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
            // A writer task, so a large stdin cannot block against a full
            // stdout pipe.
            tokio::spawn(async move {
                let _ = pipe.write_all(&bytes).await;
                let _ = pipe.shutdown().await;
            });
        }
        let secs = self.options.timeout.as_secs();
        let output = tokio::time::timeout(self.options.timeout, child.wait_with_output())
            .await
            .map_err(|_| GitError::Timeout {
                args: args.join(" "),
                secs,
            })?
            .map_err(GitError::Spawn)?;
        self.ended();
        let code = output.status.code().unwrap_or(-1);
        let stderr = shorten_stderr(&self.confined(code, &output.stderr)?);
        Ok((code, output.stdout, stderr))
    }
}

/// The stderr bytes as trimmed text of at most [`STDERR_KEEP`] bytes.
fn shorten_stderr(bytes: &[u8]) -> String {
    let mut stderr = String::from_utf8_lossy(bytes).trim().to_owned();
    if stderr.len() > STDERR_KEEP {
        let mut cut = STDERR_KEEP;
        while !stderr.is_char_boundary(cut) {
            cut -= 1;
        }
        stderr.truncate(cut);
    }
    stderr
}

impl Git {
    /// Runs `git <args>` and returns `(exit code, stderr)`. Stdout is
    /// discarded. At most `max_stderr` bytes of stderr are kept; the rest is
    /// read and dropped until the end, so the memory use is bounded (NV-30).
    /// A signal gives the code -1. The timeout, the kill and the errors are
    /// as for [`Self::status`].
    pub async fn status_stderr(
        &self,
        args: &[&str],
        env: &[(&str, &OsStr)],
        max_stderr: usize,
    ) -> Result<(i32, String), GitError> {
        use tokio::io::AsyncReadExt;
        let mut cmd = self.command();
        cmd.current_dir(&self.dir)
            .args(SAFE_CONFIG)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        self.grouped(&mut cmd);
        for name in REDIRECTING_VARS {
            cmd.env_remove(name);
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C");
        if self.options.isolated {
            cmd.env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null");
        }
        for (name, value) in env {
            cmd.env(name, value);
        }
        let mut child = cmd.spawn().map_err(GitError::Spawn)?;
        self.spawned(&child);
        let Some(mut stderr) = child.stderr.take() else {
            return Err(GitError::Spawn(std::io::Error::other("no stderr pipe")));
        };
        // Room for the status line of the helper (B9).
        let keep = max_stderr
            + if self.options.confine.is_some() {
                128
            } else {
                0
            };
        let work = async {
            let mut kept = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = stderr.read(&mut chunk).await.map_err(GitError::Spawn)?;
                if n == 0 {
                    break;
                }
                let room = keep.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
            let status = child.wait().await.map_err(GitError::Spawn)?;
            Ok::<_, GitError>((status.code().unwrap_or(-1), kept))
        };
        let secs = self.options.timeout.as_secs();
        let (code, kept) = tokio::time::timeout(self.options.timeout, work)
            .await
            .map_err(|_| GitError::Timeout {
                args: args.join(" "),
                secs,
            })??;
        self.ended();
        let mut stderr = self.confined(code, &kept)?;
        stderr.truncate(max_stderr);
        Ok((code, shorten_stderr(&stderr)))
    }

    /// Runs `git <args>` and keeps at most `max_bytes` of stdout; with more
    /// output the process is killed. Returns `(exit code, stdout, cut)`; a
    /// killed process gives the code -1. Stderr is discarded.
    pub async fn bounded(
        &self,
        args: &[&str],
        max_bytes: usize,
    ) -> Result<(i32, Vec<u8>, bool), GitError> {
        self.bounded_with_env(args, &[], max_bytes).await
    }

    /// [`Self::bounded`] with extra environment variables (DL-191). A
    /// variable here replaces one that the runner sets.
    pub async fn bounded_with_env(
        &self,
        args: &[&str],
        envs: &[(&str, &str)],
        max_bytes: usize,
    ) -> Result<(i32, Vec<u8>, bool), GitError> {
        use tokio::io::AsyncReadExt;
        let mut cmd = self.command();
        cmd.current_dir(&self.dir)
            .args(SAFE_CONFIG)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        self.grouped(&mut cmd);
        for name in REDIRECTING_VARS {
            cmd.env_remove(name);
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C");
        if self.options.isolated {
            cmd.env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null");
        }
        for (name, value) in envs {
            cmd.env(name, value);
        }
        let mut child = cmd.spawn().map_err(GitError::Spawn)?;
        self.spawned(&child);
        let Some(stdout) = child.stdout.take() else {
            return Err(GitError::Spawn(std::io::Error::other("no stdout pipe")));
        };
        let limit = u64::try_from(max_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let timeout = || GitError::Timeout {
            args: args.join(" "),
            secs: self.options.timeout.as_secs(),
        };
        let work = async {
            let mut out = Vec::new();
            stdout
                .take(limit)
                .read_to_end(&mut out)
                .await
                .map_err(GitError::Spawn)?;
            let cut = out.len() > max_bytes;
            if cut {
                out.truncate(max_bytes);
                let _ = child.start_kill();
            }
            let status = child.wait().await.map_err(GitError::Spawn)?;
            Ok::<_, GitError>((status.code().unwrap_or(-1), out, cut))
        };
        let result = tokio::time::timeout(self.options.timeout, work)
            .await
            .map_err(|_| timeout())?;
        self.ended();
        result
    }
}

/// The options of a hardened call (DL-191): no pager, no external diff, no
/// file-system monitor. They come before the subcommand.
const HARDENED: &[&str] = &[
    "-c",
    "core.pager=cat",
    "-c",
    "diff.external=",
    "-c",
    "core.fsmonitor=false",
];

/// The variables of a hardened call (DL-191): no lazy fetch, no prompt, no
/// pager. A Git below 2.45 does not know `GIT_NO_LAZY_FETCH`, so the caller
/// refuses such a Git first.
pub const HARDENED_ENV: &[(&str, &str)] = &[
    ("GIT_NO_LAZY_FETCH", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_PAGER", "cat"),
];

/// `sub` with the hardened options in front of it (DL-191).
#[must_use]
pub fn hardened_args<'a>(sub: &[&'a str]) -> Vec<&'a str> {
    let mut args: Vec<&'a str> = HARDENED.to_vec();
    args.extend_from_slice(sub);
    args
}

/// Splits NUL-terminated `-z` output into its fields.
#[must_use]
pub fn nul_fields(out: &[u8]) -> Vec<&[u8]> {
    let mut fields: Vec<&[u8]> = out.split(|b| *b == 0).collect();
    if fields.last().is_some_and(|f| f.is_empty()) {
        fields.pop();
    }
    fields
}

/// A path field of `-z` output as an OS string.
#[must_use]
pub fn os_path(field: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    OsStr::from_bytes(field).to_owned()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A fake `fleetdeck`: writes the arguments before `--` next to itself,
    /// prints the status line on stderr, may exit with `code`, and else runs
    /// the program after `--`.
    fn fake_helper(dir: &Path, status: &str, code: Option<i32>) -> PathBuf {
        let script = dir.join("fleetdeck");
        let exit = code.map_or(String::new(), |c| format!("exit {c}\n"));
        let body = format!(
            "#!/bin/sh\nargs=\"\"\nfor a in \"$@\"; do [ \"$a\" = \"--\" ] && break; args=\"$args $a\"; done\necho \"$args\" > \"$(dirname \"$0\")/args.txt\"\necho 'fleetdeck-confine: {status}' >&2\n{exit}while [ \"$1\" != \"--\" ]; do shift; done\nshift\nexec \"$@\"\n"
        );
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn confined(dir: &Path, exe: PathBuf, roots: &[&str]) -> Git {
        let options = GitOptions {
            confine: Some(Confine {
                exe,
                roots: roots.iter().map(PathBuf::from).collect(),
            }),
            ..GitOptions::isolated()
        };
        Git::new(dir, options)
    }

    #[tokio::test]
    async fn a_confined_call_runs_git_through_the_helper_without_its_status_line() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_helper(dir.path(), "fully_enforced", None);
        let git = confined(dir.path(), exe, &["/a", "/b"]);
        let (code, out, stderr) = git.status(&["--version"], &[], None).await.unwrap();
        assert_eq!(code, 0);
        assert!(String::from_utf8_lossy(&out).starts_with("git version"));
        assert_eq!(stderr, "");
        let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
        assert_eq!(args.trim(), "confine-exec --write-root /a --write-root /b");
        // The text of a failing call keeps the stderr of Git only.
        let (code, stderr) = git
            .status_stderr(&["cat-file", "-p", "no-such-object"], &[], 4096)
            .await
            .unwrap();
        assert_ne!(code, 0);
        assert!(stderr.contains("fatal"), "{stderr}");
        assert!(!stderr.contains("fleetdeck-confine:"), "{stderr}");
        let err = git
            .run(&["cat-file", "-p", "no-such-object"])
            .await
            .unwrap_err();
        assert!(!err.to_string().contains("fleetdeck-confine:"), "{err}");
    }

    #[tokio::test]
    async fn a_helper_that_fails_gives_the_confine_error() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_helper(dir.path(), "cannot enforce", Some(125));
        let git = confined(dir.path(), exe, &["/a"]);
        let err = git.run(&["--version"]).await.unwrap_err();
        assert!(matches!(err, GitError::Confine), "{err}");
        assert_eq!(
            err.to_string(),
            "the station cannot confine Git (fleetdeck confine-exec failed)"
        );
        let err = git.status(&["--version"], &[], None).await.unwrap_err();
        assert!(matches!(err, GitError::Confine));
        let err = git
            .status_stderr(&["--version"], &[], 100)
            .await
            .unwrap_err();
        assert!(matches!(err, GitError::Confine));
    }

    #[tokio::test]
    async fn a_call_without_confinement_runs_git_directly() {
        let dir = tempfile::tempdir().unwrap();
        let git = Git::new(dir.path(), GitOptions::isolated());
        let out = git.text(&["--version"]).await.unwrap();
        assert!(out.starts_with("git version"));
        assert!(!dir.path().join("args.txt").exists());
    }
}
