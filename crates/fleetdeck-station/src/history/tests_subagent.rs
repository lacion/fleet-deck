//! Tests of the subagent log (DL-191): the binding row, the path and trust
//! checks, the one read of a captured extent, the parsing mode and the
//! bounds of the answer.

use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::event::{Event, ProviderKind, ToolKind, ToolStatus};
use fleetdeck_proto::history::{HistoryKind, HistoryState, HistoryUnavailable, ToolResult};
use fleetdeck_proto::subagent::{MAX_ANSWER_BYTES, MAX_ITEMS, SubagentLog, SubagentQuery};
use serde_json::json;

use super::locate::encode_cwd;
use super::subagent::{self, LaunchFacts};
use super::tests_support::*;
use super::{HistoryError, Limits};
use crate::store::NewEvent;
use crate::store::subagent_logs::SubagentLogRow;

const AGENT: &str = "afef5265450389d2c";

fn my_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// A subagent record: `isSidechain`, the parent session and the agent id.
fn sub_record(kind: &str, n: u32, message: serde_json::Value) -> serde_json::Value {
    json!({
        "type": kind, "uuid": format!("{kind}-{n}"), "sessionId": NATIVE,
        "agentId": AGENT, "isSidechain": true,
        "timestamp": format!("2026-10-14T10:00:{:02}.000Z", n % 60),
        "message": message,
    })
}

fn sub_user(n: u32, text: &str) -> String {
    sub_record("user", n, json!({"role": "user", "content": text})).to_string()
}

fn sub_assistant(n: u32, text: &str) -> String {
    sub_record(
        "assistant",
        n,
        json!({"role": "assistant", "content": [{"type": "text", "text": text}]}),
    )
    .to_string()
}

fn sub_tool_call(n: u32, call_id: &str) -> String {
    sub_record(
        "assistant",
        n,
        json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": call_id, "name": "Bash", "input": {"command": "ls"}}]}),
    )
    .to_string()
}

fn sub_tool_result(n: u32, call_id: &str, is_error: bool) -> String {
    sub_record(
        "user",
        n,
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": call_id, "is_error": is_error, "content": "x"}]}),
    )
    .to_string()
}

/// Writes the subagent transcript below `home` and returns its path.
fn write_sub(rig: &Rig, home: &std::path::Path, agent: &str, lines: &[String]) -> PathBuf {
    let dir = home
        .join("projects")
        .join(encode_cwd(&rig.cwd()))
        .join(NATIVE)
        .join("subagents");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("agent-{agent}.jsonl"));
    write_private(&path, &join(lines));
    path
}

fn row(
    rig: &Rig,
    session: &str,
    tool: &str,
    agent: &str,
    home: &std::path::Path,
) -> SubagentLogRow {
    SubagentLogRow {
        session_id: session.into(),
        tool_use_id: tool.to_owned(),
        agent_id: agent.to_owned(),
        native_id: NATIVE.to_owned(),
        cwd: rig.cwd(),
        provider_home: home.to_string_lossy().into_owned(),
        created_at: 5,
    }
}

async fn ask(rig: &Rig, session: &str, tool: &str) -> Result<SubagentLog, HistoryError> {
    subagent::answer(
        &rig.store,
        &rig.history,
        &rig.private,
        SubagentQuery {
            session_id: session.into(),
            tool_use_id: tool.to_owned(),
        },
    )
    .await
}

/// A rig with a session `s1` and a binding row `t1` for the default home.
async fn bound() -> Rig {
    let rig = Rig::new(ProviderKind::Claude).await;
    rig.session("s1", Some(NATIVE)).await;
    assert!(
        rig.store
            .put_subagent_log(row(&rig, "s1", "t1", AGENT, &rig.home()))
            .await
            .unwrap()
    );
    rig
}

fn unavailable(log: &SubagentLog) -> Option<HistoryUnavailable> {
    match log.state {
        HistoryState::Unavailable { reason } => Some(reason),
        _ => None,
    }
}

// ----------------------------------------------------------- the binding

fn tool_started(node: &str, tool_use: &str) -> Event {
    let mut refs = serde_json::Map::new();
    refs.insert("tool_use_id".to_owned(), json!(tool_use));
    Event::ToolStarted {
        node_id: node.into(),
        parent_node_id: None,
        tool_kind: ToolKind::Subagent,
        title: "Agent".to_owned(),
        input_summary: String::new(),
        files: Vec::new(),
        provider_refs: refs,
    }
}

