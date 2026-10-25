//! The station side of the TUI handover (DL-140, `docs/v2/UI-PLAN.md` §3).
//!
//! `shell.handover` runs the provider's own TUI on an ended session in a PTY
//! shell. While it runs, the session is **held** (`summary.terminal`): the
//! hold starts in the decision transaction of the command ([`admit`]) and
//! ends only when the station knows that the TUI process is gone (the exit
//! report of the pump, [`spawn_exit_release`]), when the TUI never started,
//! or when the owner clears a hold that the station cannot see after a
//! restart (`shell.release`, [`decide_release`]; [`recover_holds`] at start).
//! A failed handover effect with a hold and no pid is an uncertain start, so
//! a restart keeps that hold.
//! The station publishes the death of a TUI only after it observed it.
//!
//! A closed TUI stays in the closing set of [`Shells`] until its process is
//! reaped, and `shell.release` is refused meanwhile. Every signal to the
//! process group of a TUI goes through [`signal_group`], which sends nothing
//! once the leader is reaped: while the leader is not reaped its pid (and so
//! its group id) cannot be reused. The pump reaps the leader only after it
//! has marked the shell reaped under the same lock. The station stop closes
//! every TUI the same way (HUP, grace, KILL; [`Shells::close_all`]).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_core::config::Driver;
use fleetdeck_proto::command::{Reject, RejectCode};
use fleetdeck_proto::{Event, ProviderInstanceId, SessionId};
use fleetdeck_providers::claude::args::{BYPASS_MODE, NATIVE_MODES, SAFEST_MODE};
use fleetdeck_providers::codex::{FULL_ACCESS_SANDBOX, Mode};
use rustix::io::Errno;
use rustix::process::{
    Pid, Signal, WaitId, WaitIdOptions, kill_process_group, test_kill_process, waitid,
};
use tokio::sync::mpsc::UnboundedReceiver;

use super::{Shell, Shells, lock};
use crate::manager::policy::Policy;
use crate::manager::resume::{not_held, resumable};
use crate::store::shell_effects::PendingShell;
use crate::store::{DecideError, Decision, Store, StoreError};

/// How long a closed TUI gets after SIGHUP before its process group is
/// killed.
pub(super) const HANGUP_GRACE: Duration = Duration::from_secs(3);

/// What the station needs to start the TUI of one provider instance.
#[derive(Clone)]
pub struct TerminalInstance {
    pub driver: Driver,
    pub binary: PathBuf,
    /// The scrubbed environment of the instance (its secrets included).
    pub env: ChildEnv,
}

impl std::fmt::Debug for TerminalInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the environment: it may hold the provider's credentials.
        f.debug_struct("TerminalInstance")
            .field("driver", &self.driver)
            .field("binary", &self.binary)
            .finish_non_exhaustive()
    }
}

/// The report of the pump that a TUI process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalExit {
    pub session_id: SessionId,
    pub shell_id: String,
    pub code: Option<u32>,
}

fn model_args(flag: &str, model: Option<&str>, args: &mut Vec<OsString>) {
    if let Some(model) = model.filter(|m| !m.is_empty() && *m != "default") {
        args.extend([OsString::from(flag), OsString::from(model)]);
    }
}

