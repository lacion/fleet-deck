//! The human shell service (PLAN §6.8, D10; M3 item 6-11, DL-404).
//!
//! - The station owns each PTY (`portable-pty`) and runs the owner's login
//!   shell as the station's OS user with the scrubbed environment (§3
//!   rule 6, [`ChildEnv::from_process`]).
//! - `shell.open` checks `allow_shell`, `max_shells` and the allowed roots;
//!   `shell.open`, `shell.resize` and `shell.close` are mutating link
//!   methods with stored receipts (the audit). The receipt transaction only
//!   checks and enqueues a durable shell effect ([`decide`]); the PTY I/O
//!   runs after the commit ([`dispatch`], DL-405).
//! - Each attached stream has a bounded queue ([`SUBSCRIBER_QUEUE`] chunks).
//!   A reader that falls behind is cut off; it attaches again and gets the
//!   scrollback, so a slow board never grows the station memory.
//! - A reader thread per shell keeps the scrollback ([`Scrollback`]) and
//!   copies each chunk to the attached data streams ([`data`]). Attaching
//!   takes the scrollback and subscribes under one lock, so no byte is lost
//!   or sent twice between them.
//! - The shells belong to the station process, not to a link: a lost link,
//!   deck or board does not end them. A station restart does (A10).

pub mod data;
pub mod handover;
pub mod scrollback;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, PoisonError};

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_proto::ProviderInstanceId;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::{CommandResult, Reject, RejectCode};
use fleetdeck_proto::shell::{ShellChange, ShellEnvelope, ShellInfo, ShellState};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;

use crate::manager::policy::Policy;
use crate::store::effects::EffectId;
use crate::store::shell_effects::{self, PendingShell};
use crate::store::terminal::HandoverLaunch;
use crate::store::{DecideError, Decision, Store, StoreError};
pub use handover::{TerminalExit, TerminalInstance};
pub use scrollback::Scrollback;

/// The settings of the shell service (`station.toml`).
#[derive(Debug, Clone)]
pub struct ShellConfig {
    pub allow: bool,
    pub max_shells: u32,
    /// The program and its arguments; `None` is the owner's login shell
    /// (`$SHELL -l`, else `/bin/sh -l`).
    pub program: Option<(OsString, Vec<OsString>)>,
    /// The provider instances that have a binary for a terminal handover
    /// (DL-140).
    pub instances: BTreeMap<ProviderInstanceId, TerminalInstance>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            allow: true,
            max_shells: fleetdeck_core::config::DEFAULT_MAX_SHELLS,
            program: None,
            instances: BTreeMap::new(),
        }
    }
}

/// The output chunks (at most 16 KiB each) that one attached stream may
/// have in its queue; a reader that falls further behind is cut off.
pub const SUBSCRIBER_QUEUE: usize = 256;

struct Output {
    ring: Scrollback,
    subscribers: Vec<mpsc::Sender<Vec<u8>>>,
}

/// What one new shell runs.
struct Launch {
    program: (OsString, Vec<OsString>),
    env: ChildEnv,
    /// The session that a TUI handover holds.
    session: Option<SessionId>,
}

struct Shell {
    info: Mutex<ShellInfo>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    output: Mutex<Output>,
    /// The session that the shell holds (a TUI handover).
    session: Option<SessionId>,
    /// The process of the shell, when the PTY reports it.
    pid: Option<u32>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The shells of one station.
pub struct Shells {
    config: ShellConfig,
    env: ChildEnv,
    shells: Mutex<BTreeMap<String, Arc<Shell>>>,
    /// One dispatcher at a time keeps the effects in commit order.
    dispatch: tokio::sync::Mutex<()>,
    /// The exit reports of the TUI shells (DL-140).
    exits: mpsc::UnboundedSender<TerminalExit>,
    exit_reports: Mutex<Option<mpsc::UnboundedReceiver<TerminalExit>>>,
}

impl std::fmt::Debug for Shells {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shells")
            .field("config", &self.config)
            .field("open", &lock(&self.shells).len())
            .finish_non_exhaustive()
    }
}

