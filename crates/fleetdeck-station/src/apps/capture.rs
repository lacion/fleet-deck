//! The capture service (B15, design `mcp-apps.md` §4.3 items 3 to 9).
//!
//! A provider core reports a completed MCP tool call as an `AppCandidate`.
//! [`Capture::offer`] queues it. A task resolves the server and the tool (a
//! Claude name through [`ClaudeToolMap`], never by guessing), reads the
//! `ui://` resource through the session's [`AppBridge`], validates the
//! document, sanitizes the CSP and stores the row with its `tool.app` event.
//! Every stop writes at most one `warn` line and stores nothing: the tool call
//! stays a plain row. A candidate lives only in memory; the turn is never held.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::apps::MAX_APP_DOC_BYTES;
use fleetdeck_providers::apps::{
    AppBridge, AppCandidate, AppError, AppTool, ClaudeName, ClaudeToolMap, READ_TIMEOUT,
    TOOLS_TIMEOUT, is_app_mime,
};
use serde_json::Value;
use tokio::time::Instant;

use super::sanitize;
use crate::store::apps::{NewApp, ToolAppEvent};
use crate::store::{Store, StoreError};

/// At most this many captures run at a time on a station.
const MAX_RUNNING: usize = 4;
/// At most this many captures of one session run at a time.
const MAX_PER_SESSION: usize = 2;
/// The candidates that wait for a slot; a full queue drops the newest.
const QUEUE_LIMIT: usize = 32;
/// How long the tools of a session stay cached.
const TOOLS_TTL: Duration = Duration::from_secs(30);
/// The most base64 characters of a document: `ceil(2 MiB / 3) * 4`.
const MAX_BLOB_CHARS: usize = MAX_APP_DOC_BYTES.div_ceil(3) * 4;

/// What the service is doing now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureStats {
    pub running: usize,
    pub queued: usize,
}

/// Why a capture stopped.
#[derive(Debug)]
enum Stop {
    /// The tool has no app, or its name is not one of the session's tools:
    /// the common case of an MCP tool without a UI. Never logged above debug.
    NoApp(&'static str),
    /// The provider has no app path (`AppError::Unsupported`). Debug only.
    Unsupported,
    /// A failure that writes the `warn` line `reason`.
    Warn(&'static str, String),
}

impl Stop {
    fn warn(reason: &'static str, detail: impl std::fmt::Display) -> Self {
        Self::Warn(reason, detail.to_string())
    }

    fn provider(reason: &'static str, error: AppError) -> Self {
        match error {
            AppError::Unsupported => Self::Unsupported,
            other => Self::warn(reason, other),
        }
    }
}

struct Job {
    session_id: SessionId,
    candidate: AppCandidate,
    bridge: Arc<dyn AppBridge>,
}

struct CachedTools {
    at: Instant,
    tools: Arc<Vec<AppTool>>,
}

#[derive(Default)]
struct State {
    running: usize,
    per_session: HashMap<SessionId, usize>,
    queue: VecDeque<Job>,
    tools: HashMap<SessionId, CachedTools>,
}

struct Inner {
    store: Store,
    state: Mutex<State>,
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The capture service of one station. Clones share one queue.
#[derive(Clone)]
pub struct Capture {
    inner: Arc<Inner>,
}

/// A running slot. Dropping it frees the slot and starts the next job, also
/// when the capture task panics.
struct Slot {
    inner: Arc<Inner>,
    session: SessionId,
}

impl Drop for Slot {
    fn drop(&mut self) {
        {
            let mut state = self.inner.state();
            state.running = state.running.saturating_sub(1);
            if let Some(n) = state.per_session.get_mut(&self.session) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    state.per_session.remove(&self.session);
                }
            }
        }
        pump(&self.inner);
    }
}

/// Starts every queued job that the limits admit, oldest first.
fn pump(inner: &Arc<Inner>) {
    loop {
        let job = {
            let mut state = inner.state();
            if state.running >= MAX_RUNNING {
                return;
            }
            let Some(at) = state.queue.iter().position(|job| {
                state.per_session.get(&job.session_id).copied().unwrap_or(0) < MAX_PER_SESSION
            }) else {
                return;
            };
            let Some(job) = state.queue.remove(at) else {
                return;
            };
            state.running += 1;
            *state.per_session.entry(job.session_id.clone()).or_insert(0) += 1;
            job
        };
        let slot = Slot {
            inner: Arc::clone(inner),
            session: job.session_id.clone(),
        };
        let task_inner = Arc::clone(inner);
        tokio::spawn(async move {
            run(&task_inner, job).await;
            drop(slot);
        });
    }
}

async fn run(inner: &Inner, job: Job) {
    let Job {
        session_id,
        candidate,
        bridge,
    } = job;
    match capture(inner, &session_id, &candidate, bridge.as_ref()).await {
        Ok(()) => {}
        Err(Stop::NoApp(why)) => {
            tracing::debug!(session = %session_id, tool = %candidate.tool, why, "no app to capture");
        }
        Err(Stop::Unsupported) => {
            tracing::debug!(session = %session_id, tool = %candidate.tool, "provider has no app path");
        }
        Err(Stop::Warn(reason, detail)) => {
            tracing::warn!(
                session = %session_id,
                server = candidate.server.as_deref().or(candidate.server_hint.as_deref()).unwrap_or(""),
                tool = %candidate.tool,
                uri = candidate.resource_uri.as_deref().unwrap_or(""),
                detail,
                "{reason}"
            );
        }
    }
}

impl Capture {
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// Queues the candidate of `session_id`. It returns at once. At most 4
    /// captures run on the station and 2 per session; 32 candidates wait; a
    /// full queue drops the newest candidate with a warning. Call it inside a
    /// Tokio runtime.
    pub fn offer(
        &self,
        session_id: SessionId,
        candidate: AppCandidate,
        bridge: Arc<dyn AppBridge>,
    ) {
        {
            let mut state = self.inner.state();
            state.queue.push_back(Job {
                session_id: session_id.clone(),
                candidate,
                bridge,
            });
        }
        pump(&self.inner);
        let mut state = self.inner.state();
        if state.queue.len() > QUEUE_LIMIT
            && let Some(dropped) = state.queue.pop_back()
        {
            tracing::warn!(
                session = %dropped.session_id,
                tool = %dropped.candidate.tool,
                "app capture queue is full; the newest candidate is dropped"
            );
        }
    }

