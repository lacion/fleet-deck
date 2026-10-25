//! The scrollback ring of one shell (PLAN §6.8): at most
//! [`SCROLLBACK_LINES`] lines and [`SCROLLBACK_BYTES`] bytes. The oldest
//! bytes go first; a cut by lines drops whole lines.

use std::collections::VecDeque;

use fleetdeck_proto::shell::{SCROLLBACK_BYTES, SCROLLBACK_LINES};

#[derive(Debug)]
pub struct Scrollback {
    buf: VecDeque<u8>,
    newlines: usize,
    max_lines: usize,
    max_bytes: usize,
}

impl Default for Scrollback {
    fn default() -> Self {
        Self::with_limits(SCROLLBACK_LINES, SCROLLBACK_BYTES)
    }
}

impl Scrollback {
    /// A ring with other limits (tests). `max_lines` counts the lines that
    /// the ring shows, the last unfinished line included.
    #[must_use]
    pub fn with_limits(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            buf: VecDeque::new(),
            newlines: 0,
            max_lines: max_lines.max(1),
            max_bytes,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes);
        self.newlines += bytes.iter().filter(|b| **b == b'\n').count();
        if self.buf.len() > self.max_bytes {
            let cut = self.buf.len() - self.max_bytes;
            let dropped = self.buf.drain(..cut).filter(|b| *b == b'\n').count();
            self.newlines -= dropped;
        }
        // N newlines show N + 1 lines (the last one can be empty).
        while self.newlines >= self.max_lines {
            let Some(end) = self.buf.iter().position(|b| *b == b'\n') else {
                break;
            };
            self.buf.drain(..=end);
            self.newlines -= 1;
        }
    }

    /// A copy of the kept bytes, oldest first.
    #[must_use]
    pub fn snapshot(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The lines that the ring shows.
    #[must_use]
    pub fn lines(&self) -> usize {
        self.newlines + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scrollback_keeps_at_most_5000_lines_and_8_mib() {
        let mut ring = Scrollback::default();
        for i in 0..6_000 {
            ring.push(format!("line {i}\n").as_bytes());
        }
        assert_eq!(ring.lines(), SCROLLBACK_LINES);
        let text = String::from_utf8(ring.snapshot()).expect("utf8");
        assert!(text.starts_with("line 1001\n"), "{}", &text[..20]);
        assert!(text.ends_with("line 5999\n"));

        let mut big = Scrollback::default();
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..10 {
            big.push(&chunk);
        }
        assert_eq!(big.len(), SCROLLBACK_BYTES);
    }

    #[test]
    fn a_byte_cut_keeps_the_line_count_right() {
        let mut ring = Scrollback::with_limits(3, 8);
        ring.push(b"ab\ncd\nef\ngh");
        // The byte cut drops "ab\n"; three lines stay.
        assert_eq!(ring.snapshot(), b"cd\nef\ngh".to_vec());
        assert_eq!(ring.lines(), 3);
        ring.push(b"\nij\n");
        assert_eq!(ring.snapshot(), b"gh\nij\n".to_vec());
        assert_eq!(ring.lines(), 3);
    }
}