/// The program and arguments of the provider TUI that continues `native`
/// with the stored `mode` and `model`. An unknown mode becomes the most
/// restrictive one (L8), never a more permissive one. A bypass or
/// full-access mode becomes the most restrictive one too, unless
/// `allow_bypass` is set (NV-24).
#[must_use]
pub fn tui_command(
    driver: Driver,
    binary: &Path,
    native: &str,
    mode: Option<&str>,
    model: Option<&str>,
    allow_bypass: bool,
) -> (OsString, Vec<OsString>) {
    let mut args: Vec<OsString> = Vec::new();
    match driver {
        Driver::Claude => {
            let mode = mode
                .filter(|m| NATIVE_MODES.contains(m))
                .unwrap_or(SAFEST_MODE);
            // The bypass setting may be off now (NV-24).
            let mode = if mode == BYPASS_MODE && !allow_bypass {
                SAFEST_MODE
            } else {
                mode
            };
            args.extend(["--resume", native, "--permission-mode", mode].map(OsString::from));
            model_args("--model", model, &mut args);
        }
        Driver::Codex => {
            let mode = mode.and_then(Mode::parse).unwrap_or_else(Mode::safest);
            // The full-access setting may be off now (NV-24).
            let mode = if mode.sandbox == FULL_ACCESS_SANDBOX && !allow_bypass {
                Mode::safest()
            } else {
                mode
            };
            args.extend(
                [
                    "resume",
                    "-a",
                    mode.approval_policy.as_str(),
                    "-s",
                    mode.sandbox.as_str(),
                ]
                .map(OsString::from),
            );
            model_args("-m", model, &mut args);
            args.push(OsString::from(native));
        }
    }
    (binary.as_os_str().to_owned(), args)
}

fn invalid_state(message: impl Into<String>) -> DecideError {
    DecideError::Reject(Reject::new(RejectCode::InvalidState, message))
}

/// A new shell (`shell.open` or `shell.handover`) never takes the id of a
/// shell whose terminal holds a session: the hold's recovery reads the
/// handover effects of that id.
///
/// # Errors
/// A stored rejection, or a database error.
pub(super) fn free_of_holds(d: &Decision<'_>, shell_id: &str) -> Result<(), DecideError> {
    match d.shell_holds(shell_id)? {
        Some(other) => Err(invalid_state(format!(
            "shell `{shell_id}` holds session `{other}`; release it first"
        ))),
        None => Ok(()),
    }
}

/// The decision of `shell.handover` after [`Shells::check`]: the session is
/// resumable (the checks of `session.resume`), nothing holds it or its
/// native session, nothing else drives the native session, and the
/// provider instance has a binary for a terminal. Then the hold starts.
///
/// # Errors
/// A stored rejection, or a database error.
pub(super) fn admit(
    d: &mut Decision<'_>,
    policy: &Policy,
    instances: &BTreeMap<ProviderInstanceId, TerminalInstance>,
    session_id: &SessionId,
    shell_id: &str,
) -> Result<(), DecideError> {
    let r = resumable(d, policy, session_id)?;
    not_held(d, &r)?;
    if let Some(other) = d.native_session_in_use(&r.native)? {
        return Err(invalid_state(format!(
            "session `{other}` already drives this native session"
        )));
    }
    if !instances.contains_key(&r.instance) {
        return Err(invalid_state(format!(
            "provider instance `{}` has no binary for a terminal",
            r.instance
        )));
    }
    d.append(
        session_id,
        Event::SessionTerminal {
            shell_id: Some(shell_id.to_owned()),
            pid: None,
            reason: Some("handover requested".to_owned()),
        }
        .into(),
    )?;
    Ok(())
}

/// The decision of `shell.release`: it clears a hold that no live shell
/// backs (after a restart) and appends the `session.terminal` that ends it.
///
/// # Errors
/// A stored rejection, or a database error.
pub(super) fn decide_release(
    d: &mut Decision<'_>,
    shells: &Shells,
    pending: &[PendingShell],
    shell_id: &str,
) -> Result<(), DecideError> {
    if shells.exists(shell_id) {
        return Err(invalid_state(format!(
            "shell `{shell_id}` still runs; close it first"
        )));
    }
    if shells.holds_process(shell_id) {
        return Err(invalid_state(format!(
            "shell `{shell_id}` is closing and its process still runs; try again when it has stopped"
        )));
    }
    if pending.iter().any(|p| p.shell_id == shell_id) {
        return Err(invalid_state(format!(
            "shell `{shell_id}` has a pending change; try again when it settles"
        )));
    }
    let Some(session) = d.session_held_by_shell(shell_id)? else {
        return Err(invalid_state(format!(
            "no session is held by shell `{shell_id}`"
        )));
    };
    d.append(
        &session,
        Event::SessionTerminal {
            shell_id: None,
            pid: None,
            reason: Some("the owner released the terminal hold".to_owned()),
        }
        .into(),
    )?;
    Ok(())
}

