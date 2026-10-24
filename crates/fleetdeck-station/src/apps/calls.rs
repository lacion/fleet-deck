//! App tool calls and their workspace protection (B15, design
//! `docs/v2/design/mcp-apps.md` §4.6 and §4.6.1).
//!
//! An app tool call can change the files of the session's folder at any time,
//! outside a turn, like a `!` shell command (B8). Every `tools/call` is a
//! durable row of `mcp_app_calls`, inserted under the workspace fence; a
//! non-read-only call advances the folder generation at its start and after
//! its settlement. The call task awaits the provider (no host timeout) and
//! settles the row by the outcome table of §4.6.1. **Only** two answers prove
//! that a tool stopped: a [`ToolAnswer`] with `proves_end` and
//! [`AppError::Ended`]. A provider error proves nothing: the row becomes
//! `uncertain` and keeps the revert barrier until an end is proven (the end
//! proof of §4.6.1: `AppError::Ended`, `process_ended`, [`ProcessExits`], a
//! shim proof) or the owner releases it.
//!
//! The five link methods of the apps use [`app_call`], [`app_calls`] and
//! [`app_release`] with an [`AppEnv`]; `app_meta` and `app_document` are
//! store reads in `link/intake_apps.rs`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError};
use std::time::Duration;

use fleetdeck_core::{SharedIds, new_id};
use fleetdeck_proto::SessionId;
use fleetdeck_proto::apps::{
    AppCallParams, AppCallReply, AppCallState, AppCallsReply, AppOpenCall, AppReleaseReply,
    AppRequest, MAX_APP_ARGS_BYTES, MAX_APP_REPLY_BYTES,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::apps::{AppError, CALL_REPLY_WAIT, ClaudeToolMap, ToolAnswer, json_len};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};
use tokio::time::Instant;

use crate::manager::target::AppHost;
use crate::store::app_calls::{
    AppCallEnd, AppCallRow, CallState, NewAppCall, Release, UncertainCause,
};
use crate::store::apps::AppLookup;
use crate::store::resumes::proves_end;
use crate::store::{Result as StoreResult, Store};

/// How long a call start waits for the workspace fence.
pub const FENCE_WAIT: Duration = Duration::from_secs(5);
/// The most call tasks of one session that run in this station process.
pub const MAX_TASKS_PER_SESSION: usize = 2;
/// The most calls of one session in one minute.
pub const CALLS_PER_MINUTE: usize = 30;
/// The most `uncertain` rows of one session before a new `tools/call` is busy.
pub const MAX_UNCERTAIN_PER_SESSION: u32 = 8;
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// The longest provider text in an error message.
const ERROR_TEXT_CHARS: usize = 300;

/// The error codes of the app methods (design §4.6 and §4.7).
pub mod code {
    pub const UNKNOWN_APP: &str = "unknown_app";
    pub const APP_GONE: &str = "app_gone";
    pub const SESSION_STOPPED: &str = "session_stopped";
    pub const BUSY: &str = "busy";
    pub const INVALID_PARAMS: &str = "invalid_params";
    pub const TOOL_NOT_CALLABLE: &str = "tool_not_callable";
    pub const TOO_LARGE: &str = "too_large";
    pub const TIMEOUT: &str = "timeout";
    pub const PROVIDER_ERROR: &str = "provider_error";
    pub const CALL_RUNNING: &str = "call_running";
    pub const UNKNOWN_CALL: &str = "unknown_call";
    pub const INTERNAL: &str = "internal";
}

/// A refused or failed app request: a code of [`code`] and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppFail {
    pub code: &'static str,
    pub message: String,
}

impl AppFail {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AppFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

fn internal(e: impl std::fmt::Display) -> AppFail {
    tracing::error!(error = %e, "app call store failure");
    AppFail::new(code::INTERNAL, e.to_string())
}

/// The process exits (`HostEnd::Exited`) that this station process observed.
/// Never filled by a detach. One entry per observed exit, kept for the life
/// of the station process (a session id and a process text; no bound).
#[derive(Debug, Default)]
pub struct ProcessExits {
    seen: StdMutex<HashSet<(SessionId, String)>>,
}

impl ProcessExits {
    fn set(&self) -> MutexGuard<'_, HashSet<(SessionId, String)>> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `drop_app_host(.., HostEnd::Exited)` calls it before `process_ended`.
    pub fn record(&self, session: &SessionId, process: &str) {
        self.set().insert((session.clone(), process.to_owned()));
    }

