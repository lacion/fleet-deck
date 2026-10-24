//! Scenario `mcp_app` of the Codex sim (B15, design
//! `docs/v2/design/mcp-apps.md` §6, T5b): the sim answers the three app
//! requests of the host with one MCP server `demo`, and every turn completes
//! one `mcpToolCall` item that names the app `ui://demo/counter.html`.
//!
//! - `mcpServerStatus/list`: the server `demo` with `show_counter` (visibility
//!   model and app, read-only, names the app), `bump` (app only, not
//!   read-only) and `slow_bump` (app only, not read-only).
//! - `mcpServer/resource/read`: a small HTML document with the app MIME.
//! - `mcpServer/tool/call`: `show_counter` answers at once; `bump` answers
//!   after [`BUMP_DELAY_MS`] (the sim reads one frame at a time, so it
//!   answers nothing else meanwhile); `slow_bump` answers at once with the
//!   JSON-RPC error `timed out awaiting tools/call after 1s` and writes the
//!   file `slow_bump.txt` into the thread's cwd [`SLOW_WRITE_MS`] later, as a
//!   server that goes on after the client's timeout.
//! - Every turn: one `mcpToolCall` item of `show_counter` with
//!   `mcpAppResourceUri`, completed right before the agent message (so a
//!   `slow <ms> <text>` turn completes it after the delay, and an
//!   `exit-after <text>` turn completes it before the exit).
//!
//! The version probe of this scenario is `0.163.0`, the first version with an
//! app path (`fake.rs`).

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use super::{Sim, str_of};

/// The scenario name.
pub const SCENARIO: &str = "mcp_app";
/// The delay of the answer of `bump`.
pub const BUMP_DELAY_MS: u64 = 3000;
/// The delay of the write of `slow_bump.txt` after the error answer.
pub const SLOW_WRITE_MS: u64 = 2000;
/// The URI of the demo app.
pub const APP_URI: &str = "ui://demo/counter.html";
/// The MIME of an MCP App document.
const APP_MIME: &str = "text/html;profile=mcp-app";
const HTML: &str = "<!doctype html><html><head><title>counter</title></head>\
<body><p id=\"count\">0</p><script>document.getElementById('count').textContent='ready';</script></body></html>";

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn counter_result(count: u64) -> Value {
    json!({
        "content": [{"type": "text", "text": format!("The counter is {count}.")}],
        "structuredContent": {"count": count},
    })
}

fn tools() -> Value {
    json!({
        "show_counter": {
            "name": "show_counter",
            "title": "Show counter",
            "annotations": {"readOnlyHint": true},
            "_meta": {"ui": {"resourceUri": APP_URI, "visibility": ["model", "app"]}},
        },
        "bump": {
            "name": "bump",
            "title": "Bump",
            "annotations": {"readOnlyHint": false},
            "_meta": {"ui": {"resourceUri": APP_URI, "visibility": ["app"]}},
        },
        "slow_bump": {
            "name": "slow_bump",
            "title": "Slow bump",
            "annotations": {"readOnlyHint": false},
            "_meta": {"ui": {"resourceUri": APP_URI, "visibility": ["app"]}},
        },
    })
}

impl<W: Write> Sim<W> {
    fn rpc_error(&mut self, id: &Value, code: i64, message: &str) -> std::io::Result<()> {
        self.emit(&json!({"id": id, "error": {"code": code, "message": message}}))
    }

    /// The three app requests of the host (scenario `mcp_app` only).
    pub(super) fn on_mcp(
        &mut self,
        id: &Value,
        method: &str,
        params: &Value,
    ) -> std::io::Result<()> {
        match method {
            "mcpServerStatus/list" => self.reply(
                id,
                json!({"data": [{"name": "demo", "tools": tools()}], "nextCursor": null}),
            ),
            "mcpServer/resource/read" => {
                let uri = str_of(params, &["uri"]).unwrap_or_default().to_owned();
                if str_of(params, &["server"]) == Some("demo") && uri == APP_URI {
                    self.reply(
                        id,
                        json!({"contents": [{
                            "uri": uri,
                            "mimeType": APP_MIME,
                            "text": HTML,
                            "_meta": {"ui": {"csp": {"connectDomains": [], "resourceDomains": []}}},
                        }]}),
                    )
                } else {
                    self.rpc_error(id, -32602, "unknown resource")
                }
            }
            _ => self.on_mcp_tool(id, params),
        }
    }

    fn on_mcp_tool(&mut self, id: &Value, params: &Value) -> std::io::Result<()> {
        if str_of(params, &["server"]) != Some("demo") {
            return self.rpc_error(id, -32602, "unknown server");
        }
        match str_of(params, &["tool"]).unwrap_or_default() {
            "show_counter" => {
                let count = COUNTER.load(Ordering::SeqCst);
                self.reply(id, counter_result(count))
            }
            "bump" => {
                std::thread::sleep(std::time::Duration::from_millis(BUMP_DELAY_MS));
                let count = COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
                self.reply(id, counter_result(count))
            }
            "slow_bump" => {
                let path = std::path::Path::new(&self.cwd).join("slow_bump.txt");
                // A thread of this process: the "server" outlives the answer.
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(SLOW_WRITE_MS));
                    let _ = std::fs::write(path, "slow_bump wrote this\n");
                });
                self.rpc_error(id, -32603, "timed out awaiting tools/call after 1s")
            }
            _ => self.rpc_error(id, -32602, "unknown tool"),
        }
    }

    /// Completes the `mcpToolCall` item of `show_counter` (scenario `mcp_app`
    /// only; the dispatch line in `answer` checks the scenario).
    pub(super) fn mcp_item(&mut self, turn: &str) -> std::io::Result<()> {
        let n = self.next();
        let id = format!("mcp-sim-{n}");
        let count = COUNTER.load(Ordering::SeqCst);
        let item = |status: &str, result: Value| {
            json!({
                "appContext": null,
                "arguments": {},
                "durationMs": 1,
                "error": null,
                "id": id,
                "mcpAppResourceUri": APP_URI,
                "mcpAppUi": null,
                "pluginId": null,
                "readOnlyHint": true,
                "result": result,
                "server": "demo",
                "status": status,
                "tool": "show_counter",
                "type": "mcpToolCall",
            })
        };
        self.item("started", turn, item("inProgress", Value::Null))?;
        let mut done = counter_result(count);
        done["_meta"] = Value::Null;
        self.item("completed", turn, item("completed", done))
    }
}
