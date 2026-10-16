//! The execution of a `!` shell command of a Claude session (B8, DL-196).
//!
//! The effect worker only starts a task ([`SessionTarget::dispatch_shell`]);
//! the task runs outside the worker, as the setup task does, so a command of
//! up to 600 s never stalls the other sessions. The task holds the session
//! fence while the command runs, advances the folder generation at the start
//! (the settlement advance follows in `shell_advance`), keeps the tails of
//! both streams and settles the row and the effect. A Codex command runs
//! through the provider instead (`shell_codex`).

use std::collections::HashMap;
#[cfg(test)]
use std::future::Future;
#[cfg(test)]
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::{Event, ProviderKind, ToolKind, ToolStatus};
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::{Notify, watch};

use super::SessionTarget;
use crate::manager::shell_advance::MARKER_VAR;
use crate::store::shell_runs::ShellState;
use crate::store::{Dispatch, Effect, EffectPayload};

/// The longest run of a command.
const TIMEOUT: Duration = Duration::from_secs(600);
/// The wait for the open or due capture of the session.
pub(super) const CAPTURE_WAIT: Duration = Duration::from_secs(5);
pub(super) const CAPTURE_BUSY: &str = "The session is still saving its last turn; try again.";
pub(super) const SHELLS_OFF: &str = "Shells are off on this station (allow_shell = false).";
/// The wait for the first evidence of a Codex command (B8).
const CODEX_WATCH: Duration = Duration::from_secs(30);
/// The wait between TERM and KILL, and for the group to be gone.
const GRACE: Duration = Duration::from_secs(2);
const GROUP_POLL: Duration = Duration::from_millis(50);
/// The wait for the processes that carry the marker.
const MARKED_WAIT: Duration = Duration::from_secs(5);
/// The wait for the output readers after the group is gone.
const READER_WAIT: Duration = Duration::from_secs(1);
/// The characters that the row keeps of each stream.
const TAIL_CHARS: usize = 30_000;
/// The bytes that the ring keeps (4 per character covers any text).
const RING_BYTES: usize = TAIL_CHARS * 4;
/// The bytes of the `tool.output` event, and of its stderr part.
const EVENT_BYTES: usize = 16 * 1024;
const EVENT_STDERR_BYTES: usize = 4096;
const STDERR_SEPARATOR: &str = "\n--- stderr ---\n";

/// A test hook that the pre-spawn gates await after the session folder read,
/// with the effect id (B8 PRE-PR r1, r2).
#[cfg(test)]
pub(super) type PreSpawnHook =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// The shell tasks of a target: the cancel of each running task by effect
/// id, the wake of the advance loop and the run limit.
pub(super) struct ShellRunState {
    tasks: StdMutex<HashMap<String, watch::Sender<bool>>>,
    notify: Arc<Notify>,
    /// The run limit in ms (a test shortens it).
    timeout_ms: AtomicU64,
    /// The wait for the proof of a Codex command in ms (a test shortens it).
    watch_ms: AtomicU64,
    /// The hook of the window between the append and the spawn (tests only).
    #[cfg(test)]
    pre_spawn: StdMutex<Option<PreSpawnHook>>,
}

impl Default for ShellRunState {
    fn default() -> Self {
        Self {
            tasks: StdMutex::default(),
            notify: Arc::new(Notify::new()),
            timeout_ms: AtomicU64::new(u64::try_from(TIMEOUT.as_millis()).unwrap_or(u64::MAX)),
            watch_ms: AtomicU64::new(u64::try_from(CODEX_WATCH.as_millis()).unwrap_or(u64::MAX)),
            #[cfg(test)]
            pre_spawn: StdMutex::default(),
        }
    }
}

/// The last [`TAIL_CHARS`] characters of one stream.
#[derive(Default)]
struct Ring {
    bytes: StdMutex<Vec<u8>>,
    dropped: AtomicBool,
}

impl Ring {
    fn push(&self, chunk: &[u8]) {
        let Ok(mut bytes) = self.bytes.lock() else {
            return;
        };
        bytes.extend_from_slice(chunk);
        if bytes.len() > RING_BYTES {
            let extra = bytes.len() - RING_BYTES;
            bytes.drain(..extra);
            self.dropped.store(true, Ordering::Relaxed);
        }
    }

