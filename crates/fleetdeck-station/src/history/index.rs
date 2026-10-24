//! The index of a snapshot, its cache and the page reads (DL-189 §3.5).
//!
//! One scan of a snapshot gives, for each item, its byte offset, the length
//! of its record, its time, its kind and its tool status; no text. A page
//! reads only its own records again, by position. The scan also checks that
//! the snapshot has the stored size and SHA-256.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::{
    HistoryItem, HistoryKind, HistoryState, HistoryTool, HistoryUnavailable, SessionHistory,
    ToolResult,
};

use super::lines::{Line, Lines, ReadError};
use super::parse::{self, Parsed};
use super::{Fail, Limits};

/// The tool calls that wait for their result during a scan, at most.
const PENDING_MAX: usize = 100_000;
/// The room of the answer envelope and the cursor, in bytes.
const ENVELOPE_BYTES: usize = 256;

/// One item of the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub offset: u64,
    /// The bytes of the record, without its newline.
    pub len: u32,
    pub at: i64,
    pub kind: HistoryKind,
    /// The place of the item inside its record.
    pub sub: u16,
    pub status: ToolResult,
}

/// The index of one snapshot.
#[derive(Debug)]
pub struct Index {
    /// The newest items, oldest first.
    pub entries: Vec<Entry>,
    /// The older items that the cap left out.
    pub dropped: u64,
    /// The records that the parser left out.
    pub skipped: u64,
    /// The records of a known shape.
    pub recognized: u64,
    /// The snapshot size in bytes.
    pub size: u64,
}

impl Index {
    /// The memory that the index holds, about.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.entries.len() * std::mem::size_of::<Entry>() + 64
    }

    /// The number of items from the start of the snapshot.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.dropped + self.entries.len() as u64
    }
}

/// Scans a snapshot. The snapshot must have `size` bytes and the SHA-256
/// `sha`, else the answer is `not_trusted`.
///
/// # Errors
/// [`Fail`].
pub fn build(
    file: &File,
    kind: ProviderKind,
    size: u64,
    sha: &str,
    limits: &Limits,
    deadline: Instant,
    delay: Option<Duration>,
) -> Result<Index, Fail> {
    let on_disk = file.metadata().map_err(|_| Fail::Io)?.len();
    if on_disk != size {
        return Err(Fail::Unavailable(HistoryUnavailable::NotTrusted));
    }
    if let Some(delay) = delay {
        std::thread::sleep(delay);
    }
    let mut lines = Lines::new(file, 0, size, limits.line_max)
        .with_deadline(deadline)
        .hashing();
    let mut entries: VecDeque<Entry> = VecDeque::new();
    let (mut dropped, mut skipped, mut recognized) = (0_u64, 0_u64, 0_u64);
    let mut pending: HashMap<String, u64> = HashMap::new();
    let mut last_at: i64 = 0;
    loop {
        let next = lines.next_line().map_err(fail_of)?;
        let Some((span, line)) = next else { break };
        let bytes = match line {
            Line::TooLong => {
                skipped += 1;
                continue;
            }
            Line::Record(bytes) => bytes,
        };
        let record = match parse::parse_line(kind, bytes) {
            Parsed::Ignored => continue,
            Parsed::Unknown => {
                skipped += 1;
                continue;
            }
            Parsed::Known(record) => record,
        };
        recognized += 1;
        if record.partial {
            skipped += 1;
        }
        if let Some(at) = record.at {
            last_at = at;
        }
        for (call_id, status) in &record.results {
            if let Some(abs) = pending.remove(call_id)
                && let Some(pos) = abs.checked_sub(dropped)
                && let Some(entry) = entries.get_mut(usize::try_from(pos).unwrap_or(usize::MAX))
            {
                entry.status = *status;
            }
        }
        // A record over the page budget cannot be read for a page.
        let len = bytes.len();
        if len > limits.page_read_max {
            if !record.items.is_empty() {
                skipped += 1;
            }
            continue;
        }
        for (sub, item) in record.items.iter().enumerate() {
            let Ok(sub) = u16::try_from(sub) else { break };
            let abs = dropped + entries.len() as u64;
            if let Some(tool) = &item.tool
                && !tool.call_id.is_empty()
                && pending.len() < PENDING_MAX
            {
                pending.insert(tool.call_id.clone(), abs);
            }
            entries.push_back(Entry {
                offset: span.offset,
                len: u32::try_from(len).unwrap_or(u32::MAX),
                at: last_at,
                kind: item.kind,
                sub,
                status: item
                    .tool
                    .as_ref()
                    .and_then(|t| t.status)
                    .unwrap_or(ToolResult::Unknown),
            });
            if entries.len() > limits.index_items {
                entries.pop_front();
                dropped += 1;
            }
        }
    }
    if lines.consumed_to() != size {
        return Err(Fail::Unavailable(HistoryUnavailable::NotTrusted));
    }
    let digest = lines.finish().map(|d| hex(d.as_ref())).unwrap_or_default();
    if digest != sha {
        return Err(Fail::Unavailable(HistoryUnavailable::NotTrusted));
    }
    Ok(Index {
        entries: entries.into(),
        dropped,
        skipped,
        recognized,
        size,
    })
}

fn fail_of(e: ReadError) -> Fail {
    match e {
        ReadError::Deadline => Fail::Busy,
        ReadError::Io(_) => Fail::Io,
    }
}

/// Lowercase hex of `bytes`.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The cache key of an index: the session and the snapshot hash.
pub type Key = (String, String);

