//! The process side of the shim: socket bind, provider spawn and exec,
//! process-group signals, descendants and reaping.

use super::*;

pub(super) fn bind(spec: &LaunchSpec) -> std::io::Result<UnixListener> {
    let dir = spec
        .socket_path
        .parent()
        .ok_or_else(|| std::io::Error::other("the socket path has no directory"))?;
    prepare_dir(dir)?;
    // The launch lock is held: a socket file here belongs to a dead shim.
    match std::fs::remove_file(&spec.socket_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(&spec.socket_path)?;
    std::fs::set_permissions(&spec.socket_path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Starts the provider through the shim's own executable object (DL-194),
/// with the session `token` in its environment when there is one.
pub(super) fn spawn_provider(
    spec: &LaunchSpec,
    token: Option<&fleetdeck_core::Secret<String>>,
) -> std::io::Result<Child> {
    let exe = crate::host::shim::exe_hash::own_exe()?;
    let mut command = tokio::process::Command::new(exe);
    command
        .arg("shim-exec")
        .arg("--parent")
        .arg(std::process::id().to_string())
        .arg("--")
        .arg(spec.program())
        .args(spec.args())
        .current_dir(spec.cwd())
        .env_clear()
        .envs(spec.env());
    if let Some(token) = token {
        command.env("FLEETDECK_COORDINATOR_TOKEN", token.expose());
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    command.spawn()
}

/// The provider side of `shim-exec`: a new session, the parent-death
/// signal (Linux), the parent check, then `exec`. Returns only on error.
#[must_use]
pub fn exec_provider(parent: u32, program: &std::ffi::OsStr, args: &[std::ffi::OsString]) -> i32 {
    use std::os::unix::process::CommandExt as _;
    if let Err(e) = rustix::process::setsid() {
        eprintln!("fleetdeck shim-exec: setsid: {e}");
        return 126;
    }
    #[cfg(target_os = "linux")]
    if let Err(e) =
        rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))
    {
        eprintln!("fleetdeck shim-exec: parent death signal: {e}");
        return 126;
    }
    // The shim may have died before the signal was armed.
    let ppid = rustix::process::getppid().map(|p| p.as_raw_nonzero().get());
    if u32::try_from(ppid.unwrap_or(0)).ok() != Some(parent) {
        eprintln!("fleetdeck shim-exec: the shim is gone");
        return 126;
    }
    let error = std::process::Command::new(program).args(args).exec();
    eprintln!(
        "fleetdeck shim-exec: {}: {error}",
        program.to_string_lossy()
    );
    127
}

/// Signals the process group and session `pid` (the provider is the
/// leader of both). A group that is gone is fine; init is never signalled.
pub(super) fn signal_group(pid: u32, sig: rustix::process::Signal) {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .filter(|p| *p > 1)
        .and_then(rustix::process::Pid::from_raw)
    else {
        return;
    };
    match rustix::process::kill_process_group(pid, sig) {
        Ok(()) => {}
        // No such group yet: the exec helper has not called `setsid()`, so
        // the child is still in the shim's group. Signal the child itself;
        // it has started nothing else. A signal sent here is never lost
        // (a kill right after the launch once waited for a natural exit).
        Err(rustix::io::Errno::SRCH) => {
            if let Err(e) = rustix::process::kill_process(pid, sig)
                && e != rustix::io::Errno::SRCH
            {
                tracing::warn!(?sig, error = %e, "cannot signal the provider");
            }
        }
        Err(e) => tracing::warn!(?sig, error = %e, "cannot signal the provider group"),
    }
}

/// The live members of the provider's process group or session (Linux;
/// zombies do not count). `None` when the inspection is incomplete or the
/// platform has no reader: the count then proves nothing (DL-182 §1a).
pub(super) fn group_members(leader: u32) -> Option<u32> {
    crate::host::shim::inspect::group_members_reaping(leader, &mut reap_orphans)
}

/// Reaps orphaned descendants that came back to the subreaper shim.
pub(super) fn reap_orphans() {
    while let Ok(Some(_)) = rustix::process::waitpid(None, rustix::process::WaitOptions::NOHANG) {}
}
