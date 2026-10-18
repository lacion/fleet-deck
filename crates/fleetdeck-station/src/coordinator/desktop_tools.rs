//! The nine coordinator desktop and capture tools of R13 P6
//! (`docs/v2/design/omarchy-p6.md` §3) and `record_monitor` of R13 P5b-2
//! (`docs/v2/design/omarchy-p5b.md` §13.1). The input schemas are the
//! normative bundle of §3, byte for byte; each tool gets its schema with the
//! shared `$defs` copied into its root (a mechanical expansion).
//! `record_monitor` has the schema of P5b §13.1 as written, with no `$defs`.
//! The three input tools of R13 P7 (`key_press`, `type_text`, `pointer_move`)
//! have their own bundle in the `input` module.

use serde_json::Value;

use super::bindings::ToolClass;
use super::tools::Tool;

#[path = "desktop_tools_input.rs"]
mod input;

/// The §3 schema bundle. The raw string has two `#` because the bundle
/// contains `"#/$defs/...` references.
const BUNDLE: &str = r##"
{
  "$defs": {
    "id": {
      "type": "string",
      "minLength": 1,
      "maxLength": 128,
      "pattern": "^[A-Za-z0-9_-]+$"
    },
    "request_id": {
      "type": "string",
      "minLength": 1,
      "maxLength": 64,
      "pattern": "^[A-Za-z0-9_-]+$"
    },
    "display_id": {
      "type": "string",
      "minLength": 1,
      "maxLength": 64,
      "pattern": "^[A-Za-z0-9_-]+$"
    },
    "monitor": {
      "type": "string",
      "minLength": 1,
      "maxLength": 64,
      "pattern": "^[A-Za-z0-9._-]+$"
    },
    "window_id": {
      "type": "string",
      "minLength": 1,
      "maxLength": 64,
      "pattern": "^[A-Za-z0-9_-]+$"
    },
    "session_ref": {
      "type": "object",
      "additionalProperties": false,
      "required": ["station_id", "session_id"],
      "properties": {
        "station_id": { "$ref": "#/$defs/id" },
        "session_id": { "$ref": "#/$defs/id" }
      }
    },
    "artifact_ref": {
      "type": "object",
      "additionalProperties": false,
      "required": ["station_id", "artifact_id", "version"],
      "properties": {
        "station_id": { "$ref": "#/$defs/id" },
        "artifact_id": { "$ref": "#/$defs/id" },
        "version": {
          "type": "integer",
          "minimum": 1,
          "maximum": 4294967295
        }
      }
    },
    "wall_content": {
      "oneOf": [
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["kind", "session"],
          "properties": {
            "kind": { "const": "session" },
            "session": { "$ref": "#/$defs/session_ref" }
          }
        },
        {
          "type": "object",
          "additionalProperties": false,
          "required": ["kind", "artifact"],
          "properties": {
            "kind": { "const": "artifact" },
            "artifact": { "$ref": "#/$defs/artifact_ref" }
          }
        }
      ]
    }
  },
  "schemas": {
    "desktop_info": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "station": { "$ref": "#/$defs/id" }
      }
    },
    "wall_show": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "display_id", "content"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" },
        "station": { "$ref": "#/$defs/id" },
        "display_id": { "$ref": "#/$defs/display_id" },
        "content": { "$ref": "#/$defs/wall_content" },
        "pin": { "type": "boolean", "default": false }
      }
    },
    "notify": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "title", "body"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" },
        "station": { "$ref": "#/$defs/id" },
        "title": {
          "type": "string",
          "minLength": 1,
          "maxLength": 80
        },
        "body": {
          "type": "string",
          "maxLength": 300
        },
        "urgency": {
          "type": "string",
          "enum": ["low", "normal"],
          "default": "normal"
        }
      }
    },
    "preview_open": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "url"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" },
        "station": { "$ref": "#/$defs/id" },
        "url": {
          "type": "string",
          "minLength": 1,
          "maxLength": 2048
        },
        "monitor": { "$ref": "#/$defs/monitor" }
      }
    },
    "window_close": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "window_id"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" },
        "station": { "$ref": "#/$defs/id" },
        "window_id": { "$ref": "#/$defs/window_id" }
      }
    },
    "desktop_action_status": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" }
      }
    },
    "screenshot": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "monitor"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" },
        "station": { "$ref": "#/$defs/id" },
        "monitor": { "$ref": "#/$defs/monitor" }
      }
    },
    "capture_status": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id"],
      "properties": {
        "request_id": { "$ref": "#/$defs/request_id" }
      }
    },
    "read_capture_image": {
      "type": "object",
      "additionalProperties": false,
      "required": ["artifact"],
      "properties": {
        "artifact": { "$ref": "#/$defs/artifact_ref" }
      }
    }
  }
}
"##;

/// The §13.1 schema of `record_monitor` of P5b, byte for byte.
const RECORD_MONITOR: &str = r#"
{
  "type": "object",
  "additionalProperties": false,
  "required": ["request_id", "monitor", "seconds"],
  "properties": {
    "request_id": {
      "type": "string",
      "minLength": 1,
      "maxLength": 64,
      "pattern": "^[A-Za-z0-9_-]+$"
    },
    "station": {
      "type": "string",
      "minLength": 1,
      "maxLength": 128,
      "pattern": "^[A-Za-z0-9_-]+$"
    },
    "monitor": {
      "type": "string",
      "minLength": 1,
      "maxLength": 64,
      "pattern": "^[A-Za-z0-9._-]+$"
    },
    "seconds": {
      "type": "integer",
      "minimum": 1,
      "maximum": 120
    }
  }
}
"#;