    /// `true` only when `record` ran for exactly this session and process text.
    #[must_use]
    pub fn has(&self, session: &SessionId, process: &str) -> bool {
        self.set().contains(&(session.clone(), process.to_owned()))
    }
}

/// The test hook of the call task. Outside tests the struct is empty.
#[derive(Debug, Default, Clone)]
pub struct CallHooks {
    /// `(reached, resume)`: the task calls `reached.notify_one()` right after
    /// `call_tool` returned, then waits for `resume` before any settlement.
    #[cfg(test)]
    pub(crate) before_record: Option<(Arc<Notify>, Arc<Notify>)>,
}

impl CallHooks {
    #[cfg(test)]
    async fn pause(&self) {
        if let Some((reached, resume)) = &self.before_record {
            reached.notify_one();
            resume.notified().await;
        }
    }

    #[cfg(not(test))]
    #[allow(clippy::unused_async)]
    async fn pause(&self) {}
}

#[derive(Debug, Default)]
struct SessionLimit {
    tasks: usize,
    starts: VecDeque<Instant>,
}

/// The call tasks and the call rate of every session (this station process).
#[derive(Debug, Default)]
pub struct CallLimits {
    sessions: StdMutex<HashMap<SessionId, SessionLimit>>,
}

impl CallLimits {
    fn map(&self) -> MutexGuard<'_, HashMap<SessionId, SessionLimit>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A permit for one call task, or `None` when the session has
    /// [`MAX_TASKS_PER_SESSION`] tasks or made [`CALLS_PER_MINUTE`] calls in
    /// the last minute. The permit is held until its task ends.
    #[must_use]
    pub fn acquire(self: &Arc<Self>, session: &SessionId) -> Option<Permit> {
        let now = Instant::now();
        let mut map = self.map();
        let entry = map.entry(session.clone()).or_default();
        while entry
            .starts
            .front()
            .is_some_and(|t| now.duration_since(*t) >= RATE_WINDOW)
        {
            entry.starts.pop_front();
        }
        if entry.tasks >= MAX_TASKS_PER_SESSION || entry.starts.len() >= CALLS_PER_MINUTE {
            return None;
        }
        entry.tasks += 1;
        entry.starts.push_back(now);
        Some(Permit {
            limits: Arc::clone(self),
            session: session.clone(),
        })
    }

    /// The call tasks of `session` that hold a permit now.
    #[must_use]
    pub fn tasks(&self, session: &SessionId) -> usize {
        self.map().get(session).map_or(0, |e| e.tasks)
    }
}

/// One permit of [`CallLimits::acquire`]; dropping it frees the slot.
#[derive(Debug)]
pub struct Permit {
    limits: Arc<CallLimits>,
    session: SessionId,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut map = self.limits.map();
        if let Some(entry) = map.get_mut(&self.session) {
            entry.tasks = entry.tasks.saturating_sub(1);
        }
    }
}

/// What the app methods use of the station.
#[derive(Clone)]
pub struct AppEnv {
    pub store: Store,
    pub ids: SharedIds,
    pub exits: Arc<ProcessExits>,
    pub limits: Arc<CallLimits>,
    /// Wakes the advance task (`manager/shell_advance.rs`).
    pub wake: Arc<Notify>,
    /// How long a call start waits for the fence ([`FENCE_WAIT`]).
    pub fence_wait: Duration,
    /// How long the reply waits for the call task ([`CALL_REPLY_WAIT`]).
    pub reply_wait: Duration,
    pub(crate) hooks: CallHooks,
}

