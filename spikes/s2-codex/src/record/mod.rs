//! Recorder for live Codex fixtures (`record-codex`).
//!
//! Each scenario runs the real `codex app-server` in a new temporary directory
//! with a cheap model and low reasoning effort. The thread `cwd` is that
//! directory, and the approval policy is `untrusted` unless a scenario sets a
//! mode, so every command that writes asks first. The recorder saves the
//! scrubbed records as a fixture file.

mod extra;
pub mod rollout;
mod scenarios;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use spike_canon::fixture::{Fixture, Header, Record, Scrubber};
use spike_canon::{CanonEvent, TurnStatus};
use tokio::time::Instant;

use crate::adapter::{HostCmd, SessionSpec};
use crate::driver::LiveSession;

pub use scenarios::{SCENARIOS, Scenario};

/// Configuration of one recording run.
#[derive(Debug, Clone)]
pub struct Config {
    pub bin: String,
    pub model: String,
    pub effort: String,
    pub cli_version: String,
    pub out_dir: PathBuf,
}

/// The state of one scenario recording.
pub struct Ctx {
    pub cfg: Config,
    pub workdir: PathBuf,
    pub log: Vec<Record>,
    seq: u32,
}

/// Wait limit for one turn of a live scenario.
pub const TURN_LIMIT: Duration = Duration::from_secs(120);

impl Ctx {
    /// The session settings of this recording.
    #[must_use]
    pub fn spec(&self, mode: &str, resume: Option<String>) -> SessionSpec {
        SessionSpec {
            model: Some(self.cfg.model.clone()),
            effort: Some(self.cfg.effort.clone()),
            mode: Some(mode.to_owned()),
            cwd: Some(self.workdir.to_string_lossy().into_owned()),
            resume,
        }
    }

    /// Starts `codex app-server`, runs the `initialize` handshake, and opens
    /// (or resumes) the thread.
    pub async fn start(&mut self, mode: &str, resume: Option<String>) -> Result<LiveSession> {
        let mut s = LiveSession::spawn(&self.cfg.bin, &self.workdir, &mut self.log)?;
        s.cmd(HostCmd::Initialize, &mut self.log).await?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while !s.core.is_initialized() {
            let left = deadline.saturating_duration_since(Instant::now());
            ensure!(!left.is_zero(), "no initialize reply");
            if s.step(left, &mut self.log).await?.is_none() {
                bail!("app-server ended during initialize");
            }
        }
        let spec = self.spec(mode, resume);
        s.cmd(HostCmd::open_thread(&spec), &mut self.log).await?;
        s.until(Duration::from_secs(30), &mut self.log, |e| {
            matches!(e, CanonEvent::SessionStarted { .. })
        })
        .await?;
        Ok(s)
    }

    /// Adds a recorder observation.
    pub fn note(&mut self, text: impl Into<String>) {
        self.log.push(Record::Note { text: text.into() });
    }

    fn next_input_id(&mut self) -> String {
        self.seq += 1;
        format!("fd-in-{}", self.seq)
    }

