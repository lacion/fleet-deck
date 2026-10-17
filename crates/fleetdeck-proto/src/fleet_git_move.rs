//! Wire types of the session move (design §12, ADR-009 §4, ADR-010). Phase 4
//! (FG-9). Everything here is additive (O5).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::event::ProviderKind;
use crate::ids::{ProviderInstanceId, SessionId, StationId};

/// The most bytes of the files of one move (design §12.3, O29): 64 MiB.
pub const MOVE_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// The most files in one manifest (design §12.3 step 3).
pub const MOVE_MAX_FILES: usize = 256;
/// Stream purpose prefix of `session.move_export` (`move-export:<move_id>`).
pub const MOVE_EXPORT_PURPOSE: &str = "move-export:";
/// Stream purpose prefix of `session.move_prepare` (`move-import:<move_id>`).
pub const MOVE_IMPORT_PURPOSE: &str = "move-import:";

/// What a move carries (design §12.3). `Native` moves the transcript files,
/// `BranchOnly` only the branch (O30).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum MoveKind {
    Native,
    BranchOnly,
    /// A kind of a newer peer: the station refuses it (`unsupported_mode`).
    #[serde(other)]
    Unknown,
}

/// What the source captured under its hold (design §12.3, M8). The target
/// never uses a URL from the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MovePublication {
    pub local_branch: String,
    pub dest: String,
    pub ref_name: String,
    pub oid: String,
}

/// The state of a move on the target (`move_in_runs.state`, design §12.3).
/// `LaunchRefused` is a pre-commit state (DL-227).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum MoveInState {
    Staged,
    Imported,
    Launching,
    LaunchRefused,
    Resumed,
    Released,
    Confirmed,
    ResumeFailed,
    Failed,
    Abandoned,
    Discarded,
    #[serde(other)]
    Unknown,
}

/// The source of a move, with its publication (`from` of `MoveIn`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MoveFrom {
    pub station_id: StationId,
    pub session_id: SessionId,
    pub publication: MovePublication,
    /// The last summary of the source session for the held prompt of a
    /// branch-only move (design §12.6): the digest, else the last assistant
    /// message, cut to 4000 characters by the deck. FG-9.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
}

/// The source of a move without the publication (`source` of
/// `session.move_prepare`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MoveSourceRef {
    pub station_id: StationId,
    pub session_id: SessionId,
}

/// Where a moved session went (`summary.moved_to`, design §12.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MovedTo {
    pub move_id: String,
    pub station_id: StationId,
    pub session_id: SessionId,
}

/// The move state of the target session (`summary.move_in`, from the event
/// `session.move_in`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MoveInInfo {
    pub move_id: String,
    pub state: MoveInState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
}

/// One file of a transcript manifest (design §12.3 step 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct ManifestFile {
    pub key: String,
    pub rel: String,
    pub size: u64,
    pub sha256: String,
}

/// The files of a native move, in stream order (design §12.3 step 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct TranscriptManifest {
    pub provider: ProviderKind,
    pub native_session_id: String,
    pub files: Vec<ManifestFile>,
    pub total: u64,
}

/// Params of the methods that name only a move: `session.move_export`,
/// `move_release`, `move_abort`, `move_discard`, `move_status`, `move_fail`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MoveIdParams {
    pub move_id: String,
}

/// The answer of `session.move_export` (design §12.3 step 2). `manifest` and
/// `stream_token` are absent for a branch-only move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MoveExportAnswer {
    pub publication: MovePublication,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub manifest: Option<TranscriptManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stream_token: Option<String>,
    /// The provider instance of the source session (FG-9): the deck sends it
    /// in `MoveIn` when the summary has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider_instance: Option<ProviderInstanceId>,
}

/// Params of `session.move_prepare` (design §12.3 step 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MovePrepareParams {
    pub move_id: String,
    pub manifest: TranscriptManifest,
    pub source: MoveSourceRef,
}

/// The answer of `session.move_prepare`: the token of the upload stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MovePrepareAnswer {
    pub stream_token: String,
}

/// The `state` of the answer of `session.move_release`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum MoveReleaseState {
    Released,
    Confirmed,
    #[serde(other)]
    Unknown,
}

/// The answer of `session.move_release` (design §12.3 step 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MoveReleaseAnswer {
    pub state: MoveReleaseState,
}

/// The answer of `session.move_abort`: `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MoveAbortAnswer {}

/// The `state` of the answer of `session.move_discard`. `Settling` is not an
/// acknowledgement (design §12.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum MoveDiscardState {
    Discarded,
    Abandoned,
    Resumed,
    Settling,
    #[serde(other)]
    Unknown,
}

/// What the discard did with the `MoveIn` effect and the upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum MoveDiscardEffect {
    None,
    Cancelled,
    Settled,
    #[serde(other)]
    Unknown,
}

/// The answer of `session.move_discard` (design §12.3, "Abort"). `effect` is
/// absent in the `{ state: "settling" }` form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MoveDiscardAnswer {
    pub state: MoveDiscardState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effect: Option<MoveDiscardEffect>,
}

impl MoveDiscardAnswer {
    /// True when the target acknowledged the discard: a final state with a
    /// known `effect`. `Settling` and any unknown value are not an
    /// acknowledgement (fail closed).
    #[must_use]
    pub fn is_ack(&self) -> bool {
        matches!(
            self.state,
            MoveDiscardState::Discarded | MoveDiscardState::Abandoned | MoveDiscardState::Resumed
        ) && matches!(
            self.effect,
            Some(
                MoveDiscardEffect::None | MoveDiscardEffect::Cancelled | MoveDiscardEffect::Settled
            )
        )
    }
}

/// The answer of `session.move_fail` (design §12.4A step U1): the state of the
/// target after the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MoveFailAnswer {
    pub state: MoveInState,
}

/// The `state` of the answer of `session.move_status`: a target state
/// (`MoveInState`) or a source state (`move_out_runs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum MoveStatusState {
    Holding,
    Finished,
    Aborted,
    Undone,
    Staged,
    Imported,
    Launching,
    LaunchRefused,
    Resumed,
    Released,
    Confirmed,
    ResumeFailed,
    Failed,
    Abandoned,
    Discarded,
    #[serde(other)]
    Unknown,
}

/// The answer of `session.move_status` (design §12.3, "Deck restart").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct MoveStatusAnswer {
    pub state: MoveStatusState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
}
