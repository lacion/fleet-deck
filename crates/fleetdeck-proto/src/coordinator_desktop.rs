//! The arguments and results of the P6 coordinator desktop tools (feature
//! `desktop_mcp.v1`; `docs/v2/design/omarchy-p6.md` §3, §4.1, §4.8, §4.9).
//!
//! Every struct and enum refuses unknown fields. [`DesktopCall::validate`]
//! and [`DesktopQuery::validate`] check the patterns and lengths of the §3
//! schema bundle; they name the field that failed and never show its value.
//! The URL grammar and the markup stripping are in [`crate::desktop_text`]
//! and run later, on the deck.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::desktop::DesktopKind;
use crate::display_directive::{ArtifactRef, SessionRef};
use crate::recording::StopReason;

pub use crate::coordinator_input::{KeyPressArguments, PointerMoveArguments, TypeTextArguments};

/// A tool argument that failed validation: the name of the field only,
/// never its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgError {
    pub field: &'static str,
}

pub(crate) const fn bad(field: &'static str) -> ArgError {
    ArgError { field }
}

/// What a wall shows: a session, or one version of an artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub enum WallContent {
    Session { session: SessionRef },
    Artifact { artifact: ArtifactRef },
}

/// The urgency of a coordinator notification; `critical` is not offered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "coordinator_desktop/")]
pub enum CoordUrgency {
    Low,
    #[default]
    Normal,
}

/// `desktop_info` arguments. An absent `station` is the caller's station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct DesktopInfoArguments {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub station: Option<String>,
}

/// `wall_show` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct WallShowArguments {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub station: Option<String>,
    pub display_id: String,
    pub content: WallContent,
    #[serde(default)]
    pub pin: bool,
}

/// `notify` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct NotifyArguments {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub station: Option<String>,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub urgency: CoordUrgency,
}

/// `preview_open` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct PreviewOpenArguments {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub station: Option<String>,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub monitor: Option<String>,
}

/// `window_close` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct WindowCloseArguments {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub station: Option<String>,
    pub window_id: String,
}

/// `screenshot` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct ScreenshotArguments {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub station: Option<String>,
    pub monitor: String,
}

/// `record_monitor` arguments (R13 P5b-2 §13.1). `seconds` is 1 to 120.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct RecordMonitorArguments {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub station: Option<String>,
    pub monitor: String,
    pub seconds: u32,
}

/// `desktop_action_status` and `capture_status` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct StatusArguments {
    pub request_id: String,
}

/// `read_capture_image` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct ImageArguments {
    pub artifact: ArtifactRef,
}

/// A mutating tool call of a coordinator (the payload of
/// `coordinator.desktop_call`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(
    tag = "tool",
    content = "arguments",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[ts(export_to = "coordinator_desktop/")]
pub enum DesktopCall {
    WallShow(WallShowArguments),
    Notify(NotifyArguments),
    PreviewOpen(PreviewOpenArguments),
    WindowClose(WindowCloseArguments),
    Screenshot(ScreenshotArguments),
    RecordMonitor(RecordMonitorArguments),
    /// R13 P7 §4, §6.2 (feature `desktop_input_mcp.v1`).
    KeyPress(KeyPressArguments),
    /// R13 P7 §4, §6.2.
    TypeText(TypeTextArguments),
    /// R13 P7 §4, §6.2.
    PointerMove(PointerMoveArguments),
}

/// A read tool of a coordinator (the payload of `coordinator.desktop_query`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(
    tag = "tool",
    content = "arguments",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[ts(export_to = "coordinator_desktop/")]
pub enum DesktopQuery {
    DesktopInfo(DesktopInfoArguments),
    DesktopActionStatus(StatusArguments),
    CaptureStatus(StatusArguments),
    ReadCaptureImage(ImageArguments),
}