    /// Sends a prompt (`turn/start`, or the host queue while a turn runs).
    /// Returns its input id (`clientUserMessageId`).
    pub async fn prompt(&mut self, s: &mut LiveSession, text: &str) -> Result<String> {
        let input_id = self.next_input_id();
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

    /// Steers the running turn (`turn/steer`). Returns the input id.
    pub async fn steer(&mut self, s: &mut LiveSession, text: &str) -> Result<String> {
        let input_id = self.next_input_id();
        s.cmd(
            HostCmd::Steer {
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
            other => bail!("unexpected event {other:?}"),
        }
    }

    /// Waits for the first event that matches `pred`.
    pub async fn wait_for(
        &mut self,
        s: &mut LiveSession,
        pred: impl FnMut(&CanonEvent) -> bool,
    ) -> Result<CanonEvent> {
        s.until(TURN_LIMIT, &mut self.log, pred).await
    }

    /// Waits for the next approval or question. Returns its app request id and
    /// its native parameters.
    pub async fn approval(&mut self, s: &mut LiveSession) -> Result<(String, Value)> {
        let e = s
            .until(TURN_LIMIT, &mut self.log, |e| {
                matches!(
                    e,
                    CanonEvent::ApprovalRequested { .. } | CanonEvent::TurnCompleted { .. }
                )
            })
            .await?;
        let CanonEvent::ApprovalRequested { request_id, .. } = e else {
            bail!("the turn ended before an approval request: {e:?}")
        };
        let params = s
            .core
            .request_params(&request_id)
            .cloned()
            .unwrap_or_default();
        Ok((request_id, params))
    }

    /// Answers a request.
    pub async fn answer(
        &mut self,
        s: &mut LiveSession,
        request_id: &str,
        option_id: &str,
        updated_input: Option<Value>,
    ) -> Result<()> {
        s.cmd(
            HostCmd::Answer {
                request_id: request_id.to_owned(),
                option_id: option_id.to_owned(),
                updated_input,
                message: None,
            },
            &mut self.log,
        )
        .await?;
        Ok(())
    }

    /// Steps until the core is idle, answers every approval on the way with
    /// `option_id` (`None`: answer nothing), then reads until quiet.
    pub async fn idle_answering(
        &mut self,
        s: &mut LiveSession,
        option_id: Option<&str>,
    ) -> Result<()> {
        let deadline = Instant::now() + TURN_LIMIT;
        while !s.core.is_idle() && !s.exited() {
            let left = deadline.saturating_duration_since(Instant::now());
            ensure!(!left.is_zero(), "the core did not become idle");
            let Some(events) = s.step(left, &mut self.log).await? else {
                break;
            };
            for e in events {
                if let (Some(option), CanonEvent::ApprovalRequested { request_id, .. }) =
                    (option_id, e)
                {
                    self.note(format!("auto-answer {option} {request_id}"));
                    self.answer(s, &request_id, option, None).await?;
                }
            }
        }
        s.drain(Duration::from_millis(1500), &mut self.log).await
    }

    /// Steps until the core is idle, then reads until quiet.
    pub async fn idle(&mut self, s: &mut LiveSession) -> Result<()> {
        self.idle_answering(s, None).await
    }

    /// Closes stdin and waits for the exit.
    pub async fn close(&mut self, s: LiveSession) -> Result<()> {
        s.close(&mut self.log).await?;
        Ok(())
    }
}

/// Builds the scrubber for this machine and work directory.
#[must_use]
pub fn scrubber(workdir: &Path) -> Scrubber {
    let mut s = Scrubber::new()
        .redact_key("email")
        .redact_key("account")
        .redact_key("planType")
        .redact_key("userId")
        .redact_key("accountId")
        .redact_key("serverName")
        .redact_key("installationId");
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
        .prefix("fd-s2-")
        .tempdir()
        .context("temp dir")?;
    let workdir = tmp.path().to_path_buf();
    let mut ctx = Ctx {
        cfg: cfg.clone(),
        workdir: workdir.clone(),
        log: Vec::new(),
        seq: 0,
    };
    let res = tokio::time::timeout(Duration::from_secs(300), (sc.run)(&mut ctx)).await;
    let outcome = match res {
        Ok(r) => r,
        Err(_) => Err(anyhow::anyhow!("scenario timeout")),
    };
    let rollout_notes = rollout::notes(&ctx.log);
    ctx.log.extend(rollout_notes);
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
            provider: "codex".to_owned(),
            scenario: sc.name.to_owned(),
            cli_version: cfg.cli_version.clone(),
            recorder: "record-codex".to_owned(),
            model: Some(cfg.model.clone()),
            notes: scrub.scrub_str(&notes),
        },
        records: coalesce(&scrub, &ctx.log),
    };
    let path = cfg.out_dir.join(name);
    fixture.save(&path)?;
    outcome.map(|()| path)
}

/// Scrubs every record. A path split across `item/agentMessage/delta`
/// notifications is rare with a temp dir prefix; the hygiene test checks the
/// joined deltas of every fixture.
fn coalesce(scrub: &Scrubber, log: &[Record]) -> Vec<Record> {
    log.iter().map(|r| scrub.scrub_record(r)).collect()
}
