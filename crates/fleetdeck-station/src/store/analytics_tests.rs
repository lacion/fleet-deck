//! The analytics store (B29, design `analytics.md` sections 3.3 to 3.7).

use fleetdeck_proto::analytics::{Audience, HOUR_MS};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_testkit::Det;
use rusqlite::Connection;

use super::analytics::{FileCommit, FileRow, seen_key};
use super::migrations::{migrations, migrations_upto};
use fleetdeck_proto::event::RunOrigin;

use super::tests::{DAY, new_session, store};
use super::*;
use crate::analytics::UsageRecord;

const HOME: &str = "home-1";

async fn exec(s: &Store, sql: String) {
    call(&s.inner.db, move |conn| {
        conn.execute_batch(&sql)?;
        Ok(())
    })
    .await
    .unwrap();
}

async fn scalar(s: &Store, sql: &'static str) -> i64 {
    call(&s.inner.db, move |conn| {
        Ok(conn.query_row(sql, [], |r| r.get::<_, i64>(0))?)
    })
    .await
    .unwrap()
}

/// A session that holds `native` of `provider`.
async fn holder(s: &Store, id: &str, provider: &str, native: &str) {
    s.create_session(new_session(id)).await.unwrap();
    exec(
        s,
        format!(
            "UPDATE sessions SET provider = '{provider}', native_session_id = '{native}'
             WHERE session_id = '{id}'"
        ),
    )
    .await;
}

async fn run(s: &Store, id: &str, start: i64, end: Option<i64>) {
    let end = end.map_or("NULL".to_owned(), |e| e.to_string());
    exec(
        s,
        format!("INSERT INTO analytics_exec (session_id, started_at, ended_at) VALUES ('{id}', {start}, {end})"),
    )
    .await;
}

fn rec(at: i64, conversation: &str, key: Option<&str>, input: u64) -> UsageRecord {
    UsageRecord {
        at,
        model: "m1".to_owned(),
        conversation: conversation.to_owned(),
        key: key.map(str::to_owned),
        input,
        cache_read: 1,
        cache_write: 2,
        output: 3,
        reasoning: 0,
    }
}

fn file(provider: ProviderKind, rel: &str, offset: u64) -> FileRow {
    FileRow {
        home: HOME.to_owned(),
        rel_path: rel.to_owned(),
        provider,
        dev: 1,
        ino: 2,
        mtime_ns: 3,
        offset,
        guard: String::new(),
        state: "{}".to_owned(),
        seen_size: offset,
        malformed: 0,
        scanned_at: 9,
    }
}

async fn commit(s: &Store, provider: ProviderKind, records: Vec<UsageRecord>) {
    s.analytics_commit_file(FileCommit {
        file: file(provider, "a.jsonl", 100),
        records,
    })
    .await
    .unwrap();
}

async fn usage(s: &Store) -> Vec<super::analytics::UsageRaw> {
    s.analytics_usage_rows(0, i64::MAX, Audience::Owner, None)
        .await
        .unwrap()
}

fn seed_session(conn: &Connection, id: &str, created_at: i64) {
    conn.execute(
        "INSERT INTO sessions (session_id, provider, provider_instance, cwd, mode, state,
             offline_approval_policy, summary, created_at, updated_at)
         VALUES (?1, 'claude', 'claude', '/w', 'default', 'running', '{}', '{}', ?2, ?2)",
        rusqlite::params![id, created_at],
    )
    .unwrap();
}

fn seed_event(conn: &Connection, seq: i64, at: i64) {
    conn.execute(
        "INSERT INTO events (station_seq, session_id, session_seq, kind, payload, at)
         VALUES (?1, 's1', ?1, 'k', '{}', ?2)",
        rusqlite::params![seq, at],
    )
    .unwrap();
}

