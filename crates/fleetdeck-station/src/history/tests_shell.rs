//! `!` shell commands of the Claude and Codex CLIs (B8).

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::{HistoryKind, SessionHistory, ToolResult};

use super::super::index;
use super::super::tests_support::{claude_user, codex_message, codex_meta, items_of, join};
use super::{Parsed, RawItem, parse_line};

fn claude_items(content: &serde_json::Value) -> Vec<RawItem> {
    let line = serde_json::json!({
        "type": "user", "uuid": "u-1",
        "message": {"role": "user", "content": content},
    })
    .to_string();
    items(ProviderKind::Claude, &line)
}

fn items(kind: ProviderKind, line: &str) -> Vec<RawItem> {
    match parse_line(kind, line.as_bytes()) {
        Parsed::Known(record) => record.items,
        other => panic!("not a known record: {other:?}"),
    }
}

fn shell(item: &RawItem) -> (&str, &str, &str) {
    let tool = item.tool.as_ref().expect("a tool item");
    assert_eq!(item.kind, HistoryKind::Tool);
    (&tool.name, &tool.summary, &item.text)
}

fn bash_out(stdout: &str, stderr: &str) -> serde_json::Value {
    serde_json::json!(format!(
        "<bash-stdout>{stdout}</bash-stdout><bash-stderr>{stderr}</bash-stderr>"
    ))
}

#[test]
fn a_claude_bash_input_is_one_shell_command_item() {
    let got = claude_items(&serde_json::json!(
        "<bash-input> for m in a; do echo $m &amp;&amp; ls &gt; x; done </bash-input>"
    ));
    assert_eq!(got.len(), 1);
    assert_eq!(
        shell(&got[0]),
        (
            "Shell command",
            "for m in a; do echo $m && ls > x; done",
            ""
        )
    );
    assert_eq!(got[0].id.as_deref(), Some("u-1"));
}

#[test]
fn a_claude_bash_output_with_both_streams_has_the_separator() {
    let got = claude_items(&bash_out("one\ntwo\n", "bad\n"));
    assert_eq!(got.len(), 1);
    assert_eq!(
        shell(&got[0]),
        ("Shell output", "", "one\ntwo\n--- stderr ---\nbad")
    );
}

#[test]
fn a_claude_bash_output_with_one_stream() {
    let out = claude_items(&bash_out("only out\n", ""));
    assert_eq!(shell(&out[0]), ("Shell output", "", "only out"));
    let err = claude_items(&bash_out("", "only err"));
    assert_eq!(
        shell(&err[0]),
        ("Shell output", "", "--- stderr ---\nonly err")
    );
}

#[test]
fn a_claude_bash_output_with_no_text_has_no_item() {
    assert!(claude_items(&bash_out("", "")).is_empty());
    assert!(claude_items(&bash_out("  \n", "\n")).is_empty());
}

#[test]
fn a_claude_bash_output_is_unescaped() {
    let got = claude_items(&bash_out("go: v1 =&gt; v2 &lt;ok&gt; &amp;lt;", ""));
    assert_eq!(got[0].text, "go: v1 => v2 <ok> &lt;");
}

#[test]
fn a_bash_text_in_a_block_keeps_its_place_after_a_plain_text() {
    let got = claude_items(&serde_json::json!([
        {"type": "text", "text": "before"},
        {"type": "text", "text": "<bash-input>ls</bash-input>"},
        {"type": "text", "text": "after"},
    ]));
    assert_eq!(got.len(), 3);
    assert_eq!(
        (got[0].kind, got[0].text.as_str()),
        (HistoryKind::User, "before")
    );
    assert_eq!(shell(&got[1]), ("Shell command", "ls", ""));
    assert_eq!(
        (got[2].kind, got[2].text.as_str()),
        (HistoryKind::User, "after")
    );
}

#[test]
fn an_assistant_text_with_bash_tags_stays_a_text() {
    let line = serde_json::json!({
        "type": "assistant", "uuid": "a-1",
        "message": {"role": "assistant", "content": [
            {"type": "text", "text": "<bash-input>ls</bash-input>"}]},
    })
    .to_string();
    let got = items(ProviderKind::Claude, &line);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].kind, HistoryKind::Assistant);
    assert!(got[0].tool.is_none());
}

