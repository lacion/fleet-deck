//! Child processes of the integration service: `gh`, `glab` and `curl`.
//!
//! - Every call has a time limit ([`CALL_TIMEOUT`]) and a limit on the bytes
//!   of stdout that it keeps.
//! - A tracker token never reaches a child through argv or the environment:
//!   `curl` gets its URL, headers and body on stdin as a config file
//!   (`curl -K -`), and the variables that name a tracker token are removed
//!   from the environment of every child.
//! - `gh` and `glab` run with the station owner's environment (their own
//!   login, `GH_HOST`, proxies), without the provider variables that
//!   [`ChildEnv`] removes.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::integrations::code;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// The time limit of one call (R3: 20 s).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(20);
/// The most bytes of stdout that a call keeps.
pub const MAX_STDOUT: usize = 8 * 1024 * 1024;
/// The most bytes of stderr that a call keeps.
const MAX_STDERR: usize = 64 * 1024;
/// The longest error text in an answer, in characters.
pub const MESSAGE_CHARS: usize = 500;

/// A failed integration call: a wire error code and a scrubbed message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fail {
    pub code: &'static str,
    pub message: String,
}

impl Fail {
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn upstream(message: impl AsRef<str>) -> Self {
        Self::new(code::UPSTREAM, scrub(message.as_ref(), &[]))
    }

    #[must_use]
    pub fn bad_answer(what: impl AsRef<str>) -> Self {
        Self::new(code::BAD_ANSWER, what.as_ref().to_owned())
    }

    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(code::UNAVAILABLE, message)
    }
}

/// How the service starts its helpers. The programs are replaceable in
/// tests.
#[derive(Debug, Clone)]
pub struct Programs {
    pub gh: PathBuf,
    pub glab: PathBuf,
    pub curl: PathBuf,
}

impl Default for Programs {
    fn default() -> Self {
        Self {
            gh: "gh".into(),
            glab: "glab".into(),
            curl: "curl".into(),
        }
    }
}

/// One call of a helper program.
#[derive(Debug)]
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
    pub stdin: Option<Vec<u8>>,
    pub max_stdout: usize,
    /// The time limit of [`run`] (`CALL_TIMEOUT` for every caller except the
    /// forge calls of a land, FG-8).
    pub timeout: Duration,
}

/// What a helper returned.
#[derive(Debug)]
pub struct Output {
    /// The exit code; -1 for a signal.
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

/// Why a helper gave no output.
#[derive(Debug)]
pub enum ExecError {
    /// The program does not start (not installed, not executable).
    Spawn(std::io::Error),
    Timeout,
    /// The output is larger than the limit.
    TooLarge,
}

/// The environment of `gh` and `glab`: the station's, scrubbed, without the
/// variables that hold a tracker token.
#[must_use]
pub fn cli_env(hidden: &[String]) -> Vec<(OsString, OsString)> {
    let mut env = ChildEnv::from_process();
    for name in hidden {
        env.remove(name);
    }
    env.iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// The environment of `curl`: what it needs to reach a host, nothing else.
#[must_use]
pub fn curl_env() -> Vec<(OsString, OsString)> {
    const KEEP: &[&str] = &[
        "PATH",
        "HOME",
        "LANG",
        "LC_ALL",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "CURL_CA_BUNDLE",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
    ];
    std::env::vars_os()
        .filter(|(k, _)| k.to_str().is_some_and(|k| KEEP.contains(&k)))
        .collect()
}

async fn read_capped<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>, max: usize) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(pipe) = pipe {
        // One byte more than the limit tells the caller about a cut.
        let mut limited = pipe.take(u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1));
        // A read error ends the output; the exit code tells the rest.
        let _ = limited.read_to_end(&mut out).await;
    }
    out
}

/// Runs `spec` with its own `timeout`.
///
/// # Errors
/// [`ExecError`].
pub async fn run(spec: Spec) -> Result<Output, ExecError> {
    let limit = spec.timeout;
    run_for(spec, limit).await
}

