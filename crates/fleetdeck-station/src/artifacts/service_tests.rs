//! The artifact service on real directories: bundling, versions, the path
//! walls, chunked reads, the suggestion and the session's own tools.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_proto::artifacts::{
    ArtifactFileQuery, ArtifactGetQuery, ArtifactKind, ArtifactListQuery, ArtifactPublish,
    MAX_CHUNK_BYTES, code,
};
use fleetdeck_proto::event::{Event, FileChange, ProviderKind};
use fleetdeck_proto::{EventBody, SessionId};
use fleetdeck_testkit::Det;
use serde_json::json;

use super::fleet::StationFleet;
use super::{ArtifactService, suggest};
use crate::coordinator::{Caller, FleetTools};
use crate::manager::policy::Policy;
use crate::manager::policy_hub::PolicyHub;
use crate::store::{NewSession, OfflineApprovalPolicy, Store, StoreConfig};
use crate::workspace::PrivatePaths;

const S1: &str = "s1";

pub(super) struct Rig {
    _tmp: tempfile::TempDir,
    pub(super) store: Store,
    pub(super) service: ArtifactService,
    /// The session workspace (canonical).
    pub(super) work: PathBuf,
    pub(super) root: PathBuf,
    /// The allowed root that holds the workspace and the station layout.
    pub(super) base: PathBuf,
}

async fn rig() -> Rig {
    rig_with(|_| PrivatePaths::default()).await
}