    /// The running and the waiting captures.
    #[must_use]
    pub fn stats(&self) -> CaptureStats {
        let state = self.inner.state();
        CaptureStats {
            running: state.running,
            queued: state.queue.len(),
        }
    }
}

/// The server, tool and resource URI of one capture.
struct Resolved {
    server: String,
    tool: String,
    uri: String,
}

async fn capture(
    inner: &Inner,
    session: &SessionId,
    candidate: &AppCandidate,
    bridge: &dyn AppBridge,
) -> Result<(), Stop> {
    let Resolved { server, tool, uri } = resolve(inner, session, candidate, bridge).await?;
    let value = match tokio::time::timeout(READ_TIMEOUT, bridge.read_resource(&server, &uri)).await
    {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return Err(Stop::provider("app_read_failed", error)),
        Err(_) => return Err(Stop::warn("app_read_timeout", "no answer in time")),
    };
    let (html, meta) = document(&value, &uri)?;
    let app = NewApp {
        session_id: session.clone(),
        html,
        csp: sanitize::csp(meta),
        tool_input: candidate.arguments.clone(),
        tool_result: candidate.result.clone(),
    };
    let event = ToolAppEvent {
        node_id: candidate.node_id.clone(),
        server,
        tool,
        resource_uri: uri,
        prefers_border: sanitize::prefers_border(meta),
    };
    inner
        .store
        .insert_app(app, event)
        .await
        .map(|_| ())
        .map_err(|e: StoreError| Stop::warn("app_store_failed", e))
}

/// Item 4: the unique (server, tool) pair and the URI of its app.
async fn resolve(
    inner: &Inner,
    session: &SessionId,
    candidate: &AppCandidate,
    bridge: &dyn AppBridge,
) -> Result<Resolved, Stop> {
    // Codex: the server and the URI come with the candidate.
    if let Some(server) = &candidate.server {
        let Some(uri) = &candidate.resource_uri else {
            return Err(Stop::NoApp("no resource uri"));
        };
        return Ok(Resolved {
            server: server.clone(),
            tool: candidate.tool.clone(),
            uri: uri.clone(),
        });
    }
    // Claude: the unique pair of the full name, from the tools of the session.
    let (mut tools, fresh) = tools_of(inner, session, bridge, false).await?;
    let mut found = ClaudeToolMap::build(&tools).resolve(&candidate.tool);
    if found == ClaudeName::Unknown && !fresh {
        // A miss refreshes the cache once.
        tools = tools_of(inner, session, bridge, true).await?.0;
        found = ClaudeToolMap::build(&tools).resolve(&candidate.tool);
    }
    let (server, tool) = match found {
        ClaudeName::Unique { server, tool } => (server, tool),
        ClaudeName::Ambiguous => {
            return Err(Stop::warn("ambiguous_tool_name", &candidate.tool));
        }
        ClaudeName::Unknown => return Err(Stop::NoApp("unknown tool")),
    };
    if let Some(hint) = &candidate.server_hint
        && *hint != server
    {
        return Err(Stop::warn(
            "server_hint_mismatch",
            format!("hint {hint}, resolved {server}"),
        ));
    }
    let uri = tools
        .iter()
        .find(|t| t.server == server && t.name == tool)
        .and_then(|t| t.resource_uri.clone())
        .ok_or(Stop::NoApp("no resource uri"))?;
    Ok(Resolved { server, tool, uri })
}

/// The tools of a session, cached for 30 seconds. `fresh` is true when this
/// call asked the provider.
async fn tools_of(
    inner: &Inner,
    session: &SessionId,
    bridge: &dyn AppBridge,
    refresh: bool,
) -> Result<(Arc<Vec<AppTool>>, bool), Stop> {
    if !refresh {
        let state = inner.state();
        if let Some(cached) = state.tools.get(session)
            && cached.at.elapsed() < TOOLS_TTL
        {
            return Ok((Arc::clone(&cached.tools), false));
        }
    }
    let list = match tokio::time::timeout(TOOLS_TIMEOUT, bridge.tools()).await {
        Ok(Ok(list)) => Arc::new(list),
        Ok(Err(error)) => return Err(Stop::provider("app_tools_failed", error)),
        Err(_) => return Err(Stop::warn("app_tools_timeout", "no answer in time")),
    };
    let now = Instant::now();
    let mut state = inner.state();
    state
        .tools
        .retain(|_, c| now.duration_since(c.at) < TOOLS_TTL);
    state.tools.insert(
        session.clone(),
        CachedTools {
            at: now,
            tools: Arc::clone(&list),
        },
    );
    Ok((list, true))
}

/// Item 5: the first contents item whose `uri` is absent or `uri` and whose
/// MIME type is the app type; its bytes (size checked before the base64
/// decoding; UTF-8; not empty after trim) and its `_meta`.
fn document<'v>(value: &'v Value, uri: &str) -> Result<(Vec<u8>, Option<&'v Value>), Stop> {
    let item = value
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|item| {
            let uri_ok = match item.get("uri") {
                None => true,
                Some(Value::String(u)) => u == uri,
                Some(_) => false,
            };
            uri_ok
                && item
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .is_some_and(is_app_mime)
        })
        .ok_or_else(|| Stop::warn("app_no_document", "no contents item with the app MIME type"))?;
    let bytes = if let Some(text) = item.get("text").and_then(Value::as_str) {
        if text.len() > MAX_APP_DOC_BYTES {
            return Err(Stop::warn("app_too_large", text.len()));
        }
        text.as_bytes().to_vec()
    } else if let Some(blob) = item.get("blob").and_then(Value::as_str) {
        if blob.len() > MAX_BLOB_CHARS {
            return Err(Stop::warn("app_too_large", blob.len()));
        }
        let decoded = STANDARD
            .decode(blob)
            .map_err(|e| Stop::warn("app_bad_base64", e))?;
        if decoded.len() > MAX_APP_DOC_BYTES {
            return Err(Stop::warn("app_too_large", decoded.len()));
        }
        decoded
    } else {
        return Err(Stop::warn("app_no_document", "no text and no blob"));
    };
    let text = std::str::from_utf8(&bytes).map_err(|e| Stop::warn("app_not_utf8", e))?;
    if text.trim().is_empty() {
        return Err(Stop::warn("app_empty", "the document is empty"));
    }
    Ok((bytes, item.get("_meta")))
}
