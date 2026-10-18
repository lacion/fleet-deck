//! R13 P5b: the monitor recording values (design `omarchy-p5b.md` §3.3, §4,
//! §6, §7.5, §8, §11.1, §14; decisions O12-7 and O12-10).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::artifacts::{ArtifactKind, ArtifactPublished};
use crate::capture::{CaptureKind, CaptureStatus, code};
use crate::coordinator_desktop_text::code as cd_code;

/// The largest recording file: 256 MiB (§8).
pub const MAX_RECORDING_BYTES: u64 = 268_435_456;

/// The peak reservation of one recording: a staging file plus the import
/// copy, 512 MiB (§8).
pub const PEAK_RESERVATION: u64 = 536_870_912;

/// The file name of a recording in its artifact (§11.1).
pub const RECORDING_FILE: &str = "capture.mp4";

/// The most pixels of one recorded frame (§8).
const MAX_PIXELS: u64 = 16_777_216;

/// True for a requested duration of 1 to 120 seconds (§3.2).
#[must_use]
pub fn valid_seconds(n: u64) -> bool {
    (1..=120).contains(&n)
}

/// True for a recordable monitor size: each side 2 to 8192 and even, at most
/// 16,777,216 pixels (§8).
#[must_use]
pub fn valid_dimensions(w: u32, h: u32) -> bool {
    let side = |n: u32| (2..=8192).contains(&n) && n.is_multiple_of(2);
    side(w) && side(h) && u64::from(w) * u64::from(h) <= MAX_PIXELS
}

/// The closed list of the reasons that stop a recording (§7.5, O12-7).
/// `Duration` and `OwnerStop` save; every other reason discards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "recording/")]
pub enum StopReason {
    Duration,
    OwnerStop,
    PolicyOff,
    ScreenLocked,
    MonitorChanged,
    LinkClosed,
    LeaseExpired,
    LeaseDenied,
    DeckPolicy,
    OwnerLoginEnded,
    CoordinatorAuthorityLost,
    OutputLimit,
    DiskReserve,
    RecorderExited,
    StationShutdown,
    RecordingInterrupted,
    ArtifactDeleted,
    Deadline,
}

impl StopReason {
    /// True for the two save reasons.
    #[must_use]
    pub fn is_save(self) -> bool {
        matches!(self, Self::Duration | Self::OwnerStop)
    }

    /// True when a lease answer may carry this reason with this decision:
    /// `owner_stop` with `save`, or one of `deck_policy`, `owner_login_ended`,
    /// `coordinator_authority_lost`, `artifact_deleted`, `deadline` with
    /// `discard` (§7.5). A `continue` answer carries no reason.
    #[must_use]
    pub fn lease_allowed(self, d: LeaseDecision) -> bool {
        match d {
            LeaseDecision::Save => self == Self::OwnerStop,
            LeaseDecision::Discard => matches!(
                self,
                Self::DeckPolicy
                    | Self::OwnerLoginEnded
                    | Self::CoordinatorAuthorityLost
                    | Self::ArtifactDeleted
                    | Self::Deadline
            ),
            LeaseDecision::Continue => false,
        }
    }

    /// The terminal code that the owner sees (§7.5, §16). `ceiling` tells the
    /// two causes of `output_limit` apart: the size reached the ceiling, or a
    /// write failed. The table has no owner code for
    /// `coordinator_authority_lost`, and a save reason is no failure: both
    /// answer `recording_interrupted`.
    #[must_use]
    pub fn owner_code(self, ceiling: bool) -> &'static str {
        match self {
            Self::ScreenLocked => code::SCREEN_LOCKED,
            Self::MonitorChanged => cd_code::DESKTOP_NOT_FOUND,
            Self::OutputLimit if ceiling => code::RECORDING_TOO_LARGE,
            Self::OutputLimit | Self::RecorderExited => code::RECORDING_FAILED,
            Self::DiskReserve => code::DISK_FULL,
            Self::Deadline => cd_code::DESKTOP_TIMEOUT,
            Self::Duration
            | Self::OwnerStop
            | Self::PolicyOff
            | Self::LinkClosed
            | Self::LeaseExpired
            | Self::LeaseDenied
            | Self::DeckPolicy
            | Self::OwnerLoginEnded
            | Self::CoordinatorAuthorityLost
            | Self::StationShutdown
            | Self::RecordingInterrupted
            | Self::ArtifactDeleted => code::RECORDING_INTERRUPTED,
        }
    }

    /// The terminal code that a coordinator sees (§7.5, §16). `None` for
    /// `owner_login_ended` (the table has no coordinator code) and for a save
    /// reason.
    #[must_use]
    pub fn coordinator_code(self, ceiling: bool) -> Option<&'static str> {
        Some(match self {
            Self::PolicyOff
            | Self::DeckPolicy
            | Self::CoordinatorAuthorityLost
            | Self::ArtifactDeleted => cd_code::AUTHORITY_LOST,
            Self::ScreenLocked => code::SCREEN_LOCKED,
            Self::MonitorChanged => cd_code::DESKTOP_NOT_FOUND,
            Self::LinkClosed
            | Self::LeaseExpired
            | Self::LeaseDenied
            | Self::StationShutdown
            | Self::RecordingInterrupted => cd_code::AUTHORITY_UNKNOWN,
            Self::OutputLimit if ceiling => code::RECORDING_TOO_LARGE,
            Self::OutputLimit | Self::RecorderExited => code::RECORDING_FAILED,
            Self::DiskReserve => code::DISK_FULL,
            Self::Deadline => cd_code::DESKTOP_READ_TIMEOUT,
            Self::Duration | Self::OwnerStop | Self::OwnerLoginEnded => return None,
        })
    }
}