#[test]
fn v50_creates_the_tables_and_the_meta_row() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrations_upto(46).to_latest(&mut conn).unwrap();
    seed_session(&conn, "s1", 1);
    for seq in 5..=9 {
        seed_event(&conn, seq, 1_000 + seq - 5);
    }
    migrations().to_latest(&mut conn).unwrap();
    let (since, next): (Option<i64>, i64) = conn
        .query_row(
            "SELECT events_since, next_seq FROM analytics_meta WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((since, next), (Some(1_000), 5));
    for table in [
        "analytics_files",
        "analytics_threads",
        "analytics_usage",
        "analytics_seen",
        "analytics_activity",
        "analytics_open",
        "analytics_exec",
        "analytics_workflows",
    ] {
        let n: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }

    // An empty events table: the next sequence of the meta row.
    let mut empty = Connection::open_in_memory().unwrap();
    migrations_upto(46).to_latest(&mut empty).unwrap();
    empty
        .execute(
            "INSERT INTO meta (id, incarnation_id, station_seq_next, created_at)
             VALUES (1, 'i', 7, 1)",
            [],
        )
        .unwrap();
    migrations().to_latest(&mut empty).unwrap();
    let (since, next): (Option<i64>, i64) = empty
        .query_row(
            "SELECT events_since, next_seq FROM analytics_meta WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((since, next), (None, 7));

    // A new database has no meta row when V50 runs: the first event is 1.
    let mut fresh = Connection::open_in_memory().unwrap();
    migrations().to_latest(&mut fresh).unwrap();
    let next: i64 = fresh
        .query_row(
            "SELECT next_seq FROM analytics_meta WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(next, 1);
}

#[tokio::test]
async fn a_file_commit_counts_each_key_once() {
    let det = Det::new();
    let s = store(&det).await;
    commit(
        &s,
        ProviderKind::Claude,
        vec![rec(10, "c1", Some("k1"), 5), rec(11, "c1", Some("k2"), 5)],
    )
    .await;
    let report = s
        .analytics_commit_file(FileCommit {
            file: file(ProviderKind::Claude, "a.jsonl", 200),
            records: vec![rec(12, "c1", Some("k2"), 5), rec(13, "c1", Some("k3"), 5)],
        })
        .await
        .unwrap();
    assert_eq!((report.counted, report.duplicates), (1, 1));
    let rows = usage(&s).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].requests, 3);
    assert_eq!(rows[0].input, 15);
    let files = s.analytics_files(HOME.to_owned()).await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].offset, 200);
    assert_eq!(scalar(&s, "SELECT count(*) FROM analytics_seen").await, 3);
}

#[tokio::test]
async fn records_without_a_key_always_count() {
    let det = Det::new();
    let s = store(&det).await;
    commit(&s, ProviderKind::Claude, vec![rec(10, "c1", None, 4)]).await;
    commit(&s, ProviderKind::Claude, vec![rec(10, "c1", None, 4)]).await;
    let rows = usage(&s).await;
    assert_eq!((rows[0].requests, rows[0].input), (2, 8));
    assert_eq!(scalar(&s, "SELECT count(*) FROM analytics_seen").await, 0);
    assert_ne!(seen_key("a"), seen_key("b"));
}

#[tokio::test]
async fn hour_buckets_floor_to_the_utc_hour() {
    let det = Det::new();
    let s = store(&det).await;
    let base = 5 * HOUR_MS;
    commit(
        &s,
        ProviderKind::Claude,
        vec![
            rec(base, "c1", Some("a"), 1),
            rec(base + HOUR_MS - 1, "c1", Some("b"), 1),
            rec(base + HOUR_MS, "c1", Some("c"), 1),
        ],
    )
    .await;
    let rows = usage(&s).await;
    let hours: Vec<(i64, u64)> = rows.iter().map(|r| (r.hour, r.requests)).collect();
    // Newest hour first.
    assert_eq!(hours, vec![(base + HOUR_MS, 1), (base, 2)]);
    // The window is `since <= hour < top`.
    let page = s
        .analytics_usage_rows(base, base + HOUR_MS, Audience::Owner, None)
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].hour, base);
}

