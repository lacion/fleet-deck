//! GitHub and GitLab accounts through the `gh` and `glab` CLIs (DL-173,
//! design §2.2 and §2.4): the device login, the token login, the logout and
//! the login status of the CLIs.
//!
//! - A device login runs `gh auth login --web` or `glab auth login
//!   --device` with the scrubbed environment (no token variable, `BROWSER=true`,
//!   `GH_PROMPT_DISABLED=1`), no TTY, and its own process group. The
//!   station reads **both** output streams (each capped at 64 KiB and
//!   drained to the end), parses the one-time code and the page, and checks
//!   them before the board sees them.
//! - A token goes to `gh auth login --with-token` or `glab auth login
//!   --stdin` on stdin, never in argv.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::accounts::{
    AccountKind, AccountMethod, AccountStatus, AccountView, ForgeLogin, ForgeLoginState,
};
use fleetdeck_proto::integrations::code;
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::forge_parse::{HostStatus, Parsed, forge_user, parse_login, run_status, unsupported};
use super::{Accounts, codes};
use crate::integrations::exec::{self, Fail, Spec};

/// The variables that name a forge token. A login is refused while one is
/// set, and no login child gets one.
pub const TOKEN_VARS: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GITLAB_TOKEN",
    "GITLAB_ACCESS_TOKEN",
    "OAUTH_TOKEN",
];

/// The most bytes of each output stream that the station parses.
pub const OUTPUT_CAP: usize = 64 * 1024;
/// The most device logins that run at once on a station.
pub const MAX_LOGINS: usize = 3;
/// How long a device login lives.
pub const LOGIN_TTL: Duration = Duration::from_secs(15 * 60);
/// How long the start waits for the code and the page.
pub const START_WAIT: Duration = Duration::from_secs(20);
/// How many finished logins the station keeps for the status calls.
const KEEP_FINISHED: usize = 16;

/// One device login.
#[derive(Debug, Clone)]
pub struct Login {
    id: String,
    kind: AccountKind,
    host: String,
    code: String,
    url: String,
    state: ForgeLoginState,
    started: Instant,
    ttl: Duration,
}

impl Login {
    fn answer(&self) -> ForgeLogin {
        ForgeLogin {
            login_id: self.id.clone(),
            code: self.code.clone(),
            url: self.url.clone(),
            expires_in: u32::try_from(self.ttl.saturating_sub(self.started.elapsed()).as_secs())
                .unwrap_or(u32::MAX),
        }
    }

    fn pending(&self) -> bool {
        self.state == ForgeLoginState::Pending
    }
}

/// The device logins of a station.
pub type Logins = Arc<Mutex<Vec<Login>>>;

/// True for a bare host name of a forge: `[a-z0-9.-]`, 1 to 253 characters,
/// with at least one dot, that does not start or end with `.` or `-`.
#[must_use]
pub fn host_ok(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-'])
        && !host.contains("..")
}

/// The account id of the CLI login of `kind` on `host`.
#[must_use]
pub fn cli_account_id(kind: AccountKind, host: &str) -> String {
    let name = if kind == AccountKind::Github {
        "github"
    } else {
        "gitlab"
    };
    format!("cli-{name}-{host}")
}

/// The kind and the host of a CLI account id.
#[must_use]
pub fn parse_cli_account_id(id: &str) -> Option<(AccountKind, String)> {
    let (kind, host) = if let Some(h) = id.strip_prefix("cli-github-") {
        (AccountKind::Github, h)
    } else {
        (AccountKind::Gitlab, id.strip_prefix("cli-gitlab-")?)
    };
    host_ok(host).then(|| (kind, host.to_owned()))
}

pub(super) fn cli_name(kind: AccountKind) -> &'static str {
    if kind == AccountKind::Github {
        "gh"
    } else {
        "glab"
    }
}

fn is_forge(kind: AccountKind) -> bool {
    matches!(kind, AccountKind::Github | AccountKind::Gitlab)
}

