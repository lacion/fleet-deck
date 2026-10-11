//! The provider adapter boundary (PLAN §6.2): [`ProviderAdapter`] starts and
//! resumes sessions, [`LiveSession`] drives one running session.
//!
//! Deviations from the PLAN §6.2 sketch (DL-032): `prompt`, `steer` and
//! `follow_up` take a [`HostInput`] (the station mints the input id and knows
//! the run origin), and `answer` takes an [`Answer`] (it also names who
//! answered, for `approval.resolved.by`).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use fleetdeck_proto::command::{ApprovalDecision, SessionSpec, StopMode, UserInput};
use fleetdeck_proto::event::{ExpiredReason, ProviderKind, RawRef, ResolvedBy, RunOrigin};
use fleetdeck_proto::{Capabilities, Event, InputId, Operation, RequestId, Unsupported};
use tokio::sync::mpsc;

use crate::host::HostError;

/// The native session (Claude) or thread (Codex) id. It goes into
/// `provider_refs` and is never a global key (L6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NativeSessionId(pub String);

/// A native permission mode name, for example `acceptEdits` (L8).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PermissionMode(pub String);

/// A model id as the provider lists it. `default` is a sentinel for "the
/// provider's default model", never a model name (trap 2).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelId(pub String);

/// The host's id for a control request, echoed in the provider's answer
/// (`interrupt.result`). The station passes its effect id, so the answer
/// confirms exactly that effect (ADR-001 §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ControlId(pub String);

/// One input from the host: the station mints `id` and reports it in the
/// command receipt; the provider reports the fate of the input under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInput {
    pub id: InputId,
    pub origin: RunOrigin,
    pub input: UserInput,
}

/// The answer to a runtime request. `decision.option_id` is a native option id.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub request: RequestId,
    pub decision: ApprovalDecision,
    pub by: ResolvedBy,
}

/// The sign-in state of a provider CLI (trap 9: a failed check is `Unknown`,
/// never "signed out").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthState {
    SignedIn { method: Option<String> },
    SignedOut,
    Unknown,
}

/// What a probe learns without opening a provider session (L11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProbe {
    pub kind: ProviderKind,
    pub version: Option<String>,
    pub auth: AuthState,
}

/// Errors of the adapter boundary. The board receives typed results only (L5).
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error(transparent)]
    Unsupported(#[from] Unsupported),
    /// A setting was used before the provider advertised its valid values (trap 1).
    #[error("{0} is not known yet: the provider did not advertise it")]
    NotReady(&'static str),
    #[error("mode `{0}` is not offered by this session")]
    InvalidMode(String),
    #[error("model `{0}` is not offered by this provider")]
    InvalidModel(String),
    #[error("input id `{0}` cannot be used")]
    InvalidInput(String),
    #[error("unknown request {0}")]
    UnknownRequest(RequestId),
    #[error("option `{option}` is not offered by request {request}")]
    UnknownOption { request: RequestId, option: String },
    #[error("request {request} is closed ({reason:?})")]
    RequestClosed {
        request: RequestId,
        reason: ExpiredReason,
    },
    #[error("no turn is running and no input waits")]
    NoTurn,
    #[error("the session has ended")]
    SessionEnded,
    #[error("the event sink is closed")]
    SinkClosed,
    /// The process stopped, but the event sink stayed full: the station did
    /// not receive the last events (for example `session.ended`).
    #[error("the process stopped, but its last events were not delivered")]
    EventsUndelivered,
    #[error("workspace `{path}` is not usable: {source}")]
    Workspace {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("probe failed: {0}")]
    Probe(String),
    #[error(transparent)]
    Host(#[from] HostError),
}

pub type Result<T, E = ProviderError> = std::result::Result<T, E>;

/// `Ok` if the session supports `op`, else the typed degradation (L5).
pub fn require(caps: &Capabilities, op: Operation) -> Result<()> {
    caps.check(op).map_err(ProviderError::from)
}

/// One canonical event and, if the adapter logged the raw frame that caused
/// it, where that frame is (L12).
#[derive(Debug, Clone, PartialEq)]
pub struct Emitted {
    pub event: Event,
    pub raw_ref: Option<RawRef>,
}

/// Where an adapter sends the canonical events of one session. The station
/// appends them to its event log in order. Bounded: a slow log slows the
/// adapter, it never drops an event.
#[derive(Debug, Clone)]
pub struct EventSink {
    tx: mpsc::Sender<Emitted>,
}

impl EventSink {
    /// A sink and the receiver that the station reads.
    #[must_use]
    pub fn channel(capacity: usize) -> (Self, mpsc::Receiver<Emitted>) {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        (Self { tx }, rx)
    }

    /// Sends one event. Fails only when the station dropped the receiver.
    pub async fn emit(&self, event: Event) -> Result<()> {
        self.emit_with(event, None).await
    }

    /// Sends one event with the reference of its raw frame.
    pub async fn emit_with(&self, event: Event, raw_ref: Option<RawRef>) -> Result<()> {
        self.tx
            .send(Emitted { event, raw_ref })
            .await
            .map_err(|_| ProviderError::SinkClosed)
    }
}

/// The real directory of a workspace (trap 8): symlinks are resolved before
/// the provider starts, so the provider, the policy check and the board all
/// see one path.
pub fn canonical_workspace(path: &Path) -> Result<PathBuf> {
    let err = |source| ProviderError::Workspace {
        path: path.to_path_buf(),
        source,
    };
    let real = std::fs::canonicalize(path).map_err(err)?;
    if !real.is_dir() {
        return Err(err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "not a directory",
        )));
    }
    Ok(real)
}