    /// The tail text, and whether older output was dropped.
    fn text(&self) -> (String, bool) {
        let Ok(bytes) = self.bytes.lock() else {
            return (String::new(), false);
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut dropped = self.dropped.load(Ordering::Relaxed);
        let count = text.chars().count();
        if count > TAIL_CHARS {
            dropped = true;
            return (text.chars().skip(count - TAIL_CHARS).collect(), dropped);
        }
        (text, dropped)
    }
}

async fn read_into(mut stream: impl AsyncRead + Unpin, ring: Arc<Ring>) {
    let mut buf = vec![0_u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => ring.push(&buf[..n]),
        }
    }
}

/// The last `max` bytes of `text`, cut at a character boundary.
fn tail_bytes(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (&text[start..], true)
}

/// The text of the `tool.output` event: the last part of stdout, then the
/// separator and the last 4 KiB of stderr when it is not empty, within 16 KiB
/// in all. The flag tells that either stream was cut.
fn event_chunk(stdout: &str, stderr: &str) -> (String, bool) {
    if stderr.is_empty() {
        let (out, cut) = tail_bytes(stdout, EVENT_BYTES);
        return (out.to_owned(), cut);
    }
    let (err, err_cut) = tail_bytes(stderr, EVENT_STDERR_BYTES);
    let budget = EVENT_BYTES - STDERR_SEPARATOR.len() - err.len();
    let (out, out_cut) = tail_bytes(stdout, budget);
    (format!("{out}{STDERR_SEPARATOR}{err}"), out_cut || err_cut)
}

/// `bash`, or `/bin/sh` when `bash` is not on `PATH`.
fn shell_program() -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("bash"))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .unwrap_or_else(|| "/bin/sh".into())
}

/// Resolves when a cancel is signalled; never when the sender is gone.
async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow() {
            return;
        }
        if cancel.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// How the command ended.
enum End {
    Exited(std::io::Result<std::process::ExitStatus>),
    Timeout,
    Cancelled,
}

/// What the run left for the row and the events.
struct Ran {
    state: ShellState,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    dropped: bool,
}

/// TERM to the group, the grace, then KILL to the group; the leader is
/// reaped. The processes that carry the marker follow.
async fn stop_group(pgid: Option<Pid>, child: &mut tokio::process::Child, id: &str) {
    if let Some(pgid) = pgid {
        signal_group(pgid, Signal::TERM);
        let deadline = tokio::time::Instant::now() + GRACE;
        loop {
            // A zombie leader still counts as a member: reap it first.
            let _ = child.try_wait();
            if test_kill_process_group(pgid).is_err() || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(GROUP_POLL).await;
        }
        signal_group(pgid, Signal::KILL);
    }
    if tokio::time::timeout(GRACE, child.wait()).await.is_err() {
        tracing::warn!(effect = %id, "the shell did not end after KILL");
    }
    let marker = id.to_owned();
    match tokio::task::spawn_blocking(move || {
        crate::workspace::setup_cleanup::kill_marked(MARKER_VAR, &marker, MARKED_WAIT)
    })
    .await
    {
        Ok(Ok(0)) => {}
        Ok(Ok(left)) => tracing::error!(effect = %id, left, "a shell process may still run"),
        Ok(Err(e)) => tracing::error!(error = %e, "the shell processes cannot be listed"),
        Err(e) => tracing::error!(error = %e, "the shell scan stopped"),
    }
}

fn signal_group(pgid: Pid, signal: Signal) {
    if let Err(e) = kill_process_group(pgid, signal)
        && e != rustix::io::Errno::SRCH
    {
        tracing::warn!(error = %e, "cannot signal the shell process group");
    }
}

