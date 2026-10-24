//! Tests of `session.workflows` and `session.workflow_agent` (B12 §4.4): the
//! transcript of a workflow agent, its trust checks, and the pages of the
//! projection.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetdeck_proto::event::{
    Event, ProviderKind, WORKFLOWS_PAGE_BUDGET, WorkflowAgent, WorkflowPhase, WorkflowUpdate,
    WorkflowsCursor, WorkflowsPage,
};
use fleetdeck_proto::history::{HistoryKind, HistoryState, HistoryUnavailable, ToolResult};
use fleetdeck_proto::link::{ControlFrame, MAX_CONTROL_FRAME_BYTES, Outcome};
use fleetdeck_proto::subagent::{SubagentLog, WorkflowAgentQuery};
use fleetdeck_proto::{EventEnvelope, SessionId};
use fleetdeck_testkit::Det;
use serde_json::json;

use super::read;
use crate::history::locate::encode_cwd;
use crate::history::subagent::{LaunchFacts, read_log};
use crate::history::{History, HistoryError, Limits};
use crate::store::workflow_agent_logs::WorkflowAgentLogRow;
use crate::store::{NewEvent, NewSession, OfflineApprovalPolicy, Store, StoreConfig};
use crate::workspace::PrivatePaths;

const NATIVE: &str = "11111111-2222-3333-4444-555555555555";
const RUN: &str = "wf_0918c8f1-e62";
const AGENT: &str = "a1b2c3d4e5f60718";
const S1: &str = "s1";

/// A store, a provider home and a history in a temp directory.
struct Rig {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    history: History,
    private: Arc<PrivatePaths>,
}

impl Rig {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let history = History::with_limits(Limits::default());
        history.set_dir(root.join("data/history"));
        let det = Det::new();
        let store = Store::open_in_memory_with_history(
            det.clock(),
            det.ids(),
            StoreConfig::default(),
            history.clone(),
        )
        .await
        .unwrap();
        store.recover(Vec::new()).await.unwrap();
        Self {
            _tmp: tmp,
            root,
            store,
            history,
            private: Arc::new(PrivatePaths::default()),
        }
    }

    fn home(&self) -> PathBuf {
        self.root.join(".claude")
    }

    fn cwd(&self) -> String {
        self.root
            .join("work/project")
            .to_string_lossy()
            .into_owned()
    }

    /// Creates a session row; a Codex session has no native id.
    async fn session(&self, id: &str, provider: ProviderKind, native: Option<&str>) {
        let instance = if provider == ProviderKind::Codex {
            "codex"
        } else {
            "claude"
        };
        self.store
            .create_session(NewSession {
                session_id: id.into(),
                provider,
                provider_instance: instance.into(),
                cwd: self.cwd(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        if let Some(native) = native {
            self.store
                .exec_for_tests(format!(
                    "UPDATE sessions SET native_session_id = '{native}' WHERE session_id = '{id}'"
                ))
                .await;
        }
    }

    /// A Claude session with its native id and a process that has launch
    /// facts for the home of the rig.
    async fn launched(&self) {
        self.session(S1, ProviderKind::Claude, Some(NATIVE)).await;
        self.store
            .process_launched(
                S1.into(),
                "p1".to_owned().into(),
                None,
                Some(LaunchFacts {
                    provider_home: self.home(),
                    cwd: self.cwd(),
                }),
            )
            .await
            .unwrap();
    }

    /// A workflow event that carries the run id and the agent: the station
    /// binds the agent in the event transaction.
    async fn bind(&self, run_id: &str, agent_id: &str) {
        let agent = WorkflowAgent {
            index: 0,
            label: "agent".to_owned(),
            state: "running".to_owned(),
            agent_id: Some(agent_id.to_owned()),
            ..WorkflowAgent::default()
        };
        self.store
            .append(
                S1.into(),
                vec![event(update("t1", Some(run_id), vec![agent], Vec::new()))],
            )
            .await
            .unwrap();
        assert!(
            self.store
                .workflow_agent_log(S1.into(), agent_id.to_owned())
                .await
                .unwrap()
                .is_some(),
            "the agent is bound"
        );
    }

    /// The directory of the agent transcripts of a run.
    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.home()
            .join("projects")
            .join(encode_cwd(&self.cwd()))
            .join(NATIVE)
            .join("subagents/workflows")
            .join(run_id)
    }

    fn write_agent(&self, run_id: &str, agent_id: &str, lines: &[String]) -> PathBuf {
        let dir = self.run_dir(run_id);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("agent-{agent_id}.jsonl"));
        write_private(&path, &join(lines));
        path
    }

    async fn ask(&self, session: &str, agent: &str) -> Result<SubagentLog, HistoryError> {
        read::answer(
            &self.store,
            &self.history,
            &self.private,
            WorkflowAgentQuery {
                session_id: session.into(),
                agent_id: agent.to_owned(),
            },
        )
        .await
    }

    async fn page(&self, after: Option<WorkflowsCursor>) -> Option<WorkflowsPage> {
        self.store.workflow_page(S1.into(), after).await.unwrap()
    }
}