impl AppEnv {
    /// An environment with its own exits, limits and wake.
    #[must_use]
    pub fn new(store: Store, ids: SharedIds) -> Self {
        Self {
            store,
            ids,
            exits: Arc::default(),
            limits: Arc::default(),
            wake: Arc::default(),
            fence_wait: FENCE_WAIT,
            reply_wait: CALL_REPLY_WAIT,
            hooks: CallHooks::default(),
        }
    }
}

/// The provider text of an error message: control characters removed, cut to
/// [`ERROR_TEXT_CHARS`] characters.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(ERROR_TEXT_CHARS)
        .collect()
}

const STOPPED: &str = "The session that made this app is not running. Resume it to use the app.";

/// The error of an [`AppError`] (design §4.6, step 6).
fn map_error(e: &AppError, read_only: bool) -> AppFail {
    match e {
        AppError::Answered(text) if read_only => AppFail::new(
            code::PROVIDER_ERROR,
            format!("The provider reported an error: {}", clean(text)),
        ),
        AppError::Answered(text) => AppFail::new(
            code::PROVIDER_ERROR,
            format!(
                "The provider reported an error: {}. The tool may still run. A revert of this \
                 folder waits until the session stops or you mark the action as stopped.",
                clean(text)
            ),
        ),
        AppError::Ended => AppFail::new(code::SESSION_STOPPED, STOPPED),
        AppError::Timeout => AppFail::new(code::TIMEOUT, "The provider did not answer in time."),
        AppError::Unsupported => AppFail::new(
            code::PROVIDER_ERROR,
            "The provider reported an error: not supported by this provider",
        ),
    }
}

/// `session.app_call` (design §4.6): the five steps in order, then the
/// workspace protection of §4.6.1 for a `tools/call`.
///
/// # Errors
/// An [`AppFail`] with the code of the failed step.
pub async fn app_call(
    env: &AppEnv,
    host: Option<AppHost>,
    params: AppCallParams,
) -> Result<AppCallReply, AppFail> {
    let session = params.session_id.clone();
    let meta = match env
        .store
        .app_lookup(session.clone(), params.app_id.clone())
        .await
        .map_err(internal)?
    {
        AppLookup::Found(meta) => meta,
        AppLookup::Gone => {
            return Err(AppFail::new(
                code::APP_GONE,
                "This app is no longer stored.",
            ));
        }
        AppLookup::Unknown => return Err(AppFail::new(code::UNKNOWN_APP, "Unknown app.")),
    };
    let host = host.ok_or_else(|| AppFail::new(code::SESSION_STOPPED, STOPPED))?;
    if matches!(params.request, AppRequest::ToolsCall { .. }) {
        let (_, uncertain) = env
            .store
            .app_call_open_count(session.clone())
            .await
            .map_err(internal)?;
        if uncertain >= MAX_UNCERTAIN_PER_SESSION {
            return Err(AppFail::new(
                code::BUSY,
                "Too many app actions of this session may still run. Mark them as stopped, or \
                 stop the session.",
            ));
        }
    }
    let Some(permit) = env.limits.acquire(&session) else {
        return Err(AppFail::new(
            code::BUSY,
            "The app sends too many requests. Wait a moment.",
        ));
    };
    match params.request {
        AppRequest::ResourcesRead { uri } => {
            if !uri.starts_with("ui://") || uri.len() > 4096 {
                return Err(AppFail::new(
                    code::INVALID_PARAMS,
                    "resources/read needs a ui:// URI of at most 4096 characters.",
                ));
            }
            let read = host.bridge.read_resource(&meta.server, &uri).await;
            drop(permit);
            reply_of(
                read.map(|result| ToolAnswer {
                    result,
                    proves_end: true,
                }),
                true,
            )
        }
        AppRequest::ToolsCall { name, arguments } => {
            let call = Call {
                env,
                host,
                session,
                app_id: params.app_id,
                server: meta.server,
                name,
                arguments,
                confirmed: params.confirmed,
            };
            tools_call(call, permit).await
        }
    }
}

struct Call<'a> {
    env: &'a AppEnv,
    host: AppHost,
    session: SessionId,
    app_id: String,
    server: String,
    name: String,
    arguments: Value,
    confirmed: bool,
}