impl SessionTarget {
    /// The wake of the shell advance loop (B8).
    pub(in crate::manager) fn shell_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.shell.notify)
    }

    /// Shortens the run limit of the next commands (tests only).
    #[cfg(test)]
    pub(in crate::manager) fn set_shell_timeout(&self, limit: Duration) {
        let ms = u64::try_from(limit.as_millis()).unwrap_or(u64::MAX);
        self.shell.timeout_ms.store(ms, Ordering::Relaxed);
    }

    /// Shortens the wait for the proof of a Codex command (tests only).
    #[cfg(test)]
    pub(in crate::manager) fn set_shell_watch(&self, limit: Duration) {
        let ms = u64::try_from(limit.as_millis()).unwrap_or(u64::MAX);
        self.shell.watch_ms.store(ms, Ordering::Relaxed);
    }

    /// Sets the hook that runs between the `ToolStarted` append and the
    /// spawn (tests only).
    #[cfg(test)]
    pub(in crate::manager) fn set_shell_pre_spawn<F>(&self, hook: F)
    where
        F: Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
    {
        if let Ok(mut slot) = self.shell.pre_spawn.lock() {
            *slot = Some(Arc::new(hook));
        }
    }

    /// The wait for the proof of a Codex command (B8).
    pub(super) fn shell_watch(&self) -> Duration {
        Duration::from_millis(self.shell.watch_ms.load(Ordering::Relaxed))
    }

    /// Dispatches a shell effect (B8): a Claude command runs in a task and
    /// the effect stays claimed until the task confirms it. A Codex command
    /// goes through `dispatch_codex_shell`.
    pub(super) fn dispatch_shell(&self, effect: &Effect) -> Dispatch {
        let EffectPayload::ShellCommand { provider, .. } = &effect.payload else {
            return Dispatch::NotDispatched("not_a_shell_command".into());
        };
        if *provider != ProviderKind::Claude {
            return Dispatch::NotDispatched("shell_provider_unsupported".into());
        }
        let (Some(this), Some(fence)) = (self.this.upgrade(), effect.fence_token) else {
            return Dispatch::NotDispatched("shell_no_owner".into());
        };
        let (cancel, cancel_rx) = watch::channel(false);
        let Ok(mut tasks) = self.shell.tasks.lock() else {
            return Dispatch::NotDispatched("shell_tasks_unreadable".into());
        };
        tasks.insert(effect.effect_id.as_str().to_owned(), cancel);
        tokio::spawn(this.shell_task(effect.clone(), fence, cancel_rx));
        Dispatch::Sent
    }

    /// Signals the running task of the effect `id`, if there is one.
    fn signal_shell(&self, id: &str) {
        if let Ok(tasks) = self.shell.tasks.lock()
            && let Some(cancel) = tasks.get(id)
        {
            cancel.send_replace(true);
        }
    }

    /// Signals every running task (the station shutdown).
    pub(in crate::manager) fn signal_all_shells(&self) {
        if let Ok(tasks) = self.shell.tasks.lock() {
            for cancel in tasks.values() {
                cancel.send_replace(true);
            }
        }
    }

    /// After a decision: an accepted `Interrupt` (`session` is its session)
    /// also cancels the running shell tasks of that session.
    pub(in crate::manager) async fn cancel_shells_after(
        &self,
        session: Option<SessionId>,
        decided: &crate::store::Result<crate::store::Decided>,
    ) {
        let accepted = decided
            .as_ref()
            .is_ok_and(|d| d.receipt.state == fleetdeck_proto::command::ReceiptState::Accepted);
        if let (Some(session), true) = (session, accepted) {
            self.cancel_shells_of(&session).await;
        }
    }

    /// Signals the running tasks of a session.
    async fn cancel_shells_of(&self, session: &SessionId) {
        match self.store.shell_runs_unsettled().await {
            Ok(rows) => {
                for row in rows
                    .iter()
                    .filter(|r| r.session_id == *session && !r.state.is_terminal())
                {
                    self.signal_shell(&row.effect_id);
                }
            }
            Err(e) => tracing::warn!(error = %e, "the shell rows of the session are unreadable"),
        }
    }

    /// Waits until the tasks of `ids` have ended (their rows are settled),
    /// at most `limit`. A row without a task (never dispatched) is not
    /// waited for; a row still running after the limit is left, and the next
    /// start-up settles it.
    pub(in crate::manager) async fn wait_shells_settled(&self, ids: &[String], limit: Duration) {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let running = self.shell.tasks.lock().map_or(0, |tasks| {
                ids.iter().filter(|id| tasks.contains_key(*id)).count()
            });
            if running == 0 {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(running, "shell commands did not settle at the shutdown");
                return;
            }
            tokio::time::sleep(GROUP_POLL).await;
        }
    }

    /// The task of one command: the run, then the confirmation of the
    /// effect (always last, after the row settled), then the wake of the
    /// advance loop.
    async fn shell_task(
        self: Arc<Self>,
        effect: Effect,
        fence: u64,
        mut cancel: watch::Receiver<bool>,
    ) {
        self.shell_run(&effect, &mut cancel).await;
        if let Err(e) = self
            .store
            .settle(effect.effect_id.clone(), fence, Dispatch::Confirmed)
            .await
        {
            tracing::warn!(error = %e, effect = %effect.effect_id, "the shell effect was not confirmed");
        }
        if let Ok(mut tasks) = self.shell.tasks.lock() {
            tasks.remove(effect.effect_id.as_str());
        }
        self.shell.notify.notify_one();
    }

    /// Settles a row that never ran: `state` with `text` as its stderr.
    async fn shell_stop(&self, id: &str, state: ShellState, text: Option<&str>) {
        let settled = self
            .store
            .shell_settle(id.to_owned(), state, None, None, text.map(str::to_owned))
            .await;
        if let Err(e) = settled {
            tracing::error!(error = %e, effect = %id, "a shell row was not settled");
        }
    }

    async fn shell_run(&self, effect: &Effect, cancel: &mut watch::Receiver<bool>) {
        let EffectPayload::ShellCommand {
            command,
            worktree_root,
            ..
        } = &effect.payload
        else {
            return;
        };
        let id = effect.effect_id.as_str();
        let session = &effect.session_id;
        match self.store.shell_set_running(id.to_owned()).await {
            Ok(true) => {}
            // A cancel is stored (or the row ended already).
            Ok(false) => return self.shell_stop(id, ShellState::Cancelled, None).await,
            Err(e) => return tracing::error!(error = %e, effect = %id, "a shell row did not start"),
        }
        let Some(live) = self.live(session) else {
            let text = "The session has no running provider.";
            return self.shell_stop(id, ShellState::Failed, Some(text)).await;
        };
        let turns = live.lock().await.turns.clone();
        // The wait ends when no capture is open or due; the guard stays as
        // the fence of the run (Git: the root fence).
        let Ok(fence) = tokio::time::timeout(CAPTURE_WAIT, turns.fence()).await else {
            return self
                .shell_stop(id, ShellState::Failed, Some(CAPTURE_BUSY))
                .await;
        };
        if let Err(e) = self
            .store
            .shell_advance(id.to_owned(), worktree_root.clone(), true)
            .await
        {
            let text = format!("The folder generation did not advance: {e}");
            return self.shell_stop(id, ShellState::Failed, Some(&text)).await;
        }
        let stored_cancel = match self.store.shell_run(id.to_owned()).await {
            Ok(row) => row.is_none_or(|r| r.cancel_requested),
            Err(e) => {
                let text = format!("The command row is unreadable: {e}");
                return self.shell_stop(id, ShellState::Failed, Some(&text)).await;
            }
        };
        if stored_cancel || *cancel.borrow() {
            return self.shell_stop(id, ShellState::Cancelled, None).await;
        }
        if let Err(text) = self.shell_cwd(session).await {
            return self
                .shell_stop(id, ShellState::Cancelled, Some(&text))
                .await;
        }
        let ran = self.shell_spawn(id, session, command, cancel).await;
        // The fence is released before the row settles.
        drop(fence);
        self.shell_finish(id, session, ran).await;
    }

    /// The session folder, checked again against the current policy.
    pub(super) async fn shell_cwd(
        &self,
        session: &SessionId,
    ) -> Result<std::path::PathBuf, String> {
        let policy = self
            .current_policy()
            .ok_or_else(|| "The station policy is not available.".to_owned())?;
        if !policy.allow_shell() {
            return Err(SHELLS_OFF.to_owned());
        }
        let cwd = self
            .store
            .summary(session.clone())
            .await
            .map_err(|e| format!("The session is unreadable: {e}"))?
            .and_then(|s| s.cwd)
            .ok_or_else(|| "The session has no folder.".to_owned())?;
        policy.workspace(&cwd).map_err(|e| e.to_string())
    }

    /// The gates right before the spawn (B8 PRE-PR r1): the stored cancel,
    /// the cancel receiver and the policy. The awaited reads come first; the
    /// receiver and the policy are checked after the last `await`, so nothing
    /// that arrives during a read is missed (B8 PRE-PR r2). `Err` carries the
    /// end without a process.
    async fn shell_gates(
        &self,
        id: &str,
        session: &SessionId,
        cancel: &watch::Receiver<bool>,
    ) -> Result<std::path::PathBuf, Ran> {
        let never = |state, stderr: String| Ran {
            state,
            exit_code: None,
            stdout: String::new(),
            stderr,
            dropped: false,
        };
        let cwd = match self.store.summary(session.clone()).await {
            Ok(summary) => summary.and_then(|s| s.cwd),
            Err(e) => {
                let text = format!("The session is unreadable: {e}");
                return Err(never(ShellState::Cancelled, text));
            }
        };
        let Some(cwd) = cwd else {
            let text = "The session has no folder.".to_owned();
            return Err(never(ShellState::Cancelled, text));
        };
        #[cfg(test)]
        {
            let hook = self.shell.pre_spawn.lock().ok().and_then(|h| h.clone());
            if let Some(hook) = hook {
                hook(id.to_owned()).await;
            }
        }
        match self.store.shell_run(id.to_owned()).await {
            Ok(row) => {
                if row.is_none_or(|r| r.cancel_requested) {
                    return Err(never(ShellState::Cancelled, String::new()));
                }
            }
            Err(e) => {
                let text = format!("The command row is unreadable: {e}");
                return Err(never(ShellState::Failed, text));
            }
        }
        // No `await` from here to the spawn.
        if *cancel.borrow() {
            return Err(never(ShellState::Cancelled, String::new()));
        }
        let Some(policy) = self.current_policy() else {
            let text = "The station policy is not available.".to_owned();
            return Err(never(ShellState::Cancelled, text));
        };
        if !policy.allow_shell() {
            return Err(never(ShellState::Cancelled, SHELLS_OFF.to_owned()));
        }
        policy
            .workspace(&cwd)
            .map_err(|e| never(ShellState::Cancelled, e.to_string()))
    }

    /// Starts the command and waits for its end.
    async fn shell_spawn(
        &self,
        id: &str,
        session: &SessionId,
        command: &str,
        cancel: &mut watch::Receiver<bool>,
    ) -> Ran {
        let node = format!("shell-{id}");
        self.shell_event(
            session,
            Event::ToolStarted {
                node_id: node.clone().into(),
                parent_node_id: None,
                tool_kind: ToolKind::Shell,
                title: "Shell command".to_owned(),
                input_summary: command.to_owned(),
                files: Vec::new(),
                provider_refs: serde_json::Map::new(),
            },
        )
        .await;
        // (B8 PRE-PR r1) The append above was awaited: a cancel or a policy
        // change may have come in. Nothing starts when a gate is closed.
        let cwd = match self.shell_gates(id, session, cancel).await {
            Ok(cwd) => cwd,
            Err(ran) => return ran,
        };
        let env = ChildEnv::from_process();
        let spawned = Command::new(shell_program())
            .args(["-c", command])
            .current_dir(&cwd)
            .env_clear()
            .envs(env.iter())
            .env(MARKER_VAR, id)
            .process_group(0)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                return Ran {
                    state: ShellState::Failed,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!("The shell did not start: {e}"),
                    dropped: false,
                };
            }
        };
        let pgid = child
            .id()
            .and_then(|p| i32::try_from(p).ok())
            .and_then(Pid::from_raw);
        let (out_ring, err_ring) = (Arc::new(Ring::default()), Arc::new(Ring::default()));
        let readers: Vec<_> = [
            child
                .stdout
                .take()
                .map(|s| tokio::spawn(read_into(s, Arc::clone(&out_ring)))),
            child
                .stderr
                .take()
                .map(|s| tokio::spawn(read_into(s, Arc::clone(&err_ring)))),
        ]
        .into_iter()
        .flatten()
        .collect();
        let limit = Duration::from_millis(self.shell.timeout_ms.load(Ordering::Relaxed));
        let end = tokio::select! {
            r = child.wait() => End::Exited(r),
            () = tokio::time::sleep(limit) => End::Timeout,
            () = cancelled(cancel) => End::Cancelled,
        };
        // Whatever the end, nothing of the command stays.
        stop_group(pgid, &mut child, id).await;
        for mut reader in readers {
            if tokio::time::timeout(READER_WAIT, &mut reader)
                .await
                .is_err()
            {
                reader.abort();
            }
        }
        let ((stdout, out_dropped), (mut stderr, err_dropped)) = (out_ring.text(), err_ring.text());
        let (state, exit_code) = match end {
            End::Exited(Ok(status)) => match status.code() {
                Some(0) => (ShellState::Done, Some(0)),
                code => (ShellState::Failed, code),
            },
            End::Exited(Err(e)) => {
                stderr.push_str(&format!("\nThe command status is unknown: {e}"));
                (ShellState::Failed, None)
            }
            End::Timeout => (ShellState::Timeout, None),
            End::Cancelled => (ShellState::Cancelled, None),
        };
        Ran {
            state,
            exit_code,
            stdout,
            stderr,
            dropped: out_dropped || err_dropped,
        }
    }

    /// Settles the row, then tells the session how the command ended.
    async fn shell_finish(&self, id: &str, session: &SessionId, ran: Ran) {
        let Ran {
            state,
            exit_code,
            stdout,
            stderr,
            dropped,
        } = ran;
        let settled = self
            .store
            .shell_settle(
                id.to_owned(),
                state,
                exit_code,
                Some(stdout.clone()),
                Some(stderr.clone()),
            )
            .await;
        match settled {
            Ok(true) => {}
            // Another path settled the row and told the session.
            Ok(false) => return,
            Err(e) => {
                return tracing::error!(error = %e, effect = %id, "a shell row was not settled");
            }
        }
        let node = format!("shell-{id}");
        let (chunk, cut) = event_chunk(&stdout, &stderr);
        self.shell_event(
            session,
            Event::ToolOutput {
                node_id: node.clone().into(),
                chunk,
                truncated: cut || dropped,
            },
        )
        .await;
        let label = match state {
            ShellState::Cancelled => "cancelled",
            ShellState::Timeout => "timeout",
            _ => "done",
        };
        let mut refs = serde_json::Map::new();
        refs.insert("shell".to_owned(), label.into());
        self.shell_event(
            session,
            Event::ToolCompleted {
                node_id: node.into(),
                status: if state == ShellState::Done {
                    ToolStatus::Ok
                } else {
                    ToolStatus::Failed
                },
                files: Vec::new(),
                exit_code,
                provider_refs: refs,
            },
        )
        .await;
    }

    async fn shell_event(&self, session: &SessionId, event: Event) {
        if let Err(e) = self.store.append(session.clone(), vec![event.into()]).await {
            tracing::warn!(error = %e, session = %session, "a shell event was not stored");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_keeps_the_end_of_stdout_and_the_last_4_kib_of_stderr() {
        assert_eq!(event_chunk("out\n", ""), ("out\n".to_owned(), false));
        let (chunk, cut) = event_chunk("out\n", "err\n");
        assert_eq!(chunk, "out\n\n--- stderr ---\nerr\n");
        assert!(!cut);
        let (chunk, cut) = event_chunk(&"o".repeat(20_000), &"e".repeat(5000));
        assert!(cut);
        assert_eq!(chunk.len(), EVENT_BYTES);
        let (out, err) = chunk.split_once(STDERR_SEPARATOR).unwrap();
        assert_eq!(err.len(), EVENT_STDERR_BYTES);
        assert_eq!(out.len(), EVENT_BYTES - STDERR_SEPARATOR.len() - 4096);
    }

    #[test]
    fn a_cut_lands_on_a_character_boundary() {
        let text = "é".repeat(10_000);
        let (chunk, cut) = event_chunk(&text, "");
        assert!(cut);
        assert!(chunk.len() <= EVENT_BYTES);
        assert!(chunk.chars().all(|c| c == 'é'));
    }

    #[test]
    fn the_ring_keeps_the_last_30000_characters() {
        let ring = Ring::default();
        ring.push("a".repeat(20_000).as_bytes());
        assert_eq!(ring.text(), ("a".repeat(20_000), false));
        ring.push("b".repeat(20_000).as_bytes());
        let (text, dropped) = ring.text();
        assert!(dropped);
        assert_eq!(text.chars().count(), TAIL_CHARS);
        assert!(text.ends_with('b'));
    }
}
