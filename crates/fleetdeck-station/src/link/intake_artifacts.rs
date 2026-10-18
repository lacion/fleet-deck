//! The `artifacts.*` link methods (R12, DL-165; R13 P5a `artifacts.delete`;
//! R13 P5b: the legacy projection for a link without `artifacts_video.v1`,
//! design `omarchy-p5b.md` §14.6 to §14.9).

use fleetdeck_link::control::Control;
use fleetdeck_proto::artifacts::{
    Artifact, ArtifactDetail, ArtifactKind, ArtifactList, ArtifactPublished, ArtifactVersion,
};
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::recording::project_legacy;
use fleetdeck_proto::version::feature;
use serde::Serialize;

use super::{NOT_IMPLEMENTED, bad_params, ok};
use crate::link::StationLink;

/// An artifact and one of its versions for a peer without
/// `artifacts_video.v1`: [`project_legacy`] on the pair.
fn project_pair(artifact: &mut Artifact, version: &mut ArtifactVersion) {
    let mut published = ArtifactPublished {
        artifact: artifact.clone(),
        version: version.clone(),
        created: false,
    };
    project_legacy(&mut published);
    *artifact = published.artifact;
    *version = published.version;
}

/// `artifacts.list` for a peer without `artifacts_video.v1`: kind `video`
/// becomes `other`.
fn project_list(list: &mut ArtifactList) {
    for artifact in &mut list.artifacts {
        if artifact.kind == ArtifactKind::Video {
            artifact.kind = ArtifactKind::Other;
        }
    }
}

/// `artifacts.get` for a peer without `artifacts_video.v1`: the artifact, the
/// version and every version of the list.
fn project_detail(detail: &mut ArtifactDetail) {
    project_pair(&mut detail.artifact, &mut detail.version);
    for version in &mut detail.versions {
        let mut artifact = detail.artifact.clone();
        project_pair(&mut artifact, version);
    }
}

/// The `artifacts.*` methods (R12, DL-165). A station without an artifact
/// directory answers `unsupported`; a service error keeps its code. A link
/// without `artifacts_video.v1` gets the legacy projection on `list`, `get` and
/// the result of `publish`; the stored rows never change.
pub(super) async fn artifact_method(
    ctx: &StationLink,
    control: &Control,
    method: &str,
    params: serde_json::Value,
) -> Outcome {
    use crate::artifacts::ArtifactError;
    use fleetdeck_proto::artifacts::code;
    fn done<T: Serialize>(r: Result<T, ArtifactError>) -> Outcome {
        match r {
            Ok(v) => ok(v),
            Err(e) => Outcome::Err(WireError::new(e.code, e.message)),
        }
    }
    let Some(service) = ctx.manager.artifacts() else {
        return Outcome::Err(WireError::new(
            code::UNSUPPORTED,
            "this station has no artifact directory",
        ));
    };
    let video = control.params().negotiated.has(feature::ARTIFACTS_VIDEO);
    match method {
        "artifacts.publish" => match serde_json::from_value(params) {
            Ok(q) => done(service.publish(q).await.map(|mut p| {
                if !video {
                    project_legacy(&mut p);
                }
                p
            })),
            Err(e) => bad_params(e),
        },
        "artifacts.list" => match serde_json::from_value(params) {
            Ok(q) => done(service.list(q).await.map(|mut l| {
                if !video {
                    project_list(&mut l);
                }
                l
            })),
            Err(e) => bad_params(e),
        },
        "artifacts.get" => match serde_json::from_value(params) {
            Ok(q) => done(service.get(q).await.map(|mut d| {
                if !video {
                    project_detail(&mut d);
                }
                d
            })),
            Err(e) => bad_params(e),
        },
        "artifacts.file" => match serde_json::from_value(params) {
            Ok(q) => done(service.file(q).await),
            Err(e) => bad_params(e),
        },
        "artifacts.delete" => match serde_json::from_value(params) {
            Ok(q) => done(service.delete(q).await),
            Err(e) => bad_params(e),
        },
        other => Outcome::Err(WireError::new(
            NOT_IMPLEMENTED,
            format!("`{other}` is not served by this station build"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use fleetdeck_proto::artifacts::{Artifact, ArtifactDetail, ArtifactKind, ArtifactVersion};
    use fleetdeck_proto::capture::{CaptureCaller, CaptureKind, CaptureProvenance};
    use fleetdeck_proto::recording::{RecordingMeta, StopReason};

    use super::*;

    fn artifact(kind: ArtifactKind) -> Artifact {
        Artifact {
            artifact_id: "A1".to_owned(),
            station_id: "s".to_owned(),
            session_id: None,
            title: "t".to_owned(),
            kind,
            latest_version: 1,
            created_at: 1,
            updated_at: 1,
            author: "owner".to_owned(),
        }
    }

    fn version(kind: CaptureKind) -> ArtifactVersion {
        ArtifactVersion {
            artifact_id: "A1".to_owned(),
            version: 1,
            size: 3,
            sha256: "ab".repeat(32),
            files: vec!["capture.mp4".to_owned()],
            created_at: 1,
            capture: Some(CaptureProvenance {
                capture_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
                station_id: "s".to_owned(),
                monitor: "DP-1".to_owned(),
                kind,
                caller: CaptureCaller::Owner,
                caller_station: None,
                caller_session: None,
                caller_grant_id: None,
                at: 1,
                recording: (kind == CaptureKind::Recording).then_some(RecordingMeta {
                    requested_seconds: 3,
                    duration_ms: 3000,
                    width: 2,
                    height: 2,
                    stop_reason: StopReason::Duration,
                }),
            }),
        }
    }

    #[test]
    fn a_video_list_becomes_other_for_a_legacy_peer() {
        let mut list = ArtifactList {
            artifacts: vec![artifact(ArtifactKind::Video), artifact(ArtifactKind::Image)],
        };
        project_list(&mut list);
        assert_eq!(list.artifacts[0].kind, ArtifactKind::Other);
        assert_eq!(list.artifacts[1].kind, ArtifactKind::Image);
    }

    #[test]
    fn a_recording_detail_loses_its_kind_and_provenance_for_a_legacy_peer() {
        let mut detail = ArtifactDetail {
            artifact: artifact(ArtifactKind::Video),
            version: version(CaptureKind::Recording),
            versions: vec![version(CaptureKind::Recording)],
        };
        project_detail(&mut detail);
        assert_eq!(detail.artifact.kind, ArtifactKind::Other);
        assert!(detail.version.capture.is_none());
        assert!(detail.versions[0].capture.is_none());
    }

    #[test]
    fn a_screenshot_detail_is_not_changed() {
        let mut detail = ArtifactDetail {
            artifact: artifact(ArtifactKind::Image),
            version: version(CaptureKind::Screenshot),
            versions: vec![version(CaptureKind::Screenshot)],
        };
        let before = detail.clone();
        project_detail(&mut detail);
        assert_eq!(detail, before);
    }
}
