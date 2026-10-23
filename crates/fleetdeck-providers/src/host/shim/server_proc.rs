//! The process side of the shim: socket bind, provider spawn and exec,
//! process-group signals, descendants and reaping.

use fleetdeck_core::launch_dir::LaunchDir;

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
    // With an identity the provider starts in the checked directory object,
    // or not at all; `dir` lives until the spawn returned (B9).
    let dir = spec
        .launch
        .as_ref()
        .map(|id| LaunchDir::reopen(id).map_err(|e| std::io::Error::other(e.to_string())))
        .transpose()?;
    let cwd = match &dir {
        Some(dir) => dir
            .cwd()
            .map_err(|e| std::io::Error::other(e.to_string()))?,
        None => spec.cwd(),
    };
    let mut command = tokio::process::Command::new(exe);
    command
        .arg("shim-exec")
        .arg("--parent")
        .arg(std::process::id().to_string())
        .arg("--")
        .arg(spec.program())
        .args(spec.args())
        .current_dir(cwd)
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
    let child = command.spawn();
    drop(dir);
    child
}

/// The options of `shim-exec` for a child that needs more than the plain
/// launch (the recorder: its output file as FD 3, a file-size limit).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecOptions {
    /// `RLIMIT_FSIZE` (soft and hard) in bytes.
    pub fsize: Option<u64>,
    /// Move the inherited stdin to FD 3 and replace stdin with `/dev/null`.
    pub stdin_fd3: bool,
}

/// The provider side of `shim-exec`: a new session, the parent-death
/// signal (Linux), the parent check, then `exec`. Returns only on error.
#[must_use]
pub fn exec_provider(parent: u32, program: &std::ffi::OsStr, args: &[std::ffi::OsString]) -> i32 {
    exec_provider_with(parent, ExecOptions::default(), program, args)
}

/// Moves stdin to FD 3 (inheritable) and puts `/dev/null` on stdin.
fn stdin_to_fd3() -> Result<(), String> {
    use std::os::fd::IntoRawFd as _;
    let fd3 = rustix::io::fcntl_dupfd_cloexec(rustix::stdio::stdin(), 3)
        .map_err(|e| format!("duplicate stdin: {e}"))?;
    if std::os::fd::AsRawFd::as_raw_fd(&fd3) != 3 {
        return Err("descriptor 3 is not free".to_owned());
    }
    rustix::io::fcntl_setfd(&fd3, rustix::io::FdFlags::empty())
        .map_err(|e| format!("clear close-on-exec on descriptor 3: {e}"))?;
    // Descriptor 3 stays open for the child: `exec` keeps it.
    let _ = fd3.into_raw_fd();
    let null = rustix::fs::open(
        "/dev/null",
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|e| format!("open /dev/null: {e}"))?;
    rustix::stdio::dup2_stdin(&null).map_err(|e| format!("replace stdin: {e}"))
}

/// [`exec_provider`] with `o`: the options apply before the new session.
#[must_use]
pub fn exec_provider_with(
    parent: u32,
    o: ExecOptions,
    program: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
) -> i32 {
    use std::os::unix::process::CommandExt as _;
    if o.stdin_fd3
        && let Err(e) = stdin_to_fd3()
    {
        eprintln!("fleetdeck shim-exec: {e}");
        return 126;
    }
    if let Some(n) = o.fsize
        && let Err(e) = rustix::process::setrlimit(
            rustix::process::Resource::Fsize,
            rustix::process::Rlimit {
                current: Some(n),
                maximum: Some(n),
            },
        )
    {
        eprintln!("fleetdeck shim-exec: file size limit: {e}");
        return 126;
    }
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

/// Reaps every ended child of the shim (`waitpid(-1)`). Only for the exit
/// path, after tokio has reaped the provider: the shim has no other child
/// whose status another owner needs (DL-430).
pub(super) fn reap_orphans() {
    while let Ok(Some(_)) = rustix::process::wait(rustix::process::WaitOptions::NOHANG) {}
}

/// How often the shim reaps the ended orphans while the provider runs.
pub(super) const REAP_EVERY: Duration = Duration::from_secs(5);

/// While the provider runs, reaps the ended orphans that came back to the
/// subreaper shim, every `every`, on a blocking thread. It ends when
/// `exited` is set (the exit path then reaps every ended child with
/// [`reap_orphans`], `waitpid(-1)`). The provider (`provider`) is never
/// reaped here: its status belongs to tokio.
pub(super) async fn reap_while_running(
    provider: u32,
    every: Duration,
    mut exited: watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = exited.wait_for(|e| *e) => return,
            _ = tick.tick() => {}
        }
        if let Err(e) =
            tokio::task::spawn_blocking(move || reap_orphans_except(Some(provider))).await
        {
            tracing::warn!(error = %e, "the periodic reaping did not run");
        }
    }
}

/// Reaps the ended children of the shim, except `keep` (the provider: tokio
/// takes its status). It never calls `waitpid(None)`: only a child that
/// `/proc` shows as a zombie is waited for, by its pid. A child that
/// vanished meanwhile (`ECHILD`, no `/proc` entry) is fine. A no-op on
/// platforms without `/proc`.
#[cfg(target_os = "linux")]
pub(super) fn reap_orphans_except(keep: Option<u32>) {
    for pid in zombie_children().into_iter().filter(|p| Some(*p) != keep) {
        let Some(target) = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        else {
            continue;
        };
        match rustix::process::waitpid(Some(target), rustix::process::WaitOptions::NOHANG) {
            Ok(_) | Err(rustix::io::Errno::CHILD) => {}
            Err(e) => tracing::warn!(pid, error = %e, "cannot reap an orphan"),
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn reap_orphans_except(_keep: Option<u32>) {}

/// The children of this process that are zombies now.
#[cfg(target_os = "linux")]
fn zombie_children() -> Vec<u32> {
    child_pids()
        .into_iter()
        .filter(|pid| matches!(stat_state_ppid(*pid), Some(('Z', _))))
        .collect()
}

/// The children of this process: the `children` file of every thread, or,
/// when no thread has one (a kernel without `CONFIG_PROC_CHILDREN`), the
/// processes in `/proc` whose parent is this process.
#[cfg(target_os = "linux")]
fn child_pids() -> Vec<u32> {
    let mut listed = false;
    let mut pids = Vec::new();
    if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
        for task in tasks.flatten() {
            // A thread that ended meanwhile has no file: skip it.
            if let Ok(text) = std::fs::read_to_string(task.path().join("children")) {
                listed = true;
                pids.extend(
                    text.split_whitespace()
                        .filter_map(|p| p.parse::<u32>().ok()),
                );
            }
        }
    }
    if listed {
        return pids;
    }
    let me = std::process::id();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return pids;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if matches!(stat_state_ppid(pid), Some((_, ppid)) if ppid == me) {
            pids.push(pid);
        }
    }
    pids
}

/// The state (field 3) and the parent pid (field 4) of `/proc/<pid>/stat`.
/// The command name (field 2) is in parentheses and may hold spaces, so the
/// parse starts after the last `)`. `None` when the entry is gone.
#[cfg(target_os = "linux")]
fn stat_state_ppid(pid: u32) -> Option<(char, u32)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = text.get(text.rfind(')')? + 1..)?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let ppid = fields.next()?.parse().ok()?;
    Some((state, ppid))
}

#[cfg(all(test, target_os = "linux"))]
#[path = "server_proc_tests.rs"]
mod tests;
