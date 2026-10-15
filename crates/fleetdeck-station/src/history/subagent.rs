//! The log of one subagent (DL-191, station part).
//!
//! Two parts:
//!
//! - [`observe`] binds a subagent tool use to its transcript. When a running
//!   process reports the result of an `Agent` or `Task` call with an agent id
//!   (`tool.completed` with `provider_refs.agent_id`), the station stores one
//!   row with the launch facts of that process ([`LaunchFacts`]).
//! - [`answer`] serves `session.subagent` from that row alone. The caller
//!   gives a session and a tool use, never an agent id or a path. The file is
//!   opened without following a link, read once up to the size captured at
//!   open, and every record must name the parent session and the agent.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use fleetdeck_proto::command::check_native_session_id;
use fleetdeck_proto::event::{Event, ProviderKind};
use fleetdeck_proto::history::{
    HistoryItem, HistoryState, HistoryTool, HistoryUnavailable, ToolResult, code,
};
use fleetdeck_proto::subagent::{MAX_ANSWER_BYTES, MAX_ITEMS, SubagentLog, SubagentQuery};
use fleetdeck_proto::{ProviderInstanceId, SessionId};

use super::lines::{Line, Lines, ReadError};
use super::parse::{self, Parsed};
use super::{Fail, History, HistoryError, Limits, locate};
use crate::manager::Policy;
use crate::store::Store;
use crate::store::subagent_logs::SubagentLogRow;
use crate::workspace::PrivatePaths;

/// The room of the answer envelope, in bytes.
const ENVELOPE_BYTES: usize = 64;
/// The tool calls that wait for their result during a read, at most.
const PENDING_MAX: usize = 10_000;

// ------------------------------------------------------------- binding

/// What the launch of a process says about where its provider writes: the
/// absolute provider home and the working directory that the launch used.
/// The native session id is read from the store when a row is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchFacts {
    pub provider_home: PathBuf,
    pub cwd: String,
}

impl LaunchFacts {
    /// The facts of a launch of `instance`, from the policy snapshot of the
    /// launch. `None` when the launch environment gives no absolute home.
    #[must_use]
    pub fn of(
        policy: Option<&Policy>,
        instance: &ProviderInstanceId,
        kind: ProviderKind,
        cwd: &str,
    ) -> Option<Self> {
        let env = super::env_for(policy, instance);
        let provider_home = locate::resolve_home(&env, kind).filter(|h| h.is_absolute())?;
        Some(Self {
            provider_home,
            cwd: cwd.to_owned(),
        })
    }
}