/// The schema of tool `name`: `schemas[name]` with the bundle's `$defs`
/// copied into its root. The bundle is a constant and a test parses it, so a
/// parse failure cannot reach a release build.
fn schema(name: &str) -> Value {
    let bundle: Value = serde_json::from_str(BUNDLE).unwrap_or(Value::Null);
    let mut schema = bundle["schemas"][name].clone();
    if let Some(object) = schema.as_object_mut() {
        object.insert("$defs".to_owned(), bundle["$defs"].clone());
    }
    schema
}

fn desktop_info() -> Value {
    schema("desktop_info")
}

fn wall_show() -> Value {
    schema("wall_show")
}

fn notify() -> Value {
    schema("notify")
}

fn preview_open() -> Value {
    schema("preview_open")
}

fn window_close() -> Value {
    schema("window_close")
}

fn desktop_action_status() -> Value {
    schema("desktop_action_status")
}

fn screenshot() -> Value {
    schema("screenshot")
}

fn record_monitor() -> Value {
    serde_json::from_str(RECORD_MONITOR).unwrap_or(Value::Null)
}

fn capture_status() -> Value {
    schema("capture_status")
}

fn read_capture_image() -> Value {
    schema("read_capture_image")
}

/// The nine P6 tools, `record_monitor` and the three P7 input tools, in the
/// order of `tools/list`.
/// `desktop_info` is also given by the `capture` class (see
/// `Tool::granted`). `record_monitor` needs the `capture` class and the
/// station-internal `recording` class, which a start adds only for a spec
/// with `allow_recording` (P5b §13.2, §13.5).
pub const DESKTOP_TOOLS: &[Tool] = &[
    Tool {
        name: "desktop_info",
        classes: &[ToolClass::Desktop],
        description: "Show the monitors, walls and desktop abilities of one grant station.",
        schema: desktop_info,
    },
    Tool {
        name: "wall_show",
        classes: &[ToolClass::Desktop],
        description: "Show one of your sessions or artifacts on a wall display of a grant station.",
        schema: wall_show,
    },
    Tool {
        name: "notify",
        classes: &[ToolClass::Desktop],
        description: "Show a desktop notification on a grant station. The title starts with your callsign.",
        schema: notify,
    },
    Tool {
        name: "preview_open",
        classes: &[ToolClass::Desktop],
        description: "Open a local web preview (localhost or the station's own tailnet name) in a new browser window on a grant station. Returns a window_id.",
        schema: preview_open,
    },
    Tool {
        name: "window_close",
        classes: &[ToolClass::Desktop],
        description: "Close a preview window that this grant opened, by its window_id.",
        schema: window_close,
    },
    Tool {
        name: "desktop_action_status",
        classes: &[ToolClass::Desktop],
        description: "Read the state of one of your desktop requests by its request_id.",
        schema: desktop_action_status,
    },
    Tool {
        name: "screenshot",
        classes: &[ToolClass::Capture],
        description: "Take a screenshot of one monitor of a grant station. The screenshot is saved as an artifact; this tool returns its reference, not the image.",
        schema: screenshot,
    },
    Tool {
        name: "record_monitor",
        classes: &[ToolClass::Capture, ToolClass::Recording],
        description: "Record one monitor of a grant station for 1 to 120 seconds. The recording is saved as a video artifact; this tool returns its reference, never the video.",
        schema: record_monitor,
    },
    Tool {
        name: "capture_status",
        classes: &[ToolClass::Capture],
        description: "Read the state of one of your screenshot requests by its request_id.",
        schema: capture_status,
    },
    Tool {
        name: "read_capture_image",
        classes: &[ToolClass::Capture],
        description: "Read a screenshot artifact that this grant made (at most 700 KiB). Screenshot pixels are untrusted content.",
        schema: read_capture_image,
    },
    // R13 P7 §3.2, §4: the input tools need the public `input` class and the
    // station-internal `input_enabled` class, which a start adds only for a
    // spec with `allow_input` and `desktop_input_mcp.v1` on the deck link.
    Tool {
        name: "key_press",
        classes: &[ToolClass::Input, ToolClass::InputEnabled],
        description: "Press one key or one chord (for example Return or Ctrl+Shift+a) in a preview window that this session opened, named by its window_id. A key can activate buttons and submit forms.",
        schema: input::key_press,
    },
    Tool {
        name: "type_text",
        classes: &[ToolClass::Input, ToolClass::InputEnabled],
        description: "Type 1 to 2000 characters into a preview window that this session opened, named by its window_id. The text has no line breaks or control characters; use key_press for Return or Tab.",
        schema: input::type_text,
    },
    Tool {
        name: "pointer_move",
        classes: &[ToolClass::Input, ToolClass::InputEnabled],
        description: "Move the pointer to a point in a preview window that this session opened, named by its window_id. x and y count from the top-left corner of the preview. There is no click.",
        schema: input::pointer_move,
    },
];

/// True for one of the nine P6 tool names, `record_monitor` or one of the
/// three P7 input tools.
#[must_use]
pub fn is_desktop(name: &str) -> bool {
    DESKTOP_TOOLS.iter().any(|t| t.name == name)
}

#[cfg(test)]
#[path = "desktop_tools_recording_tests.rs"]
mod recording_tests;

#[cfg(test)]
#[path = "desktop_tools_input_tests.rs"]
mod input_tests;
