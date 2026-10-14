//! The recording `Deps` of the tests: no program runs, no socket opens.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::Deps;
use super::runner::{Output, RunError};

/// One recorded call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub program: String,
    pub args: Vec<String>,
    pub stdin: Option<String>,
}

impl Call {
    /// `program arg arg`.
    pub fn line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

struct Rule {
    program: String,
    prefix: Vec<String>,
    result: Result<Output, RunError>,
}

impl Rule {
    fn matches(&self, program: &str, args: &[String]) -> bool {
        self.program == program && args.starts_with(&self.prefix)
    }
}

/// A `Deps` with fixed answers. A call without an answer panics.
#[derive(Default)]
pub struct Fake {
    calls: RefCell<Vec<Call>>,
    /// Answers that one call uses up, first in first out.
    queue: RefCell<Vec<Rule>>,
    /// Answers that stay; a later one wins over an earlier one.
    fixed: RefCell<Vec<Rule>>,
    sockets: RefCell<HashMap<PathBuf, VecDeque<bool>>>,
    sleeps: Cell<u32>,
}

pub fn ok(stdout: &str) -> Result<Output, RunError> {
    Ok(Output {
        status: Some(0),
        stdout: stdout.to_owned(),
        stderr: String::new(),
    })
}

pub fn fail(status: i32, stderr: &str) -> Result<Output, RunError> {
    Ok(Output {
        status: Some(status),
        stdout: String::new(),
        stderr: stderr.to_owned(),
    })
}

fn rule(program: &str, prefix: &[&str], result: Result<Output, RunError>) -> Rule {
    Rule {
        program: program.to_owned(),
        prefix: prefix.iter().map(|s| (*s).to_owned()).collect(),
        result,
    }
}

impl Fake {
    /// The next call that matches `program` and starts with `prefix` gets
    /// `result`, once.
    pub fn once(&self, program: &str, prefix: &[&str], result: Result<Output, RunError>) {
        self.queue.borrow_mut().push(rule(program, prefix, result));
    }

    /// Every call that matches gets `result` (after the `once` answers).
    pub fn always(&self, program: &str, prefix: &[&str], result: Result<Output, RunError>) {
        self.fixed.borrow_mut().push(rule(program, prefix, result));
    }

    /// The answers of `socket`; the last one repeats.
    pub fn socket(&self, socket: &Path, answers: &[bool]) {
        self.sockets
            .borrow_mut()
            .insert(socket.to_path_buf(), answers.iter().copied().collect());
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }

    /// Every call as `program arg arg`.
    pub fn lines(&self) -> Vec<String> {
        self.calls.borrow().iter().map(Call::line).collect()
    }

    /// The calls that change something (not the read-only queries).
    pub fn changes(&self) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|l| !is_read_only(l))
            .collect()
    }

    pub fn sleeps(&self) -> u32 {
        self.sleeps.get()
    }
}

/// The read-only commands of the design (decision 15).
pub fn is_read_only(line: &str) -> bool {
    line.starts_with("systemctl --user show --property=UnitPath")
        || line.starts_with("systemctl --user is-active ")
        || line.starts_with("systemctl --user is-enabled ")
        || line.starts_with("loginctl show-user ")
        || line.starts_with("launchctl print ")
}

impl Deps for Fake {
    fn run(
        &self,
        program: &str,
        args: &[String],
        stdin: Option<&str>,
        _limit: Duration,
    ) -> Result<Output, RunError> {
        self.calls.borrow_mut().push(Call {
            program: program.to_owned(),
            args: args.to_vec(),
            stdin: stdin.map(str::to_owned),
        });
        let mut queue = self.queue.borrow_mut();
        if let Some(at) = queue.iter().position(|r| r.matches(program, args)) {
            return queue.remove(at).result;
        }
        let fixed = self.fixed.borrow();
        match fixed.iter().rev().find(|r| r.matches(program, args)) {
            Some(r) => r.result.clone(),
            None => panic!("no answer for `{program} {}`", args.join(" ")),
        }
    }

    fn answers(&self, socket: &Path) -> bool {
        let mut sockets = self.sockets.borrow_mut();
        match sockets.get_mut(socket) {
            Some(seq) if seq.len() > 1 => seq.pop_front().unwrap_or(false),
            Some(seq) => seq.front().copied().unwrap_or(false),
            None => false,
        }
    }

    fn sleep(&self, _d: Duration) {
        self.sleeps.set(self.sleeps.get() + 1);
    }
}
