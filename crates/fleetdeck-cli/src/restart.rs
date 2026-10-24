//! The restart of the process (DL-172, design 1.6): `fleetdeck` executes
//! itself again with the startup context that `main` captured at its start.
//! No request data enters the context.
//!
//! The deck (standalone) and the station side ask for a restart through
//! [`fleetdeck_core::restart::Restart`]. A request starts the same graceful
//! shutdown as SIGTERM. After the drain, [`StartupContext::exec`] runs. When
//! it fails, the process logs "restart failed: <errno>" and exits with code
//! [`EXIT_RESTART_FAILED`] (docs/v2/operations.md).

use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::Arc;

use fleetdeck_core::restart::Restart;
use tokio::sync::watch;

/// The exit code of a process whose restart failed (`EX_TEMPFAIL`).
pub const EXIT_RESTART_FAILED: u8 = 75;

/// What the process was started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupContext {
    /// `/proc/self/exe` on Linux, `current_exe()` elsewhere.
    pub exe: PathBuf,
    pub argv: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

impl StartupContext {
    /// Captures the context. Call it once, at the start of `main`.
    #[must_use]
    pub fn capture() -> Self {
        let argv: Vec<OsString> = std::env::args_os().collect();
        let exe = if cfg!(target_os = "linux") {
            PathBuf::from("/proc/self/exe")
        } else {
            std::env::current_exe()
                .unwrap_or_else(|_| argv.first().map(PathBuf::from).unwrap_or_default())
        };
        Self {
            exe,
            argv,
            env: std::env::vars_os().collect(),
        }
    }

    /// Executes the program again with the captured argv and environment.
    /// It returns only when the exec failed.
    #[must_use]
    pub fn exec(&self) -> io::Error {
        let mut command = Command::new(&self.exe);
        if let Some((arg0, rest)) = self.argv.split_first() {
            command.arg0(arg0).args(rest);
        }
        command.env_clear().envs(self.env.iter().cloned());
        command.exec()
    }
}

/// Logs a failed exec and gives the exit code of the process.
#[must_use]
pub fn restart_failed(e: &io::Error) -> ExitCode {
    let errno = e
        .raw_os_error()
        .map_or_else(|| e.to_string(), |n| n.to_string());
    tracing::error!("restart failed: {errno}");
    eprintln!("fleetdeck: restart failed: {errno}");
    ExitCode::from(EXIT_RESTART_FAILED)
}

/// The restart request of this process. It is cheap to clone.
#[derive(Clone)]
pub struct RestartHandle(Arc<watch::Sender<bool>>);

impl Default for RestartHandle {
    fn default() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }
}

impl std::fmt::Debug for RestartHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestartHandle")
            .field("requested", &self.requested())
            .finish()
    }
}

impl Restart for RestartHandle {
    fn request(&self) {
        self.0.send_replace(true);
    }
}

impl RestartHandle {
    /// True once a restart was requested.
    #[must_use]
    pub fn requested(&self) -> bool {
        *self.0.borrow()
    }

    /// Completes when a restart is requested.
    pub async fn wait(&self) {
        let mut rx = self.0.subscribe();
        // The sender lives in `self`, so the wait ends only on a request.
        let _ = rx.wait_for(|requested| *requested).await;
    }
}

/// Completes at SIGINT or SIGTERM, or when a restart is requested.
pub fn stop_or_restart(handle: &RestartHandle) -> impl Future<Output = ()> + use<> {
    let handle = handle.clone();
    async move {
        tokio::select! {
            () = crate::station::shutdown_signal() => {}
            () = handle.wait() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_context_holds_the_argv_the_environment_and_the_executable_of_this_process() {
        let c = StartupContext::capture();
        assert_eq!(c.argv, std::env::args_os().collect::<Vec<_>>());
        assert!(!c.argv.is_empty());
        let path = std::env::var_os("PATH").unwrap();
        assert!(c.env.iter().any(|(k, v)| k == "PATH" && *v == path));
        if cfg!(target_os = "linux") {
            assert_eq!(c.exe, PathBuf::from("/proc/self/exe"));
        } else {
            assert_eq!(c.exe, std::env::current_exe().unwrap());
        }
    }

    #[test]
    fn a_failed_exec_returns_the_error_and_the_process_goes_on() {
        let c = StartupContext {
            exe: PathBuf::from("/nonexistent/fleetdeck-restart-test"),
            argv: vec![OsString::from("fleetdeck"), OsString::from("up")],
            env: Vec::new(),
        };
        let e = c.exec();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert_eq!(e.raw_os_error(), Some(2));
    }

    #[tokio::test]
    async fn a_request_completes_the_wait_and_a_second_request_changes_nothing() {
        let h = RestartHandle::default();
        assert!(!h.requested());
        let waiting = tokio::spawn({
            let h = h.clone();
            async move { h.wait().await }
        });
        h.request();
        h.request();
        tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap();
        assert!(h.requested());
        // A wait after the request completes at once.
        tokio::time::timeout(std::time::Duration::from_secs(5), h.wait())
            .await
            .unwrap();
    }

    #[test]
    fn the_exit_code_of_a_failed_restart_is_75() {
        let code = restart_failed(&io::Error::from_raw_os_error(2));
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(75)));
    }
}
