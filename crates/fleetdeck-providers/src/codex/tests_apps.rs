//! MCP Apps in the Codex adapter (B15, design `docs/v2/design/mcp-apps.md`
//! §4.2): the `initialize` extension, the candidate of a completed `mcpToolCall`,
//! the three app requests and the direct-driver [`AppBridge`].

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::Event;
use fleetdeck_proto::apps::{MAX_APP_INPUT_BYTES, MAX_APP_RESULT_BYTES};
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, DuplexStream};
use tokio::sync::{Mutex, Notify, mpsc, watch};
use tokio::task::JoinHandle;

use super::super::{Shared, read_loop};
use super::CodexAppBridge;
use crate::adapter::EventSink;
use crate::apps::{AppBridge, AppCandidate, AppError, AppTool, CALL_REPLY_WAIT, ToolAnswer};
use crate::codex::map::apps::{AppOp, parse_tools_page};
use crate::codex::rpc::parse_value;
use crate::codex::tests::{T, config, feed, notify, ready_core, start_turn, writes};
use crate::codex::{CodexCore, HostCmd, Output};
use crate::stdio::test_support::FakeControl;
use crate::stdio::{Write, release_write};

const LIMIT: Duration = Duration::from_secs(5);

/// The outputs of a recorded fixture (`spikes/s2-codex/fixtures`) through the
/// production core, in order.
fn replay(name: &str) -> Vec<Output> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../spikes/s2-codex/fixtures")
        .join(name);
    let text = std::fs::read_to_string(path).unwrap();
    let mut core: Option<CodexCore> = None;
    let mut all = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let rec: Value = serde_json::from_str(line).unwrap();
        let out = match rec.get("dir").and_then(Value::as_str) {
            Some("spawn") => {
                core = Some(CodexCore::new(config("<workdir>")));
                continue;
            }
            Some("host") => {
                let cmd: HostCmd = serde_json::from_value(rec["cmd"].clone()).unwrap();
                core.as_mut().unwrap().apply(&cmd).unwrap()
            }
            Some("out") => core
                .as_mut()
                .unwrap()
                .on_frame(parse_value(rec["frame"].clone())),
            Some("exit") => core.as_mut().unwrap().on_exit(None),
            _ => continue,
        };
        all.extend(out);
    }
    all
}

fn candidates(out: &[Output]) -> Vec<AppCandidate> {
    out.iter()
        .filter_map(|o| match o {
            Output::AppCandidate(c) => Some(c.clone()),
            _ => None,
        })
        .collect()
}

/// A completed `mcpToolCall` item of thread `thread`.
fn mcp_item(id: &str, thread: &str, extra: Value) -> Value {
    let mut item = json!({
        "type": "mcpToolCall", "id": id, "server": "demo", "tool": "show_counter",
        "status": "completed", "arguments": {}, "error": null,
        "mcpAppResourceUri": "ui://demo/counter.html", "mcpAppUi": null,
        "result": {"content": [{"type": "text", "text": "n"}], "structuredContent": {"count": 1}, "_meta": null},
    });
    for (k, v) in extra.as_object().unwrap() {
        item[k] = v.clone();
    }
    json!({"threadId": thread, "item": item})
}

fn complete_item(core: &mut CodexCore, params: Value) -> Vec<Output> {
    notify(core, "item/completed", params)
}

#[test]
fn initialize_offers_the_ui_extension_only_when_set() {
    let mut core = CodexCore::new(config("/w"));
    let out = core.initialize().unwrap();
    assert_eq!(
        writes(&out)[0]["params"]["capabilities"],
        json!({"experimentalApi": true})
    );
    let mut core = CodexCore::new(config("/w"));
    core.set_offer_mcp_apps(true);
    let out = core.initialize().unwrap();
    assert_eq!(
        writes(&out)[0]["params"]["capabilities"],
        json!({
            "experimentalApi": true,
            "extensions": {"io.modelcontextprotocol/ui": {"mimeTypes": ["text/html;profile=mcp-app"]}},
        })
    );
}

