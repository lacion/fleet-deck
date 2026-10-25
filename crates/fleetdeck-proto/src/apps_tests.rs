use super::*;
use crate::event::{Event, EventBody};
use crate::ids::SessionId;
use crate::session::SessionSummary;
use serde_json::json;

#[test]
fn app_request_wire_names() {
    let call = AppRequest::ToolsCall {
        name: "save".into(),
        arguments: json!({"a": 1}),
    };
    let json = serde_json::to_value(&call).unwrap();
    assert_eq!(
        json,
        json!({"method": "tools/call", "name": "save", "arguments": {"a": 1}})
    );
    assert_eq!(serde_json::from_value::<AppRequest>(json).unwrap(), call);
    // `arguments` may be missing: it decodes as `null`.
    let bare: AppRequest =
        serde_json::from_value(json!({"method": "tools/call", "name": "x"})).unwrap();
    assert_eq!(
        bare,
        AppRequest::ToolsCall {
            name: "x".into(),
            arguments: Value::Null
        }
    );
    let read = AppRequest::ResourcesRead {
        uri: "ui://x/a".into(),
    };
    let json = serde_json::to_value(&read).unwrap();
    assert_eq!(json, json!({"method": "resources/read", "uri": "ui://x/a"}));
    assert_eq!(serde_json::from_value::<AppRequest>(json).unwrap(), read);
    // `confirmed` defaults to false.
    let params: AppCallParams = serde_json::from_value(json!({
        "session_id": "s1", "app_id": "a1",
        "request": {"method": "resources/read", "uri": "ui://x/a"}
    }))
    .unwrap();
    assert!(!params.confirmed);
}

#[test]
fn app_call_reply_outcomes() {
    let done = AppCallReply::Done {
        result: json!({"content": []}),
    };
    let json = serde_json::to_value(&done).unwrap();
    assert_eq!(json, json!({"outcome": "done", "result": {"content": []}}));
    assert_eq!(serde_json::from_value::<AppCallReply>(json).unwrap(), done);
    let confirm = AppCallReply::Confirm {
        tool: "save".into(),
        title: Some("Save".into()),
    };
    let json = serde_json::to_value(&confirm).unwrap();
    assert_eq!(
        json,
        json!({"outcome": "confirm", "tool": "save", "title": "Save"})
    );
    assert_eq!(
        serde_json::from_value::<AppCallReply>(json).unwrap(),
        confirm
    );
}

#[test]
fn app_open_call_round_trips_and_omits_cause() {
    let running = AppOpenCall {
        call_id: "01JABCDEFGHJKMNPQRSTVWXYZ0".into(),
        session_id: SessionId::from("s1"),
        app_id: "a1".into(),
        server: "srv".into(),
        tool: "t".into(),
        state: AppCallState::Running,
        cause: None,
        created_at: 5,
    };
    let json = serde_json::to_value(&running).unwrap();
    assert!(json.get("cause").is_none());
    assert_eq!(json["state"], "running");
    assert_eq!(json["session_id"], "s1");
    assert_eq!(json["app_id"], "a1");
    assert_eq!(
        serde_json::from_value::<AppOpenCall>(json).unwrap(),
        running
    );
    for (cause, name) in [
        (AppUncertainCause::StationRestart, "station_restart"),
        (AppUncertainCause::ProviderError, "provider_error"),
    ] {
        let call = AppOpenCall {
            state: AppCallState::Uncertain,
            cause: Some(cause),
            ..running.clone()
        };
        let json = serde_json::to_value(&call).unwrap();
        assert_eq!(json["state"], "uncertain");
        assert_eq!(json["cause"], name);
        assert_eq!(serde_json::from_value::<AppOpenCall>(json).unwrap(), call);
    }
}

#[test]
fn app_release_params_have_no_app_id() {
    let params = AppReleaseParams {
        session_id: SessionId::from("s1"),
        call_id: "c1".into(),
    };
    let json = serde_json::to_value(&params).unwrap();
    assert_eq!(json, json!({"session_id": "s1", "call_id": "c1"}));
    assert!(json.get("app_id").is_none());
    assert_eq!(
        serde_json::from_value::<AppReleaseParams>(json).unwrap(),
        params
    );
    assert_eq!(
        serde_json::to_value(AppReleaseReply { released: true }).unwrap(),
        json!({"released": true})
    );
}

fn tool_app(prefers_border: Option<bool>) -> Event {
    Event::ToolApp {
        node_id: "node-1".into(),
        app_id: "app-1".into(),
        server: "srv".into(),
        tool: "show".into(),
        resource_uri: "ui://srv/show".into(),
        prefers_border,
    }
}

#[test]
fn tool_app_round_trips_and_omits_border() {
    let bare = tool_app(None);
    let json = serde_json::to_value(&bare).unwrap();
    assert_eq!(
        json,
        json!({"kind": "tool.app", "node_id": "node-1", "app_id": "app-1",
               "server": "srv", "tool": "show", "resource_uri": "ui://srv/show"})
    );
    assert_eq!(serde_json::from_value::<Event>(json).unwrap(), bare);
    let bordered = tool_app(Some(false));
    let json = serde_json::to_value(&bordered).unwrap();
    assert_eq!(json["prefers_border"], false);
    assert_eq!(serde_json::from_value::<Event>(json).unwrap(), bordered);
}

#[test]
fn old_peer_keeps_tool_app_as_unknown() {
    let raw = serde_json::to_value(tool_app(Some(true))).unwrap();
    // A build without the variant has no `tool.app` in its kind list: the
    // body stays `Unknown` and re-encodes verbatim, as for `task.updated`.
    let old_kinds: Vec<&str> = Event::KINDS
        .iter()
        .copied()
        .filter(|k| *k != "tool.app")
        .collect();
    let payload = raw.as_object().cloned().unwrap();
    let kind = payload["kind"].as_str().unwrap().to_owned();
    assert!(!old_kinds.contains(&kind.as_str()));
    let old = EventBody::Unknown { kind, payload };
    assert!(old.known().is_none());
    assert_eq!(old.kind_name(), "tool.app");
    assert_eq!(serde_json::to_value(&old).unwrap(), raw);
    // This build decodes the same JSON as a known event.
    let now: EventBody = serde_json::from_value(raw).unwrap();
    assert_eq!(now.known(), Some(&tool_app(Some(true))));
}

#[test]
fn tool_app_kind_name_is_listed() {
    assert_eq!(tool_app(None).kind_name(), "tool.app");
    assert!(Event::KINDS.contains(&"tool.app"));
    assert_eq!(Event::KINDS.len(), 68);
}

#[test]
fn tool_app_changes_no_summary_field() {
    let mut summary = SessionSummary::new(SessionId::from("s1"));
    let before = summary.clone();
    summary.apply(&crate::event::EventEnvelope {
        station_seq: 101,
        session_id: SessionId::from("s1"),
        session_seq: 1,
        at: 1_001,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(tool_app(Some(true))),
    });
    // Only the cursor fields of `apply` move.
    assert_eq!(summary.session_seq, 1);
    assert_eq!(summary.updated_at, 1_001);
    let mut moved = summary.clone();
    moved.session_seq = before.session_seq;
    moved.updated_at = before.updated_at;
    assert_eq!(moved, before);
}