/// The phase of a recording that a station reports (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "recording/")]
pub enum RecordingPhase {
    Preparing,
    Recording,
    Stopping,
    Finalizing,
}

/// The progress of a recording; a current station answer only (§4.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "recording/")]
pub struct RecordingProgress {
    pub phase: RecordingPhase,
    pub requested_seconds: u32,
    /// Station Unix ms of the actual recorder start.
    #[ts(type = "number | null")]
    pub started_at: Option<i64>,
    #[ts(type = "number | null")]
    pub elapsed_ms: Option<u64>,
}

/// The immutable metadata of a committed recording (§4.2, §11.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "recording/")]
pub struct RecordingMeta {
    pub requested_seconds: u32,
    #[ts(type = "number")]
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    /// `duration` or `owner_stop`.
    pub stop_reason: StopReason,
}

/// A capture status of a recording-aware path: the P5a status with the
/// optional progress and stop reason (§4.2). A peer without
/// `desktop_recording.v1` never receives the two extra fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "recording/")]
pub struct RecordingCaptureStatus {
    #[serde(flatten)]
    pub status: CaptureStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recording: Option<RecordingProgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stop_reason: Option<StopReason>,
}

/// The answer of `desktop.capture_active` (§4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export_to = "recording/")]
#[expect(
    clippy::large_enum_variant,
    reason = "an answer lives only between decode and reply; boxing the status \
              would change the field type of the wire shape"
)]
pub enum ActiveRecording {
    Idle,
    Active {
        capture_id: String,
        status: RecordingCaptureStatus,
    },
}

/// The params of `desktop.capture_stop` (§4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "recording/")]
pub struct CaptureStopParams {
    pub capture_id: String,
    pub reason: StopReason,
}

/// The params of `desktop.capture_policy_now` (§6.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "recording/")]
pub struct CapturePolicyNowParams {
    pub capture_id: String,
    pub action_hash: String,
}

/// The answer of `desktop.capture_policy_now` (§6.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "recording/")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "a wire shape of independent policy answers"
)]
pub struct CapturePolicyAnswer {
    pub valid: bool,
    pub desktop_epoch: String,
    pub allow_desktop: bool,
    pub allow_desktop_capture: bool,
    pub monitor_allowed: bool,
    pub monitor_present: bool,
    pub unlocked: bool,
}

/// The phase of a lease request (§6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "recording/")]
pub enum LeasePhase {
    Start,
    Record,
    Import,
}

/// The decision of a lease answer (§6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "recording/")]
pub enum LeaseDecision {
    Continue,
    Save,
    Discard,
}

/// The params of `desktop.capture_lease` (§6.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "recording/")]
pub struct CaptureLeaseRequest {
    pub capture_id: String,
    pub action_hash: String,
    #[ts(type = "number")]
    pub sequence: u64,
    pub phase: LeasePhase,
}

/// The answer of `desktop.capture_lease` (§6.3, §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "recording/")]
pub struct CaptureLeaseAnswer {
    #[ts(type = "number")]
    pub sequence: u64,
    pub decision: LeaseDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<StopReason>,
}

/// Projects an artifact for a peer without `artifacts_video.v1` (§14.6,
/// §14.7): kind `video` becomes `other`, and the capture provenance is
/// dropped when it is a recording. Stored values never change (§14.9).
pub fn project_legacy(p: &mut ArtifactPublished) {
    if p.artifact.kind == ArtifactKind::Video {
        p.artifact.kind = ArtifactKind::Other;
    }
    if p.version
        .capture
        .as_ref()
        .is_some_and(|c| c.kind == CaptureKind::Recording)
    {
        p.version.capture = None;
    }
}

#[cfg(test)]
#[path = "recording_tests.rs"]
mod tests;
