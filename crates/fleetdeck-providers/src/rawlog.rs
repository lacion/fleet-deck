//! The raw-frame diagnostic log of one session (PLAN §6.2, L12).
//!
//! Files, not a database table: `raw-000001.jsonl`, `raw-000002.jsonl`, ...
//! in the session's log directory. A file rotates when the next line would
//! make it larger than `max_file_bytes`; only the newest `max_files` files
//! stay. File names never change, so a [`RawRef`] (file + byte offset) stays
//! valid until its file is removed.
//!
//! One line per frame: `{"at": <unix ms>, "dir": "in"|"out"|"stderr",
//! "frame": <JSON>}`. A frame larger than the payload budget (64 KiB) is
//! replaced by a structural summary (`{"summary": ..., "bytes": N}`). The
//! adapter filters token deltas before it calls [`RawFrameLog::record`].
//!
//! The log is diagnostic: a write error never stops a session. The caller
//! drops the log after the first error ([`RawFrameLog::record`] returns it).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use fleetdeck_proto::event::RawRef;
use serde_json::{Map, Value, json};

/// Each payload has this budget (PLAN §6.2).
pub const PAYLOAD_BUDGET: usize = 64 * 1024;
/// A string in a structural summary keeps at most this many bytes.
const SUMMARY_TEXT: usize = 256;

/// The limits of one session log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLogLimits {
    pub max_file_bytes: u64,
    pub max_files: usize,
    pub payload_budget: usize,
}

impl Default for RawLogLimits {
    /// 4 files of 4 MiB: at most 16 MiB per session.
    fn default() -> Self {
        Self {
            max_file_bytes: 4 * 1024 * 1024,
            max_files: 4,
            payload_budget: PAYLOAD_BUDGET,
        }
    }
}

/// The direction of a raw frame, seen from the station.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawDir {
    /// Written to the provider (stdin).
    In,
    /// Read from the provider (stdout).
    Out,
    /// Diagnostic text from the provider (stderr).
    Stderr,
}

impl RawDir {
    fn as_str(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
            Self::Stderr => "stderr",
        }
    }
}

/// The bounded rotating log of one session.
#[derive(Debug)]
pub struct RawFrameLog {
    dir: PathBuf,
    limits: RawLogLimits,
    /// The generation number of the current file.
    generation: u64,
    file: File,
    len: u64,
}