fn tool_completed(node: &str, agent: Option<&str>) -> Event {
    let mut refs = serde_json::Map::new();
    if let Some(agent) = agent {
        refs.insert("agent_id".to_owned(), json!(agent));
    }
    Event::ToolCompleted {
        node_id: node.into(),
        status: ToolStatus::Ok,
        files: Vec::new(),
        exit_code: None,
        provider_refs: refs,
    }
}

fn facts(rig: &Rig) -> LaunchFacts {
    LaunchFacts {
        provider_home: rig.home(),
        cwd: rig.cwd(),
    }
}

#[tokio::test]
async fn a_completed_agent_call_in_the_batch_stores_one_row_with_the_launch_facts() {
    let rig = Rig::new(ProviderKind::Claude).await;
    rig.session("s1", Some(NATIVE)).await;
    let observed = vec![
        tool_started("n1", "toolu_a"),
        tool_completed("n1", Some(AGENT)),
    ];
    subagent::observe(&rig.store, &"s1".into(), Some(&facts(&rig)), &observed).await;
    let stored = rig
        .store
        .subagent_log("s1".into(), "toolu_a".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.agent_id, AGENT);
    assert_eq!(stored.native_id, NATIVE);
    assert_eq!(stored.cwd, rig.cwd());
    assert_eq!(stored.provider_home, rig.home().to_string_lossy());
}

#[tokio::test]
async fn the_tool_use_is_found_in_the_event_log_when_the_batch_lacks_the_start() {
    let rig = Rig::new(ProviderKind::Claude).await;
    rig.session("s1", Some(NATIVE)).await;
    rig.store
        .append(
            "s1".into(),
            vec![NewEvent::from(tool_started("n7", "toolu_old"))],
        )
        .await
        .unwrap();
    let observed = vec![tool_completed("n7", Some(AGENT))];
    subagent::observe(&rig.store, &"s1".into(), Some(&facts(&rig)), &observed).await;
    let stored = rig
        .store
        .subagent_log("s1".into(), "toolu_old".to_owned())
        .await
        .unwrap();
    assert!(stored.is_some());
}

#[tokio::test]
async fn no_row_without_an_agent_id_a_tool_use_a_native_id_or_launch_facts() {
    let rig = Rig::new(ProviderKind::Claude).await;
    rig.session("s1", Some(NATIVE)).await;
    rig.session("s2", None).await;
    let started = tool_started("n1", "toolu_a");
    // No agent id.
    subagent::observe(
        &rig.store,
        &"s1".into(),
        Some(&facts(&rig)),
        &[started.clone(), tool_completed("n1", None)],
    )
    .await;
    // A bad agent id.
    subagent::observe(
        &rig.store,
        &"s1".into(),
        Some(&facts(&rig)),
        &[started.clone(), tool_completed("n1", Some("../x"))],
    )
    .await;
    // No tool.started anywhere.
    subagent::observe(
        &rig.store,
        &"s1".into(),
        Some(&facts(&rig)),
        &[tool_completed("n-unknown", Some(AGENT))],
    )
    .await;
    // No launch facts (an adopted process).
    subagent::observe(
        &rig.store,
        &"s1".into(),
        None,
        &[started.clone(), tool_completed("n1", Some(AGENT))],
    )
    .await;
    // No native id on the session.
    subagent::observe(
        &rig.store,
        &"s2".into(),
        Some(&facts(&rig)),
        &[started, tool_completed("n1", Some(AGENT))],
    )
    .await;
    for session in ["s1", "s2"] {
        let stored = rig
            .store
            .subagent_log(session.into(), "toolu_a".to_owned())
            .await
            .unwrap();
        assert!(stored.is_none(), "{session}");
    }
}

#[tokio::test]
async fn the_first_binding_of_a_tool_use_stays() {
    let rig = bound().await;
    let other = row(&rig, "s1", "t1", "bbbb", &rig.root.join("other/.claude"));
    assert!(!rig.store.put_subagent_log(other).await.unwrap());
    let kept = rig
        .store
        .subagent_log("s1".into(), "t1".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.agent_id, AGENT);
}

