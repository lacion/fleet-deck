//! MCP P1 log test (`docs/v2/design/mcp-p1.md` §3.5, test 13): no log line
//! at any level carries a tool argument, a tool result or an image, whatever
//! the filter text is.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::sync::{Arc, Mutex};

use fleetdeck_core::logging::{LogFormat, subscriber};
use serde_json::json;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

use super::bindings::{BindingState, ToolClass};
use super::desktop_forward::McpToolResult;
use super::mcp_p1_rig_tests::{Rig, legacy};

/// A captured log: every line that the subscriber wrote.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'w> MakeWriter<'w> for Capture {
    type Writer = Capture;

    fn make_writer(&'w self) -> Self::Writer {
        self.clone()
    }
}

const TYPED: &str = "SENTINEL_TYPED_TEXT_7f3a";
const REFUSED: &str = "SENTINEL_REFUSED_ARG_91c2";
const PNG_BASE64: &str = "SENTINELPNGDATA0b8d";

/// Drives the calls through the real ingress.
async fn drive() {
    let classes = [
        ToolClass::Input,
        ToolClass::InputEnabled,
        ToolClass::Capture,
        ToolClass::Own,
    ];
    let r = Rig::start(BindingState::Active, &classes).await;
    r.bindings.set_fleet_control(true);
    *r.fake.desktop_result.lock().unwrap() = McpToolResult {
        content: vec![
            json!({"type": "text", "text": "{\"capture_id\":\"cap-1\"}"}),
            json!({"type": "image", "mimeType": "image/png", "data": PNG_BASE64}),
        ],
        is_error: false,
    };
    let calls = [
        (
            "type_text",
            json!({"request_id": "r1", "window_id": "w1", "text": TYPED}),
        ),
        (
            "spawn",
            json!({"station": REFUSED, "spec": {"provider_instance": "claude", "cwd": REFUSED}}),
        ),
        (
            "read_capture_image",
            json!({"artifact": {"station_id": "st", "artifact_id": "a1", "version": 1}}),
        ),
    ];
    for (name, args) in calls {
        let params = json!({"name": name, "arguments": args});
        let reply = r.http_modern(&r.token, "tools/call", params).await;
        assert_eq!(reply.status, 200, "{name}: {}", reply.body);
    }
    // A legacy call too, and a request without a credential (the station
    // writes a line for it, which proves that the capture works).
    let legacy_call = legacy(
        1,
        "tools/call",
        json!({"name": "type_text", "arguments": {"request_id": "r2", "window_id": "w1", "text": TYPED}}),
    );
    r.http(Some(&r.token), &[], &legacy_call).await;
    r.http(None, &[], &legacy_call).await;
}

#[test]
fn no_log_line_carries_a_tool_argument_a_result_or_an_image_at_trace() {
    let filters = [
        "trace",
        "rmcp=trace",
        "rmcp::service=trace",
        "rmcp::transport=trace",
        "rmcp::transport::streamable_http_server::tower=trace",
        "[s]=trace",
        "[{secret}]=trace",
        "[s{secret}]=trace",
    ];
    for format in [LogFormat::Text, LogFormat::Json] {
        for text in filters {
            let capture = Capture::default();
            let filter = EnvFilter::try_new(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            let sub = subscriber(format, filter, capture.clone(), false);
            // `rmcp` spawns tasks: a current-thread runtime inside the
            // scope keeps them on the thread that has the subscriber.
            tracing::subscriber::with_default(sub, || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(drive());
            });
            let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
            for sentinel in [TYPED, REFUSED, PNG_BASE64] {
                assert!(!log.contains(sentinel), "{text}: {sentinel} in the log");
            }
            if text == "trace" {
                // The capture works: a line of the station is there.
                assert!(
                    log.contains("coordinator credential refused"),
                    "{text}: {log}"
                );
            }
        }
    }
}
