//! Parser, fixtures, lines and time tests of the session history.

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::{HistoryState, HistoryUnavailable};

use super::index::state_of;
use super::parse::{Parsed, parse_line, parse_time_ms};
use super::tests_support::*;

/// The expected file of a fixture: the state and the items.
fn expected_json(page: &fleetdeck_proto::history::SessionHistory) -> serde_json::Value {
    let items: Vec<serde_json::Value> = page
        .items
        .iter()
        .map(|i| {
            serde_json::json!({
                "kind": i.kind,
                "id": i.id,
                "text": i.text,
                "tool": i.tool.as_ref().map(|t| serde_json::json!({
                    "name": t.name, "summary": t.summary, "status": t.status,
                })),
            })
        })
        .collect();
    serde_json::json!({ "state": page.state, "items": items })
}

fn check_fixture(name: &str, kind: ProviderKind) {
    let bytes = std::fs::read(fixture(name)).unwrap();
    let (page, _) = items_of(kind, &bytes);
    let got = expected_json(&page);
    let expected_path = fixture(&name.replace(".jsonl", ".expected.json"));
    if std::env::var_os("FD_UPDATE_FIXTURES").is_some() {
        std::fs::write(
            &expected_path,
            serde_json::to_string_pretty(&got).unwrap() + "\n",
        )
        .unwrap();
    }
    let want: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&expected_path).unwrap()).unwrap();
    assert_eq!(got, want, "{name}");
}

#[test]
fn every_fixture_equals_its_expected_file() {
    for name in [
        "claude-2.1.300-basic.jsonl",
        "claude-2.1.300-tools.jsonl",
        "claude-2.1.300-sidechain-meta.jsonl",
        "claude-2.1.300-resumed.jsonl",
        "claude-unknown-shapes.jsonl",
    ] {
        check_fixture(name, ProviderKind::Claude);
    }
    for name in ["codex-0.162.1-basic.jsonl", "codex-0.162.1-tools.jsonl"] {
        check_fixture(name, ProviderKind::Codex);
    }
}

#[test]
fn a_file_of_a_future_format_is_unsupported_not_empty() {
    let bytes = std::fs::read(fixture("claude-unknown-shapes.jsonl")).unwrap();
    let (page, idx) = items_of(ProviderKind::Claude, &bytes);
    assert!(page.items.is_empty());
    assert_eq!(
        state_of(&idx),
        HistoryState::Unavailable {
            reason: HistoryUnavailable::UnsupportedFormat
        }
    );
}

#[test]
fn a_bad_json_line_and_an_unknown_shape_count_as_skipped() {
    let lines = vec![
        claude_user(1, "hello"),
        "{not json".to_owned(),
        serde_json::json!({"type": "user", "uuid": "x", "message": "no object"}).to_string(),
        claude_assistant(2, "answer"),
    ];
    let (page, idx) = items_of(ProviderKind::Claude, &join(&lines));
    assert_eq!(page.items.len(), 2);
    assert_eq!(idx.skipped, 2);
    assert_eq!(page.state, HistoryState::Partial { skipped: 2 });
}

#[test]
fn an_oversize_line_is_skipped_and_the_next_record_parses() {
    let big = format!(
        "{{\"type\":\"user\",\"uuid\":\"big\",\"message\":{{\"role\":\"user\",\"content\":\"{}\"}}}}",
        "x".repeat(17 * 1024 * 1024)
    );
    let lines = vec![claude_user(1, "before"), big, claude_user(2, "after")];
    let (page, idx) = items_of(ProviderKind::Claude, &join(&lines));
    let texts: Vec<&str> = page.items.iter().map(|i| i.text.as_str()).collect();
    assert_eq!(texts, ["before", "after"]);
    assert_eq!(idx.skipped, 1);
    assert_eq!(page.state, HistoryState::Partial { skipped: 1 });
}