#[tokio::test]
async fn the_launch_facts_need_an_absolute_provider_home() {
    let rig = Rig::new(ProviderKind::Claude).await;
    let got = LaunchFacts::of(
        Some(&rig.policy),
        &"claude".into(),
        ProviderKind::Claude,
        "/w",
    );
    assert_eq!(got.unwrap().provider_home, rig.home());
    let mut env = ChildEnv::empty();
    env.set("HOME", "relative/home").unwrap();
    let mut envs = crate::manager::policy::SetupEnvs::new();
    envs.insert("claude".into(), env);
    let relative = rig.policy.clone().with_setup_envs(envs);
    let got = LaunchFacts::of(
        Some(&relative),
        &"claude".into(),
        ProviderKind::Claude,
        "/w",
    );
    assert!(got.is_none());
}

// ------------------------------------------------------------ the answer

#[tokio::test]
async fn it_reads_the_items_of_the_subagent_and_pairs_the_tool_result() {
    let rig = bound().await;
    write_sub(
        &rig,
        &rig.home(),
        AGENT,
        &[
            sub_user(1, "do the thing"),
            sub_tool_call(2, "call-1"),
            sub_tool_result(3, "call-1", true),
            sub_assistant(4, "done"),
        ],
    );
    let log = ask(&rig, "s1", "t1").await.unwrap();
    assert_eq!(log.state, HistoryState::Complete);
    // A tool call is an assistant item that carries the tool.
    let kinds: Vec<HistoryKind> = log.items.iter().map(|i| i.kind).collect();
    assert_eq!(
        kinds,
        [
            HistoryKind::User,
            HistoryKind::Assistant,
            HistoryKind::Assistant
        ]
    );
    assert_eq!(log.items[0].text, "do the thing");
    let tool = log.items[1].tool.as_ref().unwrap();
    assert_eq!((tool.name.as_str(), tool.summary.as_str()), ("Bash", "ls"));
    assert_eq!(tool.status, ToolResult::Failed);
}

#[tokio::test]
async fn a_bad_tool_use_id_an_unknown_session_and_a_codex_session_are_refused() {
    let rig = bound().await;
    for bad in ["", "a b", "a/b", &"x".repeat(129)] {
        let err = ask(&rig, "s1", bad).await.unwrap_err();
        assert_eq!(err.code, "invalid_params", "{bad}");
    }
    let err = ask(&rig, "nope", "t1").await.unwrap_err();
    assert_eq!(err.code, "unknown_session");

    let codex = Rig::new(ProviderKind::Codex).await;
    codex.session("c1", Some(NATIVE)).await;
    let log = ask(&codex, "c1", "t1").await.unwrap();
    assert_eq!(
        unavailable(&log),
        Some(HistoryUnavailable::UnsupportedFormat)
    );
}

#[tokio::test]
async fn a_tool_use_without_a_row_is_not_found() {
    let rig = bound().await;
    write_sub(&rig, &rig.home(), AGENT, &[sub_user(1, "x")]);
    let log = ask(&rig, "s1", "t-other").await.unwrap();
    assert_eq!(unavailable(&log), Some(HistoryUnavailable::NotFound));
    // A row whose file is missing is `not_found` too.
    let rig = bound().await;
    let log = ask(&rig, "s1", "t1").await.unwrap();
    assert_eq!(unavailable(&log), Some(HistoryUnavailable::NotFound));
}

#[tokio::test]
async fn after_a_resume_in_another_home_each_card_reads_its_own_home() {
    let rig = Rig::new(ProviderKind::Claude).await;
    rig.session("s1", Some(NATIVE)).await;
    let (h1, h2) = (rig.root.join("h1/.claude"), rig.root.join("h2/.claude"));
    for (tool, home) in [("t1", &h1), ("t2", &h2)] {
        rig.store
            .put_subagent_log(row(&rig, "s1", tool, AGENT, home))
            .await
            .unwrap();
    }
    write_sub(&rig, &h1, AGENT, &[sub_user(1, "from home one")]);
    write_sub(&rig, &h2, AGENT, &[sub_user(1, "from home two")]);
    let first = ask(&rig, "s1", "t1").await.unwrap();
    let second = ask(&rig, "s1", "t2").await.unwrap();
    assert_eq!(first.items[0].text, "from home one");
    assert_eq!(second.items[0].text, "from home two");
}

