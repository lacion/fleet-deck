//! The snapshot of a transcript prefix (DL-189 §3.3).
//!
//! A capture copies the bytes `[0, prefix)` of the provider's file into the
//! station's own file. The copy is staged, synced and renamed. Three hashes
//! must agree: the source range as it was copied, the copy as it was read
//! back, and the source range read again. A difference means that the
//! provider wrote during the copy.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

use fleetdeck_proto::event::ProviderKind;
use ring::digest;

use super::index::hex;
use super::lines::{Line, Lines, ReadError};
use super::parse::{self, Parsed};
use super::{Fail, Limits};

/// The size of one read or write of a copy.
const CHUNK: usize = 1024 * 1024;
/// The attempts of one capture.
const ATTEMPTS: u32 = 2;

/// A snapshot that was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
}

fn deadline_check(deadline: Instant) -> Result<(), Fail> {
    if Instant::now() >= deadline {
        Err(Fail::Busy)
    } else {
        Ok(())
    }
}

/// Feeds the bytes `[0, end)` of `file` to `each`, in chunks.
fn each_chunk(
    file: &File,
    end: u64,
    deadline: Instant,
    mut each: impl FnMut(&[u8]) -> Result<(), Fail>,
) -> Result<(), Fail> {
    let mut buf = vec![0_u8; CHUNK];
    let mut pos = 0_u64;
    while pos < end {
        deadline_check(deadline)?;
        let want = usize::try_from((end - pos).min(CHUNK as u64)).unwrap_or(CHUNK);
        let n = file.read_at(&mut buf[..want], pos).map_err(|_| Fail::Io)?;
        if n == 0 {
            return Err(Fail::Changed);
        }
        each(&buf[..n])?;
        pos += n as u64;
    }
    Ok(())
}

fn hash_range(file: &File, end: u64, deadline: Instant) -> Result<String, Fail> {
    let mut ctx = digest::Context::new(&digest::SHA256);
    each_chunk(file, end, deadline, |chunk| {
        ctx.update(chunk);
        Ok(())
    })?;
    Ok(hex(ctx.finish().as_ref()))
}

/// The size of `file` cut back to the end of its last complete line.
///
/// # Errors
/// The deadline, or a read error.
pub fn prefix_end(file: &File, size: u64, deadline: Instant) -> Result<u64, Fail> {
    let mut end = size;
    let mut buf = vec![0_u8; CHUNK];
    while end > 0 {
        deadline_check(deadline)?;
        let start = end.saturating_sub(CHUNK as u64);
        let len = usize::try_from(end - start).unwrap_or(CHUNK);
        file.read_exact_at(&mut buf[..len], start)
            .map_err(|_| Fail::Io)?;
        if let Some(at) = buf[..len].iter().rposition(|b| *b == b'\n') {
            return Ok(start + at as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

/// The end of the last conversation record whose time is before `cutoff_ms`
/// (a record without a parseable time takes the time of the record before
/// it). Zero when no record qualifies.
///
/// # Errors
/// The deadline, or a read error.
pub fn lazy_boundary(
    file: &File,
    size: u64,
    kind: ProviderKind,
    cutoff_ms: i64,
    limits: &Limits,
    deadline: Instant,
) -> Result<u64, Fail> {
    let mut lines = Lines::new(file, 0, size, limits.line_max).with_deadline(deadline);
    let mut boundary = 0_u64;
    let mut previous: Option<i64> = None;
    loop {
        let next = lines.next_line().map_err(|e| match e {
            ReadError::Deadline => Fail::Busy,
            ReadError::Io(_) => Fail::Io,
        })?;
        let Some((span, line)) = next else { break };
        let (own, complete) = match line {
            Line::TooLong => (None, true),
            Line::Record(bytes) => {
                let own = match parse::parse_line(kind, bytes) {
                    Parsed::Ignored => continue,
                    Parsed::Known(record) => record.at,
                    Parsed::Unknown => parse::line_time(bytes),
                };
                (own, span.len > bytes.len() as u64)
            }
        };
        let time = own.or(previous);
        if own.is_some() {
            previous = own;
        }
        if complete && time.is_some_and(|t| t < cutoff_ms) {
            boundary = span.offset + span.len;
        }
    }
    Ok(boundary)
}

/// Copies `[0, prefix)` of `src` into `dir/<name>.jsonl`. `after_copy` is a
/// test seam: it runs after each copy pass, with the attempt number.
///
/// # Errors
/// [`Fail::Changed`] when the three hashes differ twice, or an I/O error or
/// the deadline.
pub fn copy_prefix(
    src: &File,
    prefix: u64,
    dir: &Path,
    name: &str,
    deadline: Instant,
    after_copy: Option<&(dyn Fn(u32) + Send + Sync)>,
) -> Result<Snapshot, Fail> {
    fleetdeck_core::paths::ensure_private_dir(dir).map_err(|_| Fail::Io)?;
    let final_path = dir.join(format!("{name}.jsonl"));
    let tmp = dir.join(format!("{name}.jsonl.tmp"));
    for attempt in 0..ATTEMPTS {
        match remove_if_there(&tmp) {
            Ok(()) => {}
            Err(_) => return Err(Fail::Io),
        }
        let copied = copy_pass(src, prefix, &tmp, deadline)?;
        if let Some(hook) = after_copy {
            hook(attempt);
        }
        let back = File::open(&tmp).map_err(|_| Fail::Io)?;
        let readback = hash_range(&back, prefix, deadline)?;
        let again = hash_range(src, prefix, deadline);
        let again = match again {
            Ok(h) => h,
            // The source got shorter: it changed.
            Err(Fail::Changed) => String::new(),
            Err(e) => return Err(e),
        };
        if copied == readback && copied == again {
            std::fs::rename(&tmp, &final_path).map_err(|_| Fail::Io)?;
            File::open(dir)
                .and_then(|d| d.sync_all())
                .map_err(|_| Fail::Io)?;
            return Ok(Snapshot {
                path: final_path,
                size: prefix,
                sha256: copied,
            });
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(Fail::Changed)
}

/// Captures the prefix of an already opened transcript into
/// `dir/<name>.jsonl`: the whole file up to its last complete line. The
/// caller opened and checked `file` (B7: a verified import reads through the
/// descriptor that it verified).
///
/// # Errors
/// [`Fail::Io`] for a bad `name`, a read or write error; [`Fail::Changed`]
/// when the file changed during the copy; the deadline.
pub fn capture_file(
    file: &File,
    size: u64,
    dir: &Path,
    name: &str,
    deadline: Instant,
    after_copy: Option<&(dyn Fn(u32) + Send + Sync)>,
) -> Result<Snapshot, Fail> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(Fail::Io);
    }
    let prefix = prefix_end(file, size, deadline)?;
    copy_prefix(file, prefix, dir, name, deadline, after_copy)
}

fn remove_if_there(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Copies the range into `tmp` (mode 0600) and syncs it; returns the hash of
/// the bytes as they were read from the source.
fn copy_pass(src: &File, prefix: u64, tmp: &Path, deadline: Instant) -> Result<String, Fail> {
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)
        .map_err(|_| Fail::Io)?;
    let mut ctx = digest::Context::new(&digest::SHA256);
    each_chunk(src, prefix, deadline, |chunk| {
        ctx.update(chunk);
        out.write_all(chunk).map_err(|_| Fail::Io)
    })?;
    out.sync_all().map_err(|_| Fail::Io)?;
    Ok(hex(ctx.finish().as_ref()))
}