#[tokio::test]
async fn the_prune_keeps_events_that_the_cursor_did_not_count() {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session("s1")).await.unwrap();
    s.pair_deck("deck-1".to_owned(), None).await.unwrap();
    let events = (1..=10).map(run_started).collect();
    s.append("s1".into(), events).await.unwrap();
    det.advance(8 * DAY);
    s.append("s1".into(), vec![run_started(11)]).await.unwrap();
    s.ack("deck-1".to_owned(), 10).await.unwrap();
    exec(
        &s,
        "UPDATE analytics_meta SET next_seq = 4 WHERE id = 1".to_owned(),
    )
    .await;
    let report = s.prune().await.unwrap();
    assert_eq!((report.events, report.pruned_through), (3, 3));
    exec(
        &s,
        "UPDATE analytics_meta SET next_seq = 11 WHERE id = 1".to_owned(),
    )
    .await;
    let report = s.prune().await.unwrap();
    assert_eq!((report.events, report.pruned_through), (7, 10));
}

#[tokio::test]
async fn a_record_inside_one_run_goes_to_that_session() {
    let det = Det::new();
    let s = store(&det).await;
    holder(&s, "sa", "claude", "conv-1").await;
    run(&s, "sa", 100, Some(200)).await;
    commit(
        &s,
        ProviderKind::Claude,
        vec![
            rec(100, "conv-1", Some("a"), 1),
            rec(200, "conv-1", Some("b"), 1),
        ],
    )
    .await;
    let rows = usage(&s).await;
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].session_id.as_str(), rows[0].requests), ("sa", 2));
}

#[tokio::test]
async fn a_record_outside_every_run_has_no_session() {
    let det = Det::new();
    let s = store(&det).await;
    // Session A is shared and ended; the conversation went on later.
    holder(&s, "sa", "claude", "conv-1").await;
    run(&s, "sa", 100, Some(200)).await;
    commit(
        &s,
        ProviderKind::Claude,
        vec![rec(500, "conv-1", Some("a"), 1)],
    )
    .await;
    let rows = usage(&s).await;
    assert_eq!(rows[0].session_id, "");
}

#[tokio::test]
async fn an_imported_prefix_has_no_session() {
    let det = Det::new();
    let s = store(&det).await;
    // Session B imported the conversation at t = 1000.
    holder(&s, "sb", "claude", "conv-1").await;
    run(&s, "sb", 1_000, None).await;
    commit(
        &s,
        ProviderKind::Claude,
        vec![
            rec(10, "conv-1", Some("old1"), 1),
            rec(999, "conv-1", Some("old2"), 1),
            rec(1_500, "conv-1", Some("new"), 1),
        ],
    )
    .await;
    let rows = usage(&s).await;
    let by: Vec<(&str, u64)> = rows
        .iter()
        .map(|r| (r.session_id.as_str(), r.requests))
        .collect();
    assert_eq!(by, vec![("sb", 1), ("", 2)]);
}

#[tokio::test]
async fn a_record_in_two_runs_has_no_session() {
    let det = Det::new();
    let s = store(&det).await;
    holder(&s, "sa", "claude", "conv-1").await;
    holder(&s, "sb", "claude", "conv-1").await;
    run(&s, "sa", 100, Some(300)).await;
    run(&s, "sb", 200, Some(400)).await;
    commit(
        &s,
        ProviderKind::Claude,
        vec![
            rec(150, "conv-1", Some("a"), 1),
            rec(250, "conv-1", Some("b"), 1),
            rec(350, "conv-1", Some("c"), 1),
        ],
    )
    .await;
    let mut by: Vec<(String, u64)> = usage(&s)
        .await
        .into_iter()
        .map(|r| (r.session_id, r.requests))
        .collect();
    by.sort();
    assert_eq!(
        by,
        vec![
            (String::new(), 1),
            ("sa".to_owned(), 1),
            ("sb".to_owned(), 1)
        ]
    );
}

#[tokio::test]
async fn an_hour_across_a_run_edge_splits_into_two_buckets() {
    let det = Det::new();
    let s = store(&det).await;
    holder(&s, "sa", "claude", "conv-1").await;
    let edge = 7 * HOUR_MS + 1_000;
    run(&s, "sa", edge, None).await;
    commit(
        &s,
        ProviderKind::Claude,
        vec![
            rec(edge - 1, "conv-1", Some("a"), 1),
            rec(edge, "conv-1", Some("b"), 1),
        ],
    )
    .await;
    let rows = usage(&s).await;
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| r.hour == 7 * HOUR_MS && r.requests == 1)
    );
    let mut ids: Vec<&str> = rows.iter().map(|r| r.session_id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["", "sa"]);
}