/// Starts sessions of one provider instance (L10).
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    fn kind(&self) -> ProviderKind;
    /// Version and sign-in state only. A probe never opens a provider session.
    async fn probe(&self) -> Result<ProviderProbe>;
    async fn start(&self, spec: SessionSpec, sink: EventSink) -> Result<Box<dyn LiveSession>>;
    async fn resume(
        &self,
        native_id: &NativeSessionId,
        spec: SessionSpec,
        sink: EventSink,
    ) -> Result<Box<dyn LiveSession>>;
    /// Starts a new session with the context of `native_id` (the id that
    /// [`LiveSession::fork`] returned). Unsupported unless the adapter
    /// overrides it (DL-033).
    async fn fork(
        &self,
        native_id: &NativeSessionId,
        spec: SessionSpec,
        sink: EventSink,
    ) -> Result<Box<dyn LiveSession>> {
        let _ = (native_id, spec, sink);
        Err(Unsupported::new(Operation::Fork).into())
    }
}

/// One running provider session. Every operation that the provider does not
/// support returns [`ProviderError::Unsupported`] (L5).
#[async_trait]
pub trait LiveSession: Send {
    /// The capabilities of this session (L11). They can change after the
    /// provider advertises its settings (trap 1).
    fn capabilities(&self) -> &Capabilities;
    async fn prompt(&mut self, input: HostInput) -> Result<()>;
    /// Unsupported if `!capabilities().steer`.
    async fn steer(&mut self, input: HostInput) -> Result<()>;
    async fn follow_up(&mut self, input: HostInput) -> Result<()>;
    /// A request to the provider; the turn ends later with its own terminal
    /// event (trap 10). The provider's answer comes as `interrupt.result`
    /// with `control_id`.
    async fn interrupt(&mut self, control_id: Option<ControlId>) -> Result<()>;
    /// Idempotent: a second answer to the same request writes nothing (L3).
    async fn answer(&mut self, answer: Answer) -> Result<()>;
    async fn set_mode(&mut self, mode: PermissionMode) -> Result<()>;
    async fn set_model(&mut self, model: ModelId) -> Result<()>;
    /// The native id to fork from: the station passes it to
    /// [`ProviderAdapter::fork`] (DL-033).
    async fn fork(&mut self) -> Result<NativeSessionId>;
    /// Stops the process within the stop policy. A sink that stays full
    /// cannot hold `stop`: it then returns [`ProviderError::EventsUndelivered`]
    /// after the process is stopped.
    async fn stop(&mut self, how: StopMode) -> Result<()>;
}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;
