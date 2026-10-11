//! Live process driver: runs a real `claude` process around [`ClaudeCore`].
//!
//! The driver logs every frame in both directions as fixture records. It is
//! used only by the recorder and by live smoke tests; normal tests replay the
//! recorded fixtures (PLAN §13).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use spike_canon::CanonEvent;
use spike_canon::fixture::Record;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::{Instant, timeout};

use crate::adapter::{ClaudeCore, HostCmd, Output, SessionSpec, cli_args};
use crate::frames::parse_value;

/// A running `claude` process with its adapter core.
pub struct LiveSession {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
    /// The adapter core. A restart keeps nothing: each process gets a new core.
    pub core: ClaudeCore,
    /// Every canonical event, in order.
    pub events: Vec<CanonEvent>,
    started: Instant,
    exited: bool,
}

impl LiveSession {
    /// Starts `bin` with the arguments of `spec` in `cwd`. Logs a `spawn` record.
    ///
    /// # Errors
    /// The process cannot start.
    pub fn spawn(bin: &str, spec: &SessionSpec, cwd: &Path, log: &mut Vec<Record>) -> Result<Self> {
        let args = cli_args(spec);
        log.push(Record::Spawn { args: args.clone() });
        let mut child = Command::new(bin)
            .args(&args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {bin}"))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        Ok(Self {
            child,
            stdin: Some(stdin),
            lines: BufReader::new(stdout).lines(),
            core: ClaudeCore::new(),
            events: Vec::new(),
            started: Instant::now(),
            exited: false,
        })
    }

    /// Milliseconds since the process started.
    #[must_use]
    pub fn elapsed_ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }

    async fn handle(
        &mut self,
        outs: Vec<Output>,
        log: &mut Vec<Record>,
    ) -> Result<Vec<CanonEvent>> {
        let mut emitted = Vec::new();
        for o in outs {
            match o {
                Output::Emit(e) => emitted.push(e),
                Output::Write(frame) => self.write(frame, log).await?,
            }
        }
        self.events.extend(emitted.iter().cloned());
        Ok(emitted)
    }

    async fn write(&mut self, frame: Value, log: &mut Vec<Record>) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("stdin is closed"))?;
        let mut line = serde_json::to_vec(&frame)?;
        line.push(b'\n');
        stdin.write_all(&line).await?;
        stdin.flush().await?;
        log.push(Record::In { frame });
        Ok(())
    }

    /// Applies a host command, logs it, and writes the frames it makes.
    ///
    /// # Errors
    /// The adapter rejects the command or the write fails.
    pub async fn cmd(&mut self, cmd: HostCmd, log: &mut Vec<Record>) -> Result<Vec<CanonEvent>> {
        log.push(Record::Host {
            cmd: serde_json::to_value(&cmd)?,
        });
        let outs = self.core.apply(&cmd).map_err(|e| anyhow!("{cmd:?}: {e}"))?;
        self.handle(outs, log).await
    }

    /// Reads one output line, logs it, feeds it to the core, and writes the
    /// frames the core makes. Returns `None` at the end of the output.
    ///
    /// # Errors
    /// A read or write error, or `wait` passes with no line.
    pub async fn step(
        &mut self,
        wait: Duration,
        log: &mut Vec<Record>,
    ) -> Result<Option<Vec<CanonEvent>>> {
        if self.exited {
            return Ok(None);
        }
        let line = timeout(wait, self.lines.next_line())
            .await
            .map_err(|_| anyhow!("no output for {wait:?}"))??;
        let Some(line) = line else {
            let code = self.wait_exit(log).await?;
            let outs = self.core.on_exit(code);
            // The exit events go to `self.events`; `None` tells the caller that the output ended.
            self.handle(outs, log).await?;
            return Ok(None);
        };
        match serde_json::from_str::<Value>(&line) {
            Ok(frame) => {
                log.push(Record::Out {
                    frame: frame.clone(),
                });
                let outs = self
                    .core
                    .on_frame(parse_value(frame).map_err(|e| anyhow!("{e}"))?);
                Ok(Some(self.handle(outs, log).await?))
            }
            Err(_) => {
                log.push(Record::Raw { line });
                Ok(Some(Vec::new()))
            }
        }
    }

    /// Steps until an event matches `pred`. Returns the matching event.
    ///
    /// # Errors
    /// `limit` passes, or the process ends before a match.
    pub async fn until(
        &mut self,
        limit: Duration,
        log: &mut Vec<Record>,
        mut pred: impl FnMut(&CanonEvent) -> bool,
    ) -> Result<CanonEvent> {
        let deadline = Instant::now() + limit;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("timeout after {limit:?}");
            }
            match self.step(left, log).await? {
                None => bail!("the process ended before the expected event"),
                Some(events) => {
                    if let Some(e) = events.into_iter().find(|e| pred(e)) {
                        return Ok(e);
                    }
                }
            }
        }
    }

    /// Steps until the output is quiet for `quiet` (or `limit` passes).
    ///
    /// # Errors
    /// A read or write error.
    pub async fn drain(&mut self, quiet: Duration, log: &mut Vec<Record>) -> Result<()> {
        loop {
            match self.step(quiet, log).await {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return Ok(()),
            }
        }
    }

    /// True after the process ended.
    #[must_use]
    pub fn exited(&self) -> bool {
        self.exited
    }

    async fn wait_exit(&mut self, log: &mut Vec<Record>) -> Result<Option<i32>> {
        if self.exited {
            return Ok(None);
        }
        let status = match timeout(Duration::from_secs(10), self.child.wait()).await {
            Ok(s) => s?,
            Err(_) => {
                self.child.kill().await?;
                self.child.wait().await?
            }
        };
        self.exited = true;
        log.push(Record::Exit {
            code: status.code(),
        });
        Ok(status.code())
    }

    /// Closes stdin (a clean end of input), reads the rest of the output, and waits for the exit.
    ///
    /// # Errors
    /// A read error.
    pub async fn close(mut self, log: &mut Vec<Record>) -> Result<Vec<CanonEvent>> {
        log.push(Record::Note {
            text: "host closed stdin".to_owned(),
        });
        self.stdin = None;
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.exited {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.kill(log).await?;
                break;
            }
            if self.step(left, log).await.is_err() {
                self.kill(log).await?;
                break;
            }
        }
        Ok(self.events)
    }

    /// Kills the process (a crash or a station restart) and maps the exit.
    ///
    /// # Errors
    /// The kill fails.
    pub async fn kill(&mut self, log: &mut Vec<Record>) -> Result<Vec<CanonEvent>> {
        if self.exited {
            return Ok(Vec::new());
        }
        log.push(Record::Note {
            text: "host killed the process".to_owned(),
        });
        self.stdin = None;
        self.child.kill().await?;
        let status = self.child.wait().await?;
        self.exited = true;
        log.push(Record::Exit {
            code: status.code(),
        });
        let outs = self.core.on_exit(status.code());
        self.handle(outs, log).await
    }
}