#[test]
fn claude_wrappers_meta_side_chain_and_thinking_are_left_out() {
    let wrapper = "<command-name>/x</command-name>\n<command-args></command-args>";
    let mixed = "<local-command-stdout>out</local-command-stdout> and real words";
    let lines = vec![
        claude_user(1, wrapper),
        claude_user(2, mixed),
        serde_json::json!({"type":"user","uuid":"m","isMeta":true,
            "message":{"role":"user","content":"meta"}})
        .to_string(),
        serde_json::json!({"type":"assistant","uuid":"s","isSidechain":true,
            "message":{"role":"assistant","content":[{"type":"text","text":"side"}]}})
        .to_string(),
        serde_json::json!({"type":"assistant","uuid":"t",
            "message":{"role":"assistant","content":[{"type":"thinking","thinking":"hm"}]}})
        .to_string(),
        serde_json::json!({"type":"user","uuid":"i",
            "message":{"role":"user","content":[{"type":"image","source":{}}]}})
        .to_string(),
    ];
    let (page, idx) = items_of(ProviderKind::Claude, &join(&lines));
    let texts: Vec<&str> = page.items.iter().map(|i| i.text.as_str()).collect();
    assert_eq!(texts, [mixed, "[image]"]);
    assert_eq!(idx.skipped, 0);
}

#[test]
fn a_claude_tool_gets_its_status_from_the_result_and_a_summary() {
    let use_ = |id: &str, name: &str, input: serde_json::Value| {
        serde_json::json!({"type":"assistant","uuid":format!("u-{id}"),
            "message":{"role":"assistant","content":[
                {"type":"tool_use","id":id,"name":name,"input":input}]}})
        .to_string()
    };
    let result = |id: &str, error: bool| {
        serde_json::json!({"type":"user","uuid":format!("r-{id}"),
            "message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":id,"is_error":error,"content":"x"}]}})
        .to_string()
    };
    let lines = vec![
        use_("t1", "Bash", serde_json::json!({"command": "ls -l"})),
        result("t1", false),
        use_("t2", "Edit", serde_json::json!({"file_path": "/a/b.rs"})),
        result("t2", true),
        use_(
            "t3",
            "Grep",
            serde_json::json!({"pattern": "needle", "path": "/x"}),
        ),
        use_("t4", "Other", serde_json::json!({"anything": 1})),
    ];
    let (page, _) = items_of(ProviderKind::Claude, &join(&lines));
    let got: Vec<(String, String, String)> = page
        .items
        .iter()
        .map(|i| {
            let t = i.tool.as_ref().unwrap();
            (t.name.clone(), t.summary.clone(), format!("{:?}", t.status))
        })
        .collect();
    assert_eq!(
        got,
        [
            ("Bash".into(), "ls -l".into(), "Ok".into()),
            ("Edit".into(), "/a/b.rs".into(), "Failed".into()),
            ("Grep".into(), "needle".into(), "Unknown".into()),
            ("Other".into(), String::new(), "Unknown".into()),
        ]
    );
}

#[test]
fn codex_skips_injected_blocks_developer_and_reasoning() {
    let lines = vec![
        codex_meta(NATIVE),
        serde_json::json!({"type":"response_item","payload":{"type":"message","id":"m1",
            "role":"user","content":[
                {"type":"input_text","text":"# AGENTS.md instructions for /w"},
                {"type":"input_text","text":"<environment_context>x</environment_context>"},
                {"type":"input_text","text":"real prompt"},
                {"type":"input_text","text":"second part"}]}})
        .to_string(),
        serde_json::json!({"type":"response_item","payload":{"type":"message","id":"m2",
            "role":"user","content":[{"type":"input_text","text":"<skill>s</skill>"}]}})
        .to_string(),
        codex_message(3, "assistant", "done"),
        serde_json::json!({"type":"response_item","payload":{"type":"message","id":"d",
            "role":"developer","content":[{"type":"input_text","text":"rules"}]}})
        .to_string(),
        serde_json::json!({"type":"response_item","payload":{"type":"reasoning","id":"r"}})
            .to_string(),
    ];
    let (page, idx) = items_of(ProviderKind::Codex, &join(&lines));
    let got: Vec<(&str, &str)> = page
        .items
        .iter()
        .map(|i| (i.id.as_str(), i.text.as_str()))
        .collect();
    assert_eq!(got, [("m1", "real prompt\n\nsecond part"), ("m-3", "done")]);
    assert_eq!(idx.skipped, 0);
}

