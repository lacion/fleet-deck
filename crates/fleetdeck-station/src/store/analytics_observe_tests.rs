//! The Fleet Deck counters, the execution intervals and the event cursor
//! (B29, design `analytics.md` sections 3.6 and 3.6a).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use fleetdeck_proto::event::{
    ApprovalOption, DiffSummary, Event, ExpiredReason, RequestKind, ResolvedBy, ResponseChannel,
    SessionState, ToolKind, ToolStatus, TurnStatus, WorkflowUpdate,
};
use fleetdeck_testkit::Det;

use super::analytics_observe::close_ended_runs;
use super::tests::{new_session, store};
use super::*;

/// One test rig: a store, the clock and the sessions `s1`, `s2`.
struct Rig {
    det: Det,
    s: Store,
}

async fn rig() -> Rig {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session("s1")).await.unwrap();
    s.create_session(new_session("s2")).await.unwrap();
    Rig { det, s }
}

impl Rig {
    /// Appends `event` to `session` at the time `at`.
    async fn put(&self, session: &str, at: u64, event: Event) {
        self.det.set(at);
        self.s
            .append(session.into(), vec![event.into()])
            .await
            .unwrap();
    }

    async fn exec(&self, sql: &'static str) {
        call(&self.s.inner.db, move |conn| {
            conn.execute_batch(sql)?;
            Ok(())
        })
        .await
        .unwrap();
    }

    /// Every counter, summed over hours and sessions.
    async fn counts(&self) -> BTreeMap<String, i64> {
        call(&self.s.inner.db, |conn| {
            let mut stmt =
                conn.prepare("SELECT metric, sum(value) FROM analytics_activity GROUP BY metric")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<BTreeMap<String, i64>>>()?)
        })
        .await
        .unwrap()
    }

    async fn count(&self, metric: &str) -> i64 {
        self.counts().await.get(metric).copied().unwrap_or(0)
    }

    async fn scalar(&self, sql: &'static str) -> i64 {
        call(&self.s.inner.db, move |conn| {
            Ok(conn.query_row(sql, [], |r| r.get::<_, i64>(0))?)
        })
        .await
        .unwrap()
    }

    /// The intervals of a session as `(started_at, ended_at)`.
    async fn runs(&self, session: &'static str) -> Vec<(i64, Option<i64>)> {
        call(&self.s.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT started_at, ended_at FROM analytics_exec WHERE session_id = ?1
                 ORDER BY started_at",
            )?;
            let rows = stmt.query_map([session], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap()
    }

    /// Pretends that the cursor never ran: the counters are empty and the
    /// cursor stands at `next`.
    async fn forget(&self, next: i64) {
        let sql = format!(
            "DELETE FROM analytics_activity; DELETE FROM analytics_open;
             DELETE FROM analytics_exec; DELETE FROM analytics_workflows;
             UPDATE analytics_meta SET next_seq = {next}, events_since = NULL;"
        );
        call(&self.s.inner.db, move |conn| {
            conn.execute_batch(&sql)?;
            Ok(())
        })
        .await
        .unwrap();
    }

    async fn step(&self, limit: u32) -> bool {
        self.s.analytics_catch_up_step(limit).await.unwrap()
    }
}

fn started(resumed: Option<bool>) -> Event {
    Event::SessionStarted {
        provider: fleetdeck_proto::event::ProviderKind::Claude,
        provider_instance: "claude".into(),
        provider_version: None,
        model: None,
        cwd: "/w".to_owned(),
        mode: None,
        capabilities: Box::new(fleetdeck_proto::capability::Capabilities::minimal()),
        resumed,
        provider_refs: fleetdeck_proto::event::ProviderRefs::new(),
        title: None,
        branch: None,
        repo: None,
    }
}

fn turn_started(turn: &str) -> Event {
    Event::TurnStarted {
        run_id: "run-1".into(),
        turn_id: turn.into(),
        provider_refs: fleetdeck_proto::event::ProviderRefs::new(),
    }
}

fn turn_completed(turn: &str, status: TurnStatus) -> Event {
    Event::TurnCompleted {
        turn_id: turn.into(),
        status,
        usage: None,
    }
}

fn tool_started(node: &str, kind: ToolKind) -> Event {
    Event::ToolStarted {
        node_id: node.into(),
        parent_node_id: None,
        tool_kind: kind,
        title: "t".to_owned(),
        input_summary: String::new(),
        files: Vec::new(),
        provider_refs: fleetdeck_proto::event::ProviderRefs::new(),
    }
}

