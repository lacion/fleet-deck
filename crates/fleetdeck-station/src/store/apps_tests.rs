//! The stored MCP Apps: the row and its event, the caps, the lookup and the
//! document chunks (B15).

use fleetdeck_proto::EventBody;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_testkit::Det;
use serde_json::json;

use super::*;
use crate::store::{NewSession, OfflineApprovalPolicy, ReadResult, StoreConfig};

const S1: &str = "s1";

async fn session(s: &Store, id: &str) {
    s.create_session(NewSession {
        session_id: id.into(),
        provider: ProviderKind::Codex,
        provider_instance: "codex".into(),
        cwd: "/w".to_owned(),
        worktree: None,
        branch: None,
        mode: None,
        title: None,
        offline_approval_policy: OfflineApprovalPolicy::Wait,
    })
    .await
    .unwrap();
}

async fn store(det: &Det) -> Store {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    session(&s, S1).await;
    s
}

fn app(session: &str, html: &[u8]) -> (NewApp, ToolAppEvent) {
    (
        NewApp {
            session_id: session.into(),
            html: html.to_vec(),
            csp: AppCsp {
                connect_domains: vec!["https://api.example.com".to_owned()],
                resource_domains: Vec::new(),
            },
            tool_input: json!({"n": 1}),
            tool_result: Some(json!({"content": [{"type": "text", "text": "2"}]})),
        },
        ToolAppEvent {
            node_id: "node-1".into(),
            server: "demo".to_owned(),
            tool: "show_counter".to_owned(),
            resource_uri: "ui://demo/counter.html".to_owned(),
            prefers_border: Some(true),
        },
    )
}

async fn put(s: &Store, session: &str, html: &[u8]) -> AppId {
    let (new, event) = app(session, html);
    s.insert_app(new, event).await.unwrap()
}

