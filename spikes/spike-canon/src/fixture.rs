//! Recorded provider fixtures (PLAN §13, L20).
//!
//! A fixture file is JSON Lines. Line 1 is a [`Header`] wrapped as
//! `{"fixture": {...}}`. Each other line is one [`Record`]:
//!
//! - `spawn`: the recorder started a provider process with these arguments.
//! - `host`: the host (Fleet Deck) gave the adapter a command.
//! - `in`: a frame the adapter wrote to the provider.
//! - `out`: a frame the provider wrote to the adapter.
//! - `raw`: a provider output line that is not JSON.
//! - `exit`: the provider process ended.
//! - `note`: a recorder observation (for example, a timing).
//!
//! The replay harness feeds `out` records to the real adapter code, replays the
//! `host` commands at their recorded positions, and compares the adapter's
//! writes with the `in` records.

use std::io::{BufRead, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Metadata of one fixture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Header {
    /// `claude` or `codex`.
    pub provider: String,
    /// A PLAN §13 shared scenario name or a provider-specific scenario name.
    pub scenario: String,
    /// Version string of the provider CLI that produced the frames.
    pub cli_version: String,
    /// The program that recorded the fixture (for example `record-claude`, `sdk-recorder`).
    pub recorder: String,
    /// Model used for the recording.
    pub model: Option<String>,
    /// Free text: what the scenario does and what it proves.
    pub notes: String,
}

/// One record of a fixture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "dir", rename_all = "snake_case")]
pub enum Record {
    Spawn { args: Vec<String> },
    Host { cmd: Value },
    In { frame: Value },
    Out { frame: Value },
    Raw { line: String },
    Exit { code: Option<i32> },
    Note { text: String },
}

/// A complete fixture.
#[derive(Debug, Clone, PartialEq)]
pub struct Fixture {
    pub header: Header,
    pub records: Vec<Record>,
}

/// Error when a fixture file is read or written.
#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("line {line}: {source}")]
    Json {
        line: usize,
        source: serde_json::Error,
    },
    #[error("the fixture has no header line")]
    NoHeader,
}

#[derive(Serialize, Deserialize)]
struct HeaderLine {
    fixture: Header,
}

impl Fixture {
    /// Writes the fixture as JSON Lines.
    pub fn write_to(&self, mut w: impl Write) -> Result<(), FixtureError> {
        let head = HeaderLine {
            fixture: self.header.clone(),
        };
        let to_err = |source| FixtureError::Json { line: 0, source };
        writeln!(w, "{}", serde_json::to_string(&head).map_err(to_err)?)?;
        for r in &self.records {
            writeln!(w, "{}", serde_json::to_string(r).map_err(to_err)?)?;
        }
        Ok(())
    }

    /// Reads a fixture from JSON Lines.
    pub fn read_from(r: impl BufRead) -> Result<Self, FixtureError> {
        let mut header = None;
        let mut records = Vec::new();
        for (i, line) in r.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let to_err = |source| FixtureError::Json {
                line: i + 1,
                source,
            };
            if header.is_none() {
                let h: HeaderLine = serde_json::from_str(&line).map_err(to_err)?;
                header = Some(h.fixture);
            } else {
                records.push(serde_json::from_str(&line).map_err(to_err)?);
            }
        }
        Ok(Self {
            header: header.ok_or(FixtureError::NoHeader)?,
            records,
        })
    }

    /// Reads a fixture file.
    pub fn load(path: &Path) -> Result<Self, FixtureError> {
        let file = std::fs::File::open(path)?;
        Self::read_from(std::io::BufReader::new(file))
    }

    /// Writes a fixture file and creates the parent directory.
    pub fn save(&self, path: &Path) -> Result<(), FixtureError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::File::create(path)?;
        let mut w = std::io::BufWriter::new(file);
        self.write_to(&mut w)?;
        w.flush()?;
        Ok(())
    }
}