/// True for an agent id of the Claude stream: `[a-z0-9]{1,64}`.
fn agent_id_ok(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// True for a tool use id: `[A-Za-z0-9_-]{1,128}`.
#[must_use]
pub fn tool_use_id_ok(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// The provider tool use id of `node_id`: from the `tool.started` of the
/// same batch first, else from the session's events in the store.
async fn tool_use_of(
    store: &Store,
    session: &SessionId,
    observed: &[Event],
    node_id: &fleetdeck_proto::NodeId,
) -> Option<String> {
    let in_batch = observed.iter().find_map(|e| match e {
        Event::ToolStarted {
            node_id: n,
            provider_refs,
            ..
        } if n == node_id => provider_refs
            .get("tool_use_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        _ => None,
    });
    if in_batch.is_some() {
        return in_batch;
    }
    match store
        .tool_use_of_node(session.clone(), node_id.as_str().to_owned())
        .await
    {
        Ok(found) => found,
        Err(e) => {
            tracing::warn!(error = %e, session = %session, "subagent binding: tool lookup");
            None
        }
    }
}

/// Stores a binding row for each `tool.completed` of `observed` that carries
/// an agent id. Without launch facts (an adopted process) it stores nothing.
/// A store error is logged: a binding never stops the pump of the session.
pub async fn observe(
    store: &Store,
    session: &SessionId,
    launch: Option<&LaunchFacts>,
    observed: &[Event],
) {
    for event in observed {
        let Event::ToolCompleted {
            node_id,
            provider_refs,
            ..
        } = event
        else {
            continue;
        };
        let Some(agent_id) = provider_refs
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| agent_id_ok(id))
        else {
            continue;
        };
        let Some(launch) = launch else {
            tracing::debug!(session = %session, "subagent binding: no launch facts, no row");
            continue;
        };
        let Some(provider_home) = launch.provider_home.to_str() else {
            tracing::debug!(session = %session, "subagent binding: the home is not UTF-8");
            continue;
        };
        let Some(tool_use_id) = tool_use_of(store, session, observed, node_id)
            .await
            .filter(|id| tool_use_id_ok(id))
        else {
            tracing::debug!(session = %session, "subagent binding: no tool use id");
            continue;
        };
        let native_id = match store.session_native_id(session.clone()).await {
            Ok(Some(native)) if check_native_session_id(&native).is_ok() => native,
            Ok(_) => {
                tracing::debug!(session = %session, "subagent binding: no native id, no row");
                continue;
            }
            Err(e) => {
                tracing::warn!(error = %e, session = %session, "subagent binding: native id");
                continue;
            }
        };
        let row = SubagentLogRow {
            session_id: session.clone(),
            tool_use_id,
            agent_id: agent_id.to_owned(),
            native_id,
            cwd: launch.cwd.clone(),
            provider_home: provider_home.to_owned(),
            created_at: store.now_ms(),
        };
        if let Err(e) = store.put_subagent_log(row).await {
            tracing::warn!(error = %e, session = %session, "subagent binding not stored");
        }
    }
}

// -------------------------------------------------------------- answer

fn unavailable(reason: HistoryUnavailable) -> SubagentLog {
    SubagentLog {
        items: Vec::new(),
        state: HistoryState::Unavailable { reason },
    }
}

fn invalid(why: &str) -> HistoryError {
    HistoryError::new(code::INVALID_PARAMS, why)
}

/// `session.subagent`.
///
/// # Errors
/// [`HistoryError`] with `invalid_params`, `unknown_session`, `busy` or
/// `internal`. `unavailable` and `partial` are answers.
pub async fn answer(
    store: &Store,
    history: &History,
    private: &Arc<PrivatePaths>,
    query: SubagentQuery,
) -> Result<SubagentLog, HistoryError> {
    if !tool_use_id_ok(&query.tool_use_id) {
        return Err(invalid("tool_use_id must be 1 to 128 of A-Z a-z 0-9 _ -"));
    }
    let session_id = query.session_id;
    let facts = store
        .history_facts(session_id.clone())
        .await
        .map_err(super::internal)?
        .ok_or_else(|| HistoryError::new(code::UNKNOWN_SESSION, "no such session"))?;
    if facts.provider != ProviderKind::Claude {
        return Ok(unavailable(HistoryUnavailable::UnsupportedFormat));
    }
    let row = store
        .subagent_log(session_id, query.tool_use_id)
        .await
        .map_err(super::internal)?;
    let Some(row) = row else {
        return Ok(unavailable(HistoryUnavailable::NotFound));
    };
    // The row comes from this station, but the names become path segments:
    // check them again.
    let home = PathBuf::from(&row.provider_home);
    if !home.is_absolute()
        || !agent_id_ok(&row.agent_id)
        || check_native_session_id(&row.native_id).is_err()
    {
        return Ok(unavailable(HistoryUnavailable::NotTrusted));
    }
    let deadline = Instant::now() + history.limits().deadline;
    let this = history.clone();
    let private = Arc::clone(private);
    let worked = history
        .run(deadline, move |deadline| {
            this.read_subagent(&row, home, &private, deadline)
        })
        .await;
    match worked {
        Ok(log) => Ok(log),
        Err(Fail::Busy) => Err(super::busy()),
        Err(Fail::Unavailable(reason)) => Ok(unavailable(reason)),
        Err(fail) => Err(super::internal(format!("subagent log: {fail:?}"))),
    }
}

impl History {
    /// The blocking part of [`answer`]: open the file and read it once.
    fn read_subagent(
        &self,
        row: &SubagentLogRow,
        home: PathBuf,
        private: &PrivatePaths,
        deadline: Instant,
    ) -> Result<SubagentLog, Fail> {
        let limits = self.limits();
        let trust = locate::Trust {
            private,
            uid: self.uid(),
            limits,
            deadline,
        };
        let opened =
            locate::locate_subagent(&home, &row.cwd, &row.native_id, &row.agent_id, &trust)?;
        read_log(
            &opened.file,
            opened.size,
            &row.native_id,
            &row.agent_id,
            limits,
            deadline,
        )
    }
}

fn fail_of(e: ReadError) -> Fail {
    match e {
        ReadError::Deadline => Fail::Busy,
        ReadError::Io(_) => Fail::Io,
    }
}

/// The newest items of a read, with the counts that the state needs.
#[derive(Default)]
struct Ring {
    items: VecDeque<HistoryItem>,
    /// Items that left the ring (older than the newest `MAX_ITEMS`).
    dropped: u64,
    /// Call id of a tool item → its absolute number.
    pending: HashMap<String, u64>,
    skipped: u64,
    recognized: u64,
    last_at: i64,
}

impl Ring {
    fn record(&mut self, record: parse::Record, offset: u64) {
        self.recognized += 1;
        if record.partial {
            self.skipped += 1;
        }
        if let Some(at) = record.at {
            self.last_at = at;
        }
        for (call_id, status) in &record.results {
            if let Some(abs) = self.pending.remove(call_id)
                && let Some(pos) = abs.checked_sub(self.dropped)
                && let Some(item) = self
                    .items
                    .get_mut(usize::try_from(pos).unwrap_or(usize::MAX))
                && let Some(tool) = item.tool.as_mut()
            {
                tool.status = *status;
            }
        }
        for (sub, raw) in record.items.iter().enumerate() {
            let abs = self.dropped + self.items.len() as u64;
            if let Some(tool) = &raw.tool
                && !tool.call_id.is_empty()
                && self.pending.len() < PENDING_MAX
            {
                self.pending.insert(tool.call_id.clone(), abs);
            }
            let base = raw.id.clone().unwrap_or_else(|| format!("line:{offset}"));
            let id = if sub == 0 {
                base
            } else {
                format!("{base}#{sub}")
            };
            self.items.push_back(HistoryItem {
                id: parse::cut_bytes(&id, 128),
                at: self.last_at,
                kind: raw.kind,
                text: parse::cut_text(&raw.text, 8 * 1024),
                tool: raw.tool.as_ref().map(|t| HistoryTool {
                    name: parse::cut_chars(&t.name, 64),
                    summary: parse::cut_chars(&t.summary, 200),
                    status: ToolResult::Unknown,
                }),
            });
            if self.items.len() > MAX_ITEMS {
                self.items.pop_front();
                self.dropped += 1;
            }
        }
    }
}

/// Reads the captured extent `[0, size)` of an open subagent transcript once.
///
/// # Errors
/// [`Fail::Unavailable`] with `not_trusted` when a record names another
/// session or agent; the deadline; a read error.
pub(super) fn read_log(
    file: &File,
    size: u64,
    native: &str,
    agent_id: &str,
    limits: &Limits,
    deadline: Instant,
) -> Result<SubagentLog, Fail> {
    let mut lines = Lines::new(file, 0, size, limits.line_max).with_deadline(deadline);
    let mut ring = Ring::default();
    while let Some((span, line)) = lines.next_line().map_err(fail_of)? {
        let bytes = match line {
            Line::TooLong => {
                ring.skipped += 1;
                continue;
            }
            Line::Record(bytes) => bytes,
        };
        // A last line without its newline is not a record yet.
        if span.len <= bytes.len() as u64 {
            continue;
        }
        match parse::parse_line_subagent(bytes, native, agent_id) {
            Err(parse::NotOurs) => {
                return Err(Fail::Unavailable(HistoryUnavailable::NotTrusted));
            }
            Ok(Parsed::Ignored) => {}
            Ok(Parsed::Unknown) => ring.skipped += 1,
            Ok(Parsed::Known(record)) => ring.record(record, span.offset),
        }
    }
    // The file ended before the captured size (it shrank).
    if lines.consumed_to() < size {
        ring.skipped += 1;
    }
    Ok(finish(ring, size))
}

/// The answer of a ring: the serialized size is cut to the cap by dropping
/// the oldest items.
fn finish(ring: Ring, size: u64) -> SubagentLog {
    let Ring {
        items,
        mut dropped,
        skipped,
        recognized,
        ..
    } = ring;
    let mut items: VecDeque<HistoryItem> = items;
    let mut sizes: VecDeque<usize> = items
        .iter()
        .map(|i| serde_json::to_vec(i).map_or(0, |v| v.len()) + 1)
        .collect();
    let mut total = ENVELOPE_BYTES + sizes.iter().sum::<usize>();
    while total > MAX_ANSWER_BYTES && items.pop_front().is_some() {
        total -= sizes.pop_front().unwrap_or(0);
        dropped += 1;
    }
    // The exact check on the whole answer; normally it drops nothing.
    let mut log = SubagentLog {
        items: items.into(),
        state: HistoryState::Complete,
    };
    while serde_json::to_vec(&log).map_or(0, |v| v.len()) > MAX_ANSWER_BYTES
        && !log.items.is_empty()
    {
        log.items.remove(0);
        dropped += 1;
    }
    log.state = state_of(log.items.is_empty(), dropped, skipped, recognized, size);
    log
}

fn state_of(empty: bool, dropped: u64, skipped: u64, recognized: u64, size: u64) -> HistoryState {
    let skipped_all = skipped.saturating_add(dropped);
    if empty && dropped == 0 {
        if recognized == 0 && size > 0 {
            return HistoryState::Unavailable {
                reason: HistoryUnavailable::UnsupportedFormat,
            };
        }
        if skipped == 0 {
            return HistoryState::Empty;
        }
    }
    if skipped_all > 0 {
        HistoryState::Partial {
            skipped: u32::try_from(skipped_all).unwrap_or(u32::MAX),
        }
    } else {
        HistoryState::Complete
    }
}