async fn thread(s: &Store, id: &str, parent: Option<&str>) {
    s.analytics_put_thread(HOME.to_owned(), id.to_owned(), parent.map(str::to_owned))
        .await
        .unwrap();
}

#[tokio::test]
async fn codex_root_follows_the_ancestry() {
    let det = Det::new();
    let s = store(&det).await;
    thread(&s, "root", None).await;
    thread(&s, "child", Some("root")).await;
    thread(&s, "grand", Some("child")).await;
    assert_eq!(
        s.analytics_root(HOME.to_owned(), "grand".to_owned())
            .await
            .unwrap(),
        ("root".to_owned(), false)
    );
    commit(
        &s,
        ProviderKind::Codex,
        vec![rec(10, "grand", Some("k"), 1)],
    )
    .await;
    let rows = usage(&s).await;
    assert_eq!(rows[0].conversation, "root");
    assert_eq!(rows[0].provider, ProviderKind::Codex);
}

#[tokio::test]
async fn a_missing_parent_is_the_root() {
    let det = Det::new();
    let s = store(&det).await;
    thread(&s, "child", Some("gone")).await;
    assert_eq!(
        s.analytics_root(HOME.to_owned(), "child".to_owned())
            .await
            .unwrap(),
        ("gone".to_owned(), false)
    );
    // A thread without a row is its own root.
    assert_eq!(
        s.analytics_root(HOME.to_owned(), "other".to_owned())
            .await
            .unwrap(),
        ("other".to_owned(), false)
    );
}

#[tokio::test]
async fn an_ancestry_cycle_keeps_the_own_thread() {
    let det = Det::new();
    let s = store(&det).await;
    thread(&s, "a", Some("b")).await;
    thread(&s, "b", Some("a")).await;
    assert_eq!(
        s.analytics_root(HOME.to_owned(), "a".to_owned())
            .await
            .unwrap(),
        ("a".to_owned(), true)
    );
    let report = s
        .analytics_commit_file(FileCommit {
            file: file(ProviderKind::Codex, "r.jsonl", 10),
            records: vec![rec(10, "a", Some("k"), 1)],
        })
        .await
        .unwrap();
    assert!(report.ancestry_broken);
    assert_eq!(usage(&s).await[0].conversation, "a");
    // A chain of 17 steps is broken too; 16 steps still resolve.
    for n in 0..17 {
        thread(&s, &format!("t{n}"), Some(&format!("t{}", n + 1))).await;
    }
    assert_eq!(
        s.analytics_root(HOME.to_owned(), "t1".to_owned())
            .await
            .unwrap(),
        ("t17".to_owned(), false)
    );
    assert_eq!(
        s.analytics_root(HOME.to_owned(), "t0".to_owned())
            .await
            .unwrap(),
        ("t0".to_owned(), true)
    );
}

#[tokio::test]
async fn the_member_coverage_is_clipped_to_the_listed_sessions() {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session("sa")).await.unwrap();
    s.create_session(new_session("sb")).await.unwrap();
    exec(
        &s,
        "UPDATE sessions SET created_at = 100 WHERE session_id = 'sa'".to_owned(),
    )
    .await;
    exec(
        &s,
        "UPDATE sessions SET created_at = 900 WHERE session_id = 'sb'".to_owned(),
    )
    .await;
    exec(
        &s,
        "UPDATE analytics_meta SET events_since = 50 WHERE id = 1".to_owned(),
    )
    .await;
    let all = s.analytics_coverage(None).await.unwrap();
    assert_eq!((all.events_since, all.kept_since), (Some(50), Some(100)));
    let listed = s.analytics_coverage(Some(vec!["sb".into()])).await.unwrap();
    assert_eq!(
        (listed.events_since, listed.kept_since),
        (Some(50), Some(900))
    );
    let none = s.analytics_coverage(Some(Vec::new())).await.unwrap();
    assert_eq!(none.kept_since, None);
}