fn reject(code: RejectCode, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

impl Shells {
    /// The service with the station's own environment, scrubbed.
    #[must_use]
    pub fn new(config: ShellConfig) -> Self {
        Self::with_env(config, ChildEnv::from_process())
    }

    #[must_use]
    pub fn with_env(config: ShellConfig, env: ChildEnv) -> Self {
        let (exits, exit_reports) = mpsc::unbounded_channel();
        Self {
            config,
            env,
            shells: Mutex::default(),
            dispatch: tokio::sync::Mutex::new(()),
            exits,
            exit_reports: Mutex::new(Some(exit_reports)),
        }
    }

    /// The reports that a TUI process ended (first call only). The station
    /// releases the hold of the session for each (DL-140).
    pub fn take_exits(&self) -> Option<mpsc::UnboundedReceiver<TerminalExit>> {
        lock(&self.exit_reports).take()
    }

    /// Every shell, oldest first.
    #[must_use]
    pub fn list(&self) -> Vec<ShellInfo> {
        let mut out: Vec<ShellInfo> = lock(&self.shells)
            .values()
            .map(|s| lock(&s.info).clone())
            .collect();
        out.sort_by(|a, b| (a.created_at, &a.shell_id).cmp(&(b.created_at, &b.shell_id)));
        out
    }

    /// Checks one change against the policy, the live shells and the
    /// committed changes that are not done yet (`pending`). No I/O: it runs
    /// in the decision transaction of the receipt.
    ///
    /// # Errors
    ///
    /// The reject of the receipt: `unsupported` when shells are off,
    /// `invalid_command` for a bad field or a cwd outside the roots,
    /// `invalid_state` for a full station, a used id or an unknown shell.
    pub fn check(
        &self,
        change: &ShellChange,
        policy: &Policy,
        pending: &[PendingShell],
    ) -> Result<(), Reject> {
        if !self.config.allow {
            return Err(reject(
                RejectCode::Unsupported,
                "shells are off on this station (allow_shell = false)",
            ));
        }
        change
            .check()
            .map_err(|m| reject(RejectCode::InvalidCommand, m))?;
        let mut ids: BTreeSet<String> = lock(&self.shells).keys().cloned().collect();
        for p in pending {
            match p.op.as_str() {
                "open" | "handover" => {
                    ids.insert(p.shell_id.clone());
                }
                "close" => {
                    ids.remove(&p.shell_id);
                }
                _ => {}
            }
        }
        match change {
            ShellChange::Open { shell_id, cwd, .. } => {
                policy
                    .workspace(cwd)
                    .map_err(|e| reject(RejectCode::InvalidCommand, e.to_string()))?;
                self.room(&ids, shell_id)
            }
            ShellChange::Handover { shell_id, .. } => self.room(&ids, shell_id),
            ShellChange::Resize { shell_id, .. } | ShellChange::Close { shell_id } => {
                if ids.contains(shell_id) {
                    Ok(())
                } else {
                    Err(unknown(shell_id))
                }
            }
            // The owner releases a hold the station cannot see: the decision
            // transaction checks the hold ([`decide`]).
            ShellChange::Release { .. } => Ok(()),
        }
    }

    /// A free id and a free slot among the live shells and the committed
    /// opens and handovers (`ids`).
    fn room(&self, ids: &BTreeSet<String>, shell_id: &str) -> Result<(), Reject> {
        if ids.contains(shell_id) {
            return Err(reject(
                RejectCode::InvalidState,
                format!("shell `{shell_id}` exists"),
            ));
        }
        if ids.len() >= self.config.max_shells as usize {
            return Err(reject(
                RejectCode::InvalidState,
                format!(
                    "the station runs its maximum of {} shells (max_shells)",
                    self.config.max_shells
                ),
            ));
        }
        Ok(())
    }

    /// Does one checked change: the PTY I/O (after the receipt commit).
    ///
    /// # Errors
    /// Why the PTY could not do it (stored as the `failed` effect's detail).
    pub fn execute(&self, change: &ShellChange, policy: &Policy, now: i64) -> Result<(), String> {
        match change {
            ShellChange::Open {
                shell_id,
                cwd,
                cols,
                rows,
            } => {
                let cwd = policy.workspace(cwd).map_err(|e| e.to_string())?;
                self.open(shell_id, &cwd, *cols, *rows, now)
                    .map_err(|r| r.message)
            }
            ShellChange::Resize {
                shell_id,
                cols,
                rows,
            } => {
                let shell = self.get(shell_id).map_err(|r| r.message)?;
                lock(&shell.master)
                    .resize(size(*cols, *rows))
                    .map_err(|e| format!("resize: {e}"))?;
                let mut info = lock(&shell.info);
                info.cols = *cols;
                info.rows = *rows;
                Ok(())
            }
            ShellChange::Close { shell_id } => {
                let shell = lock(&self.shells)
                    .remove(shell_id.as_str())
                    .ok_or_else(|| unknown(shell_id).message)?;
                match shell.pid {
                    // A TUI gets SIGHUP for its whole process group; the
                    // hold ends when the pump sees the process gone.
                    Some(pid) if shell.session.is_some() => handover::hang_up(&shell, pid),
                    _ => {
                        if let Err(e) = lock(&shell.child).kill() {
                            tracing::debug!(error = %e, "shell kill");
                        }
                    }
                }
                // Dropping the subscribers ends every attached stream.
                lock(&shell.output).subscribers.clear();
                tracing::info!(shell = %shell_id, "shell closed");
                Ok(())
            }
            // A handover needs the store ([`dispatch`]); a release is decided
            // in its receipt transaction and has no effect.
            ShellChange::Handover { .. } | ShellChange::Release { .. } => Err(format!(
                "{} is not a PTY effect of its own",
                change.method()
            )),
        }
    }

    /// [`Self::check`] and [`Self::execute`] at once, without a store
    /// (unit tests).
    ///
    /// # Errors
    /// The check's reject, or `invalid_state` when the PTY fails.
    pub fn apply(&self, change: &ShellChange, policy: &Policy, now: i64) -> Result<(), Reject> {
        self.check(change, policy, &[])?;
        self.execute(change, policy, now)
            .map_err(|m| reject(RejectCode::InvalidState, m))
    }

    fn get(&self, shell_id: &str) -> Result<Arc<Shell>, Reject> {
        lock(&self.shells)
            .get(shell_id)
            .cloned()
            .ok_or_else(|| unknown(shell_id))
    }

    fn program(&self) -> (OsString, Vec<OsString>) {
        if let Some(p) = &self.config.program {
            return p.clone();
        }
        let shell = self
            .env
            .get("SHELL")
            .filter(|s| !s.is_empty())
            .map_or_else(|| OsString::from("/bin/sh"), ToOwned::to_owned);
        (shell, vec![OsString::from("-l")])
    }

    fn open(
        &self,
        shell_id: &str,
        cwd: &std::path::Path,
        cols: u16,
        rows: u16,
        now: i64,
    ) -> Result<(), Reject> {
        let launch = Launch {
            program: self.program(),
            env: self.env.clone(),
            session: None,
        };
        self.spawn(shell_id, cwd, cols, rows, now, launch)
            .map(|_| ())
    }

    /// Starts the provider TUI of `session_id` in a new shell and returns
    /// its process id (DL-140).
    ///
    /// # Errors
    /// The reject with why the shell did not start.
    pub fn handover(
        &self,
        shell_id: &str,
        session_id: &SessionId,
        launch: &HandoverLaunch,
        cols: u16,
        rows: u16,
        now: i64,
    ) -> Result<Option<u32>, Reject> {
        let instance = self
            .config
            .instances
            .get(&launch.provider_instance)
            .ok_or_else(|| {
                reject(
                    RejectCode::InvalidState,
                    format!(
                        "provider instance `{}` has no binary for a terminal",
                        launch.provider_instance
                    ),
                )
            })?;
        let program = handover::tui_command(
            instance.driver,
            &instance.binary,
            &launch.native,
            launch.mode.as_deref(),
            launch.model.as_deref(),
        );
        let launch_env = Launch {
            program,
            env: instance.env.clone(),
            session: Some(session_id.clone()),
        };
        self.spawn(shell_id, &launch.cwd, cols, rows, now, launch_env)
    }

    fn spawn(
        &self,
        shell_id: &str,
        cwd: &std::path::Path,
        cols: u16,
        rows: u16,
        now: i64,
        launch: Launch,
    ) -> Result<Option<u32>, Reject> {
        let mut shells = lock(&self.shells);
        if shells.contains_key(shell_id) {
            return Err(reject(
                RejectCode::InvalidState,
                format!("shell `{shell_id}` exists"),
            ));
        }
        if shells.len() >= self.config.max_shells as usize {
            return Err(reject(
                RejectCode::InvalidState,
                format!(
                    "the station runs its maximum of {} shells (max_shells)",
                    self.config.max_shells
                ),
            ));
        }
        let failed = |what: &str, e: &dyn std::fmt::Display| {
            reject(RejectCode::InvalidState, format!("{what}: {e}"))
        };
        let pair = native_pty_system()
            .openpty(size(cols, rows))
            .map_err(|e| failed("open a pty", &e))?;
        let (program, args) = &launch.program;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.cwd(cwd);
        cmd.env_clear();
        for (k, v) in launch.env.iter() {
            cmd.env(k, v);
        }
        cmd.env("TERM", "xterm-256color");
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| failed("start the shell", &e))?;
        let pid = child.process_id();
        drop(pair.slave);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| failed("pty reader", &e))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| failed("pty writer", &e))?;
        let shell = Arc::new(Shell {
            info: Mutex::new(ShellInfo {
                shell_id: shell_id.to_owned(),
                cwd: cwd.to_string_lossy().into_owned(),
                state: ShellState::Running,
                cols,
                rows,
                created_at: now,
                exit_code: None,
                session_id: launch.session.clone(),
            }),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            output: Mutex::new(Output {
                ring: Scrollback::default(),
                subscribers: Vec::new(),
            }),
            session: launch.session.clone(),
            pid,
        });
        let read_side = Arc::clone(&shell);
        let exits = launch.session.is_some().then(|| self.exits.clone());
        let name = format!("shell-{shell_id}");
        std::thread::Builder::new()
            .name(name)
            .spawn(move || pump(&read_side, reader, exits.as_ref()))
            .map_err(|e| failed("reader thread", &e))?;
        shells.insert(shell_id.to_owned(), shell);
        tracing::info!(shell = %shell_id, cwd = %cwd.display(), "shell opened");
        Ok(pid)
    }

    /// The scrollback and a queue of the live output after it.
    #[must_use]
    pub fn attach(&self, shell_id: &str) -> Option<(Vec<u8>, mpsc::Receiver<Vec<u8>>)> {
        let shell = self.get(shell_id).ok()?;
        let (tx, rx) = mpsc::channel(SUBSCRIBER_QUEUE);
        let running = lock(&shell.info).state == ShellState::Running;
        let mut out = lock(&shell.output);
        if running {
            out.subscribers.push(tx);
        }
        Some((out.ring.snapshot(), rx))
    }

    /// True if `shell_id` exists.
    #[must_use]
    pub fn exists(&self, shell_id: &str) -> bool {
        lock(&self.shells).contains_key(shell_id)
    }

    /// Writes keystrokes to the shell; false when it is gone.
    pub fn input(&self, shell_id: &str, bytes: &[u8]) -> bool {
        let Ok(shell) = self.get(shell_id) else {
            return false;
        };
        let mut w = lock(&shell.writer);
        w.write_all(bytes).and_then(|()| w.flush()).is_ok()
    }

    /// Ends every shell (station stop; A10).
    pub fn close_all(&self) {
        let all = std::mem::take(&mut *lock(&self.shells));
        for shell in all.values() {
            match shell.pid {
                Some(pid) if shell.session.is_some() => handover::hang_up_now(pid),
                _ => {
                    if let Err(e) = lock(&shell.child).kill() {
                        tracing::debug!(error = %e, "shell kill");
                    }
                }
            }
        }
    }
}