fn tool_completed(node: &str, status: ToolStatus) -> Event {
    Event::ToolCompleted {
        node_id: node.into(),
        status,
        files: Vec::new(),
        exit_code: None,
        provider_refs: serde_json::Map::new(),
    }
}

fn approval(request: &str) -> Event {
    Event::ApprovalRequested {
        request_id: request.into(),
        node_id: None,
        request_kind: RequestKind::Command,
        title: "Bash".to_owned(),
        details: String::new(),
        input: None,
        options: vec![ApprovalOption {
            option_id: "allow".to_owned(),
            label: "allow".to_owned(),
            kind: None,
        }],
        response: ResponseChannel::RpcReply,
        provider_refs: serde_json::Map::new(),
    }
}

fn error(code: &str) -> Event {
    Event::Error {
        code: code.to_owned(),
        message: "m".to_owned(),
        retry_at: None,
    }
}

fn workflow(task: &str, status: &str) -> Event {
    Event::WorkflowUpdated(Box::new(WorkflowUpdate {
        task_id: task.to_owned(),
        node_id: None,
        name: None,
        run_id: None,
        status: status.to_owned(),
        untracked: false,
        phases: Vec::new(),
        agents: Vec::new(),
        running_agents: 0,
        agents_total: 0,
    }))
}

fn ended() -> Event {
    Event::SessionEnded {
        reason: "done".to_owned(),
        exit_code: Some(0),
    }
}

fn state(state: SessionState) -> Event {
    Event::SessionStateChanged {
        state,
        detail: None,
    }
}

#[tokio::test]
async fn a_turn_counts_its_status_and_duration() {
    let r = rig().await;
    r.put("s1", 1_000, turn_started("t1")).await;
    r.put("s1", 4_000, turn_completed("t1", TurnStatus::Failed))
        .await;
    assert_eq!(r.count("turns.failed").await, 1);
    assert_eq!(r.count("turns.ms").await, 3_000);
    assert_eq!(r.count("turns.timed").await, 1);
    assert_eq!(r.scalar("SELECT count(*) FROM analytics_open").await, 0);
    // A completion without its start counts the status only.
    r.put("s1", 5_000, turn_completed("t2", TurnStatus::Done))
        .await;
    assert_eq!(r.count("turns.done").await, 1);
    assert_eq!(r.count("turns.timed").await, 1);
}

#[tokio::test]
async fn a_run_start_and_an_artifact_count() {
    let r = rig().await;
    r.put(
        "s1",
        1_000,
        Event::RunStarted {
            run_id: "run-1".into(),
            input_summary: "hi".to_owned(),
            origin: fleetdeck_proto::event::RunOrigin::Board,
            author: None,
            input_id: None,
            input_text: None,
        },
    )
    .await;
    r.put(
        "s1",
        2_000,
        Event::ArtifactPublished {
            artifact_id: "a1".to_owned(),
            version: 1,
            title: "t".to_owned(),
            artifact_kind: fleetdeck_proto::artifacts::ArtifactKind::Html,
        },
    )
    .await;
    assert_eq!(r.count("runs").await, 1);
    assert_eq!(r.count("artifacts").await, 1);
}

#[tokio::test]
async fn a_tool_failure_counts_under_its_kind() {
    let r = rig().await;
    r.put("s1", 1_000, tool_started("n1", ToolKind::Shell))
        .await;
    r.put("s1", 2_000, tool_completed("n1", ToolStatus::Failed))
        .await;
    r.put("s1", 3_000, tool_started("n2", ToolKind::Web)).await;
    r.put("s1", 4_000, tool_completed("n2", ToolStatus::Denied))
        .await;
    r.put("s1", 5_000, tool_started("n3", ToolKind::Read)).await;
    r.put("s1", 6_000, tool_completed("n3", ToolStatus::Ok))
        .await;
    r.put("s1", 7_000, tool_completed("n9", ToolStatus::Failed))
        .await;
    let c = r.counts().await;
    assert_eq!(c["tools.shell"], 1);
    assert_eq!(c["tools.failed.shell"], 1);
    assert_eq!(c["tools.denied.web"], 1);
    assert_eq!(c["tools.read"], 1);
    assert_eq!(c["tools.failed.other"], 1);
    assert!(!c.contains_key("tools.failed.read"));
    assert_eq!(r.scalar("SELECT count(*) FROM analytics_open").await, 0);
}