// ------------------------------------------------------------ trust

#[tokio::test]
async fn links_hard_links_other_owners_and_private_paths_are_not_trusted() {
    // A link in place of the file.
    let rig = bound().await;
    let real = write_sub(&rig, &rig.home(), "realone", &[sub_user(1, "x")]);
    symlink(
        &real,
        real.parent().unwrap().join(format!("agent-{AGENT}.jsonl")),
    )
    .unwrap();
    let log = ask(&rig, "s1", "t1").await.unwrap();
    assert_eq!(unavailable(&log), Some(HistoryUnavailable::NotTrusted));

    // A link in place of the subagents directory.
    let rig = bound().await;
    let path = write_sub(&rig, &rig.home(), AGENT, &[sub_user(1, "x")]);
    let dir = path.parent().unwrap().to_owned();
    let moved = dir.parent().unwrap().join("moved");
    std::fs::rename(&dir, &moved).unwrap();
    symlink(&moved, &dir).unwrap();
    let log = ask(&rig, "s1", "t1").await.unwrap();
    assert_eq!(unavailable(&log), Some(HistoryUnavailable::NotTrusted));

    // A hard link.
    let rig = bound().await;
    let path = write_sub(&rig, &rig.home(), AGENT, &[sub_user(1, "x")]);
    std::fs::hard_link(&path, rig.root.join("second-name")).unwrap();
    let log = ask(&rig, "s1", "t1").await.unwrap();
    assert_eq!(unavailable(&log), Some(HistoryUnavailable::NotTrusted));

    // Another owner.
    let rig = bound().await;
    write_sub(&rig, &rig.home(), AGENT, &[sub_user(1, "x")]);
    *rig.history.seams().uid.lock().unwrap() = Some(my_uid().wrapping_add(1));
    let log = ask(&rig, "s1", "t1").await.unwrap();
    assert_eq!(unavailable(&log), Some(HistoryUnavailable::NotTrusted));

    // A private path.
    let rig = bound().await;
    let path = write_sub(&rig, &rig.home(), AGENT, &[sub_user(1, "x")]);
    rig.private.register(&path);
    let log = ask(&rig, "s1", "t1").await.unwrap();
    assert_eq!(unavailable(&log), Some(HistoryUnavailable::NotTrusted));
}

#[tokio::test]
async fn a_file_over_64_mib_is_too_large() {
    let rig = bound().await;
    let path = write_sub(&rig, &rig.home(), AGENT, &[sub_user(1, "x")]);
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_len(64 * 1024 * 1024 + 1).unwrap();
    let log = ask(&rig, "s1", "t1").await.unwrap();
    assert_eq!(unavailable(&log), Some(HistoryUnavailable::TooLarge));
}

#[tokio::test]
async fn a_record_of_another_session_or_agent_is_not_trusted() {
    let other_session = sub_user(2, "y").replace(NATIVE, "99999999-0000-0000-0000-000000000000");
    let other_agent = sub_user(2, "y").replace(AGENT, "zzzz");
    for forged in [other_session, other_agent] {
        let rig = bound().await;
        write_sub(&rig, &rig.home(), AGENT, &[sub_user(1, "x"), forged]);
        let log = ask(&rig, "s1", "t1").await.unwrap();
        assert_eq!(unavailable(&log), Some(HistoryUnavailable::NotTrusted));
        assert!(log.items.is_empty());
    }
}

#[tokio::test]
async fn a_record_without_identity_or_the_sidechain_flag_is_skipped() {
    let rig = bound().await;
    let no_agent = sub_user(2, "no agent").replace(&format!("\"agentId\":\"{AGENT}\","), "");
    let no_session = sub_user(3, "no session").replace(&format!("\"sessionId\":\"{NATIVE}\","), "");
    let no_flag = sub_user(4, "no flag").replace("\"isSidechain\":true", "\"isSidechain\":false");
    assert_ne!(no_agent, sub_user(2, "no agent"));
    assert_ne!(no_session, sub_user(3, "no session"));
    assert_ne!(no_flag, sub_user(4, "no flag"));
    write_sub(
        &rig,
        &rig.home(),
        AGENT,
        &[sub_user(1, "kept"), no_agent, no_session, no_flag],
    );
    let log = ask(&rig, "s1", "t1").await.unwrap();
    assert_eq!(log.items.len(), 1);
    assert_eq!(log.state, HistoryState::Partial { skipped: 3 });
}

