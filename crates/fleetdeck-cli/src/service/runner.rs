//! The real program runner of `fleetdeck service` (design service.md §10).
//!
//! One call runs one program with arguments, optional stdin text and a time
//! limit. The tests never reach this file: they use the recording runner.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The result of a program that ran to its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// The exit status; `None` when a signal ended the program.
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// True for exit status 0.
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }

    /// The first line of stderr that has text.
    pub fn stderr_line(&self) -> Option<&str> {
        self.stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
    }

    /// The last line of stdout that has text.
    pub fn last_line(&self) -> Option<&str> {
        self.stdout
            .lines()
            .map(str::trim)
            .rev()
            .find(|line| !line.is_empty())
    }

    /// `exit <n>`, or `ended by a signal`.
    pub fn status_text(&self) -> String {
        self.status
            .map_or_else(|| "ended by a signal".to_owned(), |n| format!("exit {n}"))
    }
}

/// A program that did not run to its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The text is `cannot start <program>: <error>`.
    Start(String),
    /// The program did not end within its time limit; it was killed. The
    /// caller knows the program and the limit: [`Self::text`].
    TimedOut,
}

impl RunError {
    /// The text of the error for `program` with the time limit `limit`:
    /// `cannot start <program>: <error>` or
    /// `<program> did not answer in <n> s`.
    pub fn text(&self, program: &str, limit: Duration) -> String {
        match self {
            Self::Start(text) => text.clone(),
            Self::TimedOut => format!("{program} did not answer in {} s", limit.as_secs()),
        }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Start(text) => f.write_str(text),
            Self::TimedOut => f.write_str("the program did not answer in time"),
        }
    }
}

fn read_all(mut source: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        // A read error ends the text; the status of the program still counts.
        let _ = source.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// Runs `program` with `args`. `stdin` is written, then closed. The program
/// is killed when it runs longer than `limit`.
pub fn run_system(
    program: &str,
    args: &[String],
    stdin: Option<&str>,
    limit: Duration,
) -> Result<Output, RunError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RunError::Start(format!("cannot start {program}: {e}")))?;
    let out = child.stdout.take().map(read_all);
    let err = child.stderr.take().map(read_all);
    if let Some(mut pipe) = child.stdin.take()
        && let Some(text) = stdin
    {
        // A program that ends before it reads everything closes the pipe;
        // its exit status tells the result.
        let _ = pipe.write_all(text.as_bytes());
    }
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Start(format!("cannot wait for {program}: {e}")));
            }
        }
    };
    let join = |handle: Option<std::thread::JoinHandle<String>>| {
        handle.and_then(|h| h.join().ok()).unwrap_or_default()
    };
    let (stdout, stderr) = (join(out), join(err));
    match status {
        Some(status) => Ok(Output {
            status: status.code(),
            stdout,
            stderr,
        }),
        None => Err(RunError::TimedOut),
    }
}