#[tokio::test]
async fn an_approval_counts_who_answered_and_the_wait() {
    let r = rig().await;
    r.put("s1", 1_000, approval("q1")).await;
    r.put(
        "s1",
        3_500,
        Event::ApprovalResolved {
            request_id: "q1".into(),
            option_id: "allow".to_owned(),
            by: ResolvedBy::Board,
            answered_by: None,
        },
    )
    .await;
    r.put("s1", 4_000, approval("q2")).await;
    r.put(
        "s1",
        4_500,
        Event::ApprovalExpired {
            request_id: "q2".into(),
            reason: ExpiredReason::TurnEnded,
        },
    )
    .await;
    let c = r.counts().await;
    assert_eq!(c["approvals.asked"], 2);
    assert_eq!(c["approvals.answered.board"], 1);
    assert_eq!(c["approvals.wait_ms"], 2_500);
    assert_eq!(c["approvals.waited"], 1);
    assert_eq!(c["approvals.expired"], 1);
    assert_eq!(r.scalar("SELECT count(*) FROM analytics_open").await, 0);
}

#[tokio::test]
async fn an_error_code_that_does_not_match_counts_as_other() {
    let r = rig().await;
    r.put("s1", 1_000, error("usage_limit")).await;
    r.put("s1", 2_000, error("Bad Code!")).await;
    r.put("s1", 3_000, error("")).await;
    let c = r.counts().await;
    assert_eq!(c["errors"], 3);
    assert_eq!(c["errors.usage_limit"], 1);
    assert_eq!(c["errors.other"], 2);
}

#[tokio::test]
async fn a_finalized_turn_adds_files_and_lines() {
    let r = rig().await;
    let finalized = |summary| Event::TurnFinalized {
        turn_id: "t1".into(),
        checkpoint: None,
        diff_summary: summary,
    };
    r.put(
        "s1",
        1_000,
        finalized(Some(DiffSummary {
            files_changed: 3,
            insertions: 40,
            deletions: 7,
        })),
    )
    .await;
    r.put("s1", 2_000, finalized(None)).await;
    let c = r.counts().await;
    assert_eq!(c["files.changed"], 3);
    assert_eq!(c["lines.added"], 40);
    assert_eq!(c["lines.removed"], 7);
}

#[tokio::test]
async fn a_resume_is_not_a_start() {
    let r = rig().await;
    r.put("s1", 1_000, started(Some(true))).await;
    r.put("s1", 2_000, started(Some(false))).await;
    r.put("s1", 3_000, started(None)).await;
    assert_eq!(r.count("resumes").await, 1);
    assert_eq!(r.count("starts").await, 2);
}

#[tokio::test]
async fn a_workflow_counts_once_across_its_updates() {
    let r = rig().await;
    r.put("s1", 1_000, workflow("w1", "running")).await;
    r.put("s1", 2_000, workflow("w1", "running")).await;
    r.put("s1", 3_000, workflow("w1", "completed")).await;
    assert_eq!(r.count("workflows").await, 1);
    r.put("s2", 4_000, workflow("w1", "running")).await;
    assert_eq!(r.count("workflows").await, 2);
}

#[tokio::test]
async fn a_workflow_already_in_workflow_runs_still_counts_once() {
    let r = rig().await;
    r.exec(
        "INSERT INTO workflow_runs
           (session_id, task_id, run_order, node_id, name, run_id, status,
            untracked, running_agents, agents_total)
         VALUES ('s1', 'w1', 1, NULL, NULL, NULL, 'running', 0, 0, 0)",
    )
    .await;
    r.put("s1", 1_000, workflow("w1", "running")).await;
    r.put("s1", 2_000, workflow("w1", "completed")).await;
    assert_eq!(r.count("workflows").await, 1);
}

#[tokio::test]
async fn the_end_of_a_session_clears_its_open_rows() {
    let r = rig().await;
    let mut at = 0;
    for end in [
        ended(),
        state(SessionState::Exited),
        state(SessionState::Lost),
    ] {
        at += 10_000;
        r.put("s1", at, turn_started("t1")).await;
        r.put("s1", at + 1, tool_started("n1", ToolKind::Shell))
            .await;
        r.put("s1", at + 2, approval("q1")).await;
        r.put("s2", at + 3, turn_started("t9")).await;
        assert_eq!(
            r.scalar("SELECT count(*) FROM analytics_open WHERE session_id = 's1'")
                .await,
            3
        );
        r.put("s1", at + 4, end).await;
        assert_eq!(
            r.scalar("SELECT count(*) FROM analytics_open WHERE session_id = 's1'")
                .await,
            0
        );
        // The other session keeps its row.
        assert_eq!(
            r.scalar("SELECT count(*) FROM analytics_open WHERE session_id = 's2'")
                .await,
            1
        );
        r.put("s2", at + 5, turn_completed("t9", TurnStatus::Done))
            .await;
    }
}