// ---------------------------------------------------- the captured extent

fn read(file: &std::fs::File, size: u64) -> SubagentLog {
    subagent::read_log(
        file,
        size,
        NATIVE,
        AGENT,
        &Limits::default(),
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap()
}

fn file_with(bytes: &[u8]) -> std::fs::File {
    use std::io::Write;
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    file
}

#[test]
fn bytes_appended_after_the_open_are_not_read() {
    use std::io::Write;
    let first = join(&[sub_user(1, "one"), sub_assistant(2, "two")]);
    let mut file = file_with(&first);
    let size = first.len() as u64;
    // The provider appends a third record after the size was captured.
    file.write_all(&join(&[sub_assistant(3, "three")])).unwrap();
    let log = read(&file, size);
    let texts: Vec<&str> = log.items.iter().map(|i| i.text.as_str()).collect();
    assert_eq!(texts, ["one", "two"]);
    assert_eq!(log.state, HistoryState::Complete);
}

#[test]
fn a_last_line_without_a_newline_is_not_a_record() {
    let mut bytes = join(&[sub_user(1, "one")]);
    bytes.extend_from_slice(sub_assistant(2, "half written").as_bytes());
    let log = read(&file_with(&bytes), bytes.len() as u64);
    let texts: Vec<&str> = log.items.iter().map(|i| i.text.as_str()).collect();
    assert_eq!(texts, ["one"]);
    assert_eq!(log.state, HistoryState::Complete);
}

#[test]
fn a_file_that_shrank_before_the_captured_size_is_partial() {
    let bytes = join(&[sub_user(1, "one"), sub_assistant(2, "two")]);
    let log = read(&file_with(&bytes), bytes.len() as u64 + 100);
    assert_eq!(log.items.len(), 2);
    assert!(
        matches!(log.state, HistoryState::Partial { .. }),
        "{:?}",
        log.state
    );
}

// ------------------------------------------------------------- the bounds

#[tokio::test]
async fn the_ring_keeps_the_newest_300_items() {
    let rig = bound().await;
    let lines: Vec<String> = (1..=350).map(|n| sub_user(n, &format!("m{n}"))).collect();
    write_sub(&rig, &rig.home(), AGENT, &lines);
    let log = ask(&rig, "s1", "t1").await.unwrap();
    assert_eq!(log.items.len(), MAX_ITEMS);
    assert_eq!(log.items[0].text, "m51");
    assert_eq!(log.items[MAX_ITEMS - 1].text, "m350");
    assert_eq!(log.state, HistoryState::Partial { skipped: 50 });
}

#[tokio::test]
async fn the_serialized_answer_stays_within_512_kib_with_escaping_heavy_text() {
    let rig = bound().await;
    // Each control character takes six bytes of JSON (`\u0001`).
    let heavy = "\u{1}".repeat(8000);
    let lines: Vec<String> = (1..=120)
        .map(|n| sub_user(n, &format!("{n}:{heavy}")))
        .collect();
    write_sub(&rig, &rig.home(), AGENT, &lines);
    let log = ask(&rig, "s1", "t1").await.unwrap();
    let size = serde_json::to_vec(&log).unwrap().len();
    assert!(size <= MAX_ANSWER_BYTES, "{size}");
    assert!(!log.items.is_empty());
    assert!(log.items.len() < 120, "{}", log.items.len());
    // The newest items stay.
    assert!(log.items.last().unwrap().text.starts_with("120:"));
    let HistoryState::Partial { skipped } = log.state else {
        panic!("{:?}", log.state)
    };
    assert_eq!(skipped as usize, 120 - log.items.len());
}

#[tokio::test]
async fn a_long_text_is_cut_as_in_the_history() {
    let rig = bound().await;
    write_sub(
        &rig,
        &rig.home(),
        AGENT,
        &[sub_user(1, &"é".repeat(20_000))],
    );
    let log = ask(&rig, "s1", "t1").await.unwrap();
    assert!(log.items[0].text.len() <= 8 * 1024);
    assert!(log.items[0].text.ends_with("… [cut]"));
}