/// The reply of a finished provider call.
fn reply_of(
    outcome: Result<ToolAnswer, AppError>,
    read_only: bool,
) -> Result<AppCallReply, AppFail> {
    match outcome {
        Ok(answer) if json_len(&answer.result) > MAX_APP_REPLY_BYTES => Err(AppFail::new(
            code::TOO_LARGE,
            "The answer is too large for the app.",
        )),
        Ok(answer) => Ok(AppCallReply::Done {
            result: answer.result,
        }),
        Err(e) => Err(map_error(&e, read_only)),
    }
}

/// The row of a call that has one.
struct OpenRow {
    call_id: String,
    root: String,
    process: String,
}

async fn tools_call(call: Call<'_>, permit: Permit) -> Result<AppCallReply, AppFail> {
    let Call {
        env,
        host,
        session,
        app_id,
        server,
        name,
        arguments,
        confirmed,
    } = call;
    let arguments = match arguments {
        Value::Null => json!({}),
        object @ Value::Object(_) => object,
        _ => {
            return Err(AppFail::new(
                code::INVALID_PARAMS,
                "The arguments of a tool call are an object.",
            ));
        }
    };
    if json_len(&arguments) > MAX_APP_ARGS_BYTES {
        return Err(AppFail::new(
            code::INVALID_PARAMS,
            "The arguments of the tool call are too large.",
        ));
    }
    let tools = host.bridge.tools().await.map_err(|e| map_error(&e, true))?;
    let claude = env
        .store
        .summary(session.clone())
        .await
        .map_err(internal)?
        .and_then(|s| s.provider)
        == Some(ProviderKind::Claude);
    let not_callable = || AppFail::new(code::TOOL_NOT_CALLABLE, "This app cannot call that tool.");
    let tool = tools
        .iter()
        .find(|t| t.server == server && t.name == name)
        .filter(|t| {
            t.visibility
                .as_ref()
                .is_none_or(|v| v.iter().any(|x| x == "app"))
        })
        .ok_or_else(not_callable)?;
    if claude
        && ClaudeToolMap::build(&tools)
            .address(&server, &name)
            .is_none()
    {
        return Err(not_callable());
    }
    if !tool.read_only && !confirmed {
        return Ok(AppCallReply::Confirm {
            tool: name,
            title: tool.title.clone(),
        });
    }
    let read_only = tool.read_only;
    // §4.6.1 start: the row is inserted under the workspace fence. A session
    // without a Git worktree has no checkpoints and no revert: no row.
    let row = match host.turns.worktree_root() {
        None => None,
        Some(root) => {
            let fence = tokio::time::timeout(env.fence_wait, host.turns.fence())
                .await
                .map_err(|_| {
                    AppFail::new(
                        code::BUSY,
                        "The session folder is busy (a revert or a checkpoint runs). Try again \
                         in a moment.",
                    )
                })?;
            let call_id: String = new_id(env.ids.as_ref());
            let begun = env
                .store
                .app_call_begin(NewAppCall {
                    call_id: call_id.clone(),
                    session_id: session.clone(),
                    app_id,
                    worktree_root: root.clone(),
                    server: server.clone(),
                    tool: name.clone(),
                    read_only,
                    process: host.process.as_str().to_owned(),
                    station_run: env.store.run_id().to_owned(),
                })
                .await;
            drop(fence);
            begun.map_err(internal)?;
            Some(OpenRow {
                call_id,
                root,
                process: host.process.as_str().to_owned(),
            })
        }
    };
    let (tx, rx) = oneshot::channel();
    let task = CallTask {
        env: env.clone(),
        host,
        session,
        row,
        read_only,
        server,
        tool: name,
        arguments,
        permit,
    };
    tokio::spawn(call_task(task, tx));
    match tokio::time::timeout(env.reply_wait, rx).await {
        Err(_) => Err(AppFail::new(
            code::TIMEOUT,
            "The tool did not answer in 60 s. It may still run. A revert of this folder waits \
             until it ends.",
        )),
        Ok(Err(_)) => Err(internal("the call task stopped")),
        Ok(Ok(outcome)) => reply_of(outcome, read_only),
    }
}

