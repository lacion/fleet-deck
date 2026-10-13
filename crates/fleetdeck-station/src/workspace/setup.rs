//! The setup command of a session start (DL-148): the owner's shell command
//! that runs in the workspace before the provider starts.
//!
//! The command runs under `/bin/sh` in its own process group with the
//! provider instance's scrubbed environment plus `FLEETDECK_SETUP_ID`. The
//! shell waits for a go signal on stdin, and the go signal comes only after
//! the `setup_runs` row is durable, so no setup process runs without a row.
//! When the shell ends, times out or fails, the whole process group is
//! stopped (TERM, then KILL), and then every process that carries the marker
//! is killed, also in another process group (a detached child). The row ends
//! only when the group is gone and no marked process is left; a row that
//! stays is found by the restart cleanup ([`super::setup_cleanup`]). A cancel
//! signal stops the run the same way.

use std::collections::VecDeque;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::SessionId;
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::time::{Instant, sleep, timeout as time_limit};

use crate::store::Store;

/// The environment variable that marks every process of one setup run.
pub const MARKER_VAR: &str = "FLEETDECK_SETUP_ID";
/// The longest run of a setup command.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// The wait for the process group after each signal.
const GROUP_WAIT: Duration = Duration::from_secs(5);
const GROUP_POLL: Duration = Duration::from_millis(100);
/// The wait for the output readers after the process group is gone.
const READER_WAIT: Duration = Duration::from_secs(1);
/// The wait for the marked processes after the group is gone.
const MARKED_WAIT: Duration = Duration::from_secs(5);
/// The message of a run that the cancel signal stopped.
pub const STOPPED_MESSAGE: &str = "the session was stopped during its setup command";
/// The size of the output tail that a failure reports.
const TAIL_BYTES: usize = 64 * 1024;
/// The shell script of the wrapper: wait for the go line, then run `$1`.
const WRAPPER: &str = r#"IFS= read -r _ || exit 97; exec /bin/sh -c "$1" </dev/null"#;

/// Why a setup command failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupError {
    pub message: String,
    /// The last 64 KiB of the output (stdout and stderr together).
    pub output_tail: String,
}

impl SetupError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            output_tail: String::new(),
        }
    }
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
    let mut buf = vec![0_u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => tail.push(&buf[..n]),
        }
    }
}

/// Runs `cmd` in `cwd` with `env` and waits for it to end. When this returns,
/// no process of the run is left, unless the error says so. A `true` on
/// `cancel` stops the run with [`STOPPED_MESSAGE`].
///
/// # Errors
/// [`SetupError`] when the command cannot start, exits non-zero, is killed,
/// is cancelled, times out after `timeout`, or leaves a process that does not
/// stop.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    store: &Store,
    effect_id: &str,
    session_id: &SessionId,
    cmd: &str,
    cwd: &Path,
    env: &ChildEnv,
    timeout: Duration,
    cancel: watch::Receiver<bool>,
) -> Result<(), SetupError> {
    let held = run_held(store, effect_id, session_id, cmd, cwd, env, timeout, cancel).await;
    if held.end_row {
        end_row(store, effect_id).await;
    }
    held.result
}

/// Deletes the `setup_runs` row of a run that [`run_held`] left.
pub async fn end_row(store: &Store, effect_id: &str) {
    if let Err(e) = store.setup_run_end(effect_id.to_owned()).await {
        tracing::warn!(error = %e, effect = effect_id, "the setup run row was not deleted");
    }
}

/// The end of [`run_held`].
#[derive(Debug)]
pub struct Held {
    pub result: Result<(), SetupError>,
    /// True: the caller deletes the row with [`end_row`]. False: a process of
    /// the run is left, and the row stays for the cleanup at the next start.
    pub end_row: bool,
}

