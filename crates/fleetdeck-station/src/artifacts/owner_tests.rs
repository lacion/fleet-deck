//! A session's tool updates only the artifacts of that session (PRE-PR r3).

use fleetdeck_proto::SessionId;
use fleetdeck_proto::artifacts::{ArtifactListQuery, ArtifactPublish};
use fleetdeck_proto::event::ProviderKind;
use serde_json::{Value, json};

use super::fleet::StationFleet;
use super::service_tests::{Rig, publish, rig_with, write};
use crate::coordinator::{Caller, FleetTools};
use crate::store::{NewSession, OfflineApprovalPolicy};
use crate::workspace::PrivatePaths;

fn caller(session: &str) -> Caller {
    Caller {
        binding_id: format!("{session}:fleet"),
        grant_id: String::new(),
        session_id: session.into(),
        provider_instance: "claude".to_owned(),
        native_id: format!("native-{session}"),
        generation: 1,
        input_any: false,
    }
}

/// A rig with the sessions `s1` and `s2`, both in the workspace.
async fn rig_two() -> Rig {
    let r = rig_with(|_| PrivatePaths::default()).await;
    r.store
        .create_session(NewSession {
            session_id: "s2".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: r.work.display().to_string(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    r
}

fn content(name: &str, text: &str, id: Option<&str>) -> Value {
    match id {
        Some(id) => json!({"name": name, "content": text, "artifact_id": id}),
        None => json!({"name": name, "content": text}),
    }
}

#[tokio::test]
async fn a_session_tool_refuses_the_artifact_of_another_session_in_both_forms() {
    let r = rig_two().await;
    write(&r.work.join("page.html"), "<h1>victim</h1>");
    let tools = StationFleet::new(r.store.clone(), Some(r.service.clone()));
    let made = tools
        .call(
            caller("s1"),
            "publish_artifact",
            content("a.html", "<h1>s1</h1>", None),
        )
        .await
        .unwrap();
    let id = made["artifact_id"].as_str().unwrap().to_owned();
    // The inline form.
    let err = tools
        .call(
            caller("s2"),
            "publish_artifact",
            content("a.html", "<h1>s2</h1>", Some(&id)),
        )
        .await
        .unwrap_err();
    assert_eq!(err, "forbidden: the artifact belongs to another session");
    // The path form.
    let err = tools
        .call(
            caller("s2"),
            "publish_artifact",
            json!({"path": "page.html", "artifact_id": id}),
        )
        .await
        .unwrap_err();
    assert_eq!(err, "forbidden: the artifact belongs to another session");
    let listed = r.service.list(ArtifactListQuery::default()).await.unwrap();
    assert_eq!(listed.artifacts.len(), 1);
    assert_eq!(listed.artifacts[0].latest_version, 1);
    assert_eq!(listed.artifacts[0].session_id, Some(SessionId::from("s1")));
}

#[tokio::test]
async fn a_session_updates_its_own_artifact_and_the_board_updates_any() {
    let r = rig_two().await;
    write(&r.work.join("page.html"), "<h1>path</h1>");
    let tools = StationFleet::new(r.store.clone(), Some(r.service.clone()));
    let made = tools
        .call(
            caller("s1"),
            "publish_artifact",
            content("a.html", "<h1>one</h1>", None),
        )
        .await
        .unwrap();
    let id = made["artifact_id"].as_str().unwrap().to_owned();
    // The same session: both forms make the next version.
    let second = tools
        .call(
            caller("s1"),
            "publish_artifact",
            content("a.html", "<h1>two</h1>", Some(&id)),
        )
        .await
        .unwrap();
    assert_eq!(second["version"], 2);
    let third = tools
        .call(
            caller("s1"),
            "publish_artifact",
            json!({"path": "page.html", "artifact_id": id}),
        )
        .await
        .unwrap();
    assert_eq!(third["version"], 3);
    // The board publish passes no owner and updates any artifact.
    let board = r
        .service
        .publish(ArtifactPublish {
            artifact_id: Some(id.clone()),
            session_id: Some("s2".into()),
            ..publish("page.html")
        })
        .await;
    // Same bytes as version 3: no new version, but no refusal either.
    let board = board.unwrap();
    assert_eq!(board.artifact.artifact_id, id);
    assert!(!board.created);
    write(&r.work.join("page.html"), "<h1>board</h1>");
    let board = r
        .service
        .publish(ArtifactPublish {
            artifact_id: Some(id.clone()),
            session_id: Some("s2".into()),
            ..publish("page.html")
        })
        .await
        .unwrap();
    assert_eq!(board.version.version, 4);
    assert_eq!(board.artifact.session_id, Some(SessionId::from("s1")));
}