/// A rig whose private paths come from `private`, given the allowed root.
pub(super) async fn rig_with(private: impl FnOnce(&Path) -> PrivatePaths) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let work = base.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
        .create_session(NewSession {
            session_id: S1.into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: work.display().to_string(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    let policy = Arc::new(PolicyHub::new(Policy::new(
        vec![base.clone()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    )));
    let root = base.join("data/artifacts");
    let service = ArtifactService::new(
        store.clone(),
        policy,
        root.clone(),
        Arc::new(private(&base)),
    );
    Rig {
        _tmp: tmp,
        store,
        service,
        work,
        root,
        base,
    }
}

pub(super) fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

pub(super) fn publish(path: &str) -> ArtifactPublish {
    ArtifactPublish {
        session_id: Some(S1.into()),
        path: path.to_owned(),
        title: None,
        artifact_id: None,
        author: None,
    }
}

#[tokio::test]
async fn an_html_page_is_bundled_with_the_local_files_that_it_names() {
    let r = rig().await;
    write(
        &r.work.join("out/report.html"),
        r#"<link href="style.css"><img src="img/a.png"><script src="https://cdn.example/x.js"></script>
           <img src="../outside.png"><img src="link.png"><img src="/abs.png"><a href="missing.html">m</a>"#,
    );
    write(
        &r.work.join("out/style.css"),
        "body { background: url(bg.png) }",
    );
    write(&r.work.join("out/img/a.png"), "PNG");
    write(&r.work.join("out/bg.png"), "not followed: one level only");
    write(&r.work.join("outside.png"), "outside the page directory");
    let secret = r.work.join("secret.png");
    write(&secret, "secret");
    std::os::unix::fs::symlink(&secret, r.work.join("out/link.png")).unwrap();
    let got = r.service.publish(publish("out/report.html")).await.unwrap();
    assert!(got.created);
    assert_eq!(got.artifact.kind, ArtifactKind::Html);
    assert_eq!(got.artifact.title, "report.html");
    assert_eq!(got.artifact.author, "owner");
    assert_eq!(got.artifact.latest_version, 1);
    assert_eq!(got.artifact.session_id, Some(S1.into()));
    assert_eq!(
        got.version.files,
        ["report.html", "img/a.png", "style.css"],
        "the main file first, then the named files; no symlink, no escape"
    );
    let dir = r.root.join(&got.artifact.artifact_id).join("1");
    assert_eq!(
        std::fs::read_to_string(dir.join("img/a.png")).unwrap(),
        "PNG"
    );
    // The page keeps its relative links.
    let html = std::fs::read_to_string(dir.join("report.html")).unwrap();
    assert!(html.contains(r#"src="img/a.png""#));
    assert!(!dir.join("bg.png").exists());
    assert!(!dir.join("link.png").exists());
    // The session counts the artifact once the event is folded.
    let summary = r.store.summary(S1.into()).await.unwrap().unwrap();
    assert_eq!(summary.artifacts_count, 1);
}

#[tokio::test]
async fn identical_content_makes_no_version_and_a_change_makes_the_next_one() {
    let r = rig().await;
    let page = r.work.join("a.html");
    write(&page, "<p>one</p>");
    let first = r.service.publish(publish("a.html")).await.unwrap();
    let again = r.service.publish(publish("a.html")).await.unwrap();
    assert!(
        first.created && again.created,
        "no artifact id: a new artifact"
    );
    let id = first.artifact.artifact_id.clone();
    let same = ArtifactPublish {
        artifact_id: Some(id.clone()),
        ..publish("a.html")
    };
    let unchanged = r.service.publish(same.clone()).await.unwrap();
    assert!(!unchanged.created);
    assert_eq!(unchanged.version.version, 1);
    write(&page, "<p>two</p>");
    let second = r.service.publish(same).await.unwrap();
    assert!(second.created);
    assert_eq!(second.version.version, 2);
    assert_eq!(second.artifact.latest_version, 2);
    assert_eq!(second.artifact.title, "a.html", "the title stays");
    // The old version stays and both are readable.
    let detail = r
        .service
        .get(ArtifactGetQuery {
            artifact_id: id.clone(),
            version: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(detail.version.version, 1);
    assert_eq!(detail.versions.len(), 2);
    assert_eq!(detail.artifact.latest_version, 2);
    let chunk = |version| ArtifactFileQuery {
        artifact_id: id.clone(),
        version,
        path: "a.html".to_owned(),
        offset: None,
        length: None,
    };
    let v1 = r.service.file(chunk(1)).await.unwrap();
    assert_eq!(STANDARD.decode(v1.data_base64).unwrap(), b"<p>one</p>");
    let v2 = r.service.file(chunk(2)).await.unwrap();
    assert_eq!(STANDARD.decode(v2.data_base64).unwrap(), b"<p>two</p>");
    // `artifact.published` of a version 2 does not count a new artifact.
    let summary = r.store.summary(S1.into()).await.unwrap().unwrap();
    assert_eq!(summary.artifacts_count, 2);
    // The list is newest first and filters by session.
    let all = r.service.list(ArtifactListQuery::default()).await.unwrap();
    assert_eq!(all.artifacts.len(), 2);
    let none = r
        .service
        .list(ArtifactListQuery {
            session_id: Some("other".into()),
        })
        .await
        .unwrap();
    assert!(none.artifacts.is_empty());
}

#[tokio::test]
async fn a_path_outside_the_walls_or_a_bad_input_is_refused_with_its_code() {
    let r = rig().await;
    write(&r.work.join("ok.md"), "# hi");
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("o.html"), "x");
    let target = r.work.join("real.html");
    write(&target, "<p/>");
    std::os::unix::fs::symlink(&target, r.work.join("sym.html")).unwrap();
    write(&r.work.join(".git/page.html"), "hidden");
    let code_of = |e: super::ArtifactError| e.code;
    let cases: Vec<(ArtifactPublish, &str)> = vec![
        (
            publish(&outside.path().join("o.html").display().to_string()),
            code::OUTSIDE_ROOTS,
        ),
        (publish("../escape.html"), code::INVALID_PATH),
        (publish(""), code::INVALID_PATH),
        (publish("nope.html"), code::NOT_FOUND),
        (publish("sym.html"), code::NOT_FOUND),
        (publish(".git/page.html"), code::NOT_FOUND),
        (
            ArtifactPublish {
                session_id: None,
                ..publish("ok.md")
            },
            code::INVALID_PATH,
        ),
        (
            ArtifactPublish {
                session_id: Some("ghost".into()),
                ..publish("ok.md")
            },
            code::UNKNOWN_SESSION,
        ),
        (
            ArtifactPublish {
                artifact_id: Some("ghost".into()),
                ..publish("ok.md")
            },
            code::UNKNOWN_ARTIFACT,
        ),
        (
            ArtifactPublish {
                title: Some("bad\ntitle".into()),
                ..publish("ok.md")
            },
            code::INVALID_PARAMS,
        ),
        (
            ArtifactPublish {
                author: Some(String::new()),
                ..publish("ok.md")
            },
            code::INVALID_PARAMS,
        ),
    ];
    for (i, (req, want)) in cases.into_iter().enumerate() {
        let got = r.service.publish(req).await.map(|_| ()).map_err(code_of);
        assert_eq!(got, Err(want), "case {i}");
    }
    // Nothing of a refused publish stays in the artifact directory.
    let left: Vec<_> = std::fs::read_dir(r.root.join(".staging"))
        .map(|d| d.collect())
        .unwrap_or_default();
    assert!(left.is_empty());
    assert!(
        r.service
            .list(ArtifactListQuery::default())
            .await
            .unwrap()
            .artifacts
            .is_empty()
    );
    // A person without a session publishes an absolute path in the roots.
    let ok = r
        .service
        .publish(ArtifactPublish {
            session_id: None,
            author: Some("ana".into()),
            title: Some("  Notes  ".into()),
            ..publish(&r.work.join("ok.md").display().to_string())
        })
        .await
        .unwrap();
    assert_eq!(ok.artifact.kind, ArtifactKind::Markdown);
    assert_eq!(ok.artifact.title, "Notes");
    assert_eq!(ok.artifact.author, "ana");
    assert_eq!(ok.artifact.session_id, None);
}

#[tokio::test]
async fn a_large_file_is_read_in_chunks_and_a_bad_range_is_refused() {
    let r = rig().await;
    let size = usize::try_from(MAX_CHUNK_BYTES).unwrap() * 2 + 123;
    let bytes: Vec<u8> = (0..size).map(|i| u8::try_from(i % 251).unwrap()).collect();
    std::fs::write(r.work.join("big.pdf"), &bytes).unwrap();
    let got = r.service.publish(publish("big.pdf")).await.unwrap();
    assert_eq!(got.artifact.kind, ArtifactKind::Pdf);
    assert_eq!(got.version.size, size as u64);
    let id = got.artifact.artifact_id;
    let mut out = Vec::new();
    let mut offset = 0;
    loop {
        let chunk = r
            .service
            .file(ArtifactFileQuery {
                artifact_id: id.clone(),
                version: 1,
                path: "big.pdf".to_owned(),
                offset: Some(offset),
                length: None,
            })
            .await
            .unwrap();
        assert_eq!(chunk.size, size as u64);
        assert!(chunk.length <= MAX_CHUNK_BYTES);
        out.extend(STANDARD.decode(chunk.data_base64).unwrap());
        offset += chunk.length;
        if chunk.eof {
            break;
        }
    }
    assert_eq!(out, bytes);
    let query = |path: &str, offset: Option<u64>, length: Option<u64>, version| ArtifactFileQuery {
        artifact_id: id.clone(),
        version,
        path: path.to_owned(),
        offset,
        length,
    };
    for (q, want) in [
        (
            query("big.pdf", Some(size as u64 + 1), None, 1),
            code::INVALID_PARAMS,
        ),
        (query("big.pdf", None, Some(0), 1), code::INVALID_PARAMS),
        (
            query("big.pdf", None, Some(MAX_CHUNK_BYTES + 1), 1),
            code::INVALID_PARAMS,
        ),
        (query("../big.pdf", None, None, 1), code::NOT_FOUND),
        (query("other.pdf", None, None, 1), code::NOT_FOUND),
        (query("big.pdf", None, None, 9), code::UNKNOWN_ARTIFACT),
    ] {
        assert_eq!(r.service.file(q).await.unwrap_err().code, want);
    }
    let missing = ArtifactFileQuery {
        artifact_id: "ghost".to_owned(),
        ..query("big.pdf", None, None, 1)
    };
    assert_eq!(
        r.service.file(missing).await.unwrap_err().code,
        code::UNKNOWN_ARTIFACT
    );
}

#[tokio::test]
async fn a_suggestion_is_made_once_for_each_path_and_session() {
    let r = rig().await;
    let change = |path: &str| Event::FileChanged {
        path: path.to_owned(),
        change: FileChange::Added,
        node_id: None,
    };
    let session: SessionId = S1.into();
    let events = [
        change("out/a.html"),
        change("node_modules/x/b.html"),
        change("c.rs"),
    ];
    suggest::observe(&r.store, &session, &events).await;
    suggest::observe(
        &r.store,
        &session,
        &[change("out/a.html"), change("out/d.html")],
    )
    .await;
    let suggested: Vec<String> = match r.store.read_after(0, 100).await.unwrap() {
        crate::store::ReadResult::Events { events, .. } => events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(Event::ArtifactSuggested { path }) => Some(path),
                _ => None,
            })
            .collect(),
        crate::store::ReadResult::SnapshotRequired { .. } => panic!("snapshot"),
    };
    assert_eq!(suggested, ["out/a.html", "out/d.html"]);
}

fn caller() -> Caller {
    Caller {
        binding_id: "s1:fleet".to_owned(),
        grant_id: String::new(),
        session_id: S1.into(),
        provider_instance: "claude".to_owned(),
        native_id: "native-1".to_owned(),
        generation: 1,
    }
}

#[tokio::test]
async fn the_session_tools_publish_as_the_agent_set_the_focus_and_have_no_coordinator() {
    let r = rig().await;
    write(&r.work.join("plan.html"), "<h1>plan</h1>");
    let tools = StationFleet::new(r.store.clone(), Some(r.service.clone()));
    let out = tools
        .call(
            caller(),
            "publish_artifact",
            json!({"path": "plan.html", "title": "Plan"}),
        )
        .await
        .unwrap();
    assert_eq!(out["version"], 1);
    assert_eq!(out["title"], "Plan");
    let listed = r.service.list(ArtifactListQuery::default()).await.unwrap();
    assert_eq!(listed.artifacts[0].author, "agent");
    assert_eq!(listed.artifacts[0].session_id, Some(S1.into()));
    // An argument that is not in the schema is refused, not ignored.
    let forged = tools
        .call(
            caller(),
            "publish_artifact",
            json!({"path": "plan.html", "session_id": "victim"}),
        )
        .await;
    assert!(forged.unwrap_err().starts_with("invalid_params"));
    // The focus is an event that the fold shows in the summary.
    tools
        .call(
            caller(),
            "set_focus",
            json!({"text": "  fixing the login test "}),
        )
        .await
        .unwrap();
    let summary = r.store.summary(S1.into()).await.unwrap().unwrap();
    assert_eq!(summary.focus.as_deref(), Some("fixing the login test"));
    let long = "x".repeat(121);
    for bad in [json!({"text": long}), json!({"text": "a\nb"}), json!({})] {
        let err = tools.call(caller(), "set_focus", bad).await.unwrap_err();
        assert!(err.starts_with("invalid_params"), "{err}");
    }
    tools
        .call(caller(), "set_focus", json!({"text": ""}))
        .await
        .unwrap();
    let summary = r.store.summary(S1.into()).await.unwrap().unwrap();
    assert_eq!(summary.focus, None);
    // `ask_coordinator` is not answered here: the coordinator endpoint
    // forwards it to the deck (R2.3).
    let err = tools
        .call(
            caller(),
            "ask_coordinator",
            json!({"question": "which branch?"}),
        )
        .await
        .unwrap_err();
    assert!(err.starts_with("forbidden"), "{err}");
    let err = tools
        .call(caller(), "list_sessions", json!({}))
        .await
        .unwrap_err();
    assert!(err.starts_with("forbidden"));
    // A station without an artifact directory refuses a publish.
    let bare = StationFleet::new(r.store.clone(), None);
    let err = bare
        .call(caller(), "publish_artifact", json!({"path": "plan.html"}))
        .await;
    assert!(err.unwrap_err().starts_with("unsupported"));
}
