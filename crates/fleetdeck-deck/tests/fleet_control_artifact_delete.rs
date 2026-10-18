//! The coordinator reads of artifacts after a deletion (R13 P5a task C): the
//! index leaves out a deleted artifact, a read of it is refused before the
//! station is asked, and a deletion during the station read refuses the
//! answer.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::fleet::*;
use common::*;
use fleetdeck_proto::RejectCode;
use fleetdeck_proto::artifacts::{Artifact, ArtifactDetail, ArtifactKind, ArtifactVersion};
use fleetdeck_proto::mutation::CoordinatorQuery;
use serde_json::{Value, json};

fn note(id: &str) -> Artifact {
    Artifact {
        artifact_id: id.to_owned(),
        station_id: String::new(),
        session_id: Some(s("w1")),
        title: format!("Title of {id}"),
        kind: ArtifactKind::Markdown,
        latest_version: 1,
        created_at: 5,
        updated_at: 5,
        author: "agent".to_owned(),
    }
}

fn read(id: &str) -> CoordinatorQuery {
    CoordinatorQuery::Artifact {
        artifact_id: id.to_owned(),
        version: None,
    }
}

/// The artifact ids of the answer of an `artifacts` query.
fn ids(answer: &Value) -> Vec<String> {
    answer["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["artifact_id"].as_str().unwrap().to_owned())
        .collect()
}

/// A station that holds `doc` (one text file) and, when `hook` is set, runs
/// it when it serves the first request.
fn station(
    mut hook: Option<Box<dyn FnMut()>>,
) -> impl FnMut(&str, &Value) -> fleetdeck_proto::link::Outcome {
    move |method, params| {
        if let Some(mut run) = hook.take() {
            run();
        }
        let version = ArtifactVersion {
            artifact_id: "doc".to_owned(),
            version: 1,
            size: 0,
            sha256: String::new(),
            files: vec!["a.md".to_owned()],
            created_at: 1,
            capture: None,
        };
        if method == "artifacts.get" {
            return ok(json!(ArtifactDetail {
                artifact: note("doc"),
                version: version.clone(),
                versions: vec![version],
            }));
        }
        assert_eq!(method, "artifacts.file");
        let text = b"hello";
        let offset = usize::try_from(params["offset"].as_u64().unwrap()).unwrap();
        ok(json!({"path": "a.md", "size": text.len(), "offset": offset,
            "length": text.len() - offset, "data_base64": STANDARD.encode(&text[offset..]),
            "eof": true}))
    }
}

#[tokio::test]
async fn a_deleted_artifact_leaves_the_coordinator_index_and_its_read_is_refused_before_the_station()
 {
    let mut w = world().await;
    let b = bound(&w.r.f, "coord-1", &CLASSES, true).await;
    let (store, st) = (w.r.f.store.clone(), w.r.f.station.clone());
    for id in ["keep", "doc"] {
        store.note_artifact(st.clone(), &note(id)).await.unwrap();
    }
    let all = || CoordinatorQuery::Artifacts { session_id: None };
    assert_eq!(ids(&ask(&w.r.f, &b, all()).await.unwrap()), ["keep", "doc"]);
    store
        .artifact_delete(st.clone(), "doc".to_owned())
        .await
        .unwrap();
    assert_eq!(ids(&ask(&w.r.f, &b, all()).await.unwrap()), ["keep"]);
    let session = CoordinatorQuery::Artifacts {
        session_id: Some(s("w1")),
    };
    assert_eq!(ids(&ask(&w.r.f, &b, session).await.unwrap()), ["keep"]);
    let refused = refusal(&w.r.f, &b, read("doc")).await;
    assert_eq!(refused.code, RejectCode::UnknownRequest);
    // The station was not asked.
    no_request(&mut w.r.station, std::time::Duration::from_millis(200)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deletion_during_the_station_read_refuses_the_coordinator_answer() {
    let mut w = world().await;
    let b = bound(&w.r.f, "coord-1", &CLASSES, true).await;
    let (store, st) = (w.r.f.store.clone(), w.r.f.station.clone());
    store.note_artifact(st.clone(), &note("doc")).await.unwrap();
    // A read of an artifact that is not deleted is answered.
    let (got, log) = ask_link(&mut w.r, &b, read("doc"), station(None)).await;
    assert!(got.get("reject").is_none(), "{got}");
    assert_eq!(got["files"][0]["path"], "a.md");
    assert!(!log.is_empty());
    // The station deletes the artifact while it serves the read.
    let hook: Box<dyn FnMut()> = {
        let (store, st) = (store.clone(), st.clone());
        Box::new(move || {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(store.artifact_delete(st.clone(), "doc".to_owned()))
                    .unwrap();
            });
        })
    };
    let (got, log) = ask_link(&mut w.r, &b, read("doc"), station(Some(hook))).await;
    assert!(!log.is_empty());
    let reject = rejection(&got);
    assert_eq!(reject.code, RejectCode::UnknownRequest, "{got}");
    assert!(got.get("files").is_none(), "{got}");
}
