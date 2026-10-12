//! The station side of the TUI handover (DL-140, `docs/v2/UI-PLAN.md` §3).
//!
//! `shell.handover` runs the provider's own TUI on an ended session in a PTY
//! shell. While it runs, the session is **held** (`summary.terminal`): the
//! hold starts in the decision transaction of the command ([`admit`]) and
//! ends only when the station knows that the TUI process is gone (the exit
//! report of the pump, [`spawn_exit_release`]), when the TUI never started,
//! or when the owner clears a hold that the station cannot see after a
//! restart (`shell.release`, [`decide_release`]; [`recover_holds`] at start).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_core::config::Driver;
use fleetdeck_proto::command::{Reject, RejectCode};
use fleetdeck_proto::{Event, ProviderInstanceId, SessionId};
use fleetdeck_providers::claude::args::{NATIVE_MODES, SAFEST_MODE};
use fleetdeck_providers::codex::Mode;
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process};
use tokio::sync::mpsc::UnboundedReceiver;

use super::{Shell, ShellState, Shells, lock};
use crate::manager::policy::Policy;
use crate::manager::resume::{not_held, resumable};
use crate::store::shell_effects::PendingShell;
use crate::store::{DecideError, Decision, Store, StoreError};

/// How long a closed TUI gets after SIGHUP before its process group is
/// killed.
const HANGUP_GRACE: Duration = Duration::from_secs(3);

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
/// restrictive one (L8), never a more permissive one.
#[must_use]
pub fn tui_command(
    driver: Driver,
    binary: &Path,
    native: &str,
    mode: Option<&str>,
    model: Option<&str>,
) -> (OsString, Vec<OsString>) {
    let mut args: Vec<OsString> = Vec::new();
    match driver {
        Driver::Claude => {
            let mode = mode
                .filter(|m| NATIVE_MODES.contains(m))
                .unwrap_or(SAFEST_MODE);
            args.extend(["--resume", native, "--permission-mode", mode].map(OsString::from));
            model_args("--model", model, &mut args);
        }
        Driver::Codex => {
            let mode = mode.and_then(Mode::parse).unwrap_or_else(Mode::safest);
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

/// True when a process with `pid` exists.
fn process_exists(pid: u32) -> bool {
    pid_of(pid).is_some_and(|p| test_kill_process(p).is_ok())
}

/// Hangs up the process group of a closed TUI (the TUI leads its session
/// and group: the PTY spawn makes it so). After a grace time the group is
/// killed when the pump has not reported the exit yet.
pub(super) fn hang_up(shell: &Arc<Shell>, pid: u32) {
    let Some(target) = pid_of(pid) else { return };
    if let Err(e) = kill_process_group(target, Signal::HUP) {
        tracing::debug!(error = %e, pid, "terminal hangup");
        return;
    }
    let shell = Arc::clone(shell);
    let spawned = std::thread::Builder::new()
        .name(format!("hangup-{pid}"))
        .spawn(move || {
            std::thread::sleep(HANGUP_GRACE);
            // The pid may be reused once the pump has reaped the child.
            let running = lock(&shell.info).state == ShellState::Running;
            if running && test_kill_process(target).is_ok() {
                tracing::info!(pid, "the terminal ignored the hangup; killing it");
                if let Err(e) = kill_process_group(target, Signal::KILL) {
                    tracing::debug!(error = %e, pid, "terminal kill");
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, pid, "terminal kill timer");
    }
}

/// Hangs up a TUI at the station stop (no grace thread: the process ends).
pub(super) fn hang_up_now(pid: u32) {
    if let Some(target) = pid_of(pid)
        && let Err(e) = kill_process_group(target, Signal::HUP)
    {
        tracing::debug!(error = %e, pid, "terminal hangup");
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
/// - the stored pid is gone: release;
/// - no pid and the handover was never dispatched (or failed): release;
/// - no pid otherwise (the start may have happened): keep the hold.
///
/// # Errors
/// A store error.
pub async fn recover_holds(store: &Store) -> Result<(), StoreError> {
    for (session, hold) in store.held_sessions().await? {
        let shell = hold.shell_id;
        match hold.pid {
            Some(pid) if process_exists(pid) => {
                let reason =
                    format!("the station restarted; process {pid} may still use this session");
                store
                    .note_terminal(session, shell, Some(pid), reason)
                    .await?;
            }
            Some(_) => {
                let reason = "the station restarted; the terminal process is gone".to_owned();
                store.release_terminal(session, shell, reason).await?;
            }
            None => {
                let state = store.handover_effect_state(shell.clone()).await?;
                if matches!(
                    state.as_deref(),
                    None | Some("retired_not_dispatched" | "failed")
                ) {
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
        ))
    }

    fn codex(mode: Option<&str>, model: Option<&str>) -> Vec<String> {
        text(&tui_command(
            Driver::Codex,
            Path::new("/bin/codex"),
            "thread-1",
            mode,
            model,
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
}
