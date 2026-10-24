//! The resume baseline of a Claude session (B21, DL-443).
//!
//! At a clean exit the CLI writes a `cost-state` record to the transcript. A
//! resumed or forked process restores the totals of the last record and counts
//! on from them. The adapter reads the same record before it starts the
//! process, so `first total - restored total` is the usage of the new process.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use rustix::fs::{FileType, Mode, OFlags};
use serde_json::Value;

use super::process_usage::{ProcessTotals, UsageBase};

/// The block size of the backward scan.
const BLOCK: u64 = 64 * 1024;
/// How much of the end of a transcript the scan reads.
const MAX_SCAN: u64 = 32 * 1024 * 1024;
/// How long the scan may run.
const DEADLINE: Duration = Duration::from_secs(2);
/// The longest line that can be a `cost-state` record (the history
/// `line_max`); the scan skips a longer line.
pub const MAX_LINE: usize = 16 * 1024 * 1024;

/// The directory name of `cwd` under `<home>/projects` (every character that
/// is not ASCII alphanumeric becomes `-`).
#[must_use]
pub fn project_dir_name(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// What a backward scan found.
#[derive(Debug, PartialEq)]
enum Scan {
    /// The last record of the session.
    Found(ProcessTotals),
    /// No record. `whole`: the scan reached the start of the file.
    NoRecord { whole: bool },
    /// A record of the session whose totals cannot be read.
    Unreadable,
    /// The deadline passed.
    TimedOut,
}

/// The record in `line`, if it is a `cost-state` record of `native`.
/// `None`: the line is not one. `Some(None)`: it is one, without totals.
fn record_of(line: &[u8], native: &str) -> Option<Option<ProcessTotals>> {
    const NEEDLE: &[u8] = b"\"cost-state\"";
    if !line.windows(NEEDLE.len()).any(|w| w == NEEDLE) {
        return None;
    }
    let v: Value = serde_json::from_slice(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("cost-state")
        || v.get("sessionId").and_then(Value::as_str) != Some(native)
    {
        return None;
    }
    Some(ProcessTotals::from_cost_state(&v))
}

/// The part of a line that the backward scan holds: the pieces of the line,
/// newest bytes first, while the line fits in `max_line` bytes.
struct Partial {
    /// The pieces in the order the scan read them (each piece comes before
    /// the previous one in the file).
    pieces: Vec<Vec<u8>>,
    len: usize,
    /// The line is longer than `max_line`: its bytes are dropped until its
    /// start. A longer line is not a `cost-state` record.
    oversized: bool,
}

impl Partial {
    const fn new() -> Self {
        Self {
            pieces: Vec::new(),
            len: 0,
            oversized: false,
        }
    }

    /// Adds `head`, the bytes before the held part of the line.
    fn prepend(&mut self, head: &[u8], max_line: usize) {
        if self.oversized || head.is_empty() {
            return;
        }
        self.len += head.len();
        if self.len > max_line {
            self.pieces = Vec::new();
            self.len = 0;
            self.oversized = true;
        } else {
            self.pieces.push(head.to_vec());
        }
    }

    /// The whole line: `head` and the held pieces. `None` for an oversized
    /// line. The part is empty again after the call.
    fn finish(&mut self, head: &[u8], max_line: usize) -> Option<Vec<u8>> {
        let oversized = self.oversized || self.len + head.len() > max_line;
        let pieces = std::mem::take(&mut self.pieces);
        self.len = 0;
        self.oversized = false;
        if oversized {
            return None;
        }
        let mut line = Vec::with_capacity(head.len() + pieces.iter().map(Vec::len).sum::<usize>());
        line.extend_from_slice(head);
        for piece in pieces.iter().rev() {
            line.extend_from_slice(piece);
        }
        Some(line)
    }

    fn is_empty(&self) -> bool {
        !self.oversized && self.pieces.is_empty()
    }
}

/// The scan result of one line, if it ends the scan.
fn verdict(line: &[u8], native: &str) -> Option<Scan> {
    match record_of(line, native)? {
        Some(t) => Some(Scan::Found(t)),
        None => Some(Scan::Unreadable),
    }
}

/// Reads `file` from its end in blocks, for the last record of `native`.
/// A line longer than `max_line` is not a record: the scan holds at most
/// `max_line` bytes of one line, and it copies each byte at most twice.
fn scan<R: Read + Seek>(
    file: &mut R,
    native: &str,
    max_scan: u64,
    max_line: usize,
    deadline: Instant,
) -> io::Result<Scan> {
    let mut pos = file.seek(SeekFrom::End(0))?;
    let mut scanned = 0_u64;
    // The end of a line whose start is in a block that is not read yet.
    let mut partial = Partial::new();
    let mut buf = Vec::new();
    loop {
        if pos == 0 {
            // `partial` is the first line of the file.
            if partial.is_empty() {
                return Ok(Scan::NoRecord { whole: true });
            }
            return Ok(partial
                .finish(&[], max_line)
                .and_then(|line| verdict(&line, native))
                .unwrap_or(Scan::NoRecord { whole: true }));
        }
        if scanned >= max_scan {
            return Ok(Scan::NoRecord { whole: false });
        }
        if Instant::now() > deadline {
            return Ok(Scan::TimedOut);
        }
        let n = BLOCK.min(pos).min(max_scan - scanned);
        pos -= n;
        scanned += n;
        file.seek(SeekFrom::Start(pos))?;
        buf.resize(usize::try_from(n).unwrap_or(usize::MAX), 0);
        file.read_exact(&mut buf)?;
        // Each newline ends the line after it; walk them from the end.
        let mut end = buf.len();
        while let Some(at) = buf[..end].iter().rposition(|b| *b == b'\n') {
            let tail = &buf[at + 1..end];
            let found = if partial.is_empty() {
                if tail.len() <= max_line {
                    verdict(tail, native)
                } else {
                    None
                }
            } else {
                partial
                    .finish(tail, max_line)
                    .and_then(|line| verdict(&line, native))
            };
            if let Some(found) = found {
                return Ok(found);
            }
            end = at;
        }
        partial.prepend(&buf[..end], max_line);
    }
}

/// The last `cost-state` record of `native` in an open transcript, read from
/// the end in 64 KiB blocks, at most `max_scan` bytes, until `deadline`. A
/// line longer than [`MAX_LINE`] is not a record.
/// `Ok(None)`: the scanned part has no record of that session.
///
/// # Errors
/// An I/O error of the read, a record of the session without totals
/// (`InvalidData`), or the deadline passed (`TimedOut`).
pub fn last_cost_state<R: Read + Seek>(
    file: &mut R,
    native: &str,
    max_scan: u64,
    deadline: Instant,
) -> io::Result<Option<ProcessTotals>> {
    match scan(file, native, max_scan, MAX_LINE, deadline)? {
        Scan::Found(t) => Ok(Some(t)),
        Scan::NoRecord { .. } => Ok(None),
        Scan::TimedOut => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the cost-state scan passed its deadline",
        )),
        Scan::Unreadable => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a cost-state record without totals",
        )),
    }
}