fn join(lines: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in lines {
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    out
}

fn write_private(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

// ------------------------------------------------------- transcript records

/// A subagent record: `isSidechain`, the parent session and the agent id.
fn record(kind: &str, n: u32, agent: &str, message: serde_json::Value) -> String {
    json!({
        "type": kind, "uuid": format!("{kind}-{n}"), "sessionId": NATIVE,
        "agentId": agent, "isSidechain": true,
        "timestamp": format!("2026-10-14T10:00:{:02}.000Z", n % 60),
        "message": message,
    })
    .to_string()
}

fn user(n: u32, text: &str) -> String {
    record("user", n, AGENT, json!({"role": "user", "content": text}))
}

fn assistant(n: u32, text: &str) -> String {
    record(
        "assistant",
        n,
        AGENT,
        json!({"role": "assistant", "content": [{"type": "text", "text": text}]}),
    )
}

fn tool_call(n: u32, call_id: &str) -> String {
    record(
        "assistant",
        n,
        AGENT,
        json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": call_id, "name": "Bash", "input": {"command": "ls"}}]}),
    )
}

fn tool_result(n: u32, call_id: &str, is_error: bool) -> String {
    record(
        "user",
        n,
        AGENT,
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": call_id, "is_error": is_error, "content": "x"}]}),
    )
}

fn unavailable_of(log: &SubagentLog) -> Option<HistoryUnavailable> {
    match log.state {
        HistoryState::Unavailable { reason } => Some(reason),
        _ => None,
    }
}

// ----------------------------------------------------- the agent transcript

#[tokio::test]
async fn it_reads_the_agent_transcript_of_the_run_folder() {
    let rig = Rig::new().await;
    rig.launched().await;
    rig.bind(RUN, AGENT).await;
    rig.write_agent(
        RUN,
        AGENT,
        &[
            user(1, "do the thing"),
            tool_call(2, "call-1"),
            tool_result(3, "call-1", true),
            assistant(4, "done"),
        ],
    );
    let log = rig.ask(S1, AGENT).await.unwrap();
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
async fn an_unbound_agent_is_not_found() {
    let rig = Rig::new().await;
    rig.launched().await;
    rig.write_agent(RUN, AGENT, &[user(1, "x")]);
    let log = rig.ask(S1, AGENT).await.unwrap();
    assert_eq!(unavailable_of(&log), Some(HistoryUnavailable::NotFound));
    // A bound agent of the session is not an agent of another session.
    rig.bind(RUN, AGENT).await;
    rig.session("s2", ProviderKind::Claude, Some(NATIVE)).await;
    let log = rig.ask("s2", AGENT).await.unwrap();
    assert_eq!(unavailable_of(&log), Some(HistoryUnavailable::NotFound));
    // A bad agent id and an unknown session are refused.
    for bad in ["", "A", "a b", "a/b", &"a".repeat(65)] {
        let err = rig.ask(S1, bad).await.unwrap_err();
        assert_eq!(err.code, "invalid_params", "{bad:?}");
    }
    let err = rig.ask("nope", AGENT).await.unwrap_err();
    assert_eq!(err.code, "unknown_session");
}

#[tokio::test]
async fn a_codex_session_is_unsupported() {
    let rig = Rig::new().await;
    rig.session(S1, ProviderKind::Codex, None).await;
    let log = rig.ask(S1, AGENT).await.unwrap();
    assert_eq!(
        unavailable_of(&log),
        Some(HistoryUnavailable::UnsupportedFormat)
    );
}

#[tokio::test]
async fn a_link_on_the_run_folder_or_the_file_is_not_trusted() {
    use std::os::unix::fs::symlink;
    // A link in place of the file.
    let rig = Rig::new().await;
    rig.launched().await;
    rig.bind(RUN, AGENT).await;
    let real = rig.write_agent(RUN, "realone", &[user(1, "x")]);
    symlink(
        &real,
        real.parent().unwrap().join(format!("agent-{AGENT}.jsonl")),
    )
    .unwrap();
    let log = rig.ask(S1, AGENT).await.unwrap();
    assert_eq!(unavailable_of(&log), Some(HistoryUnavailable::NotTrusted));

    // A link in place of the run folder.
    let rig = Rig::new().await;
    rig.launched().await;
    rig.bind(RUN, AGENT).await;
    let path = rig.write_agent(RUN, AGENT, &[user(1, "x")]);
    let dir = path.parent().unwrap().to_owned();
    let moved = dir.parent().unwrap().join("moved");
    std::fs::rename(&dir, &moved).unwrap();
    symlink(&moved, &dir).unwrap();
    let log = rig.ask(S1, AGENT).await.unwrap();
    assert_eq!(unavailable_of(&log), Some(HistoryUnavailable::NotTrusted));
}

#[tokio::test]
async fn a_record_of_another_agent_is_not_trusted() {
    let other_agent = user(2, "y").replace(AGENT, "zzzz");
    let other_session = user(2, "y").replace(NATIVE, "99999999-0000-0000-0000-000000000000");
    for forged in [other_agent, other_session] {
        let rig = Rig::new().await;
        rig.launched().await;
        rig.bind(RUN, AGENT).await;
        rig.write_agent(RUN, AGENT, &[user(1, "x"), forged]);
        let log = rig.ask(S1, AGENT).await.unwrap();
        assert_eq!(unavailable_of(&log), Some(HistoryUnavailable::NotTrusted));
        assert!(log.items.is_empty());
    }
}

#[tokio::test]
async fn a_bad_run_id_in_a_row_is_not_trusted() {
    for bad in ["../x", "wf_", "wf_UPPER", "run-1", "wf_a/b", ".."] {
        let rig = Rig::new().await;
        rig.session(S1, ProviderKind::Claude, Some(NATIVE)).await;
        rig.store
            .put_workflow_agent_log_for_tests(WorkflowAgentLogRow {
                session_id: S1.into(),
                agent_id: AGENT.to_owned(),
                run_id: bad.to_owned(),
                native_id: NATIVE.to_owned(),
                cwd: rig.cwd(),
                provider_home: rig.home().to_string_lossy().into_owned(),
                created_at: 5,
            })
            .await
            .unwrap();
        let log = rig.ask(S1, AGENT).await.unwrap();
        assert_eq!(
            unavailable_of(&log),
            Some(HistoryUnavailable::NotTrusted),
            "{bad:?}"
        );
    }
}

#[test]
fn bytes_appended_after_open_are_not_read() {
    use std::io::Write;
    let first = join(&[user(1, "one"), assistant(2, "two")]);
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&first).unwrap();
    let size = first.len() as u64;
    // The provider appends a third record after the size was captured.
    file.write_all(&join(&[assistant(3, "three")])).unwrap();
    let log = read_log(
        &file,
        size,
        NATIVE,
        AGENT,
        &Limits::default(),
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let texts: Vec<&str> = log.items.iter().map(|i| i.text.as_str()).collect();
    assert_eq!(texts, ["one", "two"]);
    assert_eq!(log.state, HistoryState::Complete);
}

// ------------------------------------------------------------- the pages

fn event(update: WorkflowUpdate) -> NewEvent {
    Event::WorkflowUpdated(Box::new(update)).into()
}

fn update(
    task: &str,
    run_id: Option<&str>,
    agents: Vec<WorkflowAgent>,
    phases: Vec<WorkflowPhase>,
) -> WorkflowUpdate {
    WorkflowUpdate {
        task_id: task.to_owned(),
        node_id: Some(format!("node-{task}")),
        name: Some(format!("name {task}")),
        run_id: run_id.map(str::to_owned),
        status: "running".to_owned(),
        untracked: false,
        phases,
        running_agents: 0,
        agents_total: u32::try_from(agents.len()).unwrap(),
        agents,
    }
}

fn plain(index: u32) -> WorkflowAgent {
    WorkflowAgent {
        index,
        label: format!("agent {index}"),
        state: "running".to_owned(),
        ..WorkflowAgent::default()
    }
}

/// `n` control characters: each is a 6-byte JSON escape.
fn cap(n: usize) -> String {
    "\u{1}".repeat(n)
}

/// An agent with every text at its cap (design §4.1 Limits).
fn maximum_agent(index: u32) -> WorkflowAgent {
    WorkflowAgent {
        index,
        label: cap(120),
        state: cap(32),
        phase_index: Some(99),
        agent_id: None,
        model: Some(cap(64)),
        attempt: Some(u32::MAX),
        tool_calls: Some(u32::MAX),
        tokens: Some(u64::MAX),
        last_tool: Some(cap(64)),
        prompt_preview: Some(cap(200)),
        result_preview: Some(cap(200)),
        error: Some(cap(200)),
        started_at: Some(i64::MAX),
    }
}

fn maximum_phases() -> Vec<WorkflowPhase> {
    (0..100)
        .map(|index| WorkflowPhase {
            index,
            title: cap(120),
        })
        .collect()
}

/// Adds a run of `count` maximum agents in events of 25 agents; the first
/// event carries the 100 phases. Returns the last envelope.
async fn add_maximum_run(rig: &Rig, task: &str, count: u32) -> EventEnvelope {
    let mut last = None;
    for chunk in (0..count).collect::<Vec<u32>>().chunks(25) {
        let phases = if chunk[0] == 0 {
            maximum_phases()
        } else {
            Vec::new()
        };
        let agents = chunk.iter().map(|i| maximum_agent(*i)).collect();
        let mut sent = update(task, None, agents, phases);
        sent.agents_total = count;
        let out = rig
            .store
            .append(S1.into(), vec![event(sent)])
            .await
            .unwrap();
        last = out.into_iter().next_back();
    }
    last.unwrap()
}

#[tokio::test]
async fn the_first_page_has_as_of_and_every_run_in_order() {
    let rig = Rig::new().await;
    rig.session(S1, ProviderKind::Claude, Some(NATIVE)).await;
    let phase = WorkflowPhase {
        index: 0,
        title: "plan".to_owned(),
    };
    // The task ids are not in the order of the runs.
    let mut last = Vec::new();
    for (task, agents, phases) in [
        ("t2", vec![plain(0), plain(1)], vec![phase]),
        ("t1", vec![plain(0)], Vec::new()),
        ("t0", Vec::new(), Vec::new()),
    ] {
        last = rig
            .store
            .append(S1.into(), vec![event(update(task, None, agents, phases))])
            .await
            .unwrap();
    }
    let page = rig.page(None).await.unwrap();
    assert_eq!(page.as_of, last[0].session_seq);
    assert!(page.next.is_none());
    let tasks: Vec<&str> = page
        .workflows
        .iter()
        .map(|r| r.state.task_id.as_str())
        .collect();
    assert_eq!(tasks, ["t2", "t1", "t0"]);
    let orders: Vec<u64> = page.workflows.iter().map(|r| r.run_order).collect();
    assert!(orders.windows(2).all(|w| w[0] < w[1]), "{orders:?}");
    let first = &page.workflows[0].state;
    assert_eq!(first.name.as_deref(), Some("name t2"));
    assert_eq!(first.node_id.as_deref(), Some("node-t2"));
    assert_eq!(first.status, "running");
    assert_eq!(first.phases.len(), 1);
    assert_eq!(first.agents.len(), 2);
    assert_eq!(first.agents_total, 2);
    assert_eq!(page.workflows[1].state.agents.len(), 1);
    assert!(page.workflows[2].state.agents.is_empty());
}

#[tokio::test]
async fn an_unknown_session_is_unknown_session() {
    let rig = Rig::new().await;
    assert!(
        rig.store
            .workflow_page(SessionId::from("nope"), None)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_session_without_workflows_gets_an_empty_page() {
    let rig = Rig::new().await;
    rig.session(S1, ProviderKind::Claude, Some(NATIVE)).await;
    let page = rig.page(None).await.unwrap();
    assert!(page.workflows.is_empty());
    assert!(page.next.is_none());
    // A Codex session has no workflows either.
    rig.session("s2", ProviderKind::Codex, None).await;
    let page = rig
        .store
        .workflow_page("s2".into(), None)
        .await
        .unwrap()
        .unwrap();
    assert!(page.workflows.is_empty());
    assert!(page.next.is_none());
}

/// Follows `next` until it is absent and checks every page against the
/// budget and the control frame limit. Returns the pages.
async fn walk(rig: &Rig) -> Vec<WorkflowsPage> {
    let mut pages = Vec::new();
    let mut after: Option<WorkflowsCursor> = None;
    loop {
        let page = rig.page(after).await.unwrap();
        let bytes = serde_json::to_vec(&page).unwrap().len();
        assert!(bytes <= WORKFLOWS_PAGE_BUDGET, "page of {bytes} bytes");
        let frame = ControlFrame::Response {
            id: u64::MAX,
            outcome: Outcome::Ok(serde_json::to_value(&page).unwrap()),
        };
        let framed = serde_json::to_string(&frame).unwrap().len();
        assert!(framed <= MAX_CONTROL_FRAME_BYTES, "frame of {framed} bytes");
        assert!(!page.workflows.is_empty(), "a page holds an item");
        let next = page.next;
        if let (Some(before), Some(now)) = (after, next) {
            assert!(
                (now.run_order, now.agent_index) > (before.run_order, before.agent_index),
                "{before:?} then {now:?}"
            );
        }
        pages.push(page);
        match next {
            Some(cursor) => after = Some(cursor),
            None => return pages,
        }
        assert!(pages.len() < 10_000, "the pages do not end");
    }
}

/// The agents and the run starts of a list of pages.
type Flat = (Vec<(u64, u32)>, Vec<(u64, usize)>);

/// The `(run_order, index)` of every agent of the pages, in page order, and
/// the pages on which each run starts with its phases.
fn flatten(pages: &[WorkflowsPage]) -> Flat {
    let mut agents = Vec::new();
    let mut starts = Vec::new();
    let mut seen = BTreeSet::new();
    for page in pages {
        for run in &page.workflows {
            if seen.insert(run.run_order) {
                starts.push((run.run_order, run.state.phases.len()));
            } else {
                assert!(run.state.phases.is_empty(), "phases come once per run");
            }
            for agent in &run.state.agents {
                agents.push((run.run_order, agent.index));
            }
        }
    }
    (agents, starts)
}

#[tokio::test]
async fn a_maximum_run_is_paged_under_the_budget() {
    let rig = Rig::new().await;
    rig.session(S1, ProviderKind::Claude, Some(NATIVE)).await;
    add_maximum_run(&rig, "t1", 1000).await;
    let pages = walk(&rig).await;
    assert!(pages.len() > 1, "{} pages", pages.len());
    let (agents, starts) = flatten(&pages);
    // Every agent once, in index order; the phases only on the first page.
    assert_eq!(agents.len(), 1000);
    assert!(agents.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].1, 100);
    assert_eq!(pages[0].workflows[0].state.phases.len(), 100);
    // The header repeats the absolute fields of the run.
    for page in &pages {
        let state = &page.workflows[0].state;
        assert_eq!(state.agents_total, 1000);
        assert_eq!(state.name.as_deref(), Some("name t1"));
    }
}

#[tokio::test]
async fn eight_maximum_runs_page_to_the_end() {
    let rig = Rig::new().await;
    rig.session(S1, ProviderKind::Claude, Some(NATIVE)).await;
    let mut tasks = Vec::new();
    for n in 0..8 {
        let task = format!("t{n}");
        add_maximum_run(&rig, &task, 1000).await;
        tasks.push(task);
    }
    let pages = walk(&rig).await;
    assert!(pages.len() > 64, "{} pages", pages.len());
    assert!(pages.last().unwrap().next.is_none());
    let (agents, starts) = flatten(&pages);
    // Every agent of every run once, in run order and index order.
    assert_eq!(agents.len(), 8 * 1000);
    assert!(agents.windows(2).all(|w| w[0] < w[1]));
    let orders: Vec<u64> = starts.iter().map(|s| s.0).collect();
    assert_eq!(orders.len(), 8);
    assert!(orders.windows(2).all(|w| w[0] < w[1]));
    assert!(starts.iter().all(|s| s.1 == 100));
    for (n, task) in tasks.iter().enumerate() {
        let run_order = orders[n];
        let in_run = agents.iter().filter(|a| a.0 == run_order).count();
        assert_eq!(in_run, 1000, "{task}");
    }
}