struct CallTask {
    env: AppEnv,
    host: AppHost,
    session: SessionId,
    row: Option<OpenRow>,
    read_only: bool,
    server: String,
    tool: String,
    arguments: Value,
    permit: Permit,
}

/// How a row settles after the provider's outcome (the table of §4.6.1).
enum Plan {
    Terminal(AppCallEnd),
    Uncertain,
}

fn plan(read_only: bool, outcome: &Result<ToolAnswer, AppError>) -> Plan {
    match outcome {
        Ok(answer) if answer.proves_end => Plan::Terminal(AppCallEnd::Done),
        Ok(_) if read_only => Plan::Terminal(AppCallEnd::Done),
        Err(AppError::Ended) => Plan::Terminal(AppCallEnd::Ended),
        Err(AppError::Unsupported) => Plan::Terminal(AppCallEnd::Failed),
        Err(_) if read_only => Plan::Terminal(AppCallEnd::Failed),
        Ok(_) | Err(_) => Plan::Uncertain,
    }
}

/// The call task: awaits the provider (no host timeout), hands the outcome to
/// the request handler, then settles the row. The permit is held until here.
async fn call_task(task: CallTask, reply: oneshot::Sender<Result<ToolAnswer, AppError>>) {
    let CallTask {
        env,
        host,
        session,
        row,
        read_only,
        server,
        tool,
        arguments,
        permit,
    } = task;
    let _permit = permit;
    let outcome = host.bridge.call_tool(&server, &tool, arguments).await;
    // A handler that gave up has dropped the receiver; the row still settles.
    let _ = reply.send(outcome.clone());
    env.hooks.pause().await;
    let Some(row) = row else {
        return;
    };
    match plan(read_only, &outcome) {
        Plan::Terminal(end) => finish(&env, &host, &row, read_only, end).await,
        Plan::Uncertain => {
            // Race close (§4.6.1): mark first, then read the exits. It never
            // reads the registry; after a detach the row stays `uncertain`.
            let marked = env
                .store
                .app_call_mark_uncertain(row.call_id.clone(), UncertainCause::ProviderError)
                .await;
            if let Err(e) = marked {
                tracing::error!(error = %e, call = %row.call_id, "app call not marked uncertain");
                return;
            }
            if env.exits.has(&session, &row.process) {
                finish(&env, &host, &row, read_only, AppCallEnd::Ended).await;
            }
        }
    }
}

/// Settles the row; a settled non-read-only row advances the folder
/// generation under the fence of its root (no timeout).
async fn finish(env: &AppEnv, host: &AppHost, row: &OpenRow, read_only: bool, end: AppCallEnd) {
    match env.store.app_call_settle(row.call_id.clone(), end).await {
        Ok(true) if !read_only => {
            let fence = host.turns.fence().await;
            let advanced = env
                .store
                .app_call_advance(row.call_id.clone(), row.root.clone())
                .await;
            drop(fence);
            if let Err(e) = advanced {
                // The row keeps `advance_pending = 1`: the advance task and
                // the next station start run it.
                tracing::error!(error = %e, call = %row.call_id, "app call advance failed");
            }
        }
        Ok(_) => {}
        Err(e) => tracing::error!(error = %e, call = %row.call_id, "app call not settled"),
    }
    env.wake.notify_one();
}

/// Does the newest shim row of the session prove that its execution ended?
async fn shim_proves_end(store: &Store, session: &SessionId) -> StoreResult<bool> {
    Ok(store
        .shim_rows(session.clone())
        .await?
        .last()
        .is_some_and(proves_end))
}

/// The station start, before the advance and before the app admission opens:
/// a `running` row of an older station process (its `station_run` is not
/// [`Store::run_id`], also after a reopen of the same database) has no task
/// in this process. When the session's newest shim row proves the end of
/// its execution the row ends (`ended`); else it becomes `uncertain` with the
/// cause `station_restart` (a station crash does not stop a direct host's
/// provider). An `uncertain` row of the last station process stays as it is.
///
/// # Errors
/// A database error.
pub async fn start_up(store: &Store) -> StoreResult<()> {
    for row in store.app_calls_of_older_runs().await? {
        if shim_proves_end(store, &row.session_id).await? {
            store
                .app_call_settle(row.call_id, AppCallEnd::Ended)
                .await?;
        } else {
            store
                .app_call_mark_uncertain(row.call_id, UncertainCause::StationRestart)
                .await?;
        }
    }
    Ok(())
}