/// Decides one shell command in its receipt transaction: the policy check
/// and one durable `queued` effect, no PTY I/O (DL-405).
///
/// # Errors
/// The check's reject (stored as the receipt), or a store error.
pub fn decide(
    d: &mut Decision<'_>,
    shells: &Shells,
    policy: &Policy,
    envelope: &ShellEnvelope,
) -> Result<CommandResult, DecideError> {
    let pending = shell_effects::pending(d.connection())?;
    shells.check(&envelope.change, policy, &pending)?;
    match &envelope.change {
        ShellChange::Handover {
            shell_id,
            session_id,
            ..
        } => handover::admit(d, policy, &shells.config.instances, session_id, shell_id)?,
        ShellChange::Release { shell_id } => {
            handover::decide_release(d, shells, &pending, shell_id)?;
            return Ok(CommandResult::default());
        }
        _ => {}
    }
    let effect_id: EffectId = d.new_id();
    shell_effects::enqueue(
        d.connection(),
        effect_id.as_str(),
        &envelope.command_id,
        &envelope.change,
        d.now(),
    )?;
    Ok(CommandResult::default())
}

/// Runs the queued shell effects in commit order: each is claimed before
/// its PTY I/O and ends `confirmed` or `failed`. Returns how many ran.
///
/// # Errors
/// A store error; the effect then stays claimed, and the next start
/// recovery marks it `outcome_unknown`.
pub async fn dispatch(
    store: &Store,
    shells: &Arc<Shells>,
    policy: &Policy,
) -> Result<usize, StoreError> {
    let _turn = shells.dispatch.lock().await;
    let mut done = 0;
    while let Some(effect) = store.claim_shell_effect().await? {
        let worker = Arc::clone(shells);
        let policy = policy.clone();
        let now = store.now_ms();
        let change = effect.change;
        let outcome = if let ShellChange::Handover {
            shell_id,
            session_id,
            cols,
            rows,
        } = &change
        {
            start_terminal(store, worker, shell_id, session_id, (*cols, *rows), now).await?
        } else {
            tokio::task::spawn_blocking(move || worker.execute(&change, &policy, now))
                .await
                .unwrap_or_else(|e| Err(format!("shell task: {e}")))
        };
        if let Err(e) = &outcome {
            tracing::warn!(command = %effect.command_id, error = %e, "shell effect failed");
        }
        store.finish_shell_effect(effect.effect_id, outcome).await?;
        done += 1;
    }
    Ok(done)
}