async fn tool_app_events(s: &Store) -> Vec<Event> {
    let ReadResult::Events { events, .. } = s.read_after(0, 10_000).await.unwrap() else {
        panic!("snapshot required");
    };
    events
        .into_iter()
        .filter_map(|e| match e.event {
            EventBody::Known(ev) if ev.kind_name() == "tool.app" => Some(ev),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn insert_app_writes_row_and_event_in_one_transaction() {
    let det = Det::new();
    let s = store(&det).await;
    let id = put(&s, S1, b"<html>x</html>").await;
    let AppLookup::Found(meta) = s.app_lookup(S1.into(), id.clone()).await.unwrap() else {
        panic!("the row is missing");
    };
    assert_eq!(meta.app_id, id);
    assert_eq!(meta.node_id.as_str(), "node-1");
    assert_eq!(
        (meta.server.as_str(), meta.tool.as_str()),
        ("demo", "show_counter")
    );
    assert_eq!(meta.resource_uri, "ui://demo/counter.html");
    assert_eq!(meta.prefers_border, Some(true));
    assert_eq!(meta.doc_bytes, 14);
    assert_eq!(meta.doc_sha256.len(), 64);
    assert_eq!(meta.csp.connect_domains, ["https://api.example.com"]);
    assert_eq!(meta.tool_input, json!({"n": 1}));
    assert!(meta.tool_result.is_some());
    let events = tool_app_events(&s).await;
    assert_eq!(events.len(), 1);
    let Event::ToolApp {
        app_id,
        node_id,
        prefers_border,
        ..
    } = &events[0]
    else {
        panic!("not a tool.app event");
    };
    assert_eq!(
        (app_id, node_id.as_str(), *prefers_border),
        (&id, "node-1", Some(true))
    );

    // A session that does not exist: the event fails and the row is not kept.
    let (new, event) = app("nope", b"<html>y</html>");
    let err = s.insert_app(new, event).await.unwrap_err();
    assert!(matches!(err, StoreError::UnknownSession(_)), "{err}");
    assert_eq!(s.app_count().await.unwrap(), 1);
}

#[tokio::test]
async fn oversized_input_and_result_are_not_stored() {
    let det = Det::new();
    let s = store(&det).await;
    let (mut new, event) = app(S1, b"<html>x</html>");
    new.tool_input = json!({"big": "x".repeat(MAX_APP_INPUT_BYTES)});
    new.tool_result = Some(json!({"big": "x".repeat(MAX_APP_RESULT_BYTES)}));
    let id = s.insert_app(new, event).await.unwrap();
    let AppLookup::Found(meta) = s.app_lookup(S1.into(), id).await.unwrap() else {
        panic!("the row is missing");
    };
    assert_eq!(meta.tool_input, Value::Null);
    assert_eq!(meta.tool_result, None);
}

#[tokio::test]
async fn caps_delete_oldest_rows_per_session_and_station() {
    let det = Det::new();
    let s = store(&det).await;
    let mut first = Vec::new();
    for i in 0..=MAX_APPS_PER_SESSION {
        let id = put(&s, S1, b"<html>x</html>").await;
        if i == 0 {
            first.push(id);
        }
    }
    assert_eq!(
        s.app_count().await.unwrap(),
        u64::try_from(MAX_APPS_PER_SESSION).unwrap()
    );
    assert_eq!(
        s.app_lookup(S1.into(), first[0].clone()).await.unwrap(),
        AppLookup::Gone,
        "the oldest row of the session went"
    );
    // The station cap: 11 sessions with 46 rows are 506 rows in all; the
    // first sessions lose the oldest rows, the newest session keeps all.
    let det = Det::new();
    let s = store(&det).await;
    let mut ids: Vec<(String, AppId)> = Vec::new();
    for n in 0..11 {
        let sid = format!("t{n}");
        session(&s, &sid).await;
        for _ in 0..46 {
            ids.push((sid.clone(), put(&s, &sid, b"<html>x</html>").await));
        }
    }
    assert_eq!(
        s.app_count().await.unwrap(),
        u64::try_from(MAX_APPS_PER_STATION).unwrap()
    );
    let (old_session, old_id) = ids[0].clone();
    assert_eq!(
        s.app_lookup(old_session.as_str().into(), old_id)
            .await
            .unwrap(),
        AppLookup::Gone
    );
    let (last_session, last_id) = ids[ids.len() - 1].clone();
    assert!(matches!(
        s.app_lookup(last_session.as_str().into(), last_id)
            .await
            .unwrap(),
        AppLookup::Found(_)
    ));
}

#[tokio::test]
async fn lookup_after_caps_is_gone_and_never_stored_is_unknown() {
    let det = Det::new();
    let s = store(&det).await;
    let first = put(&s, S1, b"<html>x</html>").await;
    for _ in 0..MAX_APPS_PER_SESSION {
        put(&s, S1, b"<html>x</html>").await;
    }
    assert_eq!(
        s.app_lookup(S1.into(), first).await.unwrap(),
        AppLookup::Gone
    );
    assert_eq!(
        s.app_lookup(S1.into(), "01NEVERSTORED".to_owned())
            .await
            .unwrap(),
        AppLookup::Unknown
    );
}

#[tokio::test]
async fn app_lookup_of_another_session_is_unknown() {
    let det = Det::new();
    let s = store(&det).await;
    session(&s, "s2").await;
    let id = put(&s, S1, b"<html>x</html>").await;
    assert_eq!(
        s.app_lookup("s2".into(), id.clone()).await.unwrap(),
        AppLookup::Unknown
    );
    assert_eq!(s.app_chunk("s2".into(), id, 0, 10).await.unwrap(), None);
}

#[tokio::test]
async fn chunk_reads_bounded_slices() {
    let det = Det::new();
    let s = store(&det).await;
    let id = put(&s, S1, b"0123456789").await;
    let chunk = |offset, len| s.app_chunk(S1.into(), id.clone(), offset, len);
    assert_eq!(chunk(0, 4).await.unwrap(), Some((b"0123".to_vec(), 10)));
    assert_eq!(chunk(8, 4).await.unwrap(), Some((b"89".to_vec(), 10)));
    assert_eq!(chunk(10, 4).await.unwrap(), Some((Vec::new(), 10)));
    assert_eq!(chunk(99, 4).await.unwrap(), Some((Vec::new(), 10)));
    assert_eq!(
        chunk(0, 100).await.unwrap(),
        Some((b"0123456789".to_vec(), 10))
    );
    assert_eq!(
        s.app_chunk(S1.into(), "unknown".to_owned(), 0, 4)
            .await
            .unwrap(),
        None
    );
}