/// Each `uncertain` row whose session's newest shim row proves the end ends.
///
/// # Errors
/// A database error.
pub async fn settle_proven(store: &Store) -> StoreResult<()> {
    for row in store.app_calls_uncertain().await? {
        if shim_proves_end(store, &row.session_id).await? {
            store
                .app_call_settle(row.call_id, AppCallEnd::Ended)
                .await?;
        }
    }
    Ok(())
}

/// The observed exit of a session's process (`HostEnd::Exited`): every
/// `uncertain` row of that session and process ends (the same proof as
/// [`AppError::Ended`]). Returns how many rows ended. The caller wakes the
/// advance task.
///
/// # Errors
/// A database error.
pub async fn process_ended(
    store: &Store,
    session: &SessionId,
    process: &str,
) -> StoreResult<usize> {
    let mut ended = 0;
    for row in store
        .app_calls_uncertain_of_process(session.clone(), process.to_owned())
        .await?
    {
        if store
            .app_call_settle(row.call_id, AppCallEnd::Ended)
            .await?
        {
            ended += 1;
        }
    }
    Ok(ended)
}

fn open_call(row: &AppCallRow) -> Option<AppOpenCall> {
    let state = match row.state {
        CallState::Running => AppCallState::Running,
        CallState::Uncertain => AppCallState::Uncertain,
        _ => return None,
    };
    Some(AppOpenCall {
        call_id: row.call_id.clone(),
        session_id: row.session_id.clone(),
        app_id: row.app_id.clone(),
        server: row.server.clone(),
        tool: row.tool.clone(),
        state,
        cause: row.cause.clone(),
        created_at: row.created_at,
    })
}

/// `session.app_calls`: the open calls of the session and of its folder. It
/// reads no app row and needs no live session; `host` only gives the root of
/// a live session.
///
/// # Errors
/// An [`AppFail`] with the code `internal`.
pub async fn app_calls(
    env: &AppEnv,
    host: Option<AppHost>,
    session: SessionId,
) -> Result<AppCallsReply, AppFail> {
    let extra_root = host.and_then(|h| h.turns.worktree_root());
    let rows = env
        .store
        .app_calls_open(session, extra_root)
        .await
        .map_err(internal)?;
    Ok(AppCallsReply {
        calls: rows.iter().filter_map(open_call).collect(),
    })
}

/// `session.app_release`: the owner's release of an `uncertain` call. It reads
/// only the call row of that session.
///
/// # Errors
/// `call_running` for a `running` row, `unknown_call` for no row of that
/// session, `internal` for a database error.
pub async fn app_release(
    env: &AppEnv,
    session: SessionId,
    call_id: String,
) -> Result<AppReleaseReply, AppFail> {
    let row = env
        .store
        .app_call(call_id.clone())
        .await
        .map_err(internal)?;
    match env
        .store
        .app_call_release(session, call_id)
        .await
        .map_err(internal)?
    {
        Release::Released => {
            if let Some(row) = row {
                tracing::info!(
                    session = %row.session_id,
                    app = %row.app_id,
                    call = %row.call_id,
                    server = %row.server,
                    tool = %row.tool,
                    "app call released by the owner"
                );
            }
            env.wake.notify_one();
            Ok(AppReleaseReply { released: true })
        }
        Release::NotOpen => Ok(AppReleaseReply { released: false }),
        Release::Running => Err(AppFail::new(
            code::CALL_RUNNING,
            "This app action is still running. Stop the session to end it.",
        )),
        Release::Unknown => Err(AppFail::new(code::UNKNOWN_CALL, "Unknown app action.")),
    }
}

#[cfg(test)]
#[path = "calls_rig_tests.rs"]
pub(crate) mod rig;

#[cfg(test)]
#[path = "calls_tests.rs"]
mod tests;