/// The PTY I/O of one `shell.handover`: starts the TUI and records its pid
/// in the hold, or ends the hold when the TUI did not start.
async fn start_terminal(
    store: &Store,
    shells: Arc<Shells>,
    shell_id: &str,
    session_id: &SessionId,
    (cols, rows): (u16, u16),
    now: i64,
) -> Result<Result<(), String>, StoreError> {
    let started = match store.handover_launch(session_id.clone()).await? {
        None => Err("the session has no resume identity".to_owned()),
        Some(launch) => {
            let (shell, session) = (shell_id.to_owned(), session_id.clone());
            tokio::task::spawn_blocking(move || {
                shells.handover(&shell, &session, &launch, cols, rows, now)
            })
            .await
            .unwrap_or_else(|e| Err(reject(RejectCode::InvalidState, format!("shell task: {e}"))))
            .map_err(|r| r.message)
        }
    };
    match started {
        Ok(pid) => {
            if let Some(pid) = pid {
                store
                    .record_terminal_pid(session_id.clone(), shell_id.to_owned(), pid)
                    .await?;
            }
            Ok(Ok(()))
        }
        Err(e) => {
            let reason = format!("the terminal did not start: {e}");
            store
                .release_terminal(session_id.clone(), shell_id.to_owned(), reason)
                .await?;
            Ok(Err(e))
        }
    }
}

