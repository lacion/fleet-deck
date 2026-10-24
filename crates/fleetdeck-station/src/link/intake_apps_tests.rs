//! The `session.app_*` link methods (B15 T5b, design §4.6): the chunks of the
//! document, the release states and the dispatch of the call methods. The
//! method handler runs on a store and a fake bridge (the rig of
//! `apps/calls_tests.rs`); the link layer in front of it (roles, features,
//! scopes) is tested with the method table.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use base64::Engine as _;
use fleetdeck_proto::apps::{
    APP_DOC_CHUNK_BYTES, AppCallsReply, AppCsp, AppDocumentChunk, AppMeta, AppReleaseReply,
};
use fleetdeck_proto::link::Outcome;
use ring::digest::{SHA256, digest};
use serde_json::{Value, json};

use super::handle;
use crate::apps::calls::rig::{Rig, S1, URI};
use crate::store::app_calls::CallState;
use crate::store::apps::{NewApp, ToolAppEvent};

async fn serve(rig: &Rig, method: &str, params: Value) -> Outcome {
    let host = rig.host.clone();
    handle(&rig.env, |_| Some(host.clone()), method, params).await
}

fn code(outcome: &Outcome) -> &str {
    match outcome {
        Outcome::Err(e) => &e.code,
        Outcome::Ok(_) => "ok",
    }
}

fn value<T: serde::de::DeserializeOwned>(outcome: Outcome) -> T {
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).unwrap(),
        Outcome::Err(e) => panic!("{e:?}"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn app_document_chunks_reassemble_to_sha256() {
    let rig = Rig::new().await;
    // Two chunks and a part: 2 * chunk + 1234 bytes.
    let html: Vec<u8> = (0..(2 * APP_DOC_CHUNK_BYTES + 1234))
        .map(|n| u8::try_from(n % 251).unwrap())
        .collect();
    let app = rig
        .store
        .insert_app(
            NewApp {
                session_id: S1.into(),
                html: html.clone(),
                csp: AppCsp::default(),
                tool_input: Value::Null,
                tool_result: None,
            },
            ToolAppEvent {
                node_id: "n".into(),
                server: "demo".to_owned(),
                tool: "show_counter".to_owned(),
                resource_uri: URI.to_owned(),
                prefers_border: None,
            },
        )
        .await
        .unwrap();
    let meta: AppMeta = value(
        serve(
            &rig,
            "session.app_meta",
            json!({"session_id": S1, "app_id": app}),
        )
        .await,
    );
    assert_eq!(meta.doc_bytes, html.len() as u64);
    let mut got = Vec::new();
    let mut chunks = 0;
    loop {
        let chunk: AppDocumentChunk = value(
            serve(
                &rig,
                "session.app_document",
                json!({"session_id": S1, "app_id": app, "offset": got.len()}),
            )
            .await,
        );
        chunks += 1;
        assert_eq!(chunk.total, html.len() as u64);
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&chunk.data_b64)
            .unwrap();
        assert!(raw.len() <= APP_DOC_CHUNK_BYTES);
        got.extend_from_slice(&raw);
        if chunk.eof {
            break;
        }
        assert!(chunks < 10, "no end of the document");
    }
    assert_eq!(chunks, 3);
    assert_eq!(got, html);
    assert_eq!(hex(digest(&SHA256, &got).as_ref()), meta.doc_sha256);
    // An offset past the end is refused; the offset at the end is an empty
    // last chunk.
    let past = serve(
        &rig,
        "session.app_document",
        json!({"session_id": S1, "app_id": app, "offset": html.len() + 1}),
    )
    .await;
    assert_eq!(code(&past), "invalid_params");
    let end: AppDocumentChunk = value(
        serve(
            &rig,
            "session.app_document",
            json!({"session_id": S1, "app_id": app, "offset": html.len()}),
        )
        .await,
    );
    assert!(end.eof && end.data_b64.is_empty());
    // An app of another session and an unknown app find no row.
    let other = serve(
        &rig,
        "session.app_meta",
        json!({"session_id": "s2", "app_id": app}),
    )
    .await;
    assert_eq!(code(&other), "unknown_app");
    let unknown = serve(
        &rig,
        "session.app_meta",
        json!({"session_id": S1, "app_id": "nope"}),
    )
    .await;
    assert_eq!(code(&unknown), "unknown_app");
}

#[tokio::test]
async fn app_release_states() {
    let rig = Rig::new().await;
    rig.plant_uncertain("u1", "proc-1").await;
    rig.plant("r1", "proc-1", None).await;
    let release = |call: &str| json!({"session_id": S1, "call_id": call});
    // A `running` row cannot be released.
    let running = serve(&rig, "session.app_release", release("r1")).await;
    assert_eq!(code(&running), "call_running");
    assert_eq!(rig.row("r1").await.state, CallState::Running);
    // No row.
    let unknown = serve(&rig, "session.app_release", release("nope")).await;
    assert_eq!(code(&unknown), "unknown_call");
    // An `uncertain` row is released, with its advance pending, and the
    // second release finds it terminal.
    let first: AppReleaseReply = value(serve(&rig, "session.app_release", release("u1")).await);
    assert!(first.released);
    let row = rig.row("u1").await;
    assert_eq!(row.state, CallState::Released);
    assert!(row.advance_pending);
    let second: AppReleaseReply = value(serve(&rig, "session.app_release", release("u1")).await);
    assert!(!second.released);
}

#[tokio::test]
async fn app_release_of_another_session_is_unknown_call() {
    let rig = Rig::new().await;
    rig.plant_uncertain("u1", "proc-1").await;
    let other = serve(
        &rig,
        "session.app_release",
        json!({"session_id": "s2", "call_id": "u1"}),
    )
    .await;
    assert_eq!(code(&other), "unknown_call");
    assert_eq!(rig.row("u1").await.state, CallState::Uncertain);
    // The list of the owner session shows the call without an app row and
    // without a live session.
    let reply: AppCallsReply = value(
        handle(
            &rig.env,
            |_| None,
            "session.app_calls",
            json!({"session_id": S1}),
        )
        .await,
    );
    assert_eq!(reply.calls.len(), 1);
    assert_eq!(reply.calls[0].call_id, "u1");
    assert_eq!(reply.calls[0].session_id.as_str(), S1);
    // Params that do not parse are a protocol error.
    let bad = serve(&rig, "session.app_calls", json!({"session": S1})).await;
    assert_eq!(code(&bad), "protocol_error");
}
