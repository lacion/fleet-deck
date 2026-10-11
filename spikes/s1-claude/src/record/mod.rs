//! Recorder for live Claude fixtures (`record-claude`).
//!
//! Each scenario runs the real `claude` CLI in a new temporary directory with a
//! cheap model and `--safe-mode` (no user hooks, plugins, CLAUDE.md, or MCP
//! servers, so the fixture does not depend on the machine). The recorder saves
//! the scrubbed records as a fixture file.

mod scenarios;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use spike_canon::fixture::{Fixture, Header, Record, Scrubber};
use spike_canon::{CanonEvent, TurnStatus};

use crate::adapter::{HostCmd, SessionSpec};
use crate::driver::LiveSession;

pub use scenarios::{SCENARIOS, Scenario};

/// Configuration of one recording run.
#[derive(Debug, Clone)]
pub struct Config {
    pub bin: String,
    pub model: String,
    pub cli_version: String,
    pub out_dir: PathBuf,
}

/// The state of one scenario recording.
pub struct Ctx {
    pub cfg: Config,
    pub workdir: PathBuf,
    pub log: Vec<Record>,
}

/// Wait limit for one turn of a live scenario.
pub const TURN_LIMIT: Duration = Duration::from_secs(120);

impl Ctx {
    fn spec(&self, mode: &str, resume: Option<String>) -> SessionSpec {
        SessionSpec {
            model: Some(self.cfg.model.clone()),
            mode: Some(mode.to_owned()),
            resume,
            fork: false,
            extra_args: vec!["--safe-mode".to_owned()],
        }
    }

    /// Starts a process and sends `initialize`.
    pub async fn start(&mut self, mode: &str, resume: Option<String>) -> Result<LiveSession> {
        let spec = self.spec(mode, resume);
        let mut s = LiveSession::spawn(&self.cfg.bin, &spec, &self.workdir, &mut self.log)?;
        s.cmd(HostCmd::Initialize, &mut self.log).await?;
        Ok(s)
    }

    /// Adds a recorder observation.
    pub fn note(&mut self, text: impl Into<String>) {
        self.log.push(Record::Note { text: text.into() });
    }

    /// Writes a prompt with a new client uuid. Returns the uuid.
    pub async fn prompt(&mut self, s: &mut LiveSession, text: &str) -> Result<String> {
        let input_id = new_input_id();
        s.cmd(
            HostCmd::Prompt {
                text: text.to_owned(),
                input_id: input_id.clone(),
            },
            &mut self.log,
        )
        .await?;
        Ok(input_id)
    }

    /// Waits for the next completed turn and returns its status.
    pub async fn turn_done(&mut self, s: &mut LiveSession) -> Result<TurnStatus> {
        let e = s
            .until(TURN_LIMIT, &mut self.log, |e| {
                matches!(e, CanonEvent::TurnCompleted { .. })
            })
            .await?;
        match e {
            CanonEvent::TurnCompleted { status, .. } => Ok(status),
            _ => unreachable!("the predicate matches only TurnCompleted"),
        }
    }

    /// Waits for the next approval request and returns its app request id and input.
    pub async fn approval(&mut self, s: &mut LiveSession) -> Result<(String, serde_json::Value)> {
        let e = s
            .until(TURN_LIMIT, &mut self.log, |e| {
                matches!(
                    e,
                    CanonEvent::ApprovalRequested { .. } | CanonEvent::TurnCompleted { .. }
                )
            })
            .await?;
        let CanonEvent::ApprovalRequested { request_id, .. } = e else {
            anyhow::bail!("the turn ended before an approval request: {e:?}")
        };
        let input = s
            .core
            .request_input(&request_id)
            .cloned()
            .unwrap_or_default();
        Ok((request_id, input))
    }

    /// Steps until the core is idle (no turn, no pending input), then reads until quiet.
    pub async fn idle(&mut self, s: &mut LiveSession) -> Result<()> {
        let deadline = tokio::time::Instant::now() + TURN_LIMIT;
        while !s.core.is_idle() && !s.exited() {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            anyhow::ensure!(!left.is_zero(), "the core did not become idle");
            if s.step(left, &mut self.log).await?.is_none() {
                break;
            }
        }
        s.drain(Duration::from_millis(1500), &mut self.log).await
    }

    /// Closes the session cleanly.
    pub async fn close(&mut self, s: LiveSession) -> Result<()> {
        s.close(&mut self.log).await?;
        Ok(())
    }
}

/// A new UUID (version 4 layout) from the clock and a process counter.
///
/// Unique across processes and resumes of one session, which the CLI needs for
/// `command_lifecycle`. The recorder logs it in the `host` record, so a replay
/// uses the same value.
#[must_use]
pub fn new_input_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let pid = u128::from(std::process::id());
    let v = nanos ^ (pid << 64) ^ (u128::from(seq) << 96);
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (v >> 96) as u32,
        (v >> 80) as u16,
        ((v >> 64) as u16) & 0x0fff,
        ((seq as u16) & 0x0fff),
        (v as u64) & 0xffff_ffff_ffff
    )
}

/// Builds the scrubber for this machine and work directory.
#[must_use]
pub fn scrubber(workdir: &Path) -> Scrubber {
    let mut s = Scrubber::new()
        .redact_key("account")
        .redact_key("email")
        .redact_key("emailAddress")
        .redact_key("organization")
        .redact_key("messaging_socket_path");
    let wd = workdir.to_string_lossy().into_owned();
    s = s.replace(&wd, "<workdir>");
    if let Ok(canon) = workdir.canonicalize() {
        s = s.replace(&canon.to_string_lossy(), "<workdir>");
    }
    if let Some(home) = std::env::var_os("HOME") {
        s = s.replace(&home.to_string_lossy(), "<home>");
    }
    if let Ok(user) = std::env::var("USER")
        && user.len() >= 3
    {
        s = s.replace(&user, "<user>");
    }
    s
}

/// Records one scenario and saves the fixture. Returns the fixture path.
///
/// # Errors
/// The scenario fails or the fixture cannot be saved. A failed scenario still
/// saves its partial records as `<name>.failed.jsonl` for diagnosis.
pub async fn record(cfg: &Config, sc: &Scenario) -> Result<PathBuf> {
    let tmp = tempfile::Builder::new()
        .prefix("fd-s1-")
        .tempdir()
        .context("temp dir")?;
    let workdir = tmp.path().to_path_buf();
    let mut ctx = Ctx {
        cfg: cfg.clone(),
        workdir: workdir.clone(),
        log: Vec::new(),
    };
    let res = tokio::time::timeout(Duration::from_secs(300), (sc.run)(&mut ctx)).await;
    let outcome = match res {
        Ok(r) => r,
        Err(_) => Err(anyhow::anyhow!("scenario timeout")),
    };
    let scrub = scrubber(&workdir);
    let (name, notes) = match &outcome {
        Ok(()) => (format!("{}.jsonl", sc.name), sc.notes.to_owned()),
        Err(e) => (
            format!("{}.failed.jsonl", sc.name),
            format!("FAILED: {e:#}"),
        ),
    };
    let fixture = Fixture {
        header: Header {
            provider: "claude".to_owned(),
            scenario: sc.name.to_owned(),
            cli_version: cfg.cli_version.clone(),
            recorder: "record-claude".to_owned(),
            model: Some(cfg.model.clone()),
            notes: scrub.scrub_str(&notes),
        },
        records: ctx.log.iter().map(|r| scrub.scrub_record(r)).collect(),
    };
    let path = cfg.out_dir.join(name);
    fixture.save(&path)?;
    outcome.map(|()| path)
}