fn codex_shell(body: &str) -> String {
    codex_message(
        1,
        "user",
        &format!("<user_shell_command>\n{body}\n</user_shell_command>"),
    )
}

#[test]
fn a_codex_shell_command_is_one_item_with_status_ok() {
    let line = codex_shell(
        "<command>\npwd\n</command>\n<result>\nExit code: 0\nDuration: 0.0862 seconds\nOutput:\n/home/me/app\n\n</result>",
    );
    let got = items(ProviderKind::Codex, &line);
    assert_eq!(got.len(), 1);
    assert_eq!(shell(&got[0]), ("Shell command", "pwd", "/home/me/app"));
    assert_eq!(got[0].tool.as_ref().unwrap().status, Some(ToolResult::Ok));
    assert_eq!(got[0].id.as_deref(), Some("m-1"));
}

#[test]
fn a_codex_shell_command_status_follows_the_exit_code() {
    let failed =
        codex_shell("<command>\nfalse\n</command>\n<result>\nExit code: 2\nOutput:\nno\n</result>");
    let got = items(ProviderKind::Codex, &failed);
    assert_eq!(
        got[0].tool.as_ref().unwrap().status,
        Some(ToolResult::Failed)
    );
    let none = codex_shell("<command>\nls\n</command>\n<result>\nOutput:\nx\n</result>");
    let got = items(ProviderKind::Codex, &none);
    assert_eq!(got[0].tool.as_ref().unwrap().status, None);
    assert_eq!(got[0].text, "x");
}

#[test]
fn a_codex_shell_block_without_a_command_has_no_item() {
    let line = codex_shell("<result>\nExit code: 0\nOutput:\nx\n</result>");
    assert!(items(ProviderKind::Codex, &line).is_empty());
}

fn page_of(kind: ProviderKind, bytes: &[u8], member: bool) -> SessionHistory {
    use std::io::Write;
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    let sha = index::hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref());
    let limits = super::super::Limits::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let idx = index::build(
        &file,
        kind,
        bytes.len() as u64,
        &sha,
        &limits,
        deadline,
        None,
    )
    .unwrap();
    let query = index::PageQuery {
        kind,
        hash16: &sha[..16],
        before: None,
        limit: 50,
        member,
    };
    index::page(&file, &idx, &query, &limits).unwrap()
}

#[test]
fn a_codex_shell_record_pages_with_its_status_and_a_member_gets_no_text() {
    let bytes = join(&[
        codex_meta("n"),
        codex_message(1, "user", "go"),
        codex_shell("<command>\npwd\n</command>\n<result>\nExit code: 0\nOutput:\n/app\n</result>"),
    ]);
    let (owner, _) = items_of(ProviderKind::Codex, &bytes);
    let tool = owner.items[1].tool.as_ref().unwrap();
    assert_eq!(
        (tool.name.as_str(), tool.summary.as_str()),
        ("Shell command", "pwd")
    );
    assert_eq!(tool.status, ToolResult::Ok);
    assert_eq!(owner.items[1].text, "/app");
    let member = page_of(ProviderKind::Codex, &bytes, true);
    let tool = member.items[1].tool.as_ref().unwrap();
    assert_eq!(
        (tool.name.as_str(), tool.summary.as_str()),
        ("Shell command", "")
    );
    assert_eq!(tool.status, ToolResult::Ok);
    assert_eq!(member.items[1].text, "");
    // A plain message keeps its text for a member.
    assert_eq!(member.items[0].text, "go");
}

#[test]
fn a_claude_shell_pair_pages_as_two_items() {
    let bytes = join(&[
        claude_user(1, "<bash-input>ls</bash-input>"),
        claude_user(
            2,
            "<bash-stdout>a\nb</bash-stdout><bash-stderr></bash-stderr>",
        ),
    ]);
    let (page, _) = items_of(ProviderKind::Claude, &bytes);
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].tool.as_ref().unwrap().summary, "ls");
    assert_eq!(page.items[1].text, "a\nb");
}