#[test]
fn completed_mcp_call_with_ui_uri_gives_one_candidate_after_tool_completed() {
    let out = replay("mcp_app.jsonl");
    let found = candidates(&out);
    assert_eq!(found.len(), 1, "{found:?}");
    let at = out
        .iter()
        .position(|o| matches!(o, Output::AppCandidate(_)))
        .unwrap();
    let Output::Emit(Event::ToolCompleted { node_id, .. }) = &out[at - 1] else {
        panic!("the candidate follows tool.completed: {:?}", out[at - 1]);
    };
    let c = &found[0];
    assert_eq!(&c.node_id, node_id);
    assert_eq!(c.server.as_deref(), Some("demo"));
    assert_eq!(c.server_hint, None);
    assert_eq!(c.tool, "show_counter");
    assert_eq!(c.resource_uri.as_deref(), Some("ui://demo/counter.html"));
    assert_eq!(c.arguments, json!({}));
    // The null `_meta` of the item result is left out.
    assert_eq!(
        c.result,
        Some(json!({
            "content": [{"type": "text", "text": "The counter is 0."}],
            "structuredContent": {"count": 0},
        }))
    );
}

#[test]
fn mcp_app_ui_uri_wins_over_the_legacy_field() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let params = mcp_item(
        "m1",
        T,
        json!({"mcpAppUi": {"resourceUri": "ui://demo/new.html"}}),
    );
    let found = candidates(&complete_item(&mut core, params));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].resource_uri.as_deref(), Some("ui://demo/new.html"));
}

#[test]
fn mcp_call_without_uri_or_failed_gives_no_candidate() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let cases = [
        json!({"mcpAppResourceUri": null}),
        json!({"mcpAppResourceUri": "https://x/y.html"}),
        json!({"mcpAppResourceUri": ""}),
        json!({"status": "failed"}),
        json!({"status": "inProgress"}),
        json!({"error": {"message": "boom"}}),
        json!({"result": null}),
        json!({"result": "text"}),
        json!({"server": ""}),
    ];
    for (i, extra) in cases.into_iter().enumerate() {
        let params = mcp_item(&format!("m{i}"), T, extra.clone());
        let out = complete_item(&mut core, params);
        assert!(candidates(&out).is_empty(), "{extra}");
    }
    // The control: the same item without a change gives a candidate.
    let out = complete_item(&mut core, mcp_item("ok", T, json!({})));
    assert_eq!(candidates(&out).len(), 1);
}

#[test]
fn child_thread_mcp_call_gives_no_candidate() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let out = complete_item(&mut core, mcp_item("m1", "child-9", json!({})));
    assert!(candidates(&out).is_empty());
}

#[test]
fn oversized_result_gives_candidate_without_result() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let big_result =
        json!({"content": [{"type": "text", "text": "x".repeat(MAX_APP_RESULT_BYTES)}]});
    let big_args = json!({"blob": "x".repeat(MAX_APP_INPUT_BYTES)});
    let params = mcp_item(
        "m1",
        T,
        json!({"result": big_result, "arguments": big_args}),
    );
    let found = candidates(&complete_item(&mut core, params));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].result, None);
    assert_eq!(found[0].arguments, Value::Null);
    assert_eq!(
        found[0].resource_uri.as_deref(),
        Some("ui://demo/counter.html")
    );
}

#[test]
fn app_requests_write_the_three_methods() {
    let mut core = ready_core();
    let ops = [
        (
            AppOp::Tools { cursor: None },
            "mcpServerStatus/list",
            json!({"threadId": T, "detail": "toolsAndAuthOnly"}),
        ),
        (
            AppOp::Tools {
                cursor: Some("p2".to_owned()),
            },
            "mcpServerStatus/list",
            json!({"threadId": T, "detail": "toolsAndAuthOnly", "cursor": "p2"}),
        ),
        (
            AppOp::Read {
                server: "demo".to_owned(),
                uri: "ui://demo/counter.html".to_owned(),
            },
            "mcpServer/resource/read",
            json!({"threadId": T, "server": "demo", "uri": "ui://demo/counter.html"}),
        ),
        (
            AppOp::Call {
                server: "demo".to_owned(),
                tool: "bump".to_owned(),
                arguments: json!({"by": 2}),
            },
            "mcpServer/tool/call",
            json!({"threadId": T, "server": "demo", "tool": "bump", "arguments": {"by": 2}}),
        ),
    ];
    let mut ids = Vec::new();
    for (op, method, params) in ops {
        let (id, out) = core.app_request(&op).unwrap();
        assert!(id.starts_with("fd-app-"), "{id}");
        assert_eq!(
            writes(&out),
            vec![json!({"id": id, "method": method, "params": params})]
        );
        ids.push(id);
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4, "the ids are unique");
}