/// The environment of a login or logout child: the station's, scrubbed,
/// without any token variable, with `BROWSER=true` and
/// `GH_PROMPT_DISABLED=1`.
fn login_env() -> Vec<(OsString, OsString)> {
    let mut env = ChildEnv::from_process();
    for name in TOKEN_VARS {
        env.remove(name);
    }
    // The names and values are fixed and valid, so `set` cannot fail.
    let _ = env.set("BROWSER", "true");
    let _ = env.set("GH_PROMPT_DISABLED", "1");
    env.iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

impl Accounts {
    fn program(&self, kind: AccountKind) -> PathBuf {
        if kind == AccountKind::Github {
            self.programs.gh.clone()
        } else {
            self.programs.glab.clone()
        }
    }

    /// The names of the token variables that are set in the station
    /// environment.
    #[must_use]
    pub fn env_tokens(&self) -> Vec<String> {
        TOKEN_VARS
            .iter()
            .filter(|n| (self.env_probe)(n))
            .map(|n| (*n).to_owned())
            .collect()
    }

    fn refuse_env_token(&self) -> Result<(), Fail> {
        if self.env_tokens().is_empty() {
            Ok(())
        } else {
            Err(Fail::new(
                codes::ENV_TOKEN_PRESENT,
                "a token in the station environment overrides the login",
            ))
        }
    }

    /// `accounts.forge_login`: starts the device login of `host`.
    ///
    /// # Errors
    /// `invalid_params`, `env_token_present`, `login_busy`,
    /// `device_flow_unsupported`, `unexpected_cli_output`, `unavailable`.
    pub async fn forge_login(&self, kind: AccountKind, host: &str) -> Result<ForgeLogin, Fail> {
        if !is_forge(kind) || !host_ok(host) {
            return Err(super::invalid("the kind or the host is not valid"));
        }
        self.refuse_env_token()?;
        // One start at a time: the checks and the new entry are one step.
        let _gate = self.login_gate.lock().await;
        {
            let mut logins = self.logins.lock().unwrap_or_else(PoisonError::into_inner);
            expire(&mut logins);
            if let Some(l) = logins
                .iter()
                .find(|l| l.pending() && l.kind == kind && l.host == host)
            {
                return Ok(l.answer());
            }
            if logins.iter().filter(|l| l.pending()).count() >= MAX_LOGINS {
                return Err(Fail::new(
                    codes::LOGIN_BUSY,
                    "three sign-ins are open on this station; finish one or wait",
                ));
            }
        }
        let args: Vec<&str> = if kind == AccountKind::Github {
            vec![
                "auth",
                "login",
                "--hostname",
                host,
                "--web",
                "--git-protocol",
                "https",
                "--skip-ssh-key",
            ]
        } else {
            vec!["auth", "login", "--hostname", host, "--device"]
        };
        let name = cli_name(kind);
        let mut child = Command::new(self.program(kind))
            .args(&args)
            .env_clear()
            .envs(login_env())
            .process_group(0)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| exec::fail_of(&exec::ExecError::Spawn(e), name))?;
        let pgid = child
            .id()
            .and_then(|p| i32::try_from(p).ok())
            .and_then(Pid::from_raw);
        let (tx, mut rx) = mpsc::unbounded_channel::<(usize, Vec<u8>)>();
        if let Some(out) = child.stdout.take() {
            tokio::spawn(pump(out, 0, tx.clone()));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(pump(err, 1, tx.clone()));
        }
        drop(tx);
        let found = self.read_start(kind, host, &mut child, &mut rx).await;
        let (code_text, url) = match found {
            Ok(found) => found,
            Err(fail) => {
                kill(pgid);
                let _ = child.wait().await;
                return Err(fail);
            }
        };
        let login = Login {
            id: super::random_id(12)?,
            kind,
            host: host.to_owned(),
            code: code_text,
            url,
            state: ForgeLoginState::Pending,
            started: Instant::now(),
            ttl: self.limits.login_ttl,
        };
        let answer = login.answer();
        self.logins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(login);
        tokio::spawn(finish(
            Arc::clone(&self.logins),
            answer.login_id.clone(),
            self.runner(),
            child,
            pgid,
            self.limits.login_ttl,
        ));
        Ok(answer)
    }

    /// Reads the output until the code and the page are known, the child
    /// ends, or the time is over.
    async fn read_start(
        &self,
        kind: AccountKind,
        host: &str,
        child: &mut tokio::process::Child,
        rx: &mut mpsc::UnboundedReceiver<(usize, Vec<u8>)>,
    ) -> Result<(String, String), Fail> {
        let unexpected = || {
            Fail::new(
                codes::UNEXPECTED_CLI_OUTPUT,
                "the CLI answered in a form that the station does not accept",
            )
        };
        let mut streams: [Vec<u8>; 2] = [Vec::new(), Vec::new()];
        let deadline = tokio::time::Instant::now() + self.limits.start_wait;
        let mut exited = false;
        loop {
            // A line that is still arriving is not parsed yet.
            let text = joined(&streams, exited);
            match parse_login(kind, host, &text) {
                Parsed::Found(code_text, url) => return Ok((code_text, url)),
                Parsed::Bad => {
                    return Err(if unsupported(&text) {
                        no_device_flow()
                    } else {
                        unexpected()
                    });
                }
                Parsed::Wait => {}
            }
            // A stream over the cap with no code: the code is not coming.
            if streams.iter().any(|s| s.len() >= OUTPUT_CAP) {
                return Err(unexpected());
            }
            if exited {
                return Err(if unsupported(&text) {
                    no_device_flow()
                } else {
                    unexpected()
                });
            }
            tokio::select! {
                chunk = rx.recv() => match chunk {
                    Some((i, bytes)) => {
                        let room = OUTPUT_CAP.saturating_sub(streams[i].len());
                        streams[i].extend_from_slice(&bytes[..bytes.len().min(room)]);
                    }
                    // Both streams are closed: the child is ending.
                    None => {
                        exited = true;
                        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                    }
                },
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(Fail::new(code::TIMEOUT, "the CLI gave no one-time code in time"));
                }
            }
        }
    }

    /// `accounts.forge_login_status`.
    #[must_use]
    pub fn forge_login_status(&self, login_id: &str) -> ForgeLoginState {
        let mut logins = self.logins.lock().unwrap_or_else(PoisonError::into_inner);
        expire(&mut logins);
        logins
            .iter()
            .find(|l| l.id == login_id)
            .map_or(ForgeLoginState::Expired, |l| l.state.clone())
    }

    /// `accounts.logout` for a CLI account.
    ///
    /// # Errors
    /// [`Fail`] when the CLI is missing or refuses.
    pub async fn forge_logout(&self, kind: AccountKind, host: &str) -> Result<(), Fail> {
        let name = cli_name(kind);
        let spec = Spec {
            program: self.program(kind),
            args: ["auth", "logout", "--hostname", host]
                .map(str::to_owned)
                .to_vec(),
            cwd: None,
            env: login_env(),
            stdin: None,
            max_stdout: 64 * 1024,
            timeout: exec::CALL_TIMEOUT,
        };
        let out = exec::run(spec).await.map_err(|e| exec::fail_of(&e, name))?;
        if out.code == 0 {
            Ok(())
        } else {
            Err(Fail::new(code::UPSTREAM, "the CLI could not sign out"))
        }
    }

    /// The token login of a forge: the token goes to the CLI on stdin.
    ///
    /// # Errors
    /// [`Fail`]; the message holds no text of the CLI.
    pub async fn forge_token_login(
        &self,
        kind: AccountKind,
        host: &str,
        token: &str,
    ) -> Result<AccountView, Fail> {
        if !is_forge(kind) || !host_ok(host) {
            return Err(super::invalid("the kind or the host is not valid"));
        }
        self.refuse_env_token()?;
        let name = cli_name(kind);
        let args: Vec<&str> = if kind == AccountKind::Github {
            vec!["auth", "login", "--hostname", host, "--with-token"]
        } else {
            vec!["auth", "login", "--hostname", host, "--stdin"]
        };
        let mut stdin = token.as_bytes().to_vec();
        stdin.push(b'\n');
        let spec = Spec {
            program: self.program(kind),
            args: args.into_iter().map(str::to_owned).collect(),
            cwd: None,
            env: login_env(),
            stdin: Some(stdin),
            max_stdout: 64 * 1024,
            timeout: exec::CALL_TIMEOUT,
        };
        let out = exec::run(spec).await.map_err(|e| exec::fail_of(&e, name))?;
        if out.code != 0 {
            return Err(Fail::new(code::UPSTREAM, "the CLI refused the token"));
        }
        let user = forge_user(&self.runner(), kind, host).await;
        Ok(AccountView {
            account_id: cli_account_id(kind, host),
            kind,
            site: Some(host.to_owned()),
            user,
            method: AccountMethod::Token,
            status: AccountStatus::Ok,
            message: "Connected.".to_owned(),
        })
    }

    /// The CLI accounts for `accounts.list`: github.com, gitlab.com and
    /// the hosts that the CLIs already report.
    pub async fn cli_accounts(&self) -> Vec<AccountView> {
        let runner = self.runner();
        let (gh, glab) = tokio::join!(
            run_status(&runner, AccountKind::Github, None),
            run_status(&runner, AccountKind::Gitlab, None)
        );
        let mut out = Vec::new();
        for (kind, report, default) in [
            (AccountKind::Github, gh, "github.com"),
            (AccountKind::Gitlab, glab, "gitlab.com"),
        ] {
            let mut rows = match report {
                Ok(rows) => rows,
                Err(message) => {
                    out.push(AccountView {
                        account_id: cli_account_id(kind, default),
                        kind,
                        site: Some(default.to_owned()),
                        user: None,
                        method: AccountMethod::Cli,
                        status: AccountStatus::Error,
                        message,
                    });
                    continue;
                }
            };
            if !rows.iter().any(|r| r.host == default) {
                rows.insert(
                    0,
                    HostStatus {
                        host: default.to_owned(),
                        user: None,
                        ok: false,
                    },
                );
            }
            for row in rows.into_iter().filter(|r| host_ok(&r.host)) {
                out.push(AccountView {
                    account_id: cli_account_id(kind, &row.host),
                    kind,
                    site: Some(row.host),
                    user: row.user,
                    method: AccountMethod::Cli,
                    status: if row.ok {
                        AccountStatus::Ok
                    } else {
                        AccountStatus::NeedsLogin
                    },
                    message: if row.ok {
                        "Connected.".to_owned()
                    } else {
                        "Not signed in.".to_owned()
                    },
                });
            }
        }
        out
    }
}