/// Removes personal and machine-specific data before a fixture is saved.
///
/// - Values of the `redact_keys` are replaced with `"<redacted>"`.
/// - Each path prefix is replaced with its placeholder (longest prefix first).
#[derive(Debug, Clone, Default)]
pub struct Scrubber {
    redact_keys: Vec<String>,
    replacements: Vec<(String, String)>,
}

impl Scrubber {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the value of each key with this name, at any depth.
    #[must_use]
    pub fn redact_key(mut self, key: &str) -> Self {
        self.redact_keys.push(key.to_owned());
        self
    }

    /// Replaces each occurrence of `from` in a string with `to`.
    #[must_use]
    pub fn replace(mut self, from: &str, to: &str) -> Self {
        if !from.is_empty() {
            self.replacements.push((from.to_owned(), to.to_owned()));
            self.replacements
                .sort_by_key(|(f, _)| std::cmp::Reverse(f.len()));
        }
        self
    }

    pub fn scrub_str(&self, s: &str) -> String {
        let mut out = s.to_owned();
        for (from, to) in &self.replacements {
            if out.contains(from.as_str()) {
                out = out.replace(from.as_str(), to);
            }
        }
        out
    }

    pub fn scrub(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.scrub_str(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.scrub(x)).collect()),
            Value::Object(o) => {
                let mut m = Map::new();
                for (k, x) in o {
                    let val = if self.redact_keys.iter().any(|r| r == k) {
                        Value::String("<redacted>".to_owned())
                    } else {
                        self.scrub(x)
                    };
                    m.insert(self.scrub_str(k), val);
                }
                Value::Object(m)
            }
            other => other.clone(),
        }
    }

    pub fn scrub_record(&self, r: &Record) -> Record {
        match r {
            Record::Spawn { args } => Record::Spawn {
                args: args.iter().map(|a| self.scrub_str(a)).collect(),
            },
            Record::Host { cmd } => Record::Host {
                cmd: self.scrub(cmd),
            },
            Record::In { frame } => Record::In {
                frame: self.scrub(frame),
            },
            Record::Out { frame } => Record::Out {
                frame: self.scrub(frame),
            },
            Record::Raw { line } => Record::Raw {
                line: self.scrub_str(line),
            },
            Record::Note { text } => Record::Note {
                text: self.scrub_str(text),
            },
            Record::Exit { code } => Record::Exit { code: *code },
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn fixture_round_trips_through_jsonl() {
        let fx = Fixture {
            header: Header {
                provider: "claude".into(),
                scenario: "simple".into(),
                cli_version: "2.1.289".into(),
                recorder: "test".into(),
                model: Some("haiku".into()),
                notes: "n".into(),
            },
            records: vec![
                Record::Spawn {
                    args: vec!["-p".into()],
                },
                Record::Host {
                    cmd: json!({"op":"prompt","text":"hi"}),
                },
                Record::In {
                    frame: json!({"type":"user"}),
                },
                Record::Out {
                    frame: json!({"type":"result"}),
                },
                Record::Exit { code: Some(0) },
            ],
        };
        let mut buf = Vec::new();
        fx.write_to(&mut buf).unwrap();
        let text = String::from_utf8(buf.clone()).unwrap();
        assert!(text.starts_with("{\"fixture\":"));
        assert!(text.contains("{\"dir\":\"in\",\"frame\":{\"type\":\"user\"}}"));
        let back = Fixture::read_from(buf.as_slice()).unwrap();
        assert_eq!(back, fx);
    }

    #[test]
    fn scrubber_redacts_keys_and_paths() {
        let s = Scrubber::new()
            .redact_key("account")
            .replace("/home/u", "<home>")
            .replace("/home/u/tmp/x", "<workdir>");
        let v = json!({"account":{"email":"a@b"},"cwd":"/home/u/tmp/x/f","p":["/home/u/.claude"]});
        assert_eq!(
            s.scrub(&v),
            json!({"account":"<redacted>","cwd":"<workdir>/f","p":["<home>/.claude"]})
        );
    }

    #[test]
    fn empty_input_has_no_header() {
        assert!(matches!(
            Fixture::read_from(&b""[..]),
            Err(FixtureError::NoHeader)
        ));
    }
}