#[tokio::test]
async fn a_completed_run_clears_its_open_tools_only() {
    let r = rig().await;
    r.put("s1", 1_000, turn_started("t1")).await;
    r.put("s1", 1_001, tool_started("n1", ToolKind::Shell))
        .await;
    r.put(
        "s1",
        1_002,
        Event::RunCompleted {
            run_id: "run-1".into(),
            status: TurnStatus::Done,
            background_pending: false,
        },
    )
    .await;
    assert_eq!(
        r.scalar("SELECT count(*) FROM analytics_open WHERE kind = 'tool'")
            .await,
        0
    );
    assert_eq!(
        r.scalar("SELECT count(*) FROM analytics_open WHERE kind = 'turn'")
            .await,
        1
    );
}

#[tokio::test]
async fn message_deltas_write_nothing() {
    let r = rig().await;
    r.put(
        "s1",
        1_000,
        Event::MessageDelta {
            turn_id: "t1".into(),
            parent_node_id: None,
            text: "x".to_owned(),
        },
    )
    .await;
    assert!(r.counts().await.is_empty());
    assert_eq!(r.scalar("SELECT count(*) FROM analytics_open").await, 0);
    assert_eq!(r.scalar("SELECT count(*) FROM analytics_exec").await, 0);
    // The cursor moved on and the start date is set.
    assert_eq!(r.scalar("SELECT next_seq FROM analytics_meta").await, 2);
    assert_eq!(
        r.scalar("SELECT events_since FROM analytics_meta").await,
        1_000
    );
}

#[tokio::test]
async fn a_start_opens_and_an_end_closes_the_run() {
    let r = rig().await;
    r.put("s1", 1_000, started(None)).await;
    assert_eq!(r.runs("s1").await, vec![(1_000, None)]);
    r.put("s1", 5_000, ended()).await;
    assert_eq!(r.runs("s1").await, vec![(1_000, Some(5_000))]);
    // A second start of the same session closes an interval that is open.
    r.put("s1", 6_000, started(Some(true))).await;
    r.put("s1", 7_000, started(Some(true))).await;
    assert_eq!(
        r.runs("s1").await,
        vec![(1_000, Some(5_000)), (6_000, Some(7_000)), (7_000, None)]
    );
}

#[tokio::test]
async fn a_start_of_another_holder_closes_the_open_run() {
    let r = rig().await;
    r.exec("UPDATE sessions SET native_session_id = 'conv-1' WHERE session_id IN ('s1', 's2')")
        .await;
    r.put("s1", 1_000, started(None)).await;
    r.put("s2", 4_000, started(Some(true))).await;
    assert_eq!(r.runs("s1").await, vec![(1_000, Some(4_000))]);
    assert_eq!(r.runs("s2").await, vec![(4_000, None)]);
}

#[tokio::test]
async fn a_start_of_another_conversation_closes_nothing() {
    let r = rig().await;
    r.exec("UPDATE sessions SET native_session_id = 'conv-1' WHERE session_id = 's1'")
        .await;
    r.exec("UPDATE sessions SET native_session_id = 'conv-2' WHERE session_id = 's2'")
        .await;
    r.put("s1", 1_000, started(None)).await;
    r.put("s2", 4_000, started(None)).await;
    assert_eq!(r.runs("s1").await, vec![(1_000, None)]);
}

#[tokio::test]
async fn a_lost_session_closes_its_run() {
    let r = rig().await;
    r.put("s1", 1_000, started(None)).await;
    r.put("s1", 9_000, state(SessionState::Lost)).await;
    assert_eq!(r.runs("s1").await, vec![(1_000, Some(9_000))]);
}

#[tokio::test]
async fn the_start_of_the_station_closes_the_runs_of_ended_sessions() {
    let r = rig().await;
    r.put("s1", 1_000, started(None)).await;
    r.put("s1", 3_000, turn_started("t1")).await;
    r.put("s2", 2_000, started(None)).await;
    // `s1` ended without its end event being stored as an interval end.
    r.exec("UPDATE sessions SET state = 'exited' WHERE session_id = 's1'")
        .await;
    let n = call(&r.s.inner.db, |conn| close_ended_runs(conn))
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(r.runs("s1").await, vec![(1_000, Some(3_000))]);
    assert_eq!(r.runs("s2").await, vec![(2_000, None)]);
}

