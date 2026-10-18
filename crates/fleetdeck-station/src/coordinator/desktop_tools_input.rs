//! The three coordinator input tools of R13 P7 (`docs/v2/design/omarchy-p7.md`
//! §4): `key_press`, `type_text` and `pointer_move`. Their schemas are the
//! normative bundle of §4 byte for byte; each tool gets its schema with the
//! shared `$defs` copied into its root, as in P6. There is no `pointer_click`
//! and no `scroll` (§4).

use serde_json::Value;

/// The §4 schema bundle. The raw string has two `#` because the bundle
/// contains `"#/$defs/...` references.
pub(super) const BUNDLE: &str = r##"
{
  "$defs": {
    "station": {
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
    "window_id": {
      "type": "string",
      "minLength": 32,
      "maxLength": 32,
      "pattern": "^[0-9a-f]{32}$"
    },
    "chord": {
      "type": "string",
      "minLength": 1,
      "maxLength": 32,
      "pattern": "^(Ctrl\\+)?(Shift\\+)?([a-z0-9]|Return|Tab|Escape|BackSpace|Delete|Insert|Home|End|PageUp|PageDown|Left|Right|Up|Down|Space)$"
    }
  },
  "schemas": {
    "key_press": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "window_id", "chord"],
      "properties": {
        "request_id": {"$ref": "#/$defs/request_id"},
        "station": {"$ref": "#/$defs/station"},
        "window_id": {"$ref": "#/$defs/window_id"},
        "chord": {"$ref": "#/$defs/chord"}
      }
    },
    "type_text": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "window_id", "text"],
      "properties": {
        "request_id": {"$ref": "#/$defs/request_id"},
        "station": {"$ref": "#/$defs/station"},
        "window_id": {"$ref": "#/$defs/window_id"},
        "text": {
          "type": "string",
          "minLength": 1,
          "maxLength": 2000,
          "pattern": "^[^\\u0000-\\u001F\\u007F-\\u009F\\u2028\\u2029]+$"
        }
      }
    },
    "pointer_move": {
      "type": "object",
      "additionalProperties": false,
      "required": ["request_id", "window_id", "x", "y"],
      "properties": {
        "request_id": {"$ref": "#/$defs/request_id"},
        "station": {"$ref": "#/$defs/station"},
        "window_id": {"$ref": "#/$defs/window_id"},
        "x": {
          "type": "integer",
          "minimum": 0,
          "maximum": 32767
        },
        "y": {
          "type": "integer",
          "minimum": 0,
          "maximum": 32767
        }
      }
    }
  }
}
"##;

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

pub(super) fn key_press() -> Value {
    schema("key_press")
}

pub(super) fn type_text() -> Value {
    schema("type_text")
}

pub(super) fn pointer_move() -> Value {
    schema("pointer_move")
}