/// `^[A-Za-z0-9_-]+$` with 1 to `max` Unicode scalars.
fn word(text: &str, max: usize) -> bool {
    (1..=max).contains(&text.chars().count())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `^[A-Za-z0-9._-]+$`, 1 to 64 scalars: a monitor name.
fn monitor_ok(text: &str) -> bool {
    (1..=64).contains(&text.chars().count())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn check_id(text: &str, field: &'static str) -> Result<(), ArgError> {
    if word(text, 128) {
        Ok(())
    } else {
        Err(bad(field))
    }
}

pub(crate) fn check_request(text: &str) -> Result<(), ArgError> {
    if word(text, 64) {
        Ok(())
    } else {
        Err(bad("request_id"))
    }
}

pub(crate) fn check_station(station: Option<&String>) -> Result<(), ArgError> {
    station.map_or(Ok(()), |s| check_id(s, "station"))
}

fn check_monitor(text: &str) -> Result<(), ArgError> {
    if monitor_ok(text) {
        Ok(())
    } else {
        Err(bad("monitor"))
    }
}

fn check_artifact(a: &ArtifactRef) -> Result<(), ArgError> {
    check_id(a.station_id.as_str(), "artifact.station_id")?;
    check_id(&a.artifact_id, "artifact.artifact_id")?;
    if a.version >= 1 {
        Ok(())
    } else {
        Err(bad("artifact.version"))
    }
}

impl DesktopCall {
    /// Checks the patterns and lengths of §3. Lengths are Unicode scalars.
    ///
    /// # Errors
    /// [`ArgError`] with the name of the first field that fails.
    pub fn validate(&self) -> Result<(), ArgError> {
        match self {
            Self::WallShow(a) => {
                check_request(&a.request_id)?;
                check_station(a.station.as_ref())?;
                if !word(&a.display_id, 64) {
                    return Err(bad("display_id"));
                }
                match &a.content {
                    WallContent::Session { session } => {
                        check_id(session.station_id.as_str(), "content.session.station_id")?;
                        check_id(session.session_id.as_str(), "content.session.session_id")
                    }
                    WallContent::Artifact { artifact } => check_artifact(artifact),
                }
            }
            Self::Notify(a) => {
                check_request(&a.request_id)?;
                check_station(a.station.as_ref())?;
                if !(1..=80).contains(&a.title.chars().count()) {
                    return Err(bad("title"));
                }
                if a.body.chars().count() > 300 {
                    return Err(bad("body"));
                }
                Ok(())
            }
            Self::PreviewOpen(a) => {
                check_request(&a.request_id)?;
                check_station(a.station.as_ref())?;
                if !(1..=2048).contains(&a.url.chars().count()) {
                    return Err(bad("url"));
                }
                a.monitor.as_deref().map_or(Ok(()), check_monitor)
            }
            Self::WindowClose(a) => {
                check_request(&a.request_id)?;
                check_station(a.station.as_ref())?;
                if word(&a.window_id, 64) {
                    Ok(())
                } else {
                    Err(bad("window_id"))
                }
            }
            Self::Screenshot(a) => {
                check_request(&a.request_id)?;
                check_station(a.station.as_ref())?;
                check_monitor(&a.monitor)
            }
            Self::RecordMonitor(a) => {
                check_request(&a.request_id)?;
                check_station(a.station.as_ref())?;
                check_monitor(&a.monitor)?;
                if (1..=120).contains(&a.seconds) {
                    Ok(())
                } else {
                    Err(bad("seconds"))
                }
            }
            Self::KeyPress(a) => a.validate(),
            Self::TypeText(a) => a.validate(),
            Self::PointerMove(a) => a.validate(),
        }
    }
}

impl DesktopQuery {
    /// Checks the patterns and lengths of §3.
    ///
    /// # Errors
    /// [`ArgError`] with the name of the first field that fails.
    pub fn validate(&self) -> Result<(), ArgError> {
        match self {
            Self::DesktopInfo(a) => check_station(a.station.as_ref()),
            Self::DesktopActionStatus(a) | Self::CaptureStatus(a) => check_request(&a.request_id),
            Self::ReadCaptureImage(a) => check_artifact(&a.artifact),
        }
    }
}

/// One monitor in a `desktop_info` result (§4.1): no description, serial or
/// refresh rate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct CoordinatorMonitor {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub scale_pct: u32,
    pub focused: bool,
    /// `None` when Hyprland does not say.
    pub dpms_on: Option<bool>,
    pub capture_allowed: bool,
}

/// What the station can do for a coordinator with the current classes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct CoordinatorCapabilities {
    pub notify: bool,
    pub preview: bool,
    pub screenshot: bool,
}

/// A wall display on a monitor (§4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct CoordinatorWall {
    pub display_id: String,
    pub monitor: String,
}

/// The result of `desktop_info` (§4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct CoordinatorDesktopInfo {
    pub station: String,
    pub kind: DesktopKind,
    pub monitors: Vec<CoordinatorMonitor>,
    pub locked: Option<bool>,
    pub capabilities: CoordinatorCapabilities,
    pub walls: Vec<CoordinatorWall>,
}

/// The state of a non-capture operation (§4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "coordinator_desktop/")]
pub enum ActionState {
    Unknown,
    Pending,
    Done,
    Failed,
}

/// The result of `desktop_action_status` (§4.8). `unknown` carries only the
/// request ID when the request is unknown, foreign or of another kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct DesktopActionStatus {
    pub request_id: String,
    pub state: ActionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub station: Option<String>,
    /// `{}`, `{window_id}` or `{generation}` for `done`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "Record<string, unknown>")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
}

/// The state of a capture (§4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "coordinator_desktop/")]
pub enum CaptureState {
    Unknown,
    Pending,
    Deleted,
    Committed,
    Failed,
}

/// The result of `capture_status` (§4.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct CoordinatorCaptureStatus {
    pub request_id: String,
    pub state: CaptureState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub capture_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub station: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub artifact: Option<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub monitor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
    /// R13 P5b-2 §13.4: the requested recording seconds; recordings only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub requested_seconds: Option<u32>,
    /// R13 P5b-2 §13.4: the saved recording duration in milliseconds;
    /// recordings only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub duration_ms: Option<u64>,
    /// R13 P5b-2 §13.4: the fixed stop reason; recordings only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stop_reason: Option<StopReason>,
}

/// The metadata block of `read_capture_image` (§4.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct CaptureImageMeta {
    pub artifact: ArtifactRef,
    pub capture_id: String,
    pub monitor: String,
    pub at: i64,
    pub size: u64,
    pub sha256: String,
}

/// The answer of `coordinator.desktop_query` for `read_capture_image`: the
/// metadata and the whole PNG in base64 (one frame, §4.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct CaptureImageAnswer {
    pub meta: CaptureImageMeta,
    pub png_base64: String,
}

#[cfg(test)]
#[path = "coordinator_desktop_tests.rs"]
mod tests;
