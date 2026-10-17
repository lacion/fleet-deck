//! `tailscale cert` with a secret output (certificate design section 2.3).
//!
//! The child writes a private key to stdout. So this path never logs, stores
//! or returns a byte of stdout or stderr. A failed run gives only a
//! [`CertError`], a fixed reason code.

use std::process::Stdio;
use std::time::Instant;

use tokio::process::Command;

use super::{TailscaleCli, TailscaleError, ambient_env, io_error, read_capped, reap};

/// The most stdout bytes of `tailscale cert`.
pub const MAX_CERT_STDOUT: usize = 64 * 1024;
/// The most stderr bytes of `tailscale cert`.
pub const MAX_CERT_STDERR: usize = 16 * 1024;

/// Why `tailscale cert` gave no certificate. Each value has a fixed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CertError {
    #[error(
        "HTTPS certificates are not enabled for this tailnet: admin console, DNS, HTTPS Certificates"
    )]
    HttpsDisabled,
    #[error("Tailscale is not logged in")]
    NotLoggedIn,
    #[error("the certificate request took too long")]
    Timeout,
    #[error("Tailscale is not installed")]
    NotInstalled,
    #[error("the certificate answer is too large")]
    TooLarge,
    #[error("Tailscale could not get a certificate")]
    Failed,
}

impl CertError {
    /// The reason code on the wire.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::HttpsDisabled => "https_disabled",
            Self::NotLoggedIn => "not_logged_in",
            Self::Timeout => "timeout",
            Self::NotInstalled => "not_installed",
            Self::TooLarge => "too_large",
            Self::Failed => "failed",
        }
    }
}

/// The reason of a failed exit, from fixed substrings of its stderr. The
/// bytes are not kept.
fn map_failure(stderr: &[u8]) -> CertError {
    let text = String::from_utf8_lossy(stderr).to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
    if has(&["https", "tls cert"])
        && has(&["not enabled", "enable", "not support", "does not support"])
    {
        CertError::HttpsDisabled
    } else if has(&["logged out", "not logged in", "needslogin", "needs login"]) {
        CertError::NotLoggedIn
    } else {
        CertError::Failed
    }
}

fn of_read(e: &TailscaleError) -> CertError {
    match e {
        TailscaleError::NotInstalled => CertError::NotInstalled,
        TailscaleError::TooLarge => CertError::TooLarge,
        TailscaleError::Timeout => CertError::Timeout,
        _ => CertError::Failed,
    }
}

impl TailscaleCli {
    /// `tailscale cert --cert-file - --key-file - --min-validity 720h
    /// <domain>`. Returns stdout: the PEM chain and the private key. The
    /// run ends at `deadline`.
    ///
    /// # Errors
    /// [`CertError`]. No error holds any output of the program.
    pub async fn cert(&self, domain: &str, deadline: Instant) -> Result<Vec<u8>, CertError> {
        let args = [
            "cert",
            "--cert-file",
            "-",
            "--key-file",
            "-",
            "--min-validity",
            "720h",
            domain,
        ];
        self.run_secret(&args, deadline).await
    }

    /// The same spawn, timeout and reap rules as `run`, but no byte of
    /// stdout or stderr is ever logged.
    async fn run_secret(&self, args: &[&str], deadline: Instant) -> Result<Vec<u8>, CertError> {
        let mut cmd = Command::new(&self.program);
        cmd.args(args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (name, value) in ambient_env().iter() {
            cmd.env(name, value);
        }
        let mut child = cmd.spawn().map_err(|e| of_read(&io_error(&e)))?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            reap(&mut child).await;
            return Err(CertError::Failed);
        };
        let io = async {
            let (out, err) = tokio::try_join!(
                read_capped(stdout, MAX_CERT_STDOUT),
                read_capped(stderr, MAX_CERT_STDERR)
            )
            .map_err(|e| of_read(&e))?;
            let status = child.wait().await.map_err(|e| of_read(&io_error(&e)))?;
            Ok::<_, CertError>((out, err, status))
        };
        let left = deadline.saturating_duration_since(Instant::now());
        let (out, err, status) = match tokio::time::timeout(left, io).await {
            Ok(Ok(all)) => all,
            Ok(Err(e)) => {
                reap(&mut child).await;
                return Err(e);
            }
            Err(_) => {
                reap(&mut child).await;
                return Err(CertError::Timeout);
            }
        };
        if status.success() {
            return Ok(out);
        }
        Err(map_failure(&err))
    }
}

#[cfg(test)]
#[path = "tailscale_cert_tests.rs"]
mod tests;
