//! Tests of the link handler: the params rule and the unknown method.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::desktop_tools::{StationTools, tool};
use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};

use super::call;
use super::test_support::peer;

fn error(outcome: Outcome) -> (String, String) {
    match outcome {
        Outcome::Err(e) => (e.code, e.message),
        Outcome::Ok(v) => panic!("unexpected answer: {v}"),
    }
}

#[tokio::test]
async fn bad_params_answer_invalid_request_with_the_fixed_text() {
    for method in ["station.tools", "station.desktop_tools_plan"] {
        for params in [
            json!({"x": 1}),
            json!([]),
            json!("{}"),
            json!(1),
            json!(false),
            json!([{}]),
        ] {
            let (code, text) = error(call(method, params.clone(), peer(1)).await);
            assert_eq!(code, "invalid_request", "{method} {params}");
            assert_eq!(text, "The request is not valid.");
        }
    }
}

#[tokio::test]
async fn another_method_is_not_implemented() {
    let (code, _) = error(call("station.other", Value::Null, peer(1)).await);
    assert_eq!(code, crate::link::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn null_and_empty_params_both_read_the_tools() {
    for params in [Value::Null, json!({})] {
        let Outcome::Ok(v) = call("station.tools", params, peer(1)).await else {
            panic!("an error");
        };
        let tools: StationTools = serde_json::from_value(v).unwrap();
        let ids: Vec<&str> = tools.tools.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, tool::ALL);
    }
}