#[test]
fn app_answer_is_taken_once_and_late_answers_are_dropped() {
    let mut core = ready_core();
    let call = AppOp::Call {
        server: "demo".to_owned(),
        tool: "bump".to_owned(),
        arguments: json!({}),
    };
    let (ok, _) = core.app_request(&call).unwrap();
    let (bad, _) = core.app_request(&call).unwrap();
    let (late, _) = core.app_request(&call).unwrap();
    assert!(
        core.take_app_answer(&ok).is_none(),
        "nothing before the answer"
    );

    assert!(feed(&mut core, json!({"id": ok, "result": {"content": []}})).is_empty());
    let err = json!({"id": bad, "error": {"code": -32602, "message": "unknown tool"}});
    assert!(feed(&mut core, err).is_empty());
    assert_eq!(core.take_app_answer(&ok), Some(Ok(json!({"content": []}))));
    assert_eq!(core.take_app_answer(&ok), None, "an answer is taken once");
    assert_eq!(
        core.take_app_answer(&bad),
        Some(Err("unknown tool".to_owned()))
    );

    // A request that nobody waits for any more: its answer is dropped.
    core.forget_app_request(&late);
    assert!(feed(&mut core, json!({"id": late, "result": {}})).is_empty());
    assert_eq!(core.take_app_answer(&late), None);
    // An id that this process never issued (an answer after a restart).
    assert!(feed(&mut core, json!({"id": "fd-app-999", "result": {}})).is_empty());
    assert_eq!(core.take_app_answer("fd-app-999"), None);
    assert_eq!(core.unknown_frames(), 0, "no unknown-frame count");
}

#[test]
fn app_requests_end_with_the_session() {
    let mut core = ready_core();
    core.on_exit(Some(0));
    let op = AppOp::Tools { cursor: None };
    assert!(core.app_request(&op).is_err());
}

#[test]
fn tools_page_parses_read_only_visibility_and_the_cursor() {
    let page = json!({
        "data": [{"name": "demo", "runtimeStatus": "connected", "tools": {
            "bump": {"name": "bump", "inputSchema": {},
                "_meta": {"ui": {"resourceUri": "ui://demo/counter.html", "visibility": ["app"]}}},
            "show_counter": {"name": "show_counter", "inputSchema": {},
                "annotations": {"title": "Show counter", "readOnlyHint": true},
                "_meta": {"ui": {"resourceUri": "ui://demo/counter.html", "visibility": ["model", "app"]}}},
            "plain": {"name": "plain", "annotations": {"readOnlyHint": false}},
        }}, {"name": "empty", "tools": {}}],
        "nextCursor": "p2",
    });
    let (tools, next) = parse_tools_page(&page);
    assert_eq!(next.as_deref(), Some("p2"));
    let tool =
        |name: &str, title: Option<&str>, read_only, visibility: Option<&[&str]>, uri| AppTool {
            server: "demo".to_owned(),
            name: name.to_owned(),
            title: title.map(str::to_owned),
            read_only,
            visibility: visibility.map(|v| v.iter().map(|s| (*s).to_owned()).collect()),
            resource_uri: uri,
        };
    let uri = Some("ui://demo/counter.html".to_owned());
    assert_eq!(
        tools,
        vec![
            tool("bump", None, false, Some(&["app"]), uri.clone()),
            tool("plain", None, false, None, None),
            tool(
                "show_counter",
                Some("Show counter"),
                true,
                Some(&["model", "app"]),
                uri
            ),
        ]
    );
    let (none, next) = parse_tools_page(&json!({"data": [], "nextCursor": null}));
    assert!(none.is_empty() && next.is_none());
}

/// A direct-driver bridge over a scripted provider: `provider` is what the
/// app-server writes to stdout, `stdin` receives the frames that the host
/// writes.
struct Rig {
    bridge: CodexAppBridge,
    provider: DuplexStream,
    stdin: mpsc::UnboundedReceiver<Value>,
    reader: JoinHandle<()>,
    _events: mpsc::Receiver<crate::adapter::Emitted>,
}

