//! The direct-driver [`AppBridge`] of Claude (B15, design
//! `docs/v2/design/mcp-apps.md` §4.2): a bridge over a scripted CLI.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, DuplexStream};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinHandle;

use super::super::{Shared, read_loop};
use super::ClaudeAppBridge;
use super::tests::{ALL, core_with, ready_core};
use crate::adapter::EventSink;
use crate::apps::{AppBridge, AppError, AppTool, CALL_REPLY_WAIT, ToolAnswer};
use crate::checkpoint::AdapterCheckpoint;
use crate::claude::ClaudeCore;
use crate::claude::map::apps::AppOp;
use crate::claude::tests::config;
use crate::stdio::test_support::FakeControl;
use crate::stdio::{Write, release_write};

const LIMIT: Duration = Duration::from_secs(5);

/// A direct-driver bridge over a scripted provider: `provider` is what the CLI
/// writes to stdout, `stdin` receives the frames that the host writes.
struct Rig {
    bridge: ClaudeAppBridge,
    provider: DuplexStream,
    stdin: mpsc::UnboundedReceiver<Value>,
    reader: JoinHandle<()>,
    _events: mpsc::Receiver<crate::adapter::Emitted>,
}

fn rig_with(core: ClaudeCore) -> Rig {
    let (sink, events) = EventSink::channel(64);
    let shared = Arc::new(Shared {
        core: Mutex::new(core),
        candidates: mpsc::channel(8).0,
        sink,
        raw: std::sync::Mutex::new(None),
        queued: std::sync::atomic::AtomicUsize::new(0),
    });
    let (provider, host_end) = tokio::io::duplex(1 << 20);
    let (writes, mut write_rx) = mpsc::unbounded_channel::<Write>();
    let (frames_tx, frames) = watch::channel(0);
    let (stdin_tx, stdin) = mpsc::unbounded_channel();
    let counted = Arc::clone(&shared);
    tokio::spawn(async move {
        while let Some(item) = write_rx.recv().await {
            match item {
                Write::Frame(v) => {
                    release_write(&counted.queued);
                    let _ = stdin_tx.send(v);
                }
                Write::Flush(ack) => {
                    let _ = ack.send(());
                }
                Write::Close => break,
            }
        }
    });
    let reader = tokio::spawn(read_loop(
        Arc::clone(&shared),
        Arc::new(FakeControl::default()),
        Box::new(host_end),
        writes.clone(),
        frames_tx,
        1 << 20,
    ));
    Rig {
        bridge: ClaudeAppBridge {
            shared,
            writes,
            frames,
        },
        provider,
        stdin,
        reader,
        _events: events,
    }
}

fn rig() -> Rig {
    rig_with(ready_core())
}

impl Rig {
    /// The next frame that the host wrote.
    async fn written(&mut self) -> Value {
        tokio::time::timeout(LIMIT, self.stdin.recv())
            .await
            .expect("the host wrote a frame")
            .unwrap()
    }

    /// The CLI answers the request in `frame`: a success with `body`, or an
    /// error with the text `body`.
    async fn answer(&mut self, frame: &Value, body: Result<Value, &str>) {
        let id = &frame["request_id"];
        let response = match body {
            Ok(body) => json!({"subtype": "success", "request_id": id, "response": body}),
            Err(text) => json!({"subtype": "error", "request_id": id, "error": text}),
        };
        let line = json!({"type": "control_response", "response": response});
        self.provider
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
    }

    /// Runs `call` with the bridge and answers its request with `body`.
    async fn serve<T, Fut>(
        &mut self,
        body: Result<Value, &str>,
        call: impl FnOnce(ClaudeAppBridge) -> Fut,
    ) -> (Value, T)
    where
        T: Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let task = tokio::spawn(call(self.bridge.clone()));
        let frame = self.written().await;
        self.answer(&frame, body).await;
        let done = tokio::time::timeout(LIMIT, task).await.unwrap().unwrap();
        (frame, done)
    }
}

async fn call(b: ClaudeAppBridge) -> Result<ToolAnswer, AppError> {
    b.call_tool("demo", "bump", json!({})).await
}

async fn read(b: ClaudeAppBridge) -> Result<Value, AppError> {
    b.read_resource("demo", "ui://demo/counter.html").await
}

async fn tools(b: ClaudeAppBridge) -> Result<Vec<AppTool>, AppError> {
    b.tools().await
}

#[tokio::test(start_paused = true)]
async fn claude_call_tool_ends_on_exit() {
    let mut rig = rig();
    let mut task = tokio::spawn(call(rig.bridge.clone()));
    let frame = rig.written().await;
    assert_eq!(frame["request"]["subtype"], "mcp_call");
    assert_eq!(frame["request"]["tool"], "mcp__demo__bump");
    // The station waits `CALL_REPLY_WAIT` and answers `timeout`; the call
    // itself has no timer and stays pending past it.
    tokio::time::advance(2 * CALL_REPLY_WAIT).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut task)
            .await
            .is_err(),
        "no answer after 120 s: the call stays pending"
    );
    // The process exits: the reader sees the end of stdout.
    rig.provider.shutdown().await.unwrap();
    let result = tokio::time::timeout(LIMIT, task).await.unwrap().unwrap();
    assert_eq!(result, Err(AppError::Ended));
    tokio::time::timeout(LIMIT, rig.reader)
        .await
        .unwrap()
        .unwrap();
    assert!(rig.bridge.shared.core.lock().await.has_ended());
    // A call after the exit ends at once.
    assert_eq!(
        rig.bridge.call_tool("demo", "bump", json!({})).await,
        Err(AppError::Ended)
    );
}

