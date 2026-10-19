//! The runner process (`board-gaps.md` §2.9): the PTY with the echo off,
//! the child, the socket, and the record of the exit.
//!
//! Exit codes of [`main`]: 0 the run ended and was recorded; 2 the spec or
//! the folder is refused; 3 the run already started (the lock, the state
//! file or the socket path exists); 4 the start failed; 5 the state of the
//! exit was not written.

use std::ffi::OsString;
use std::io::{self, Read as _};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fleetdeck_proto::desktop_tools::PostCheck;
use portable_pty::{CommandBuilder, PtyPair, PtySize, native_pty_system};
use rustix::fs::{FlockOperation, Mode, OFlags, flock};
use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};
use tokio::net::UnixListener;
use tokio::sync::oneshot;

use super::client::{Ctx, accept_loop};
use super::files::{self, RunState, RunStateKind, Spec};
use super::postcheck;

/// The longest wait for the last PTY output after the exit of the child.
const DRAIN_WAIT: Duration = Duration::from_secs(1);
/// The size of one read of the PTY master.
const READ_BYTES: usize = 16 * 1024;

/// What the runner starts, and what it asks after the exit.
pub(crate) struct ChildPlan {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) postcheck_program: PathBuf,
}

impl ChildPlan {
    /// The fixed transaction: `/usr/bin/sudo /usr/bin/pacman -S --needed
    /// --noconfirm <targets>`. The targets come from the compiled mapping.
    pub(crate) fn from_spec(spec: &Spec) -> Self {
        let mut args: Vec<OsString> = ["/usr/bin/pacman", "-S", "--needed", "--noconfirm"]
            .iter()
            .map(OsString::from)
            .collect();
        args.extend(files::targets(spec).into_iter().map(OsString::from));
        Self {
            program: PathBuf::from("/usr/bin/sudo"),
            args,
            postcheck_program: PathBuf::from("/usr/bin/pacman"),
        }
    }
}

/// The entry of `fleetdeck maintenance-run`. Logs only fixed words to
/// stderr.
#[must_use]
pub fn main(dir: &Path, id: &str) -> i32 {
    let spec = match files::read_spec(dir, id) {
        Ok(spec) => spec,
        Err(e) => {
            eprintln!("maintenance: the spec is refused: {e}");
            return 2;
        }
    };
    let plan = ChildPlan::from_spec(&spec);
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => {
            eprintln!("maintenance: the runtime did not start");
            return 4;
        }
    };
    let code = runtime.block_on(serve(dir, spec, plan));
    // A PTY write that is blocked must not hold the end of the process.
    runtime.shutdown_background();
    code
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// True when something is at `path` (a link too) or when the check fails.
fn occupied(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(e) => e.kind() != io::ErrorKind::NotFound,
    }
}

/// Takes the lock of the run. `Err` is the exit code.
fn take_lock(path: &Path) -> Result<OwnedFd, i32> {
    let fd = rustix::fs::open(
        path,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| {
        eprintln!("maintenance: the lock file did not open");
        4
    })?;
    match flock(&fd, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(fd),
        Err(rustix::io::Errno::WOULDBLOCK) => {
            eprintln!("maintenance: another runner holds the lock");
            Err(3)
        }
        Err(_) => {
            eprintln!("maintenance: the lock failed");
            Err(4)
        }
    }
}

