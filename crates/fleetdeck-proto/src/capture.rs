//! R13 P5a: the owner screenshot (`screen_capture`) values (design
//! `omarchy-p5a.md` §2, §4.1, §6.1, §7.3).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::artifacts::ArtifactPublished;
use crate::recording::RecordingMeta;

/// The station wire codes; the HTTP `error` equals the code.
pub mod code {
    pub const BUSY: &str = "busy";
    pub const DISK_FULL: &str = "disk_full";
    pub const CAPTURE_QUOTA: &str = "capture_quota";
    pub const CAPTURE_TOO_LARGE: &str = "capture_too_large";
    pub const CAPTURE_DIMENSIONS: &str = "capture_dimensions";
    pub const CAPTURE_FAILED: &str = "capture_failed";
    pub const CAPTURE_PUBLISH_FAILED: &str = "capture_publish_failed";
    pub const SCREEN_LOCKED: &str = "screen_locked";
    pub const CAPTURE_ID_CONFLICT: &str = "capture_id_conflict";
    pub const CAPTURE_INTERRUPTED: &str = "capture_interrupted";
    // R13 P5b recordings (design `omarchy-p5b.md` §16).
    pub const RECORDING_DURATION: &str = "recording_duration";
    pub const RECORDING_TOO_LONG: &str = "recording_too_long";
    pub const RECORDING_DIMENSIONS: &str = "recording_dimensions";
    pub const RECORDING_TOO_LARGE: &str = "recording_too_large";
    pub const RECORDING_FAILED: &str = "recording_failed";
    pub const RECORDING_INVALID: &str = "recording_invalid";
    pub const RECORDING_PUBLISH_FAILED: &str = "recording_publish_failed";
    pub const RECORDING_CANCELLED: &str = "recording_cancelled";
    pub const RECORDING_INTERRUPTED: &str = "recording_interrupted";
    pub const NOT_RECORDING: &str = "not_recording";
    pub const CAPTURE_ID_REQUIRED: &str = "capture_id_required";
}

/// The exact texts.
pub mod text {
    pub const BUSY: &str = "A capture is already running.";
    pub const DISK_FULL: &str = "The station has too little free space for a capture.";
    pub const CAPTURE_QUOTA: &str =
        "This capture would exceed the station's 4 GiB capture limit. Delete some captures first.";
    pub const CAPTURE_TOO_LARGE: &str = "The screenshot exceeds 32 MiB.";
    pub const CAPTURE_DIMENSIONS: &str = "The monitor exceeds the screenshot size limit.";
    pub const CAPTURE_FAILED: &str = "The screenshot tool failed.";
    pub const CAPTURE_PUBLISH_FAILED: &str = "The screenshot could not be saved as an artifact.";
    pub const SCREEN_LOCKED: &str = "The screen is locked or its lock state is unknown.";
    pub const CAPTURE_ID_CONFLICT: &str = "This capture ID was used for another request.";
    pub const CAPTURE_INTERRUPTED: &str = "The screenshot was interrupted before it was saved.";
    pub const UNKNOWN: &str = "The screenshot result is unknown. Check the artifact list before taking another screenshot.";
    pub const NO_GRIM: &str = "The screenshot tool grim is not installed.";
    // R13 P5b recordings (design `omarchy-p5b.md` §16).
    pub const RECORDING_DURATION: &str = "The recording length must be from 1 to 3600 seconds.";
    pub const RECORDING_TOO_LONG: &str = "This recording is longer than this station allows. Shorten it, or raise Longest recording in the station settings.";
    pub const RECORDING_DIMENSIONS: &str =
        "The monitor exceeds the recording size limit or has unsupported dimensions.";
    pub const RECORDING_TOO_LARGE: &str = "The recording exceeds 256 MiB.";
    pub const RECORDING_FAILED: &str = "The recording tool failed.";
    pub const RECORDING_INVALID: &str = "The recording file could not be verified.";
    pub const RECORDING_PUBLISH_FAILED: &str = "The recording could not be saved as an artifact.";
    pub const RECORDING_CANCELLED: &str = "The recording was stopped before it started.";
    pub const RECORDING_INTERRUPTED: &str = "The recording was interrupted before it was saved.";
    pub const NOT_RECORDING: &str = "This capture is not a recording.";
    pub const CAPTURE_ID_REQUIRED: &str = "A recording needs a capture ID.";
    /// The missing-tool text: only when the executable is missing.
    pub const NO_RECORDER: &str = "The recording tool wf-recorder is not installed.";
    /// Every other unsupported recording capability.
    pub const RECORDING_UNSUPPORTED: &str = "This station does not support monitor recordings.";
}

