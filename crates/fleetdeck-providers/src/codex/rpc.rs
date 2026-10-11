//! JSON-RPC framing of `codex app-server` (stdio, one JSON object per line), ported from `spikes/s2-codex/src/rpc.rs`.
//!
//! The app-server speaks JSON-RPC 2.0 without the `"jsonrpc":"2.0"` member on
//! its own frames (observed with codex-cli 0.160.0). It accepts frames with or
//! without that member. Request ids are integers or strings.
//!
//! The parser never panics: a line that is not JSON is an error, and a JSON
//! value that is not a valid JSON-RPC frame becomes [`Incoming::Unknown`].

use serde_json::{Map, Value, json};

/// One frame from the app-server.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// Reply to a host request.
    Response { id: Value, result: Value },
    /// Error reply to a host request.
    ErrorResponse {
        id: Value,
        code: i64,
        message: String,
    },
    /// Server → host request (approvals, questions, tool calls, ...).
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// Server → host notification.
    Notification { method: String, params: Value },
    /// Valid JSON, but not a JSON-RPC frame that this adapter understands.
    Unknown(Value),
}

/// Parses one stdout line.
pub fn parse_line(line: &str) -> Result<Incoming, serde_json::Error> {
    serde_json::from_str::<Value>(line).map(parse_value)
}

fn valid_id(id: &Value) -> bool {
    id.is_string() || id.is_i64() || id.is_u64()
}

/// Classifies one JSON value.
pub fn parse_value(v: Value) -> Incoming {
    let Some(obj) = v.as_object() else {
        return Incoming::Unknown(v);
    };
    let id = obj.get("id").filter(|id| valid_id(id)).cloned();
    let method = obj.get("method").and_then(Value::as_str).map(str::to_owned);
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    match (id, method) {
        (Some(id), Some(method)) => Incoming::Request { id, method, params },
        (None, Some(method)) if !obj.contains_key("id") => {
            Incoming::Notification { method, params }
        }
        (Some(id), None) => {
            if let Some(result) = obj.get("result") {
                Incoming::Response {
                    id,
                    result: result.clone(),
                }
            } else if let Some(err) = obj.get("error").and_then(Value::as_object) {
                Incoming::ErrorResponse {
                    id,
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                }
            } else {
                Incoming::Unknown(v)
            }
        }
        _ => Incoming::Unknown(v),
    }
}

/// Host → server request.
pub fn request(id: &str, method: &str, params: Value) -> Value {
    json!({ "id": id, "method": method, "params": params })
}

/// Host → server notification.
pub fn notification(method: &str) -> Value {
    json!({ "method": method })
}

/// Host reply to a server request.
pub fn reply(id: &Value, result: Value) -> Value {
    json!({ "id": id, "result": result })
}

/// Host error reply to a server request (fail closed).
pub fn reply_error(id: &Value, code: i64, message: &str) -> Value {
    let mut err = Map::new();
    err.insert("code".into(), code.into());
    err.insert("message".into(), message.into());
    json!({ "id": id, "error": err })
}

/// JSON-RPC error code the host uses when it refuses a server request.
pub const REFUSED: i64 = -32601;

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn classifies_frames() {
        assert!(matches!(
            parse_line(r#"{"id":0,"method":"item/tool/requestUserInput","params":{}}"#).unwrap(),
            Incoming::Request { .. }
        ));
        assert!(matches!(
            parse_line(r#"{"method":"turn/started","params":{}}"#).unwrap(),
            Incoming::Notification { .. }
        ));
        assert!(matches!(
            parse_line(r#"{"id":"fd-1","result":{}}"#).unwrap(),
            Incoming::Response { .. }
        ));
        assert!(matches!(
            parse_line(r#"{"id":"fd-1","error":{"code":-1,"message":"no"}}"#).unwrap(),
            Incoming::ErrorResponse { code: -1, .. }
        ));
        assert!(matches!(
            parse_line(r#"{"id":1.5,"method":"x"}"#).unwrap(),
            Incoming::Unknown(_)
        ));
        assert!(matches!(
            parse_line(r#"{"id":null,"method":"x"}"#).unwrap(),
            Incoming::Unknown(_)
        ));
        assert!(matches!(parse_line("[1]").unwrap(), Incoming::Unknown(_)));
        assert!(parse_line("not json").is_err());
    }

    #[test]
    fn builders_match_the_wire_shape() {
        assert_eq!(
            request("fd-1", "turn/start", json!({"a":1})),
            json!({"id":"fd-1","method":"turn/start","params":{"a":1}})
        );
        assert_eq!(reply(&json!(0), json!({"decision":"accept"}))["id"], 0);
        assert_eq!(
            reply_error(&json!(3), REFUSED, "no")["error"]["code"],
            REFUSED
        );
    }

    fn arb_json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            any::<f64>()
                .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
            prop_oneof![
                Just("id".to_owned()),
                Just("method".to_owned()),
                Just("result".to_owned()),
                Just("error".to_owned()),
                ".*"
            ]
            .prop_map(Value::from),
        ];
        leaf.prop_recursive(4, 32, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                prop::collection::btree_map(
                    prop_oneof![
                        Just("id".to_owned()),
                        Just("method".to_owned()),
                        Just("params".to_owned()),
                        Just("result".to_owned()),
                        Just("error".to_owned()),
                        "[a-z]{1,4}"
                    ],
                    inner,
                    0..5
                )
                .prop_map(|m| Value::Object(m.into_iter().collect())),
            ]
        })
    }

    proptest! {
        #[test]
        fn parser_never_panics_on_any_text(s in ".*") {
            let _ = parse_line(&s);
        }

        #[test]
        fn parser_never_panics_on_any_json(v in arb_json()) {
            let _ = parse_value(v);
        }
    }
}