/// A native id has the UUID form (the check of `args.rs`), so it is one safe
/// file name.
fn is_uuid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The Claude home of a launch environment: `CLAUDE_CONFIG_DIR` when set and
/// not empty, else `HOME/.claude` (the rule of the history locator).
fn claude_home(env: &ChildEnv) -> Option<PathBuf> {
    let set = |name: &str| env.get(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    set("CLAUDE_CONFIG_DIR").or_else(|| set("HOME").map(|home| home.join(".claude")))
}

/// Opens the transcript: no link, a regular file of this user, one link.
fn open_transcript(path: &Path) -> Option<File> {
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .ok()?;
    let st = rustix::fs::fstat(&fd).ok()?;
    let regular = FileType::from_raw_mode(st.st_mode) == FileType::RegularFile;
    (regular && st.st_uid == rustix::process::geteuid().as_raw() && st.st_nlink == 1)
        .then(|| File::from(fd))
}

/// The base of an open transcript under the limits `max_scan` and `deadline`.
fn base_of_file(file: &mut File, native: &str, max_scan: u64, deadline: Instant) -> UsageBase {
    match scan(file, native, max_scan, MAX_LINE, deadline) {
        Ok(Scan::Found(totals)) => UsageBase::Restored(totals),
        // The whole file has no record: the CLI starts at 0.
        Ok(Scan::NoRecord { whole: true }) => UsageBase::Restored(ProcessTotals::default()),
        // The limit came before the start of the file, the deadline passed,
        // or the read failed: the restored totals are not known.
        Ok(Scan::NoRecord { whole: false } | Scan::Unreadable | Scan::TimedOut) | Err(_) => {
            UsageBase::Unknown
        }
    }
}

/// The base of a resume or fork of `native` in `cwd` (DL-443).
#[must_use]
pub fn restored_base(env: &ChildEnv, cwd: &Path, native: &str) -> UsageBase {
    restored_base_with(env, cwd, native, MAX_SCAN, DEADLINE)
}

fn restored_base_with(
    env: &ChildEnv,
    cwd: &Path,
    native: &str,
    max_scan: u64,
    deadline: Duration,
) -> UsageBase {
    if !is_uuid(native) {
        return UsageBase::Unknown;
    }
    let Some(home) = claude_home(env).filter(|h| h.is_absolute()) else {
        return UsageBase::Unknown;
    };
    let path = home
        .join("projects")
        .join(project_dir_name(&cwd.to_string_lossy()))
        .join(format!("{native}.jsonl"));
    let Some(mut file) = open_transcript(&path) else {
        return UsageBase::Unknown;
    };
    base_of_file(&mut file, native, max_scan, Instant::now() + deadline)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io::Cursor;

    use super::*;

    const NATIVE: &str = "4b9142af-6b77-431a-9530-5df5f3e80e1d";
    const OTHER: &str = "473a0b6c-7199-4726-b3d9-46fff71845ef";

    fn record(native: &str, input: u64, cost: f64) -> String {
        format!(
            "{{\"type\":\"cost-state\",\"sessionId\":\"{native}\",\"totalCostUSD\":{cost},\
             \"modelUsage\":{{\"claude-haiku-5-5\":{{\"inputTokens\":{input},\"outputTokens\":2,\
             \"cacheReadInputTokens\":3,\"cacheCreationInputTokens\":4,\"costUSD\":{cost}}}}}}}\n"
        )
    }

    fn input_of(t: &ProcessTotals) -> u64 {
        t.models["claude-haiku-5-5"].input
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    /// A reader that waits on each read and counts the reads.
    struct Slow {
        inner: Cursor<Vec<u8>>,
        reads: usize,
    }

    impl Read for Slow {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            std::thread::sleep(Duration::from_millis(2));
            self.inner.read(buf)
        }
    }

    impl Seek for Slow {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.inner.seek(to)
        }
    }

    #[test]
    fn an_oversized_trailing_line_stops_at_the_deadline() {
        // A transcript header, then one 16 MiB line without an end: 256
        // blocks of 64 KiB at 2 ms each.
        let mut text = record(NATIVE, 5, 0.5).into_bytes();
        text.resize(text.len() + 16 * 1024 * 1024, b'x');
        let mut slow = Slow {
            inner: Cursor::new(text),
            reads: 0,
        };
        let started = Instant::now();
        let deadline = started + Duration::from_millis(40);
        let err = last_cost_state(&mut slow, NATIVE, u64::MAX, deadline).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(slow.reads < 100, "{} reads", slow.reads);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_passed_deadline_is_timed_out() {
        let text = record(NATIVE, 5, 0.5);
        let past = Instant::now();
        std::thread::sleep(Duration::from_millis(2));
        let err =
            last_cost_state(&mut Cursor::new(text.as_bytes()), NATIVE, u64::MAX, past).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_line_over_the_line_limit_is_skipped() {
        // The record before a long trailing line and before a long line in
        // the middle is found; the scan holds at most `max_line` bytes.
        let long = "x".repeat(300_000);
        let text = format!(
            "{{\"type\":\"user\"}}\n{}{{\"t\":\"{long}\"}}\n{long}",
            record(NATIVE, 7, 0.1)
        );
        let found = scan(
            &mut Cursor::new(text.as_bytes()),
            NATIVE,
            u64::MAX,
            100_000,
            far(),
        )
        .unwrap();
        let Scan::Found(t) = found else {
            panic!("{found:?}");
        };
        assert_eq!(input_of(&t), 7);
        // A record longer than the limit is not a record.
        let padded =
            format!("{{\"type\":\"cost-state\",\"sessionId\":\"{NATIVE}\",\"pad\":\"{long}\"}}\n");
        let found = scan(
            &mut Cursor::new(padded.as_bytes()),
            NATIVE,
            u64::MAX,
            100_000,
            far(),
        )
        .unwrap();
        assert_eq!(found, Scan::NoRecord { whole: true });
    }

    #[test]
    fn the_first_line_of_the_file_is_read() {
        // A record as the first line, across three blocks.
        let filler = format!(
            "{{\"type\":\"user\",\"text\":\"{}\"}}\n",
            "y".repeat(150_000)
        );
        let text = format!("{}{filler}", record(NATIVE, 9, 0.3));
        let found = last_cost_state(&mut Cursor::new(text.as_bytes()), NATIVE, u64::MAX, far())
            .unwrap()
            .unwrap();
        assert_eq!(input_of(&found), 9);
        let one = record(NATIVE, 3, 0.3);
        let one = one.trim_end();
        let found = last_cost_state(&mut Cursor::new(one.as_bytes()), NATIVE, u64::MAX, far())
            .unwrap()
            .unwrap();
        assert_eq!(input_of(&found), 3);
    }

    /// A transcript with a `HOME` of its own; the path of the file.
    fn home_with(cwd: &str, native: &str, text: &str) -> (tempfile::TempDir, ChildEnv, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("claude-home");
        let dir = home.join("projects").join(project_dir_name(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{native}.jsonl"));
        std::fs::write(&file, text).unwrap();
        let mut env = ChildEnv::empty();
        env.set("CLAUDE_CONFIG_DIR", home.to_str().unwrap())
            .unwrap();
        (tmp, env, file)
    }

    #[test]
    fn the_last_record_of_the_session_wins() {
        let text = format!(
            "{{\"type\":\"user\"}}\n{}{}{}{{\"type\":\"assistant\",\"note\":\"cost-state\"}}\n",
            record(NATIVE, 10, 0.1),
            record(NATIVE, 20, 0.2),
            record(OTHER, 99, 0.9),
        );
        let found = last_cost_state(&mut Cursor::new(text.as_bytes()), NATIVE, 1 << 20, far())
            .unwrap()
            .unwrap();
        assert_eq!(input_of(&found), 20);
        assert!((found.cost() - 0.2).abs() < 1e-12);
    }

    #[test]
    fn a_record_across_a_block_boundary_is_found() {
        let filler = format!(
            "{{\"type\":\"user\",\"text\":\"{}\"}}\n",
            "x".repeat(70_000)
        );
        let text = format!("{}{}{filler}", record(NATIVE, 7, 0.1), filler);
        let found = last_cost_state(&mut Cursor::new(text.as_bytes()), NATIVE, 1 << 22, far())
            .unwrap()
            .unwrap();
        assert_eq!(input_of(&found), 7);
    }

    #[test]
    fn no_record_is_a_zero_base() {
        let (_tmp, env, _) = home_with("/w/a", NATIVE, "{\"type\":\"user\"}\n");
        assert_eq!(
            restored_base(&env, Path::new("/w/a"), NATIVE),
            UsageBase::Restored(ProcessTotals::default())
        );
    }

    #[test]
    fn a_record_is_the_restored_base() {
        let (_tmp, env, _) = home_with("/w/a", NATIVE, &record(NATIVE, 5, 0.5));
        let UsageBase::Restored(t) = restored_base(&env, Path::new("/w/a"), NATIVE) else {
            panic!("not restored");
        };
        assert_eq!(input_of(&t), 5);
    }

    #[test]
    fn a_missing_file_is_unknown() {
        let (_tmp, env, _) = home_with("/w/a", NATIVE, "");
        assert_eq!(
            restored_base(&env, Path::new("/w/other"), NATIVE),
            UsageBase::Unknown
        );
        assert_eq!(
            restored_base(&env, Path::new("/w/a"), "not-a-uuid"),
            UsageBase::Unknown
        );
    }

    #[test]
    fn a_link_is_unknown() {
        let (tmp, env, file) = home_with("/w/a", NATIVE, "");
        let real = tmp.path().join("real.jsonl");
        std::fs::write(&real, record(NATIVE, 5, 0.5)).unwrap();
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(&real, &file).unwrap();
        assert_eq!(
            restored_base(&env, Path::new("/w/a"), NATIVE),
            UsageBase::Unknown
        );
    }

    #[test]
    fn a_record_past_the_limit_is_unknown() {
        let filler = format!("{{\"type\":\"user\",\"text\":\"{}\"}}\n", "x".repeat(200));
        let text = format!("{}{}", record(NATIVE, 5, 0.5), filler.repeat(20));
        let (_tmp, env, _) = home_with("/w/a", NATIVE, &text);
        assert_eq!(
            restored_base_with(&env, Path::new("/w/a"), NATIVE, 1024, DEADLINE),
            UsageBase::Unknown
        );
        // With room for the whole file the record is found.
        assert!(matches!(
            restored_base_with(&env, Path::new("/w/a"), NATIVE, 1 << 20, DEADLINE),
            UsageBase::Restored(_)
        ));
    }

    #[test]
    fn project_dir_name_replaces_every_non_alphanumeric_character() {
        assert_eq!(project_dir_name("/home/a b/proj_x.y"), "-home-a-b-proj-x-y");
    }
}
