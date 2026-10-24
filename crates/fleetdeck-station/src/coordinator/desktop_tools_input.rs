//! The coordinator input tools of R13 P7 (`docs/v2/design/omarchy-p7.md`
//! §4): `key_press`, `type_text` and `pointer_move`, and the two tools of
//! R13 P7b (`docs/v2/design/omarchy-p7b.md` §5): `pointer_click` and
//! `window_list`. The P7 schemas are the normative bundle of §4 byte for
//! byte; each P7 tool gets its schema with the `$defs` that it references (transitively)
//! copied into its root, as in P6. The P7b schemas are §5.1 and §5.2 with the referenced
//! `$defs` copied into the root. There is no `scroll`.

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
    },
    "pointer_click": {
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
    },
    "window_list": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "station": {"$ref": "#/$defs/station"}
      }
    }
  }
}
"##;

/// The schema of tool `name` in `bundle`: `schemas[name]` with only the
/// `$defs` that it references copied into its root, transitively: a copied
/// def can reference another def, which is copied too (`docs/v2/design/mcp-p1.md`
/// §4.3). The bundle is a constant and a test parses it, so a parse failure
/// cannot reach a release build.
pub(super) fn referenced_in(bundle: &str, name: &str) -> Value {
    let bundle: Value = serde_json::from_str(bundle).unwrap_or(Value::Null);
    let mut schema = bundle["schemas"][name].clone();
    let mut defs = serde_json::Map::new();
    if let Some(all) = bundle["$defs"].as_object() {
        let mut text = schema.to_string();
        loop {
            let found: Vec<(&String, &Value)> = all
                .iter()
                .filter(|(key, _)| !defs.contains_key(key.as_str()))
                .filter(|(key, _)| text.contains(&format!("\"#/$defs/{key}\"")))
                .collect();
            if found.is_empty() {
                break;
            }
            for (key, def) in found {
                text.push_str(&def.to_string());
                defs.insert(key.clone(), def.clone());
            }
        }
    }
    if let Some(object) = schema.as_object_mut() {
        object.insert("$defs".to_owned(), Value::Object(defs));
    }
    schema
}

fn schema(name: &str) -> Value {
    referenced_in(BUNDLE, name)
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

pub(super) fn pointer_click() -> Value {
    schema("pointer_click")
}

pub(super) fn window_list() -> Value {
    schema("window_list")
}