/// [`run`] without the delete of the `setup_runs` row at the end: the row
/// stays, and the restart admission lists it, until the caller calls
/// [`end_row`] (PRE-PR r2 finding 2). The start task takes its restart claim
/// between the end of the run and the delete.
#[allow(clippy::too_many_arguments)]
pub async fn run_held(
    store: &Store,
    effect_id: &str,
    session_id: &SessionId,
    cmd: &str,
    cwd: &Path,
    env: &ChildEnv,
    timeout: Duration,
    mut cancel: watch::Receiver<bool>,
) -> Held {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", WRAPPER, "fleetdeck-setup", cmd])
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter())
        .env(MARKER_VAR, effect_id)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Before the row exists: nothing to delete on these two returns.
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Held {
                result: Err(SetupError::new(format!(
                    "the setup command did not start: {e}"
                ))),
                end_row: false,
            };
        }
    };
    let Some(pgid) = child
        .id()
        .and_then(|p| i32::try_from(p).ok())
        .and_then(Pid::from_raw)
    else {
        let _ = child.kill().await;
        return Held {
            result: Err(SetupError::new("the setup command has no process id")),
            end_row: false,
        };
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
    let outcome = go_and_wait(
        store,
        effect_id,
        session_id,
        pgid,
        &mut child,
        timeout,
        &mut cancel,
    )
    .await;
    let gone = stop_group(pgid, &mut child).await;
    let left = kill_marked(effect_id).await;
    for mut reader in readers {
        if time_limit(READER_WAIT, &mut reader).await.is_err() {
            reader.abort();
        }
    }
    if !gone || left {
        return Held {
            result: Err(SetupError {
                message: "a setup process did not stop".into(),
                output_tail: tail.text(),
            }),
            end_row: false,
        };
    }
    let result = match outcome {
        Outcome::Exited(status) if status.success() => Ok(()),
        Outcome::Exited(status) => Err(SetupError {
            message: match (status.code(), status.signal()) {
                (Some(code), _) => format!("the setup command failed (exit {code})"),
                (None, Some(sig)) => format!("the setup command was killed by signal {sig}"),
                (None, None) => "the setup command ended without a status".to_owned(),
            },
            output_tail: tail.text(),
        }),
        Outcome::Cancelled => Err(SetupError {
            message: STOPPED_MESSAGE.into(),
            output_tail: tail.text(),
        }),
        Outcome::TimedOut => Err(SetupError {
            message: format!("the setup command timed out after {}", describe(timeout)),
            output_tail: tail.text(),
        }),
        Outcome::NotStarted(message) => Err(SetupError {
            message,
            output_tail: tail.text(),
        }),
    };
    Held {
        result,
        end_row: true,
    }
}

enum Outcome {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
    NotStarted(String),
}

