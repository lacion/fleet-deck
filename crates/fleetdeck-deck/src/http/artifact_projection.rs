//! The version projection of the `video` kind for an older board (R13 P5b
//! task K2, design `omarchy-p5b.md` §11.1, §14): a request or a socket without
//! `artifacts_video.v1` sees kind `other` and no recording provenance.
//!
//! - [`video_aware`] reads the request header `X-FleetDeck-Features` (a comma
//!   or space separated token list; only `artifacts_video.v1` matters).
//!   [`video_aware_socket`] adds the query parameter `features=` of a
//!   WebSocket URL, because a browser cannot set a header on a WebSocket. The
//!   declaration grants no authority.
//! - The `project_*` functions change the answer at send time. A stored row or
//!   event never changes (§14.9). The raw and range byte routes are not
//!   projected: they serve bytes.

use axum::http::HeaderMap;
use fleetdeck_proto::EventEnvelope;
use fleetdeck_proto::artifacts::{
    ArtifactKind, ArtifactPublished, ArtifactVersion, DeckArtifact, DeckArtifactDetail,
};
use fleetdeck_proto::capture::CaptureKind;
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::recording::project_legacy;
use fleetdeck_proto::version::feature;

/// The header with which a board declares the features it understands.
pub(crate) const FEATURES_HEADER: &str = "X-FleetDeck-Features";

/// The query parameter of a WebSocket URL with the same token list.
const FEATURES_PARAM: &str = "features";

/// True when a token list (comma, space or tab separated) holds
/// `artifacts_video.v1`.
fn declares_video(list: &str) -> bool {
    list.split([',', ' ', '\t'])
        .any(|token| token == feature::ARTIFACTS_VIDEO)
}

/// True when the request declares `artifacts_video.v1` (a comma or space
/// separated token list; the other tokens do not matter).
#[must_use]
pub(crate) fn video_aware(headers: &HeaderMap) -> bool {
    headers
        .get_all(FEATURES_HEADER)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(declares_video)
}

/// True when the upgrade request of a WebSocket declares `artifacts_video.v1`:
/// in the header (a non-browser client) or in the query parameter `features=`.
#[must_use]
pub(crate) fn video_aware_socket(headers: &HeaderMap, query: &[(String, String)]) -> bool {
    video_aware(headers)
        || query
            .iter()
            .any(|(k, v)| k == FEATURES_PARAM && declares_video(v))
}

/// An artifact of the index for a board without the feature: kind `video`
/// becomes `other`.
pub(crate) fn project_artifact(a: &mut DeckArtifact) {
    if a.kind == ArtifactKind::Video {
        a.kind = ArtifactKind::Other;
    }
}

/// The artifacts of the list (see [`project_artifact`]).
pub(crate) fn project_list(v: &mut [DeckArtifact]) {
    v.iter_mut().for_each(project_artifact);
}

/// A version for a board without the feature: the capture provenance of a
/// recording is dropped.
pub(crate) fn project_version(v: &mut ArtifactVersion) {
    if v.capture
        .as_ref()
        .is_some_and(|c| c.kind == CaptureKind::Recording)
    {
        v.capture = None;
    }
}

/// The answer of `GET /api/artifacts/{station}/{id}`: the index row and every
/// version.
pub(crate) fn project_detail(d: &mut DeckArtifactDetail) {
    project_artifact(&mut d.artifact);
    d.versions.iter_mut().for_each(project_version);
}

/// The answer of a publish: [`project_legacy`] of the proto crate.
pub(crate) fn project_published(p: &mut ArtifactPublished) {
    project_legacy(p);
}