fn pid_of(pid: u32) -> Option<Pid> {
    Pid::from_raw(i32::try_from(pid).ok()?)
}

/// What a probe with signal 0 says about a process id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// The process exists and may be signalled.
    Running,
    /// No such process (`ESRCH`): the only answer that proves it is gone.
    Gone,
    /// The probe cannot tell (`EPERM`, another error, or a pid that is not
    /// a valid process id): the caller must treat the process as possibly
    /// alive.
    Unknown,
}

/// Probes `pid` with signal 0. Only `ESRCH` is [`Liveness::Gone`].
#[must_use]
pub fn liveness(pid: u32) -> Liveness {
    let Some(target) = pid_of(pid) else {
        return Liveness::Unknown;
    };
    match test_kill_process(target) {
        Ok(()) => Liveness::Running,
        Err(Errno::SRCH) => Liveness::Gone,
        Err(_) => Liveness::Unknown,
    }
}

/// Sends `signal` to the process group of a TUI shell, unless its leader is
/// reaped (or the shell has no pid). The lock of `reaped` is held across the
/// send and the pump takes it before it reaps, so the group id cannot be
/// reused between the check and the signal. True when the signal was sent.
pub(super) fn signal_group(shell: &Shell, signal: Signal) -> bool {
    let reaped = lock(&shell.reaped);
    let Some(pid) = shell.pid else { return false };
    if *reaped {
        return false;
    }
    let Some(target) = pid_of(pid) else {
        return false;
    };
    match kill_process_group(target, signal) {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!(error = %e, pid, ?signal, "terminal signal");
            false
        }
    }
}

