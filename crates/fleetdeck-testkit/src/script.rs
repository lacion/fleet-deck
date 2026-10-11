//! The `fixture:<path>#<process>` scenario of the fake agents: the fake plays
//! one process of a recorded fixture (ADR-001 §2.7 record format).
//!
//! - `spawn`: the arguments must equal the recorded ones (else exit 4);
//! - `in`: the fake reads one stdin line; it must be the same JSON as the
//!   recorded frame (else exit 3);
//! - `out`: the fake writes the recorded frame as one line;
//! - `note` with the text `host closed stdin`: the fake reads stdin to its end;
//! - `exit`: the fake reads stdin to its end and exits with the recorded code.
//!   Without a code (the recorder killed the process), it waits to be killed.
//!
//! Other records (`fixture`, `host`, other notes) do nothing: the host side
//! applies the host commands.

use std::io::{BufRead, Write};
use std::path::Path;
use std::process::ExitCode;

use serde_json::Value;

/// The scenario prefix.
pub const FIXTURE_PREFIX: &str = "fixture:";

/// The note that marks the point where the recorder closed stdin.
pub const STDIN_CLOSED_NOTE: &str = "host closed stdin";

/// The records of one process: the `index`-th `spawn` up to the next one.
///
/// # Errors
/// The file does not read, a line is not JSON, or the process does not exist.
pub fn process_records(path: &Path, index: usize) -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut seen = 0usize;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: Value =
            serde_json::from_str(line).map_err(|e| format!("line {}: {e}", n + 1))?;
        if dir_of(&record) == Some("spawn") {
            seen += 1;
        }
        if seen == index + 1 {
            out.push(record);
        } else if seen > index + 1 {
            break;
        }
    }
    if out.is_empty() {
        return Err(format!("{}: no process {index}", path.display()));
    }
    Ok(out)
}

/// The `dir` of a record.
#[must_use]
pub fn dir_of(record: &Value) -> Option<&str> {
    record.get("dir").and_then(Value::as_str)
}

/// Plays `fixture:<path>#<index>` (the part after the prefix).
pub fn play(name: &str, spec: &str) -> ExitCode {
    let (path, index) = match spec.rsplit_once('#') {
        Some((p, i)) => match i.parse::<usize>() {
            Ok(i) => (p, i),
            Err(_) => return fail(name, 2, &format!("bad process index in `{spec}`")),
        },
        None => (spec, 0),
    };
    let records = match process_records(Path::new(path), index) {
        Ok(r) => r,
        Err(e) => return fail(name, 2, &e),
    };
    let mut stdin = std::io::stdin().lock();
    let mut out = std::io::stdout().lock();
    let mut stdin_open = true;
    for record in &records {
        match dir_of(record) {
            Some("spawn") => {
                let want: Vec<String> = record
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let got: Vec<String> = std::env::args().skip(1).collect();
                if want != got {
                    return fail(name, 4, &format!("args differ: want {want:?}, got {got:?}"));
                }
            }
            Some("in") => {
                let want = record.get("frame").cloned().unwrap_or(Value::Null);
                let mut line = String::new();
                match stdin.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        return fail(name, 3, &format!("stdin ended; expected {want}"));
                    }
                    Ok(_) => {}
                }
                match serde_json::from_str::<Value>(&line) {
                    Ok(got) if got == want => {}
                    _ => {
                        return fail(
                            name,
                            3,
                            &format!("stdin differs: want {want}, got {}", line.trim_end()),
                        );
                    }
                }
            }
            Some("out") => {
                let frame = record.get("frame").cloned().unwrap_or(Value::Null);
                if writeln!(out, "{frame}").and_then(|()| out.flush()).is_err() {
                    return ExitCode::from(1);
                }
            }
            Some("note")
                if record.get("text").and_then(Value::as_str) == Some(STDIN_CLOSED_NOTE) =>
            {
                drain(&mut stdin);
                stdin_open = false;
            }
            Some("exit") => {
                let code = record.get("code").and_then(Value::as_u64);
                let Some(code) = code.and_then(|c| u8::try_from(c).ok()) else {
                    // Killed by the recorder: wait for the kill.
                    loop {
                        std::thread::park();
                    }
                };
                if stdin_open {
                    drain(&mut stdin);
                }
                return ExitCode::from(code);
            }
            _ => {}
        }
    }
    if stdin_open {
        drain(&mut stdin);
    }
    ExitCode::SUCCESS
}

fn drain(stdin: &mut impl BufRead) {
    let mut sink = Vec::new();
    let _ = std::io::Read::read_to_end(stdin, &mut sink);
}

fn fail(name: &str, code: u8, message: &str) -> ExitCode {
    eprintln!("{name}: {message}");
    ExitCode::from(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../spikes/s1-claude/fixtures/rust")
            .join(name)
    }

    #[test]
    fn process_records_split_a_fixture_at_each_spawn() {
        let path = fixture("approval_restart.jsonl");
        let first = process_records(&path, 0).unwrap();
        let second = process_records(&path, 1).unwrap();
        for p in [&first, &second] {
            assert_eq!(dir_of(&p[0]), Some("spawn"));
            assert_eq!(p.iter().filter(|r| dir_of(r) == Some("spawn")).count(), 1);
            assert_eq!(p.iter().filter(|r| dir_of(r) == Some("exit")).count(), 1);
        }
        // The recorder killed process 0: its exit has no code.
        let exit = first.iter().find(|r| dir_of(r) == Some("exit")).unwrap();
        assert!(exit["code"].is_null());
        assert!(process_records(&path, 2).is_err());
    }
}