#[tokio::test]
async fn claude_call_tool_request_has_no_staged_fields() {
    let mut rig = rig();
    let body =
        json!({"content": [{"type": "text", "text": "now 1"}], "structuredContent": {"count": 1}});
    let (frame, _) = rig.serve(Ok(body), call).await;
    let request = frame["request"].as_object().unwrap();
    let mut keys: Vec<&str> = request.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["arguments", "result_not_for_model", "subtype", "tool"]
    );
    for staged in ["input_files", "output_files", "expires_at", "timeout_ms"] {
        assert!(!request.contains_key(staged), "{staged}");
    }
    assert_eq!(request["result_not_for_model"], true);
}

#[tokio::test]
async fn claude_call_tool_success_proves_end_and_timeout_error_is_answered() {
    let mut rig = rig();
    let ok =
        json!({"content": [{"type": "text", "text": "now 1"}], "structuredContent": {"count": 1}});
    let (_, answer) = rig.serve(Ok(ok.clone()), call).await;
    let answer = answer.unwrap();
    assert!(answer.proves_end, "a success relays the server's result");
    assert_eq!(answer.result, ok);

    let (_, answer) = rig
        .serve(Err("MCP server demo tool bump timed out"), call)
        .await;
    assert_eq!(
        answer,
        Err(AppError::Answered(
            "MCP server demo tool bump timed out".to_owned()
        ))
    );
}

#[tokio::test]
async fn claude_call_tool_ids_are_not_reused() {
    let mut rig = rig();
    let (first, _) = rig.serve(Ok(json!({"content": []})), call).await;
    let (second, _) = rig.serve(Err("mcp_call cancelled by client"), call).await;
    assert_eq!(first["request_id"], "fd-app-1");
    assert_eq!(second["request_id"], "fd-app-2");
    // A request that nobody waits for keeps its number.
    let mut core = rig.bridge.shared.core.lock().await;
    let (id, _) = core.app_request(&AppOp::Tools).unwrap();
    core.forget_app_request(&id);
    assert_eq!(id, "fd-app-3");
    // A restored core goes on after the last number: the CLI aborts a request
    // that reuses the id of a live one.
    let text = core.checkpoint().unwrap().to_json().unwrap();
    let stored = AdapterCheckpoint::from_json(&text).unwrap();
    let mut restored = ClaudeCore::restore(config(false), &stored).unwrap();
    assert!(restored.mcp_apps_ready);
    let (next, _) = restored.app_request(&AppOp::Tools).unwrap();
    assert_eq!(next, "fd-app-4");
}

#[tokio::test]
async fn read_resource_returns_the_result_and_an_error_answer_is_answered() {
    let mut rig = rig();
    let doc = json!({"contents": [{"uri": "ui://demo/counter.html", "mimeType": "text/html;profile=mcp-app", "text": "<p>x</p>"}]});
    let (frame, result) = rig.serve(Ok(doc.clone()), read).await;
    assert_eq!(
        frame["request"],
        json!({"subtype": "mcp_read_resource", "serverName": "demo", "uri": "ui://demo/counter.html"})
    );
    assert_eq!(result, Ok(doc));
    let (_, result) = rig
        .serve(
            Err("mcp_read_resource: uri must use the ui:// scheme"),
            read,
        )
        .await;
    assert_eq!(
        result,
        Err(AppError::Answered(
            "mcp_read_resource: uri must use the ui:// scheme".to_owned()
        ))
    );
}

#[tokio::test]
async fn tools_ask_mcp_status_once_and_parse_the_answer() {
    let mut rig = rig();
    let body = json!({"mcpServers": [{"name": "demo", "status": "connected", "tools": [
        {"name": "bump", "annotations": {}, "_meta": {"ui": {"resourceUri": "ui://demo/counter.html", "visibility": ["app"]}}}]}]});
    let (frame, listed) = rig.serve(Ok(body), tools).await;
    assert_eq!(frame["request"], json!({"subtype": "mcp_status"}));
    let listed = listed.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "bump");
    assert!(rig.stdin.try_recv().is_err(), "one request");
}

#[tokio::test(start_paused = true)]
async fn tools_time_out_and_forget_their_request() {
    let mut rig = rig();
    let task = tokio::spawn(tools(rig.bridge.clone()));
    let frame = rig.written().await;
    tokio::time::advance(crate::apps::TOOLS_TIMEOUT + Duration::from_secs(1)).await;
    let result = tokio::time::timeout(LIMIT, task).await.unwrap().unwrap();
    assert_eq!(result, Err(AppError::Timeout));
    // The late answer is dropped.
    rig.answer(&frame, Ok(json!({"mcpServers": []}))).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut core = rig.bridge.shared.core.lock().await;
    assert_eq!(core.take_app_answer("fd-app-1"), None);
    assert_eq!(core.unknown_frames(), 0);
}

#[tokio::test]
async fn bridge_without_the_capabilities_is_unsupported_and_writes_nothing() {
    let mut rig = rig_with(core_with(&ALL[..2]));
    assert_eq!(call(rig.bridge.clone()).await, Err(AppError::Unsupported));
    assert_eq!(read(rig.bridge.clone()).await, Err(AppError::Unsupported));
    assert_eq!(tools(rig.bridge.clone()).await, Err(AppError::Unsupported));
    assert!(rig.stdin.try_recv().is_err(), "nothing was sent");
}