/// Clears `ECHO` and `ECHONL` of the terminal. `portable-pty` gives no
/// descriptor and the code has no `unsafe`, so the runner opens the slave
/// device by its name: master and slave share one set of attributes.
fn echo_off(slave: &Path) -> io::Result<()> {
    let fd = rustix::fs::open(
        slave,
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut attrs = tcgetattr(&fd)?;
    attrs.local_modes &= !(LocalModes::ECHO | LocalModes::ECHONL);
    tcsetattr(&fd, OptionalActions::Now, &attrs)?;
    // Read it back: the baseline must hold before the child starts.
    let after = tcgetattr(&fd)?;
    if after
        .local_modes
        .intersects(LocalModes::ECHO | LocalModes::ECHONL)
    {
        return Err(io::Error::other("the terminal kept the echo"));
    }
    Ok(())
}

fn exit_code_of(status: &portable_pty::ExitStatus) -> i32 {
    if status.signal().is_some() {
        -1
    } else {
        i32::try_from(status.exit_code()).unwrap_or(-1)
    }
}

fn state_of(state: RunStateKind, code: Option<i32>, post: Option<PostCheck>) -> RunState {
    RunState {
        v: 1,
        state,
        exit_code: code,
        post_check: post,
        at: now_ms(),
    }
}

/// Removes the socket path that this runner created.
fn remove_socket(path: &Path) {
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != io::ErrorKind::NotFound
    {
        eprintln!("maintenance: the socket path was not removed");
    }
}

/// The testable core of the runner: the checks, the PTY, the child, the
/// socket, the exit and its record. Returns the exit code of the process.
pub(crate) async fn serve(dir: &Path, spec: Spec, plan: ChildPlan) -> i32 {
    let id = spec.maintenance_id.clone();
    if files::check_dir(dir).is_err() {
        eprintln!("maintenance: the folder is not private");
        return 2;
    }
    let _lock = match take_lock(&files::lock_path(dir, &id)) {
        Ok(fd) => fd,
        Err(code) => return code,
    };
    let sock = files::socket_path(dir, &id);
    if occupied(&files::state_path(dir, &id)) || occupied(&sock) {
        eprintln!("maintenance: this run already started");
        return 3;
    }

    let PtyPair { master, slave } = match native_pty_system().openpty(PtySize {
        rows: spec.rows,
        cols: spec.cols,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(pair) => pair,
        Err(_) => {
            eprintln!("maintenance: the terminal did not open");
            return 4;
        }
    };
    let echo = master
        .tty_name()
        .ok_or_else(|| io::Error::other("the terminal has no name"))
        .and_then(|name| echo_off(&name));
    if echo.is_err() {
        eprintln!("maintenance: the echo of the terminal stays on");
        return 4;
    }

    let listener = match bind(&sock) {
        Ok(listener) => listener,
        Err(code) => return code,
    };

    let mut command = CommandBuilder::new(&plan.program);
    command.args(&plan.args);
    command.env_clear();
    command.env("PATH", "/usr/bin:/bin");
    command.env("TERM", "xterm-256color");
    command.env("LANG", "C.UTF-8");
    command.cwd("/");
    let child = slave.spawn_command(command);
    drop(slave);
    let mut child = match child {
        Ok(child) => child,
        Err(_) => {
            eprintln!("maintenance: the child did not start");
            remove_socket(&sock);
            return 4;
        }
    };
    let (reader, writer) = match (master.try_clone_reader(), master.take_writer()) {
        (Ok(reader), Ok(writer)) => (reader, writer),
        _ => {
            eprintln!("maintenance: the terminal did not open for use");
            return abort_start(&mut *child, &sock);
        }
    };
    let ctx = Ctx::new(
        master,
        writer,
        rustix::process::geteuid().as_raw(),
        spec.station_token.clone(),
        spec.runner_token.clone(),
    );
    if files::write_state(dir, &id, &state_of(RunStateKind::Running, None, None)).is_err() {
        eprintln!("maintenance: the state was not written");
        return abort_start(&mut *child, &sock);
    }

    let (drained_tx, drained) = oneshot::channel::<()>();
    let pump_ctx = Arc::clone(&ctx);
    let pumping = std::thread::Builder::new()
        .name("maintenance-pump".to_owned())
        .spawn(move || pump(reader, &pump_ctx, drained_tx));
    let (exit_tx, exit_rx) = oneshot::channel::<i32>();
    let waiting = std::thread::Builder::new()
        .name("maintenance-wait".to_owned())
        .spawn(move || {
            let code = match child.wait() {
                Ok(status) => exit_code_of(&status),
                Err(_) => {
                    eprintln!("maintenance: the wait for the child failed");
                    -1
                }
            };
            let _ = exit_tx.send(code);
        });
    if pumping.is_err() || waiting.is_err() {
        // The child is not killed here: it ends with the runner's PTY.
        eprintln!("maintenance: a thread did not start");
        remove_socket(&sock);
        return 4;
    }
    let accepting = tokio::spawn(accept_loop(listener, Arc::clone(&ctx)));

    let code = exit_rx.await.unwrap_or(-1);
    ctx.stop_input();
    // The last output may still be in the PTY; a descendant that keeps the
    // terminal open must not hold the runner.
    let _ = tokio::time::timeout(DRAIN_WAIT, drained).await;
    let post = postcheck::check(&plan.postcheck_program, &spec.transaction).await;
    if files::write_output(dir, &id, &ctx.ring_snapshot()).is_err() {
        eprintln!("maintenance: the output was not written");
    }
    let recorded = files::write_state(
        dir,
        &id,
        &state_of(RunStateKind::Exited, Some(code), Some(post)),
    );
    ctx.finish(code).await;
    accepting.abort();
    remove_socket(&sock);
    if recorded.is_err() {
        eprintln!("maintenance: the state of the exit was not written");
        return 5;
    }
    0
}

/// Binds the socket at `path` and sets its mode to 0600. `Err` is the exit
/// code. A path that exists is never removed.
fn bind(path: &Path) -> Result<UnixListener, i32> {
    let listener = UnixListener::bind(path).map_err(|e| {
        if e.kind() == io::ErrorKind::AddrInUse {
            eprintln!("maintenance: the socket path exists");
            3
        } else {
            eprintln!("maintenance: the socket did not bind");
            4
        }
    })?;
    if std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).is_err() {
        eprintln!("maintenance: the socket mode did not change");
        remove_socket(path);
        return Err(4);
    }
    Ok(listener)
}

/// A start that failed after the child started: the child is killed (nobody
/// can attach) and the socket path of this runner is removed.
fn abort_start(child: &mut dyn portable_pty::Child, sock: &Path) -> i32 {
    if child.kill().is_err() {
        eprintln!("maintenance: the child was not stopped");
    }
    remove_socket(sock);
    4
}

/// Drains the PTY master into the ring and the client. Ends at the end of
/// the output (`EIO` after the last slave closes is an end of file).
fn pump(mut reader: Box<dyn io::Read + Send>, ctx: &Ctx, done: oneshot::Sender<()>) {
    let mut buf = vec![0_u8; READ_BYTES];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => ctx.push_output(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let _ = done.send(());
}