/// The header of the board's capture ID.
pub const CAPTURE_ID_HEADER: &str = "X-FleetDeck-Capture-Id";

/// True for a canonical ULID: 26 characters of the uppercase Crockford
/// alphabet `0-9 A-H J K M N P-T V-Z` (no I, L, O, U), the first one `0`-`7`.
#[must_use]
pub fn valid_capture_id(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 26 || !(b'0'..=b'7').contains(&b[0]) {
        return false;
    }
    b.iter().all(|c| {
        matches!(
            c,
            b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z'
        )
    })
}

/// The result of a screenshot: the capture ID and the published artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "capture/")]
pub struct CaptureResult {
    pub capture_id: String,
    pub published: ArtifactPublished,
}

/// The durable state of one capture (`desktop.capture_status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export_to = "capture/")]
#[expect(
    clippy::large_enum_variant,
    reason = "a status lives only between decode and answer; boxing the published \
              artifact would change the field type of the wire shape"
)]
pub enum CaptureStatus {
    Pending {
        capture_id: String,
    },
    Unknown {
        capture_id: String,
    },
    Committed {
        capture_id: String,
        published: ArtifactPublished,
    },
    Failed {
        capture_id: String,
        error: String,
        message: String,
    },
    Deleted {
        capture_id: String,
    },
}

/// The params of `desktop.capture_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "capture/")]
pub struct CaptureStatusQuery {
    pub capture_id: String,
}

/// The kind of a capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "capture/")]
pub enum CaptureKind {
    Screenshot,
    /// R13 P5b: a monitor recording.
    Recording,
}

/// Who asked for a capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "capture/")]
pub enum CaptureCaller {
    Owner,
    /// R13 P6 §4.7: a coordinator session with a `capture` grant.
    Coordinator,
}

/// The immutable provenance of a screenshot artifact version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "capture/")]
pub struct CaptureProvenance {
    pub capture_id: String,
    pub station_id: String,
    pub monitor: String,
    pub kind: CaptureKind,
    pub caller: CaptureCaller,
    /// `null` for an owner capture; the stable ID of the coordinator's
    /// station for a coordinator capture (R13 P6 §4.7). Always serialized.
    #[ts(type = "string | null")]
    pub caller_station: Option<String>,
    /// The coordinator session (R13 P6); omitted for an owner capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub caller_session: Option<String>,
    /// The grant of the coordinator (R13 P6); omitted for an owner capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub caller_grant_id: Option<String>,
    /// Station Unix ms at capture start.
    #[ts(type = "number")]
    pub at: i64,
    /// R13 P5b: the metadata of a recording; omitted for a screenshot, so the
    /// pinned owner screenshot bytes stay unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recording: Option<RecordingMeta>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{Artifact, ArtifactKind, ArtifactVersion};

    fn published() -> ArtifactPublished {
        ArtifactPublished {
            artifact: Artifact {
                artifact_id: "A1".into(),
                station_id: "s".into(),
                session_id: None,
                title: "t".into(),
                kind: ArtifactKind::Image,
                latest_version: 1,
                created_at: 1,
                updated_at: 1,
                author: "owner".into(),
            },
            version: ArtifactVersion {
                artifact_id: "A1".into(),
                version: 1,
                size: 3,
                sha256: "ab".into(),
                files: vec!["capture.png".into()],
                created_at: 1,
                capture: None,
            },
            created: true,
        }
    }