/// The part of the service that the background tasks need.
#[derive(Debug, Clone)]
pub struct Runner {
    pub programs: crate::integrations::exec::Programs,
}

/// Marks the logins that outlived their time as expired and drops the
/// oldest finished ones.
fn expire(logins: &mut Vec<Login>) {
    for l in logins.iter_mut() {
        if l.pending() && l.started.elapsed() >= l.ttl {
            l.state = ForgeLoginState::Expired;
        }
    }
    let finished = logins.iter().filter(|l| !l.pending()).count();
    let mut drop_n = finished.saturating_sub(KEEP_FINISHED);
    logins.retain(|l| {
        if drop_n > 0 && !l.pending() {
            drop_n -= 1;
            false
        } else {
            true
        }
    });
}

fn kill(pgid: Option<Pid>) {
    if let Some(pgid) = pgid {
        // The group may be gone already.
        let _ = kill_process_group(pgid, Signal::KILL);
    }
}

/// Waits for the end of the login process, then records the state.
async fn finish(
    logins: Logins,
    id: String,
    runner: Runner,
    mut child: tokio::process::Child,
    pgid: Option<Pid>,
    ttl: Duration,
) {
    let waited = tokio::time::timeout(ttl, child.wait()).await;
    let (kind, host) = {
        let l = logins.lock().unwrap_or_else(PoisonError::into_inner);
        match l.iter().find(|l| l.id == id) {
            Some(l) => (l.kind, l.host.clone()),
            None => return,
        }
    };
    let state = match waited {
        Ok(Ok(status)) if status.success() => match forge_user(&runner, kind, &host).await {
            Some(user) => ForgeLoginState::Done { user },
            None => ForgeLoginState::Failed {
                code: "status_unavailable".to_owned(),
            },
        },
        Ok(_) => ForgeLoginState::Failed {
            code: "login_failed".to_owned(),
        },
        Err(_) => {
            kill(pgid);
            let _ = child.wait().await;
            ForgeLoginState::Expired
        }
    };
    let mut l = logins.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(entry) = l.iter_mut().find(|l| l.id == id) {
        entry.state = state;
    }
}

