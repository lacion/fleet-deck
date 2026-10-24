//! Analytics (B29): the parsers of the provider transcripts. The scan, the
//! store and the query come in later tasks (design §3.4).

pub mod claude;
pub mod codex;

#[cfg(test)]
mod claude_tests;
#[cfg(test)]
mod codex_tests;

/// One counted API response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    pub at: i64,
    pub model: String,
    /// Claude `sessionId`; Codex: the thread of the file (the store maps it
    /// to the root thread, §3.5 step 4).
    pub conversation: String,
    /// The text of the dedupe key; `None`: no key (counted without dedupe).
    pub key: Option<String>,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: u64,
}

/// What a parser makes of one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Record(UsageRecord),
    Nothing,
    Malformed,
}

/// The cheap gate of the parsers: does the line hold these bytes?
pub(crate) fn has_bytes(line: &[u8], needle: &[u8]) -> bool {
    let Some((&first, _)) = needle.split_first() else {
        return true;
    };
    let mut from = 0;
    while let Some(at) = line[from..].iter().position(|&b| b == first) {
        let start = from + at;
        if line[start..].starts_with(needle) {
            return true;
        }
        from = start + 1;
    }
    false
}