/// Stores the row, sends the go line and waits for the shell to end.
async fn go_and_wait(
    store: &Store,
    effect_id: &str,
    session_id: &SessionId,
    pgid: Pid,
    child: &mut Child,
    timeout: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> Outcome {
    if let Err(e) = store
        .setup_run_begin(
            effect_id.to_owned(),
            session_id.clone(),
            pgid.as_raw_nonzero().get(),
        )
        .await
    {
        return Outcome::NotStarted(format!("the setup command was not started: {e}"));
    }
    let Some(mut stdin) = child.stdin.take() else {
        return Outcome::NotStarted("the setup command has no input pipe".into());
    };
    let sent = async {
        stdin.write_all(b"go\n").await?;
        stdin.flush().await
    };
    if let Err(e) = sent.await {
        return Outcome::NotStarted(format!("the setup command was not started: {e}"));
    }
    drop(stdin);
    // A dropped sender never cancels.
    let cancelled = async {
        if cancel.wait_for(|stop| *stop).await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        waited = time_limit(timeout, child.wait()) => match waited {
            Ok(Ok(status)) => Outcome::Exited(status),
            Ok(Err(e)) => Outcome::NotStarted(format!("the setup command status is unknown: {e}")),
            Err(_) => Outcome::TimedOut,
        },
        () = cancelled => Outcome::Cancelled,
    }
}

/// Kills every process that carries the run marker (a detached child is in
/// another process group). Returns true when one may be left or the scan
/// cannot run.
async fn kill_marked(effect_id: &str) -> bool {
    let effect = effect_id.to_owned();
    match tokio::task::spawn_blocking(move || {
        super::setup_cleanup::kill_marked(&effect, MARKED_WAIT)
    })
    .await
    {
        Ok(Ok(left)) => left > 0,
        Ok(Err(e)) => {
            tracing::error!(error = %e, effect = effect_id, "the setup processes cannot be listed");
            true
        }
        Err(e) => {
            tracing::error!(error = %e, effect = effect_id, "the setup scan stopped");
            true
        }
    }
}

/// Stops the process group: TERM, wait, KILL, wait. The shell itself is
/// reaped here, so it never keeps the group alive as a zombie. Returns
/// whether the group is gone.
async fn stop_group(pgid: Pid, child: &mut Child) -> bool {
    for sig in [Signal::TERM, Signal::KILL] {
        if let Err(e) = kill_process_group(pgid, sig)
            && e != rustix::io::Errno::SRCH
        {
            tracing::warn!(?sig, error = %e, "cannot signal the setup process group");
        }
        if group_gone(pgid, child).await {
            return true;
        }
    }
    false
}

async fn group_gone(pgid: Pid, child: &mut Child) -> bool {
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

fn describe(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 && secs.is_multiple_of(60) {
        format!("{} min", secs / 60)
    } else {
        format!("{secs} s")
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fleetdeck_testkit::Det;

    use super::*;
    use crate::store::StoreConfig;

    struct Rig {
        store: Store,
        dir: tempfile::TempDir,
        env: ChildEnv,
    }

    async fn rig() -> Rig {
        let det = Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        Rig {
            store,
            dir: tempfile::tempdir().unwrap(),
            env: ChildEnv::from_process(),
        }
    }

    impl Rig {
        async fn run(&self, id: &str, cmd: &str, timeout: Duration) -> Result<(), SetupError> {
            run(
                &self.store,
                id,
                &SessionId::from("s1"),
                cmd,
                self.dir.path(),
                &self.env,
                timeout,
                watch::channel(false).1,
            )
            .await
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }
    }

    const LONG: Duration = Duration::from_secs(30);

    /// Whether any process of the station's user carries the marker.
    fn marked(id: &str) -> bool {
        let marker = format!("{MARKER_VAR}={id}");
        std::fs::read_dir("/proc").unwrap().flatten().any(|e| {
            std::fs::read(e.path().join("environ"))
                .is_ok_and(|b| b.split(|c| *c == 0).any(|v| v == marker.as_bytes()))
        })
    }

    #[tokio::test]
    async fn a_command_runs_in_the_workspace_with_the_marker_and_ends_its_row() {
        let rig = rig().await;
        rig.run(
            "e1",
            "pwd > where.txt; echo $FLEETDECK_SETUP_ID > id.txt",
            LONG,
        )
        .await
        .unwrap();
        let cwd = std::fs::read_to_string(rig.path("where.txt")).unwrap();
        assert_eq!(
            PathBuf::from(cwd.trim()).canonicalize().unwrap(),
            rig.dir.path().canonicalize().unwrap()
        );
        assert_eq!(std::fs::read_to_string(rig.path("id.txt")).unwrap(), "e1\n");
        assert!(rig.store.setup_runs().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_row_is_durable_before_the_command_can_run() {
        let rig = rig().await;
        // A watcher reads the row while the command runs: the row (with the
        // process group) exists before the command, and it ends with the run.
        let store = rig.store.clone();
        let watcher = tokio::spawn(async move {
            loop {
                let rows = store.setup_runs().await.unwrap();
                if let Some(row) = rows.first() {
                    return row.clone();
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        rig.run("e2", "sleep 0.3", LONG).await.unwrap();
        let row = watcher.await.unwrap();
        assert_eq!(row.effect_id, "e2");
        assert_eq!(row.session_id, SessionId::from("s1"));
        assert!(row.pgid > 1);
        assert!(rig.store.setup_runs().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failing_command_reports_its_exit_and_the_output_tail() {
        let rig = rig().await;
        let e = rig
            .run("e3", "echo one; echo two >&2; exit 7", LONG)
            .await
            .unwrap_err();
        assert_eq!(e.message, "the setup command failed (exit 7)");
        assert!(e.output_tail.contains("one") && e.output_tail.contains("two"));
        assert!(rig.store.setup_runs().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_command_killed_by_a_signal_says_so() {
        let rig = rig().await;
        let e = rig.run("e4", "kill -9 $$", LONG).await.unwrap_err();
        assert_eq!(e.message, "the setup command was killed by signal 9");
    }

    #[tokio::test]
    async fn the_output_tail_keeps_the_last_64_kib() {
        let rig = rig().await;
        let e = rig
            .run(
                "e5",
                "i=0; while [ $i -lt 3000 ]; do echo line-$i-0123456789012345678901234567890123456789; i=$((i+1)); done; exit 1",
                LONG,
            )
            .await
            .unwrap_err();
        assert!(e.output_tail.len() <= TAIL_BYTES);
        assert!(e.output_tail.len() > TAIL_BYTES - 200);
        assert!(e.output_tail.contains("line-2999-"));
        assert!(!e.output_tail.contains("line-0-"));
    }

    #[tokio::test]
    async fn a_command_that_runs_too_long_is_stopped_with_its_group() {
        let rig = rig().await;
        let started = Instant::now();
        let e = rig
            .run(
                "e6",
                "sleep 30 & sleep 30; echo unreachable",
                Duration::from_secs(1),
            )
            .await
            .unwrap_err();
        assert_eq!(e.message, "the setup command timed out after 1 s");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!marked("e6"), "no process of the run is left");
        assert!(rig.store.setup_runs().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_background_child_dies_when_the_setup_ends() {
        let rig = rig().await;
        rig.run("e7", "sleep 30 & echo started", LONG)
            .await
            .unwrap();
        assert!(!marked("e7"), "the background child was killed");
        assert!(rig.store.setup_runs().await.unwrap().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_detached_child_in_another_process_group_is_killed_before_the_row_ends() {
        let rig = rig().await;
        rig.run("e9", "setsid sh -c 'sleep 60' >/dev/null 2>&1 &", LONG)
            .await
            .unwrap();
        assert!(!marked("e9"), "the detached child was killed");
        assert!(rig.store.setup_runs().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_cancel_stops_the_run_with_its_children() {
        let rig = rig().await;
        let (stop, cancel) = watch::channel(false);
        let started = Instant::now();
        let canceller = tokio::spawn(async move {
            sleep(Duration::from_millis(500)).await;
            stop.send(true).unwrap();
        });
        let e = run(
            &rig.store,
            "e10",
            &SessionId::from("s1"),
            "sleep 30 & sleep 30",
            rig.dir.path(),
            &rig.env,
            LONG,
            cancel,
        )
        .await
        .unwrap_err();
        canceller.await.unwrap();
        assert_eq!(e.message, STOPPED_MESSAGE);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!marked("e10"), "no process of the run is left");
        assert!(rig.store.setup_runs().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_missing_workspace_is_a_start_error() {
        let rig = rig().await;
        let e = run(
            &rig.store,
            "e8",
            &SessionId::from("s1"),
            "true",
            &rig.path("missing"),
            &rig.env,
            LONG,
            watch::channel(false).1,
        )
        .await
        .unwrap_err();
        assert!(
            e.message.starts_with("the setup command did not start"),
            "{}",
            e.message
        );
        assert!(rig.store.setup_runs().await.unwrap().is_empty());
    }

    #[test]
    fn a_timeout_reads_in_minutes_when_it_is_whole() {
        assert_eq!(describe(DEFAULT_TIMEOUT), "10 min");
        assert_eq!(describe(Duration::from_secs(90)), "90 s");
        assert_eq!(describe(Duration::from_secs(1)), "1 s");
    }
}