impl RawFrameLog {
    /// Creates the log directory (mode 0700 on Unix) and the first file.
    ///
    /// # Errors
    /// The directory or the file cannot be created.
    pub fn create(dir: &Path, limits: RawLogLimits) -> io::Result<Self> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir)?;
        let limits = RawLogLimits {
            max_file_bytes: limits.max_file_bytes.max(1),
            max_files: limits.max_files.max(1),
            payload_budget: limits.payload_budget.max(1),
        };
        let generation = 1;
        let file = open_new(&dir.join(file_name(generation)))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            limits,
            generation,
            file,
            len: 0,
        })
    }

    /// The log directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Appends one frame and returns where it is.
    ///
    /// # Errors
    /// A write, rotation or removal error.
    pub fn record(&mut self, dir: RawDir, at_ms: i64, frame: &Value) -> io::Result<RawRef> {
        let encoded = serde_json::to_string(frame).map_err(io::Error::other)?;
        if encoded.len() > self.limits.payload_budget {
            let summary = json!({"summary": summarize(frame), "bytes": encoded.len()});
            self.append(dir, at_ms, &summary)
        } else {
            self.append(dir, at_ms, frame)
        }
    }

    fn append(&mut self, dir: RawDir, at_ms: i64, payload: &Value) -> io::Result<RawRef> {
        let mut line = serde_json::to_string(&json!({
            "at": at_ms,
            "dir": dir.as_str(),
            "frame": payload,
        }))
        .map_err(io::Error::other)?;
        line.push('\n');
        let size = line.len() as u64;
        if self.len > 0 && self.len + size > self.limits.max_file_bytes {
            self.rotate()?;
        }
        let offset = self.len;
        self.file.write_all(line.as_bytes())?;
        self.len += size;
        Ok(RawRef {
            file: file_name(self.generation),
            offset,
        })
    }

    /// Appends diagnostic text (for example the stderr tail at exit).
    ///
    /// # Errors
    /// See [`Self::record`].
    pub fn note(&mut self, dir: RawDir, at_ms: i64, text: &str) -> io::Result<RawRef> {
        let budget = self.limits.payload_budget;
        let text = if text.len() > budget {
            let mut end = text.len() - budget;
            while !text.is_char_boundary(end) {
                end += 1;
            }
            &text[end..]
        } else {
            text
        };
        // The text is within the budget; its JSON quoting may add a little.
        self.append(dir, at_ms, &Value::from(text))
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.generation += 1;
        self.file = open_new(&self.dir.join(file_name(self.generation)))?;
        self.len = 0;
        let keep = self.limits.max_files as u64;
        if self.generation > keep {
            let old = self.generation - keep;
            match std::fs::remove_file(self.dir.join(file_name(old))) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

fn file_name(generation: u64) -> String {
    format!("raw-{generation:06}.jsonl")
}

fn open_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// The shape of a large frame: scalar fields, short strings, and the sizes of
/// long strings, arrays and nested objects (two levels deep).
#[must_use]
pub fn summarize(frame: &Value) -> Value {
    summarize_at(frame, 2)
}

fn summarize_at(value: &Value, depth: usize) -> Value {
    match value {
        Value::String(s) if s.len() > SUMMARY_TEXT => Value::from(format!("<{} bytes>", s.len())),
        Value::Array(items) => Value::from(format!("<{} items>", items.len())),
        Value::Object(map) if depth == 0 => Value::from(format!("<{} keys>", map.len())),
        Value::Object(map) => {
            let out: Map<String, Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), summarize_at(v, depth - 1)))
                .collect();
            Value::Object(out)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(dir: &Path, file: &str) -> Vec<Value> {
        std::fs::read_to_string(dir.join(file))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn a_frame_is_found_at_its_raw_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let mut log = RawFrameLog::create(&tmp.path().join("s1"), RawLogLimits::default()).unwrap();
        let a = log.record(RawDir::In, 1, &json!({"type":"a"})).unwrap();
        let b = log.record(RawDir::Out, 2, &json!({"type":"b"})).unwrap();
        assert_eq!(a.file, "raw-000001.jsonl");
        assert_eq!(a.offset, 0);
        let text = std::fs::read_to_string(log.dir().join(&b.file)).unwrap();
        let at: Value =
            serde_json::from_str(text[b.offset as usize..].lines().next().unwrap()).unwrap();
        assert_eq!(at, json!({"at":2,"dir":"out","frame":{"type":"b"}}));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(log.dir()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
            let mode = std::fs::metadata(log.dir().join(&a.file))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_large_payload_becomes_a_structural_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let mut log = RawFrameLog::create(tmp.path(), RawLogLimits::default()).unwrap();
        let big = "x".repeat(PAYLOAD_BUDGET + 1);
        let frame = json!({"type":"user","n":3,"message":{"content":[1,2],"text":big,
                           "deep":{"a":{"b":1}}}});
        let r = log.record(RawDir::Out, 5, &frame).unwrap();
        let got = &lines(tmp.path(), &r.file)[0]["frame"];
        assert_eq!(
            got["summary"],
            json!({"type":"user","n":3,"message":{"content":"<2 items>",
                   "text":format!("<{} bytes>", PAYLOAD_BUDGET + 1),"deep":"<1 keys>"}})
        );
        assert!(got["bytes"].as_u64().unwrap() > PAYLOAD_BUDGET as u64);
        let size = std::fs::metadata(tmp.path().join(&r.file)).unwrap().len();
        assert!(size < 1024, "the summary is small: {size}");
    }

    #[test]
    fn the_log_rotates_and_keeps_only_the_newest_files() {
        let tmp = tempfile::tempdir().unwrap();
        let limits = RawLogLimits {
            max_file_bytes: 200,
            max_files: 2,
            payload_budget: PAYLOAD_BUDGET,
        };
        let mut log = RawFrameLog::create(tmp.path(), limits).unwrap();
        let mut refs = Vec::new();
        for i in 0..20 {
            refs.push(
                log.record(RawDir::Out, i, &json!({"i": i, "pad": "p".repeat(40)}))
                    .unwrap(),
            );
        }
        let mut files: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        assert_eq!(files.len(), 2, "{files:?}");
        let total: u64 = files
            .iter()
            .map(|f| std::fs::metadata(tmp.path().join(f)).unwrap().len())
            .sum();
        assert!(total <= 400, "bounded: {total}");
        // The newest reference still points at its frame.
        let last = refs.last().unwrap();
        assert_eq!(files.last().unwrap(), &last.file);
        let text = std::fs::read_to_string(tmp.path().join(&last.file)).unwrap();
        let line: Value =
            serde_json::from_str(text[last.offset as usize..].lines().next().unwrap()).unwrap();
        assert_eq!(line["frame"]["i"], 19);
        // An old reference names a removed file, never a wrong frame.
        assert!(!tmp.path().join(&refs[0].file).exists());
    }

    #[test]
    fn a_long_note_keeps_its_end() {
        let tmp = tempfile::tempdir().unwrap();
        let limits = RawLogLimits {
            payload_budget: 8,
            ..RawLogLimits::default()
        };
        let mut log = RawFrameLog::create(tmp.path(), limits).unwrap();
        let r = log.note(RawDir::Stderr, 1, "0123456789abcdef").unwrap();
        assert_eq!(lines(tmp.path(), &r.file)[0]["frame"], "89abcdef");
    }
}