#[tokio::test]
async fn member_rows_are_attributed_rows_of_the_listed_sessions() {
    let det = Det::new();
    let s = store(&det).await;
    holder(&s, "sa", "claude", "conv-a").await;
    holder(&s, "sb", "claude", "conv-b").await;
    run(&s, "sa", 0, None).await;
    run(&s, "sb", 0, None).await;
    commit(
        &s,
        ProviderKind::Claude,
        vec![
            rec(10, "conv-a", Some("1"), 1),
            rec(10, "conv-b", Some("2"), 1),
            rec(10, "conv-x", Some("3"), 1),
        ],
    )
    .await;
    let owner = usage(&s).await;
    assert_eq!(owner.len(), 3);
    let member = s
        .analytics_usage_rows(0, i64::MAX, Audience::Member, None)
        .await
        .unwrap();
    let mut ids: Vec<&str> = member.iter().map(|r| r.session_id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["sa", "sb"]);
    let listed = s
        .analytics_usage_rows(0, i64::MAX, Audience::Member, Some(vec!["sa".into()]))
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session_id, "sa");
}

#[tokio::test]
async fn kept_counters_come_from_sessions_checkpoints_and_shell_runs() {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session("sa")).await.unwrap();
    let h = 3 * HOUR_MS;
    exec(
        &s,
        format!(
            "UPDATE sessions SET created_at = {h} + 5 WHERE session_id = 'sa';
             INSERT INTO turn_checkpoints (app_turn, session_id, scope, start_state, end_state,
                 state, created_at, updated_at)
             VALUES ('t1', 'sa', 'tracked', 'available', 'available', 'finalized', {h} + 10, 0),
                    ('t2', 'sa', 'tracked', 'available', 'open', 'open', {h} + 11, 0),
                    ('t3', 'sa', 'tracked', 'unavailable', 'unavailable', 'abandoned', {h} + 12, 0);
             INSERT INTO shell_runs (effect_id, session_id, provider, worktree_root, command,
                 state, exit_code, created_at)
             VALUES ('e1', 'sa', 'claude', '/w', 'ls', 'done', 0, {h} + 1),
                    ('e2', 'sa', 'claude', '/w', 'false', 'done', 1, {h} + 2),
                    ('e3', 'sa', 'claude', '/w', 'sleep', 'running', NULL, {h} + 3),
                    ('e4', 'sa', 'claude', '/w', 'ls', 'done', 0, {h} + {HOUR_MS});
             INSERT INTO analytics_activity (hour, session_id, metric, value)
             VALUES ({h}, 'sa', 'runs', 4);"
        ),
    )
    .await;
    let rows = s
        .analytics_activity_rows(0, i64::MAX, Some(vec!["sa".into()]))
        .await
        .unwrap();
    let get = |hour: i64, metric: &str| {
        rows.iter()
            .find(|r| r.hour == hour && r.metric == metric)
            .map(|r| r.value)
    };
    assert_eq!(get(h, "sessions.created"), Some(1));
    assert_eq!(get(h, "turns.recorded"), Some(2));
    assert_eq!(get(h, "shell.commands"), Some(3));
    assert_eq!(get(h, "shell.failed"), Some(1));
    assert_eq!(get(h, "runs"), Some(4));
    assert_eq!(get(h + HOUR_MS, "shell.commands"), Some(1));
    assert_eq!(get(h + HOUR_MS, "shell.failed"), None);
    assert!(
        rows.windows(2).all(|w| w[0].hour >= w[1].hour),
        "newest hour first"
    );
    // Another session list: nothing.
    let none = s
        .analytics_activity_rows(0, i64::MAX, Some(vec!["sb".into()]))
        .await
        .unwrap();
    assert!(none.is_empty());
}

fn run_started(n: u32) -> NewEvent {
    Event::RunStarted {
        author: None,
        input_id: None,
        run_id: format!("run-{n}").into(),
        input_summary: "hi".to_owned(),
        origin: RunOrigin::Board,
        input_text: None,
    }
    .into()
}
