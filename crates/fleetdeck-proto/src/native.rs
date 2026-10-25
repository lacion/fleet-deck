//! Browse the native sessions of a station (B7): the params and answers of
//! the link methods `station.native_sessions` and `station.native_preview`
//! (feature `native_sessions.v1`, deck → station, scope `read`, not
//! mutating). Design `docs/v2/design/native-sessions.md` §2.2 and §2.3.
//!
//! The station walks the provider homes of its configured instances and lists
//! the transcripts that it can trust. The caller gives an instance, an id and
//! a pin, never a path.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::event::{ProviderKind, SessionState};
use crate::history::{HistoryItem, HistoryState};
use crate::ids::{ProviderInstanceId, SessionId};

/// `station.native_sessions` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "native/")]
pub struct NativeSessionsQuery {
    /// Only the home of this instance; all instances when `None`.
    #[serde(default)]
    pub provider_instance: Option<ProviderInstanceId>,
    /// 1 to 200; the default is 100.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// The state of one provider home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export_to = "native/")]
pub enum HomeState {
    Ok,
    Unavailable { reason: String },
}

/// One provider home, shared by one or more instances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "native/")]
pub struct NativeHome {
    pub provider: ProviderKind,
    pub instances: Vec<ProviderInstanceId>,
    pub state: HomeState,
}

/// The identity of a listed transcript file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "native/")]
pub struct TranscriptPin {
    /// The canonical provider home.
    pub home: String,
    #[ts(type = "number")]
    pub dev: u64,
    #[ts(type = "number")]
    pub ino: u64,
}

/// The session of this station that holds a native session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "native/")]
pub struct KnownSession {
    pub session_id: SessionId,
    pub state: SessionState,
}

/// Whether and why a native session can be imported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export_to = "native/")]
pub enum ImportState {
    Ok,
    OutsideRoots,
    InUse { session_id: SessionId },
    Held { session_id: SessionId },
    ExecutionUnknown,
    CwdMismatch,
    TooLarge,
    SubagentThread,
    PathNotTrusted,
    NotTrusted,
}

/// One native session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "native/")]
pub struct NativeSession {
    pub provider: ProviderKind,
    pub native_id: String,
    pub instances: Vec<ProviderInstanceId>,
    pub pin: TranscriptPin,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub first_prompt: Option<String>,
    /// Unix ms of the last change of the transcript.
    pub modified_at: i64,
    #[ts(type = "number")]
    pub size: u64,
    /// The parent thread of a Codex child thread.
    pub parent_native_id: Option<String>,
    pub known: Option<KnownSession>,
    pub import: ImportState,
    /// The home, the transcript and the `cwd` passed the trusted-path checks
    /// on supported filesystems (design §1b).
    pub verified: bool,
}

/// The `station.native_sessions` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "native/")]
pub struct NativeSessionList {
    pub homes: Vec<NativeHome>,
    /// Newest first.
    pub sessions: Vec<NativeSession>,
    pub truncated: bool,
    pub timed_out: bool,
}

/// `station.native_preview` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "native/")]
pub struct NativePreviewQuery {
    pub provider_instance: ProviderInstanceId,
    pub native_id: String,
    pub pin: TranscriptPin,
}

/// The `station.native_preview` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "native/")]
pub struct NativePreview {
    /// Oldest first.
    pub items: Vec<HistoryItem>,
    pub state: HistoryState,
    pub cwd: String,
    #[ts(type = "number")]
    pub size: u64,
    pub modified_at: i64,
    /// The preview does not start at the start of the conversation.
    pub earlier_omitted: bool,
}
