//! A file system that fails one chosen call, for the failure-point tests.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::settings::{Fs, RealFs};

/// Which call fails: the `nth` call (from 0) of `op` whose path contains
/// `path_has`. `op` `None` counts every call. After a `crash`, every later
/// call fails too, as if the process had died.
#[derive(Clone)]
pub struct Rule {
    pub op: Option<&'static str>,
    pub path_has: &'static str,
    pub nth: usize,
    pub crash: bool,
}

impl Rule {
    pub fn fail(op: &'static str, path_has: &'static str) -> Self {
        Self {
            op: Some(op),
            path_has,
            nth: 0,
            crash: false,
        }
    }

    pub fn crash_at(index: usize) -> Self {
        Self {
            op: None,
            path_has: "",
            nth: index,
            crash: true,
        }
    }
}

#[derive(Default)]
struct State {
    rule: Option<Rule>,
    seen: usize,
    dead: bool,
    log: Vec<(String, PathBuf, bool)>,
}

#[derive(Default)]
pub struct FaultFs {
    state: Mutex<State>,
}

impl FaultFs {
    pub fn new(rule: Option<Rule>) -> Self {
        let fs = Self::default();
        fs.set_rule(rule);
        fs
    }

    pub fn set_rule(&self, rule: Option<Rule>) {
        let mut s = self.state.lock().unwrap();
        s.rule = rule;
        s.seen = 0;
        s.dead = false;
    }

    /// The calls so far: operation, path and success.
    pub fn log(&self) -> Vec<(String, PathBuf, bool)> {
        self.state.lock().unwrap().log.clone()
    }

    fn call<T>(
        &self,
        op: &'static str,
        path: &Path,
        run: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        let injected = {
            let mut s = self.state.lock().unwrap();
            let mut hit = s.dead;
            if let Some(rule) = s.rule.clone()
                && rule.op.is_none_or(|o| o == op)
                && path.to_string_lossy().contains(rule.path_has)
            {
                if s.seen == rule.nth {
                    hit = true;
                    if rule.crash {
                        s.dead = true;
                    }
                }
                s.seen += 1;
            }
            hit
        };
        let result = if injected {
            Err(io::Error::other("injected failure"))
        } else {
            run()
        };
        self.state
            .lock()
            .unwrap()
            .log
            .push((op.to_owned(), path.to_path_buf(), result.is_ok()));
        result
    }
}

impl Fs for FaultFs {
    fn create_excl_0600(&self, path: &Path) -> io::Result<File> {
        self.call("create", path, || RealFs.create_excl_0600(path))
    }

    fn write(&self, file: &mut File, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.call("write", path, || RealFs.write(file, path, bytes))
    }

    fn fsync(&self, file: &File, path: &Path) -> io::Result<()> {
        self.call("fsync", path, || RealFs.fsync(file, path))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.call("rename", to, || RealFs.rename(from, to))
    }

    fn fsync_dir(&self, dir: &Path) -> io::Result<()> {
        self.call("fsync_dir", dir, || RealFs.fsync_dir(dir))
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        self.call("remove", path, || RealFs.remove(path))
    }

    fn mkdir(&self, path: &Path) -> io::Result<()> {
        self.call("mkdir", path, || RealFs.mkdir(path))
    }
}