fn unknown(shell_id: &str) -> Reject {
    reject(RejectCode::InvalidState, format!("no shell `{shell_id}`"))
}

/// Copies the PTY output into the scrollback and to the subscribers until
/// the shell ends.
fn pump(
    shell: &Shell,
    mut reader: Box<dyn Read + Send>,
    exits: Option<&mpsc::UnboundedSender<TerminalExit>>,
) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let bytes = &buf[..n];
                let mut out = lock(&shell.output);
                out.ring.push(bytes);
                out.subscribers
                    .retain(|s| match s.try_send(bytes.to_vec()) {
                        Ok(()) => true,
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            tracing::info!("a slow shell reader is cut off");
                            false
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => false,
                    });
            }
        }
    }
    let code = lock(&shell.child).wait().ok().map(|s| s.exit_code());
    {
        let mut info = lock(&shell.info);
        info.state = ShellState::Exited;
        info.exit_code = code;
    }
    lock(&shell.output).subscribers.clear();
    // A TUI shell reports the end of its process: the hold of its session
    // ends then (DL-140). The shell id is the key of the hold.
    if let (Some(exits), Some(session_id)) = (exits, &shell.session) {
        let shell_id = lock(&shell.info).shell_id.clone();
        let report = TerminalExit {
            session_id: session_id.clone(),
            shell_id,
            code,
        };
        if exits.send(report).is_err() {
            tracing::warn!("no one releases the terminal holds");
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "effects_tests.rs"]
mod effects_tests;