/// Runs `spec` with `limit` as its time limit.
///
/// # Errors
/// [`ExecError`].
pub async fn run_for(spec: Spec, limit: Duration) -> Result<Output, ExecError> {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    let mut child = cmd.spawn().map_err(ExecError::Spawn)?;
    if let (Some(bytes), Some(mut pipe)) = (spec.stdin, child.stdin.take()) {
        tokio::spawn(async move {
            // The child may end before it reads everything; its exit code
            // reports that.
            let _ = pipe.write_all(&bytes).await;
            let _ = pipe.shutdown().await;
        });
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let max = spec.max_stdout;
    let work = async {
        let (out, err) = tokio::join!(read_capped(stdout, max), read_capped(stderr, MAX_STDERR));
        if out.len() > max {
            return Err(ExecError::TooLarge);
        }
        let status = child.wait().await.map_err(ExecError::Spawn)?;
        Ok(Output {
            code: status.code().unwrap_or(-1),
            stdout: out,
            stderr: String::from_utf8_lossy(&err).trim().to_owned(),
        })
    };
    // On a timeout or a cut the future ends and the child is killed on drop.
    tokio::time::timeout(limit, work)
        .await
        .map_err(|_| ExecError::Timeout)?
}

/// The wire failure of an [`ExecError`] of the program `name`.
#[must_use]
pub fn fail_of(e: &ExecError, name: &str) -> Fail {
    match e {
        ExecError::Spawn(io) if io.kind() == std::io::ErrorKind::NotFound => Fail::unavailable(
            format!("`{name}` is not installed on this station or not on its PATH"),
        ),
        ExecError::Spawn(io) => Fail::upstream(format!("cannot start `{name}`: {io}")),
        ExecError::Timeout => Fail::new(
            code::TIMEOUT,
            format!(
                "`{name}` did not answer within {} s",
                CALL_TIMEOUT.as_secs()
            ),
        ),
        ExecError::TooLarge => Fail::bad_answer(format!("`{name}` returned too much data")),
    }
}

/// The text of `text` without anything that looks like a credential, cut to
/// [`MESSAGE_CHARS`] characters. `secrets` are exact values that must not
/// appear (the token that a call used).
#[must_use]
pub fn scrub(text: &str, secrets: &[&str]) -> String {
    const PREFIXES: &[&str] = &[
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "gldt-",
        "lin_api_",
        "lin_oauth_",
        "ATATT",
    ];
    let mut text = text.to_owned();
    for secret in secrets.iter().filter(|s| s.len() >= 4) {
        text = text.replace(secret, "***");
    }
    let mut out = Vec::new();
    let mut mask_next = false;
    for word in text.split_whitespace() {
        let lower = word.to_ascii_lowercase();
        let masked = mask_next
            || PREFIXES.iter().any(|p| word.starts_with(p))
            || lower.starts_with("authorization:");
        mask_next = matches!(
            lower.as_str(),
            "bearer" | "basic" | "authorization" | "authorization:"
        );
        if masked {
            out.push("***".to_owned());
        } else if word.contains("://") {
            out.push(crate::workspace::provision::scrub_url(word));
        } else {
            out.push(word.to_owned());
        }
    }
    let joined = out.join(" ");
    if joined.chars().count() > MESSAGE_CHARS {
        let mut cut: String = joined.chars().take(MESSAGE_CHARS).collect();
        cut.push('…');
        cut
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_masked_in_an_error_text() {
        let got = scrub(
            "HTTP 401 Authorization: Basic YWJjOnNlY3JldA== token ghp_abcdef bearer xyz https://u:pw@h/x?k=v",
            &[],
        );
        assert!(!got.contains("YWJj"), "{got}");
        assert!(!got.contains("ghp_abcdef"), "{got}");
        assert!(!got.contains("xyz"), "{got}");
        assert!(!got.contains("pw@"), "{got}");
        assert!(got.contains("HTTP 401"));
    }

    #[test]
    fn a_known_secret_is_masked_and_the_text_is_cut() {
        let got = scrub("failed with the-secret-value inside", &["the-secret-value"]);
        assert_eq!(got, "failed with *** inside");
        let long = scrub(&"word ".repeat(400), &[]);
        assert_eq!(long.chars().count(), MESSAGE_CHARS + 1);
    }

    fn spec(program: &str, args: &[&str], stdin: Option<&str>) -> Spec {
        Spec {
            program: program.into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            cwd: None,
            env: curl_env(),
            stdin: stdin.map(|s| s.as_bytes().to_vec()),
            max_stdout: 1024,
            timeout: CALL_TIMEOUT,
        }
    }

    #[tokio::test]
    async fn a_helper_runs_with_stdin_and_a_cut_output_is_refused() {
        let out = run(spec("cat", &[], Some("hello"))).await.unwrap();
        assert_eq!((out.code, out.stdout.as_slice()), (0, b"hello".as_slice()));
        let big = "x".repeat(2000);
        let err = run(spec("cat", &[], Some(&big))).await.unwrap_err();
        assert!(matches!(err, ExecError::TooLarge));
        let err = run(spec("fleetdeck-no-such-program", &[], None))
            .await
            .unwrap_err();
        assert_eq!(fail_of(&err, "x").code, code::UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_slow_helper_times_out() {
        let err = run_for(spec("sleep", &["5"], None), Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(matches!(err, ExecError::Timeout));
    }

    #[tokio::test]
    async fn a_call_uses_the_time_limit_of_its_spec() {
        let mut slow = spec("sleep", &["5"], None);
        slow.timeout = Duration::from_millis(100);
        let err = run(slow).await.unwrap_err();
        assert!(matches!(err, ExecError::Timeout));
        // The default of the existing callers is the 20 s limit.
        assert_eq!(spec("cat", &[], None).timeout, CALL_TIMEOUT);
    }
}
