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

/// Reads `file` from its end in blocks, for the last record of `native`.
fn scan<R: Read + Seek>(
    file: &mut R,
    native: &str,
    max_scan: u64,
    deadline: Instant,
) -> io::Result<Scan> {
    let mut pos = file.seek(SeekFrom::End(0))?;
    let mut scanned = 0_u64;
    // The start of a line whose end is in a block that was read before.
    let mut carry: Vec<u8> = Vec::new();
    loop {
        if pos == 0 {
            // `carry` is the first line of the file.
            return Ok(match record_of(&carry, native) {
                Some(Some(t)) => Scan::Found(t),
                Some(None) => Scan::Unreadable,
                None => Scan::NoRecord { whole: true },
            });
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
        let mut buf = vec![0_u8; usize::try_from(n).unwrap_or(usize::MAX)];
        file.read_exact(&mut buf)?;
        buf.extend_from_slice(&carry);
        // The lines after the first newline are complete.
        let mut segments = buf.split(|b| *b == b'\n');
        let first = segments.next().map(<[u8]>::to_vec).unwrap_or_default();
        let complete: Vec<&[u8]> = segments.collect();
        for line in complete.into_iter().rev() {
            match record_of(line, native) {
                Some(Some(t)) => return Ok(Scan::Found(t)),
                Some(None) => return Ok(Scan::Unreadable),
                None => {}
            }
        }
        carry = first;
    }
}

/// The last `cost-state` record of `native` in an open transcript, read from
/// the end in 64 KiB blocks, at most `max_scan` bytes.
/// `Ok(None)`: the scanned part has no record of that session.
///
/// # Errors
/// An I/O error of the read, or a record of the session without totals.
pub fn last_cost_state<R: Read + Seek>(
    file: &mut R,
    native: &str,
    max_scan: u64,
) -> io::Result<Option<ProcessTotals>> {
    let far = Instant::now() + Duration::from_secs(60 * 60 * 24 * 365);
    match scan(file, native, max_scan, far)? {
        Scan::Found(t) => Ok(Some(t)),
        Scan::NoRecord { .. } | Scan::TimedOut => Ok(None),
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
    match scan(file, native, max_scan, deadline) {
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
        let found = last_cost_state(&mut Cursor::new(text.as_bytes()), NATIVE, 1 << 20)
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
        let found = last_cost_state(&mut Cursor::new(text.as_bytes()), NATIVE, 1 << 22)
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