fn rig() -> Rig {
    let (sink, events) = EventSink::channel(64);
    let shared = Arc::new(Shared {
        core: Mutex::new(ready_core()),
        candidates: mpsc::channel(8).0,
        sink,
        raw: std::sync::Mutex::new(None),
        poll: Notify::new(),
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
        bridge: CodexAppBridge {
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

impl Rig {
    /// The next frame that the host wrote.
    async fn written(&mut self) -> Value {
        tokio::time::timeout(LIMIT, self.stdin.recv())
            .await
            .expect("the host wrote a frame")
            .unwrap()
    }

    /// The app-server answers the request `id`.
    async fn answer(&mut self, id: &Value, body: Value) {
        let key = if body.get("code").is_some() {
            "error"
        } else {
            "result"
        };
        let mut frame = json!({"id": id});
        frame[key] = body;
        self.provider
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .unwrap();
    }

    /// Runs `call` with the bridge and answers its request with `body`.
    async fn serve<T, Fut>(
        &mut self,
        body: Value,
        call: impl FnOnce(CodexAppBridge) -> Fut,
    ) -> (Value, T)
    where
        T: Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let task = tokio::spawn(call(self.bridge.clone()));
        let frame = self.written().await;
        self.answer(&frame["id"], body).await;
        let done = tokio::time::timeout(LIMIT, task).await.unwrap().unwrap();
        (frame, done)
    }
}

async fn call(b: CodexAppBridge) -> Result<ToolAnswer, AppError> {
    b.call_tool("demo", "bump", json!({})).await
}

async fn read(b: CodexAppBridge) -> Result<Value, AppError> {
    b.read_resource("demo", "ui://demo/counter.html").await
}

async fn tools(b: CodexAppBridge) -> Result<Vec<AppTool>, AppError> {
    b.tools().await
}

#[tokio::test(start_paused = true)]
async fn codex_call_tool_waits_past_the_reply_wait_and_ends_on_exit() {
    let mut rig = rig();
    let mut task = tokio::spawn(call(rig.bridge.clone()));
    let frame = rig.written().await;
    assert_eq!(frame["method"], "mcpServer/tool/call");
    assert_eq!(frame["params"]["tool"], "bump");
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
async fn codex_call_tool_answers_prove_end_only_without_is_error() {
    let mut rig = rig();
    let ok =
        json!({"content": [{"type": "text", "text": "now 1"}], "structuredContent": {"count": 1}});
    let (frame, answer) = rig.serve(ok.clone(), call).await;
    assert_eq!(
        frame["params"],
        json!({"threadId": T, "server": "demo", "tool": "bump", "arguments": {}})
    );
    let answer = answer.unwrap();
    assert!(answer.proves_end);
    assert_eq!(answer.result, ok);

    let failed = json!({"content": [{"type": "text", "text": "tool failed"}], "isError": true});
    let (_, answer) = rig.serve(failed.clone(), call).await;
    let answer = answer.unwrap();
    assert!(!answer.proves_end, "an error result proves no end");
    assert_eq!(answer.result, failed, "the result is kept");

    let timeout = json!({"code": -32603, "message": "timed out awaiting tools/call after 1s"});
    let (_, answer) = rig.serve(timeout, call).await;
    assert_eq!(
        answer,
        Err(AppError::Answered(
            "timed out awaiting tools/call after 1s".to_owned()
        ))
    );
}

#[tokio::test]
async fn read_resource_returns_the_result_and_an_error_answer_is_answered() {
    let mut rig = rig();
    let doc = json!({"contents": [{"uri": "ui://demo/counter.html", "mimeType": "text/html;profile=mcp-app", "text": "<p>x</p>"}]});
    let (frame, result) = rig.serve(doc.clone(), read).await;
    assert_eq!(frame["method"], "mcpServer/resource/read");
    assert_eq!(result, Ok(doc));
    let refused = json!({"code": -32002, "message": "resource not found"});
    let (_, result) = rig.serve(refused, read).await;
    assert_eq!(
        result,
        Err(AppError::Answered("resource not found".to_owned()))
    );
}

#[tokio::test]
async fn tools_follow_the_next_cursor_up_to_five_pages() {
    let mut rig = rig();
    let page = |n: usize, next: Option<&str>| json!({"data": [{"name": format!("s{n}"), "tools": {"t": {"name": "t"}}}], "nextCursor": next});

    // Two pages: the second request carries the cursor of the first.
    let task = tokio::spawn(tools(rig.bridge.clone()));
    let first = rig.written().await;
    assert_eq!(
        first["params"],
        json!({"threadId": T, "detail": "toolsAndAuthOnly"})
    );
    rig.answer(&first["id"], page(1, Some("c2"))).await;
    let second = rig.written().await;
    assert_eq!(second["params"]["cursor"], "c2");
    rig.answer(&second["id"], page(2, None)).await;
    let listed = tokio::time::timeout(LIMIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        listed.iter().map(|t| t.server.as_str()).collect::<Vec<_>>(),
        ["s1", "s2"]
    );

    // A server that never stops: the fifth page is the last.
    let task = tokio::spawn(tools(rig.bridge.clone()));
    for n in 1..=5 {
        let frame = rig.written().await;
        rig.answer(&frame["id"], page(n, Some("more"))).await;
    }
    let listed = tokio::time::timeout(LIMIT, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(listed.len(), 5);
    assert!(rig.stdin.try_recv().is_err(), "no sixth request");
}
