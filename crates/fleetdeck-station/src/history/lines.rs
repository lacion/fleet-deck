//! A bounded line reader for transcript files (DL-189 §3.4).
//!
//! `fleetdeck_providers::stdio::read_line_bounded` is private to another
//! crate and leaves a too long line unread. This reader drains such a line
//! up to its newline, so parsing restarts at the next line and never inside a
//! record. It reads from a file by position, hashes the bytes that it
//! consumes, and checks a deadline.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::time::Instant;

use ring::digest;

/// The size of one read from the file.
const CHUNK: usize = 64 * 1024;

/// One line of the file.
#[derive(Debug)]
pub enum Line<'a> {
    /// A line without its newline.
    Record(&'a [u8]),
    /// A line over the cap, drained to its newline.
    TooLong,
}

/// The position of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// The offset of the first byte.
    pub offset: u64,
    /// The bytes of the line, with its newline when it has one.
    pub len: u64,
}

/// Why a read stopped.
#[derive(Debug)]
pub enum ReadError {
    Deadline,
    Io(io::Error),
}

impl From<io::Error> for ReadError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Reads the lines of `[start, end)` of a file.
pub struct Lines<'f> {
    file: &'f File,
    pos: u64,
    end: u64,
    max_line: usize,
    deadline: Option<Instant>,
    hasher: Option<digest::Context>,
    buf: Vec<u8>,
    chunk: Vec<u8>,
    chunk_at: usize,
    chunk_len: usize,
}

impl<'f> Lines<'f> {
    /// A reader of `[start, end)`; a line over `max_line` bytes is drained.
    #[must_use]
    pub fn new(file: &'f File, start: u64, end: u64, max_line: usize) -> Self {
        Self {
            file,
            pos: start,
            end,
            max_line,
            deadline: None,
            hasher: None,
            buf: Vec::new(),
            chunk: vec![0; CHUNK],
            chunk_at: 0,
            chunk_len: 0,
        }
    }

    /// Stops with [`ReadError::Deadline`] after `deadline`.
    #[must_use]
    pub fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Hashes every byte that the reader consumes.
    #[must_use]
    pub fn hashing(mut self) -> Self {
        self.hasher = Some(digest::Context::new(&digest::SHA256));
        self
    }

    /// The offset after the last consumed byte.
    #[must_use]
    pub fn consumed_to(&self) -> u64 {
        self.pos - (self.chunk_len - self.chunk_at) as u64
    }

    /// The SHA-256 of the consumed bytes, if the reader hashes.
    #[must_use]
    pub fn finish(self) -> Option<digest::Digest> {
        self.hasher.map(digest::Context::finish)
    }

    fn refill(&mut self) -> Result<bool, ReadError> {
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(ReadError::Deadline);
        }
        let want = usize::try_from((self.end - self.pos).min(CHUNK as u64)).unwrap_or(CHUNK);
        if want == 0 {
            return Ok(false);
        }
        let n = loop {
            match self.file.read_at(&mut self.chunk[..want], self.pos) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        };
        if n == 0 {
            // The file is shorter than `end`: the range ends here.
            self.end = self.pos;
            return Ok(false);
        }
        self.pos += n as u64;
        self.chunk_at = 0;
        self.chunk_len = n;
        Ok(true)
    }

    /// The next line and its span; `None` at the end of the range. The
    /// returned bytes are valid until the next call.
    ///
    /// # Errors
    /// The deadline, or a read error.
    pub fn next_line(&mut self) -> Result<Option<(Span, Line<'_>)>, ReadError> {
        self.buf.clear();
        let offset = self.consumed_to();
        let mut too_long = false;
        let mut total = 0_u64;
        loop {
            if self.chunk_at == self.chunk_len && !self.refill()? {
                if total == 0 {
                    return Ok(None);
                }
                break;
            }
            let slice = &self.chunk[self.chunk_at..self.chunk_len];
            let (take, found) = match slice.iter().position(|b| *b == b'\n') {
                Some(i) => (i + 1, true),
                None => (slice.len(), false),
            };
            if let Some(hasher) = &mut self.hasher {
                hasher.update(&slice[..take]);
            }
            let body = if found { take - 1 } else { take };
            if !too_long {
                if self.buf.len() + body > self.max_line {
                    too_long = true;
                    self.buf.clear();
                } else {
                    self.buf.extend_from_slice(&slice[..body]);
                }
            }
            total += take as u64;
            self.chunk_at += take;
            if found {
                break;
            }
        }
        let span = Span { offset, len: total };
        if too_long {
            Ok(Some((span, Line::TooLong)))
        } else {
            Ok(Some((span, Line::Record(&self.buf))))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn file_of(bytes: &[u8]) -> File {
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(bytes).unwrap();
        f
    }

    #[test]
    fn it_splits_lines_and_keeps_offsets() {
        let f = file_of(b"ab\ncd\n\nlast");
        let mut lines = Lines::new(&f, 0, 11, 100);
        let mut seen = Vec::new();
        while let Some((span, line)) = lines.next_line().unwrap() {
            let Line::Record(bytes) = line else { panic!() };
            seen.push((span.offset, span.len, bytes.to_vec()));
        }
        assert_eq!(
            seen,
            vec![
                (0, 3, b"ab".to_vec()),
                (3, 3, b"cd".to_vec()),
                (6, 1, b"".to_vec()),
                (7, 4, b"last".to_vec()),
            ]
        );
    }

    #[test]
    fn it_drains_a_too_long_line_and_goes_on_with_the_next_one() {
        let mut bytes = vec![b'x'; 300_000];
        bytes.push(b'\n');
        bytes.extend_from_slice(b"next\n");
        let f = file_of(&bytes);
        let mut lines = Lines::new(&f, 0, bytes.len() as u64, 1000);
        let (span, line) = lines.next_line().unwrap().unwrap();
        assert!(matches!(line, Line::TooLong));
        assert_eq!(span.len, 300_001);
        let (span, line) = lines.next_line().unwrap().unwrap();
        assert_eq!(span.offset, 300_001);
        assert!(matches!(line, Line::Record(b"next")));
        assert!(lines.next_line().unwrap().is_none());
    }

    #[test]
    fn it_hashes_every_consumed_byte_and_stops_at_the_range_end() {
        let f = file_of(b"one\ntwo\nthree\n");
        let mut lines = Lines::new(&f, 0, 8, 100).hashing();
        while lines.next_line().unwrap().is_some() {}
        assert_eq!(lines.consumed_to(), 8);
        let digest = lines.finish().unwrap();
        let expect = digest::digest(&digest::SHA256, b"one\ntwo\n");
        assert_eq!(digest.as_ref(), expect.as_ref());
    }
}
