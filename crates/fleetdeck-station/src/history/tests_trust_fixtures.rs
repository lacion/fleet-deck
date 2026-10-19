//! Tests of the history import against recorded provider fixtures (B7).

use fleetdeck_proto::native::ImportState;

use crate::history::Limits;
use crate::history::tests_support::*;

/// Copies a fixture into the rig: every `cwd` is the rig path, and the
/// Codex parent thread is cleared.
fn load_fixture(name: &str, cwd: &str) -> Vec<String> {
    let text = std::fs::read_to_string(fixture(name)).unwrap();
    let path = regex_free_replace(&text, cwd);
    path.lines().map(str::to_owned).collect()
}

fn regex_free_replace(text: &str, cwd: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("\"<path ") {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let end = tail.find('"').unwrap();
        out.push('"');
        out.push_str(cwd);
        rest = &tail[end..];
    }
    out.push_str(rest);
    out.replace(
        "\"parent_thread_id\":\"<text 3>\"",
        "\"parent_thread_id\":null",
    )
}

#[tokio::test]
async fn the_claude_fixtures_give_the_expected_fields() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let id = "00e00a5d-d49a-4946-b5bc-fcdf51bd4493";
    for name in [
        "claude-2.1.300-basic.jsonl",
        "claude-2.1.300-tools.jsonl",
        "claude-2.1.300-resumed.jsonl",
        "claude-2.1.300-sidechain-meta.jsonl",
    ] {
        let path = b.claude_path(&b.cwd, id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_private(&path, &join(&load_fixture(name, &b.cwd)));
        let list = b.list().await;
        let s = entry(&list, id);
        assert_eq!(s.import, ImportState::Ok, "{name}");
        assert_eq!(s.cwd.as_deref(), Some(b.cwd.as_str()), "{name}");
        assert!(s.verified, "{name}");
    }
    let path = b.claude_path(&b.cwd, id);
    write_private(
        &path,
        &join(&load_fixture("claude-2.1.300-basic.jsonl", &b.cwd)),
    );
    let list = b.list().await;
    let s = entry(&list, id);
    assert_eq!(s.first_prompt.as_deref(), Some("<text 5>"));
    // The tools fixture holds no prompt that a person typed.
    write_private(
        &path,
        &join(&load_fixture("claude-2.1.300-tools.jsonl", &b.cwd)),
    );
    assert_eq!(entry(&b.list().await, id).first_prompt, None);
    write_private(
        &path,
        &join(&load_fixture("claude-2.1.300-basic.jsonl", &b.cwd)),
    );
    let list = b.list().await;
    let s = entry(&list, id);
    assert_eq!(s.title, None);
}

#[tokio::test]
async fn the_codex_fixtures_give_the_expected_fields() {
    let Some(b) = rig(Limits::default(), &[]).await else {
        return;
    };
    let tools = b.codex_path(CODEX_ID);
    std::fs::create_dir_all(tools.parent().unwrap()).unwrap();
    write_private(
        &tools,
        &join(&load_fixture("codex-0.162.1-tools.jsonl", &b.cwd)),
    );
    let basic_id = "01a13777-3cb3-7a42-b7ff-9572798fa426";
    let basic = b.codex_path(basic_id);
    write_private(
        &basic,
        &join(&load_fixture("codex-0.162.1-basic.jsonl", &b.cwd)),
    );
    let list = b.list().await;
    let s = entry(&list, CODEX_ID);
    assert_eq!(s.import, ImportState::Ok);
    assert_eq!(s.cwd.as_deref(), Some(b.cwd.as_str()));
    assert!(s.parent_native_id.is_none());
    let s = entry(&list, basic_id);
    assert_eq!(s.import, ImportState::Ok);
    assert!(s.first_prompt.is_some());
}