/// The `artifact.published` events of a timeline batch for a socket without
/// the feature: `artifact_kind` `video` becomes `other`.
pub(crate) fn project_events(events: &mut [EventEnvelope]) {
    for e in events {
        if let EventBody::Known(Event::ArtifactPublished { artifact_kind, .. }) = &mut e.event
            && *artifact_kind == ArtifactKind::Video
        {
            *artifact_kind = ArtifactKind::Other;
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use fleetdeck_proto::capture::{CaptureCaller, CaptureProvenance};
    use fleetdeck_proto::recording::{RecordingMeta, StopReason};

    use super::*;

    fn with(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append(FEATURES_HEADER, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn deck_artifact(kind: ArtifactKind) -> DeckArtifact {
        DeckArtifact {
            station_id: "s".to_owned(),
            station_name: "alpha".to_owned(),
            artifact_id: "A1".to_owned(),
            session_id: None,
            title: "t".to_owned(),
            kind,
            latest_version: 1,
            updated_at: 1,
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

    fn envelope(kind: ArtifactKind) -> EventEnvelope {
        EventEnvelope {
            station_seq: 1,
            session_id: "s1".into(),
            session_seq: 1,
            at: 1,
            proto_version: 1,
            raw_ref: None,
            event: EventBody::Known(Event::ArtifactPublished {
                artifact_id: "A1".to_owned(),
                version: 1,
                title: "t".to_owned(),
                artifact_kind: kind,
            }),
        }
    }

    fn kind_of(e: &EventEnvelope) -> ArtifactKind {
        match &e.event {
            EventBody::Known(Event::ArtifactPublished { artifact_kind, .. }) => *artifact_kind,
            other => panic!("not an artifact.published event: {other:?}"),
        }
    }

    #[test]
    fn only_the_token_artifacts_video_declares_the_video_feature() {
        assert!(video_aware(&with(&["artifacts_video.v1"])));
        assert!(video_aware(&with(&[
            "desktop_capture.v1, artifacts_video.v1"
        ])));
        assert!(video_aware(&with(&["a.v1 artifacts_video.v1 b.v1"])));
        assert!(video_aware(&with(&["a.v1", "artifacts_video.v1"])));
        for none in [
            vec![],
            vec![""],
            vec!["artifacts_video"],
            vec!["artifacts_video.v2"],
            vec!["xartifacts_video.v1"],
            vec!["desktop_recording.v1"],
        ] {
            assert!(!video_aware(&with(&none)), "{none:?}");
        }
    }

    #[test]
    fn a_socket_declares_the_feature_in_the_header_or_the_query() {
        let q = |k: &str, v: &str| vec![(k.to_owned(), v.to_owned())];
        let none = HeaderMap::new();
        assert!(video_aware_socket(&with(&["artifacts_video.v1"]), &[]));
        assert!(video_aware_socket(
            &none,
            &q("features", "a.v1,artifacts_video.v1")
        ));
        assert!(video_aware_socket(
            &none,
            &q("features", "a.v1 artifacts_video.v1")
        ));
        assert!(!video_aware_socket(&none, &[]));
        assert!(!video_aware_socket(&none, &q("features", "a.v1")));
        assert!(!video_aware_socket(
            &none,
            &q("ticket", "artifacts_video.v1")
        ));
    }

    #[test]
    fn a_video_artifact_becomes_other_and_the_rest_stays() {
        let mut list = vec![
            deck_artifact(ArtifactKind::Video),
            deck_artifact(ArtifactKind::Image),
        ];
        project_list(&mut list);
        assert_eq!(list[0].kind, ArtifactKind::Other);
        assert_eq!(list[1].kind, ArtifactKind::Image);
    }

    #[test]
    fn a_recording_detail_loses_its_kind_and_provenance() {
        let mut detail = DeckArtifactDetail {
            artifact: deck_artifact(ArtifactKind::Video),
            versions: vec![version(CaptureKind::Recording)],
            shares: Vec::new(),
        };
        project_detail(&mut detail);
        assert_eq!(detail.artifact.kind, ArtifactKind::Other);
        assert!(detail.versions[0].capture.is_none());
    }

    #[test]
    fn a_screenshot_detail_is_not_changed() {
        let mut detail = DeckArtifactDetail {
            artifact: deck_artifact(ArtifactKind::Image),
            versions: vec![version(CaptureKind::Screenshot)],
            shares: Vec::new(),
        };
        let before = detail.clone();
        project_detail(&mut detail);
        assert_eq!(detail, before);
    }

    #[test]
    fn a_published_recording_becomes_other_without_provenance() {
        let artifact = fleetdeck_proto::artifacts::Artifact {
            artifact_id: "A1".to_owned(),
            station_id: "s".to_owned(),
            session_id: None,
            title: "t".to_owned(),
            kind: ArtifactKind::Video,
            latest_version: 1,
            created_at: 1,
            updated_at: 1,
            author: "owner".to_owned(),
        };
        let mut p = ArtifactPublished {
            artifact,
            version: version(CaptureKind::Recording),
            created: true,
        };
        project_published(&mut p);
        assert_eq!(p.artifact.kind, ArtifactKind::Other);
        assert!(p.version.capture.is_none());
    }

    #[test]
    fn a_video_event_becomes_other_and_other_events_stay() {
        let mut batch = vec![envelope(ArtifactKind::Video), envelope(ArtifactKind::Html)];
        project_events(&mut batch);
        assert_eq!(kind_of(&batch[0]), ArtifactKind::Other);
        assert_eq!(kind_of(&batch[1]), ArtifactKind::Html);
    }
}