/// Returns when the process of `shell` has been observed to exit, and marks
/// the shell reaped: from then on [`signal_group`] sends nothing. `reaped`
/// is set only after an observed exit, never on an error.
///
/// - `waitid` waits until the process is a zombie without reaping it: the
///   flag is set and the caller reaps the child right after (the pid stays
///   unusable until then).
/// - When `waitid` fails (or there is no pid), the child is polled with
///   `try_wait`, which reaps it; the reap and the flag change happen under
///   the same `reaped` lock, so [`signal_group`] (which holds `reaped`
///   across the kill) never signals a reaped pid. A later `wait` of the
///   caller returns the cached status.
///
/// Lock order here: `reaped`, then `child`. Nothing takes `child` and then
/// `reaped`.
pub(super) fn mark_exited(shell: &Shell) {
    if let Some(target) = shell.pid.and_then(pid_of) {
        loop {
            match waitid(
                WaitId::Pid(target),
                WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
            ) {
                Err(Errno::INTR) => {}
                Ok(_) => {
                    *lock(&shell.reaped) = true;
                    return;
                }
                Err(e) => {
                    tracing::warn!(error = %e, pid = shell.pid, "terminal wait failed; polling");
                    break;
                }
            }
        }
    } else {
        tracing::warn!("terminal has no usable pid; polling");
    }
    loop {
        {
            let mut reaped = lock(&shell.reaped);
            if let Ok(Some(_)) = lock(&shell.child).try_wait() {
                *reaped = true;
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Hangs up the process group of a closed TUI (the TUI leads its session
/// and group: the PTY spawn makes it so). After a grace time the group is
/// killed unless the pump has reaped the leader by then.
pub(super) fn hang_up(shell: &Arc<Shell>) {
    if !signal_group(shell, Signal::HUP) {
        return;
    }
    let shell = Arc::clone(shell);
    let name = format!("hangup-{}", shell.pid.unwrap_or_default());
    let spawned = std::thread::Builder::new().name(name).spawn(move || {
        std::thread::sleep(HANGUP_GRACE);
        if signal_group(&shell, Signal::KILL) {
            tracing::info!(
                pid = shell.pid,
                "the terminal ignored the hangup; killing it"
            );
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "terminal kill timer");
    }
}

/// Releases the hold of every TUI that the station learns has ended.
pub fn spawn_exit_release(
    store: Store,
    mut exits: UnboundedReceiver<TerminalExit>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(exit) = exits.recv().await {
            let reason = exit.code.map_or_else(
                || "the terminal process exited".to_owned(),
                |c| format!("the terminal process exited (code {c})"),
            );
            if let Err(e) = store
                .release_terminal(exit.session_id.clone(), exit.shell_id, reason)
                .await
            {
                tracing::warn!(session = %exit.session_id, error = %e, "terminal release failed");
            }
        }
    })
}

/// Start recovery of the holds (before the node listens): a TUI belongs to
/// the process that started it, so no live shell backs a stored hold.
///
/// - the stored pid still runs: keep the hold and say why (the owner checks
///   the process, then `shell.release`);
/// - the stored pid is gone (`ESRCH`): release;
/// - the stored pid cannot be checked (`EPERM`, another error): keep the
///   hold and say why;
/// - no pid and no handover effect, or one that was retired without being
///   dispatched: release;
/// - no pid otherwise, a `failed` effect included (a start that did not
///   happen releases its hold before the effect ends, so a hold that is
///   still there means the start was uncertain): keep the hold.
///
/// # Errors
/// A store error.
pub async fn recover_holds(store: &Store) -> Result<(), StoreError> {
    for (session, hold) in store.held_sessions().await? {
        let shell = hold.shell_id;
        match hold.pid {
            Some(pid) => match liveness(pid) {
                Liveness::Running => {
                    let reason =
                        format!("the station restarted; process {pid} may still use this session");
                    store
                        .note_terminal(session, shell, Some(pid), reason)
                        .await?;
                }
                Liveness::Unknown => {
                    let reason = format!(
                        "the station restarted; it cannot check process {pid}, so the hold stays"
                    );
                    store
                        .note_terminal(session, shell, Some(pid), reason)
                        .await?;
                }
                Liveness::Gone => {
                    let reason = "the station restarted; the terminal process is gone".to_owned();
                    store.release_terminal(session, shell, reason).await?;
                }
            },
            None => {
                let state = store
                    .handover_effect_state(shell.clone(), session.clone())
                    .await?;
                if matches!(state.as_deref(), None | Some("retired_not_dispatched")) {
                    let reason = "the station restarted before the terminal started".to_owned();
                    store.release_terminal(session, shell, reason).await?;
                } else {
                    let reason =
                        "the station restarted; a terminal process may still use this session"
                            .to_owned();
                    store.note_terminal(session, shell, None, reason).await?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(command: &(OsString, Vec<OsString>)) -> Vec<String> {
        command
            .1
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn claude(mode: Option<&str>, model: Option<&str>) -> Vec<String> {
        text(&tui_command(
            Driver::Claude,
            Path::new("/bin/claude"),
            "native-1",
            mode,
            model,
            true,
        ))
    }

    fn codex(mode: Option<&str>, model: Option<&str>) -> Vec<String> {
        text(&tui_command(
            Driver::Codex,
            Path::new("/bin/codex"),
            "thread-1",
            mode,
            model,
            true,
        ))
    }

    #[test]
    fn claude_keeps_a_native_mode_and_names_the_binary() {
        let command = tui_command(
            Driver::Claude,
            Path::new("/bin/claude"),
            "native-1",
            Some("plan"),
            None,
            false,
        );
        assert_eq!(command.0, OsString::from("/bin/claude"));
        assert_eq!(
            text(&command),
            ["--resume", "native-1", "--permission-mode", "plan"]
        );
    }

    #[test]
    fn claude_maps_an_unknown_or_missing_mode_to_default() {
        let want = ["--resume", "native-1", "--permission-mode", "default"];
        assert_eq!(claude(Some("yolo"), None), want);
        assert_eq!(claude(None, None), want);
    }

    #[test]
    fn claude_passes_a_model_but_not_the_default_sentinel() {
        assert_eq!(
            claude(Some("default"), Some("opus")),
            [
                "--resume",
                "native-1",
                "--permission-mode",
                "default",
                "--model",
                "opus"
            ]
        );
        for none in [Some("default"), Some(""), None] {
            assert!(!claude(Some("default"), none).contains(&"--model".to_owned()));
        }
    }

    #[test]
    fn codex_parses_the_mode_and_puts_the_thread_last() {
        let command = tui_command(
            Driver::Codex,
            Path::new("/bin/codex"),
            "thread-1",
            Some("on-request/workspace-write"),
            None,
            false,
        );
        assert_eq!(command.0, OsString::from("/bin/codex"));
        assert_eq!(
            text(&command),
            [
                "resume",
                "-a",
                "on-request",
                "-s",
                "workspace-write",
                "thread-1"
            ]
        );
    }

    #[test]
    fn codex_falls_back_to_the_safest_mode() {
        let want = ["resume", "-a", "untrusted", "-s", "read-only", "thread-1"];
        assert_eq!(codex(Some("bogus"), None), want);
        assert_eq!(codex(Some("claude/plan"), None), want);
        assert_eq!(codex(None, None), want);
    }

    #[test]
    fn codex_passes_a_model_before_the_thread() {
        assert_eq!(
            codex(Some("never/danger-full-access"), Some("gpt-x")),
            [
                "resume",
                "-a",
                "never",
                "-s",
                "danger-full-access",
                "-m",
                "gpt-x",
                "thread-1"
            ]
        );
        assert!(!codex(None, Some("default")).contains(&"-m".to_owned()));
    }

    #[test]
    fn claude_keeps_the_bypass_mode_only_when_bypass_is_allowed() {
        let command = |allow| {
            text(&tui_command(
                Driver::Claude,
                Path::new("/bin/claude"),
                "native-1",
                Some("bypassPermissions"),
                None,
                allow,
            ))
        };
        assert_eq!(
            command(true),
            [
                "--resume",
                "native-1",
                "--permission-mode",
                "bypassPermissions"
            ]
        );
        // NV-24: the setting is off now.
        assert_eq!(
            command(false),
            ["--resume", "native-1", "--permission-mode", SAFEST_MODE]
        );
    }

    #[test]
    fn codex_keeps_the_full_access_mode_only_when_bypass_is_allowed() {
        let command = |allow| {
            text(&tui_command(
                Driver::Codex,
                Path::new("/bin/codex"),
                "thread-1",
                Some("never/danger-full-access"),
                None,
                allow,
            ))
        };
        assert_eq!(
            command(true),
            [
                "resume",
                "-a",
                "never",
                "-s",
                "danger-full-access",
                "thread-1"
            ]
        );
        // NV-24: the setting is off now.
        assert_eq!(
            command(false),
            ["resume", "-a", "untrusted", "-s", "read-only", "thread-1"]
        );
    }

    #[test]
    fn the_debug_of_an_instance_hides_its_environment() {
        let mut env = ChildEnv::empty();
        assert!(env.set("ANTHROPIC_API_KEY", "sk-secret").is_ok());
        let instance = TerminalInstance {
            driver: Driver::Claude,
            binary: PathBuf::from("/bin/claude"),
            env,
        };
        let shown = format!("{instance:?}");
        assert!(shown.contains("/bin/claude"));
        assert!(!shown.contains("sk-secret"));
        assert!(!shown.contains("ANTHROPIC"));
    }

    #[test]
    fn liveness_tells_gone_from_unknown() {
        assert_eq!(liveness(std::process::id()), Liveness::Running);

        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert_eq!(liveness(pid), Liveness::Gone);

        // Not a valid process id at all.
        assert_eq!(liveness(u32::MAX), Liveness::Unknown);

        // Pid 1 exists: Running for root, otherwise `EPERM` is Unknown.
        let one = liveness(1);
        assert_ne!(one, Liveness::Gone);
        if rustix::process::geteuid().is_root() {
            assert_eq!(one, Liveness::Running);
        } else {
            assert_eq!(one, Liveness::Unknown);
        }
    }
}