/// The indexes in memory: least recently used first.
#[derive(Debug, Default)]
pub struct Cache {
    items: Vec<(Key, Arc<Index>)>,
}

impl Cache {
    /// The index of `key`, now the most recently used.
    pub fn get(&mut self, key: &Key) -> Option<Arc<Index>> {
        let at = self.items.iter().position(|(k, _)| k == key)?;
        let entry = self.items.remove(at);
        let index = Arc::clone(&entry.1);
        self.items.push(entry);
        Some(index)
    }

    /// Adds an index and drops the least recently used ones that exceed the
    /// limits. An index over the byte limit alone is not kept.
    pub fn put(&mut self, key: Key, index: Arc<Index>, limits: &Limits) {
        self.items.retain(|(k, _)| *k != key);
        if index.bytes() > limits.cache_bytes {
            return;
        }
        self.items.push((key, index));
        while self.items.len() > limits.cache_entries
            || self.items.iter().map(|(_, i)| i.bytes()).sum::<usize>() > limits.cache_bytes
        {
            self.items.remove(0);
        }
    }

    /// Forgets the index of `key`.
    pub fn remove(&mut self, key: &Key) {
        self.items.retain(|(k, _)| k != key);
    }

    /// The number of indexes in memory.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when no index is in memory.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// What a page asks for.
pub struct PageQuery<'a> {
    pub kind: ProviderKind,
    /// The first 16 hex digits of the snapshot hash.
    pub hash16: &'a str,
    /// Items with an absolute number below this one; `None` for the newest.
    pub before: Option<u64>,
    pub limit: usize,
    pub member: bool,
}

/// The state of an answer for `index`.
#[must_use]
pub fn state_of(index: &Index) -> HistoryState {
    let skipped = index.skipped.saturating_add(index.dropped);
    if index.entries.is_empty() && index.dropped == 0 {
        if index.recognized == 0 && index.size > 0 {
            return HistoryState::Unavailable {
                reason: HistoryUnavailable::UnsupportedFormat,
            };
        }
        // Skipped records with no item left are not an empty history.
        if skipped == 0 {
            return HistoryState::Empty;
        }
    }
    if skipped > 0 {
        HistoryState::Partial {
            skipped: u32::try_from(skipped).unwrap_or(u32::MAX),
        }
    } else {
        HistoryState::Complete
    }
}

/// Builds one page: the newest items before the cursor.
///
/// # Errors
/// [`Fail`]: a cursor past the end, or a record that no longer reads as the
/// index says.
pub fn page(
    file: &File,
    index: &Index,
    query: &PageQuery<'_>,
    limits: &Limits,
) -> Result<SessionHistory, Fail> {
    let total = index.total();
    let end_abs = query.before.unwrap_or(total);
    if end_abs > total {
        return Err(Fail::Invalid("the cursor is past the end of the history"));
    }
    let end_pos = usize::try_from(end_abs.saturating_sub(index.dropped)).unwrap_or(0);
    let mut picked: Vec<HistoryItem> = Vec::new();
    let mut oldest_pos = end_pos;
    let mut wire = ENVELOPE_BYTES;
    let mut read_bytes = 0_usize;
    let mut cache: Option<(u64, parse::Record)> = None;
    let mut pos = end_pos;
    while pos > 0 && picked.len() < query.limit {
        pos -= 1;
        let entry = index.entries[pos];
        if cache.as_ref().map(|(o, _)| *o) != Some(entry.offset) {
            read_bytes += entry.len as usize;
            if read_bytes > limits.page_read_max && !picked.is_empty() {
                break;
            }
            let mut buf = vec![0_u8; entry.len as usize];
            file.read_exact_at(&mut buf, entry.offset)
                .map_err(|_| Fail::Unavailable(HistoryUnavailable::NotTrusted))?;
            match parse::parse_line(query.kind, &buf) {
                Parsed::Known(record) => cache = Some((entry.offset, record)),
                _ => return Err(Fail::Unavailable(HistoryUnavailable::NotTrusted)),
            }
        }
        let Some((_, record)) = &cache else { continue };
        let Some(raw) = record.items.get(usize::from(entry.sub)) else {
            return Err(Fail::Unavailable(HistoryUnavailable::NotTrusted));
        };
        let base = raw
            .id
            .clone()
            .unwrap_or_else(|| format!("line:{}", entry.offset));
        let id = if entry.sub == 0 {
            base
        } else {
            format!("{base}#{}", entry.sub)
        };
        let item = HistoryItem {
            id: parse::cut_bytes(&id, 128),
            at: entry.at,
            kind: entry.kind,
            // A member gets no tool text, as with the summary (B8).
            text: if query.member && entry.kind == HistoryKind::Tool {
                String::new()
            } else {
                parse::cut_text(&raw.text, 8 * 1024)
            },
            tool: raw.tool.as_ref().map(|t| HistoryTool {
                name: parse::cut_chars(&t.name, 64),
                summary: if query.member {
                    String::new()
                } else {
                    parse::cut_chars(&t.summary, 200)
                },
                status: entry.status,
            }),
        };
        let size = serde_json::to_vec(&item).map_or(0, |v| v.len()) + 1;
        if wire + size > limits.answer_max && !picked.is_empty() {
            break;
        }
        wire += size;
        picked.push(item);
        oldest_pos = pos;
    }
    picked.reverse();
    let before = if picked.is_empty() || oldest_pos == 0 {
        None
    } else {
        Some(format!(
            "{}:{}",
            query.hash16,
            index.dropped + oldest_pos as u64
        ))
    };
    Ok(SessionHistory {
        items: picked,
        before,
        state: state_of(index),
    })
}