#[tokio::test]
async fn catch_up_counts_the_old_events_once_and_observes_the_new() {
    let r = rig().await;
    r.put("s1", 1_000, started(None)).await;
    r.put("s1", 2_000, turn_started("t1")).await;
    r.put("s1", 3_000, tool_started("n1", ToolKind::Shell))
        .await;
    r.put("s1", 4_000, tool_completed("n1", ToolStatus::Failed))
        .await;
    r.put("s1", 5_000, turn_completed("t1", TurnStatus::Done))
        .await;
    r.put("s1", 6_000, error("usage_limit")).await;
    r.put("s1", 7_000, workflow("w1", "running")).await;
    r.put("s1", 8_000, approval("q1")).await;
    r.put("s1", 9_000, workflow("w1", "completed")).await;
    r.put("s1", 9_500, error("usage_limit")).await;
    let expected = r.counts().await;
    // The station before V50: nothing counted for events 1..10.
    r.forget(1).await;
    r.put("s1", 10_000, turn_started("t2")).await;
    r.put("s1", 12_000, turn_completed("t2", TurnStatus::Done))
        .await;
    // The cursor is behind: the two new events wait.
    assert!(r.counts().await.is_empty());
    assert!(!r.step(100).await);
    assert_eq!(r.scalar("SELECT next_seq FROM analytics_meta").await, 13);
    let mut want = expected.clone();
    *want.entry("turns.done".to_owned()).or_default() += 1;
    *want.entry("turns.ms".to_owned()).or_default() += 2_000;
    *want.entry("turns.timed".to_owned()).or_default() += 1;
    assert_eq!(r.counts().await, want);
    assert_eq!(
        r.scalar("SELECT events_since FROM analytics_meta").await,
        1_000
    );
    // A second call counts nothing again.
    assert!(!r.step(100).await);
    assert_eq!(r.counts().await, want);
    // Now every append is counted inline, once.
    r.put("s1", 13_000, error("usage_limit")).await;
    assert_eq!(r.count("errors").await, want["errors"] + 1);
    assert_eq!(r.scalar("SELECT next_seq FROM analytics_meta").await, 14);
}

#[tokio::test]
async fn a_live_event_waits_while_the_cursor_is_behind() {
    let r = rig().await;
    for (i, at) in [1_000_u64, 2_000, 3_000, 4_000, 5_000]
        .into_iter()
        .enumerate()
    {
        let event = if i == 2 {
            turn_started("t1")
        } else {
            error("usage_limit")
        };
        r.put("s1", at, event).await;
    }
    r.forget(1).await;
    r.put("s1", 6_000, turn_completed("t1", TurnStatus::Done))
        .await;
    // Nothing is counted at the append.
    assert!(r.counts().await.is_empty());
    assert_eq!(r.scalar("SELECT count(*) FROM analytics_open").await, 0);
    assert!(!r.step(100).await);
    assert_eq!(r.count("errors").await, 4);
    assert_eq!(r.count("turns.timed").await, 1);
    assert_eq!(r.count("turns.ms").await, 3_000);
    assert_eq!(r.scalar("SELECT count(*) FROM analytics_open").await, 0);
}

#[tokio::test]
async fn catch_up_runs_in_steps() {
    let r = rig().await;
    for n in 1..=10_u64 {
        r.put("s1", n * 1_000, error("usage_limit")).await;
    }
    r.forget(1).await;
    let mut results = Vec::new();
    for _ in 0..4 {
        results.push(r.step(3).await);
    }
    assert_eq!(results, vec![true, true, true, false]);
    assert_eq!(r.scalar("SELECT next_seq FROM analytics_meta").await, 11);
    assert_eq!(r.count("errors").await, 10);
}

#[tokio::test]
async fn an_event_that_does_not_decode_counts_nothing_and_does_not_stop_the_cursor() {
    let r = rig().await;
    r.exec(
        "INSERT INTO events (station_seq, session_id, session_seq, kind, payload, at)
         VALUES (1, 's1', 1, 'turn.completed', '{\"kind\":\"turn.completed\"}', 1000);
         UPDATE meta SET station_seq_next = 2;
         UPDATE analytics_meta SET next_seq = 1;",
    )
    .await;
    assert!(!r.step(10).await);
    assert!(r.counts().await.is_empty());
    assert_eq!(r.scalar("SELECT next_seq FROM analytics_meta").await, 2);
}