#[test]
fn a_codex_exit_code_comes_from_structured_fields_only() {
    let call = |id: &str| {
        serde_json::json!({"type":"response_item","payload":{"type":"function_call",
            "name":"shell","call_id":id,"arguments":"{\"command\":[\"git\",\"status\"]}"}})
        .to_string()
    };
    let out = |id: &str, extra: serde_json::Value| {
        let mut payload = serde_json::json!({"type":"function_call_output","call_id":id,
            "output":"Process exited with code 3"});
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::json!({"type":"response_item","payload":payload}).to_string()
    };
    let lines = vec![
        call("c1"),
        out("c1", serde_json::json!({"exit_code": 0})),
        call("c2"),
        out("c2", serde_json::json!({"metadata": {"exit_code": 1}})),
        call("c3"),
        out("c3", serde_json::json!({})),
        call("c4"),
        out(
            "c4",
            serde_json::json!({"output": "{\"output\":\"x\",\"metadata\":{\"exit_code\":0}}"}),
        ),
    ];
    let (page, _) = items_of(ProviderKind::Codex, &join(&lines));
    let status: Vec<String> = page
        .items
        .iter()
        .map(|i| format!("{:?}", i.tool.as_ref().unwrap().status))
        .collect();
    assert_eq!(status, ["Ok", "Failed", "Unknown", "Ok"]);
    assert_eq!(page.items[0].tool.as_ref().unwrap().summary, "git status");
}

#[test]
fn a_record_without_an_id_gets_a_line_offset_id() {
    let lines = vec![
        serde_json::json!({"type":"user","message":{"role":"user","content":"a"}}).to_string(),
        serde_json::json!({"type":"user","message":{"role":"user","content":"b"}}).to_string(),
    ];
    let first_len = lines[0].len() as u64 + 1;
    let (page, _) = items_of(ProviderKind::Claude, &join(&lines));
    assert_eq!(page.items[0].id, "line:0");
    assert_eq!(page.items[1].id, format!("line:{first_len}"));
}

#[test]
fn a_record_with_several_items_gets_distinct_ids() {
    let line = serde_json::json!({"type":"assistant","uuid":"U",
        "message":{"role":"assistant","content":[
            {"type":"text","text":"before"},
            {"type":"tool_use","id":"t","name":"Bash","input":{"command":"ls"}},
            {"type":"text","text":"after"}]}})
    .to_string();
    let (page, _) = items_of(ProviderKind::Claude, &join(&[line]));
    let ids: Vec<&str> = page.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["U", "U#1", "U#2"]);
}

#[test]
fn times_parse_with_fractions_zones_and_garbage() {
    assert_eq!(parse_time_ms("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_time_ms("1970-01-01T00:00:01.5Z"), Some(1500));
    assert_eq!(
        parse_time_ms("2026-10-14T21:58:52.579Z"),
        Some(1_792_015_132_579)
    );
    assert_eq!(
        parse_time_ms("2026-10-14T23:58:52.579+02:00"),
        parse_time_ms("2026-10-14T21:58:52.579Z")
    );
    assert_eq!(parse_time_ms("nonsense"), None);
    assert_eq!(parse_time_ms("2026-13-01T00:00:00Z"), None);
}

#[test]
fn a_blank_line_is_ignored_and_an_unknown_provider_reads_nothing() {
    assert_eq!(parse_line(ProviderKind::Claude, b"  "), Parsed::Ignored);
    assert_eq!(parse_line(ProviderKind::Unknown, b"{}"), Parsed::Ignored);
}