    #[test]
    fn valid_capture_id_accepts_only_a_canonical_ulid() {
        assert!(valid_capture_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert!(!valid_capture_id("01arz3ndektsv4rrffq69g5fav"));
        assert!(!valid_capture_id("01ARZ3NDEKTSV4RRFFQ69G5FA"));
        assert!(!valid_capture_id("01ARZ3NDEKTSV4RRFFQ69G5FAVV"));
        assert!(!valid_capture_id("81ARZ3NDEKTSV4RRFFQ69G5FAV"));
        for bad in ['I', 'L', 'O', 'U'] {
            let s = format!("01ARZ3NDEKTSV4RRFFQ69G5FA{bad}");
            assert!(!valid_capture_id(&s), "{s}");
        }
        assert!(!valid_capture_id(""));
    }

    #[test]
    fn a_capture_status_has_the_exact_json_shape() {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned();
        let cases = [
            (
                CaptureStatus::Pending {
                    capture_id: id.clone(),
                },
                serde_json::json!({"state":"pending","capture_id":id}),
            ),
            (
                CaptureStatus::Unknown {
                    capture_id: id.clone(),
                },
                serde_json::json!({"state":"unknown","capture_id":id}),
            ),
            (
                CaptureStatus::Deleted {
                    capture_id: id.clone(),
                },
                serde_json::json!({"state":"deleted","capture_id":id}),
            ),
            (
                CaptureStatus::Failed {
                    capture_id: id.clone(),
                    error: code::CAPTURE_FAILED.into(),
                    message: text::CAPTURE_FAILED.into(),
                },
                serde_json::json!({
                    "state":"failed","capture_id":id,
                    "error":"capture_failed","message":"The screenshot tool failed."
                }),
            ),
            (
                CaptureStatus::Committed {
                    capture_id: id.clone(),
                    published: published(),
                },
                serde_json::json!({
                    "state":"committed","capture_id":id,
                    "published": serde_json::to_value(published()).unwrap()
                }),
            ),
        ];
        for (status, json) in cases {
            assert_eq!(serde_json::to_value(&status).unwrap(), json);
            assert_eq!(
                serde_json::from_value::<CaptureStatus>(json).unwrap(),
                status
            );
        }
    }

    #[test]
    fn a_status_query_refuses_unknown_fields() {
        assert!(serde_json::from_str::<CaptureStatusQuery>(r#"{"capture_id":"x","y":1}"#).is_err());
    }

    #[test]
    fn provenance_always_serializes_the_caller_station_as_null() {
        let p = CaptureProvenance {
            capture_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            station_id: "st".into(),
            monitor: "DP-1".into(),
            kind: CaptureKind::Screenshot,
            caller: CaptureCaller::Owner,
            caller_station: None,
            caller_session: None,
            caller_grant_id: None,
            at: 7,
            recording: None,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["caller_station"], serde_json::Value::Null);
        assert!(v.as_object().unwrap().contains_key("caller_station"));
        assert_eq!(v["kind"], "screenshot");
        assert_eq!(v["caller"], "owner");
        assert_eq!(serde_json::from_value::<CaptureProvenance>(v).unwrap(), p);
    }

    /// The bytes of an owner provenance, written down from the struct before
    /// the coordinator fields existed: an owner capture must not change.
    #[test]
    fn an_owner_provenance_serializes_to_its_pinned_bytes() {
        let owner = concat!(
            r#"{"capture_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","station_id":"st","#,
            r#""monitor":"DP-1","kind":"screenshot","caller":"owner","#,
            r#""caller_station":null,"at":7}"#
        );
        let p = CaptureProvenance {
            capture_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            station_id: "st".into(),
            monitor: "DP-1".into(),
            kind: CaptureKind::Screenshot,
            caller: CaptureCaller::Owner,
            caller_station: None,
            caller_session: None,
            caller_grant_id: None,
            at: 7,
            recording: None,
        };
        assert_eq!(serde_json::to_string(&p).unwrap(), owner);
        assert_eq!(serde_json::from_str::<CaptureProvenance>(owner).unwrap(), p);
    }

    #[test]
    fn a_coordinator_provenance_names_the_caller_station_session_and_grant() {
        let p = CaptureProvenance {
            capture_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            station_id: "st".into(),
            monitor: "DP-1".into(),
            kind: CaptureKind::Screenshot,
            caller: CaptureCaller::Coordinator,
            caller_station: Some("st-c".into()),
            caller_session: Some("01J0000000000000000000000S".into()),
            caller_grant_id: Some("g1".into()),
            at: 7,
            recording: None,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["caller"], "coordinator");
        assert_eq!(v["caller_station"], "st-c");
        assert_eq!(v["caller_session"], "01J0000000000000000000000S");
        assert_eq!(v["caller_grant_id"], "g1");
        assert_eq!(serde_json::from_value::<CaptureProvenance>(v).unwrap(), p);
        assert_eq!(
            serde_json::to_value(CaptureCaller::Coordinator).unwrap(),
            "coordinator"
        );
    }

    #[test]
    fn a_version_without_capture_has_no_capture_key() {
        let v = published().version;
        let json = serde_json::to_value(&v).unwrap();
        assert!(!json.as_object().unwrap().contains_key("capture"));
        let back: ArtifactVersion = serde_json::from_value(json).unwrap();
        assert_eq!(back, v);
    }
}