/// Reads one stream: the first [`OUTPUT_CAP`] bytes go to the parser, the
/// rest is read and dropped, so the child never blocks on a full pipe.
async fn pump<R: AsyncRead + Unpin>(
    mut pipe: R,
    index: usize,
    tx: mpsc::UnboundedSender<(usize, Vec<u8>)>,
) {
    let mut buf = [0u8; 4096];
    let mut sent = 0usize;
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let take = n.min(OUTPUT_CAP.saturating_sub(sent));
                if take > 0 {
                    sent += take;
                    // The receiver may be gone; the drain goes on.
                    let _ = tx.send((index, buf[..take].to_vec()));
                }
            }
        }
    }
}

/// The text of both streams. Unless `complete`, a last line without its
/// newline is left out: it may still be arriving.
fn joined(streams: &[Vec<u8>; 2], complete: bool) -> String {
    let mut text = String::new();
    for stream in streams {
        let end = if complete {
            stream.len()
        } else {
            stream
                .iter()
                .rposition(|b| *b == b'\n')
                .map_or(0, |i| i + 1)
        };
        text.push_str(&String::from_utf8_lossy(&stream[..end]));
        text.push('\n');
    }
    text
}

fn no_device_flow() -> Fail {
    Fail::new(
        codes::DEVICE_FLOW_UNSUPPORTED,
        "this CLI or server has no device login; use a token",
    )
}
