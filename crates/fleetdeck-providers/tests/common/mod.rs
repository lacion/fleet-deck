//! Shared helpers of the Claude replay tests.

#![allow(dead_code, reason = "each test binary uses a part of the helpers")]

use std::path::{Path, PathBuf};

use fleetdeck_providers::NativeSessionId;
use fleetdeck_providers::claude::Launch;
use serde_json::Value;

/// The recorded Rust-recorder Claude fixtures.
pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/s1-claude/fixtures/rust")
}

/// Every fixture file, sorted by name.
pub fn fixtures() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(fixture_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    files
}

/// The launch that the recorded `spawn` arguments describe.
pub fn launch_of(args: &[Value]) -> Launch {
    let args: Vec<&str> = args.iter().map(|a| a.as_str().unwrap()).collect();
    let after = |flag: &str| {
        args.iter()
            .position(|a| *a == flag)
            .map(|i| args[i + 1].to_owned())
    };
    Launch {
        model: after("--model"),
        effort: after("--effort"),
        mode: after("--permission-mode"),
        resume: after("--resume").map(NativeSessionId),
        fork: args.contains(&"--fork-session"),
        extra_args: vec!["--safe-mode".to_owned()],
    }
}

/// The records of a fixture, split into processes (each starts at `spawn`).
pub fn processes(path: &Path) -> Vec<Vec<Value>> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut out: Vec<Vec<Value>> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let rec: Value = serde_json::from_str(line).unwrap();
        match rec.get("dir").and_then(Value::as_str) {
            None => {}
            Some("spawn") => out.push(vec![rec]),
            Some(_) => out.last_mut().unwrap().push(rec),
        }
    }
    out
}
