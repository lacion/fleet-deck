//! Tests of the walk and the scanner (B29, design `analytics.md` section
//! 3.5). Each test runs real passes over a temporary provider home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::analytics::{Audience, SourceState};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::native::HomeState;
use fleetdeck_testkit::Det;
use tokio::sync::watch;

use super::ScanStatus;
use super::scan::{
    PassConfig, PassOutcome, Resolver, ScanError, Timing, catch_up, pass, run_loop, source_key,
};
use super::walk::walk_with;
use crate::history::browse::{HomePlan, Resolved};
use crate::store::{Store, StoreConfig};

struct Rig {
    store: Store,
    dir: tempfile::TempDir,
    status: ScanStatus,
    tx: watch::Sender<bool>,
    rx: watch::Receiver<bool>,
    uid: u32,
}

impl Rig {
    async fn new() -> Self {
        let det = Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
        let (tx, rx) = watch::channel(false);
        Self {
            store,
            dir: tempfile::tempdir().unwrap(),
            status: ScanStatus::new(),
            tx,
            rx,
            uid: rustix::process::getuid().as_raw(),
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.home().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn append(&self, rel: &str, content: &str) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(self.home().join(rel))
            .unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }

    fn plans(&self, kind: ProviderKind) -> Vec<HomePlan> {
        vec![HomePlan {
            kind,
            instances: Vec::new(),
            raw: Some(self.home()),
        }]
    }

    async fn pass_as(
        &self,
        kind: ProviderKind,
        resolve: Resolver,
        config: &PassConfig,
        uid: u32,
    ) -> PassOutcome {
        pass(
            &self.store,
            self.plans(kind),
            resolve,
            uid,
            config,
            &self.status,
            &self.rx,
        )
        .await
        .unwrap()
    }

    async fn pass(&self, kind: ProviderKind) -> PassOutcome {
        self.pass_as(kind, resolver(true), &PassConfig::default(), self.uid)
            .await
    }

    /// `(requests, input)` summed over every usage row.
    async fn totals(&self) -> (u64, u64) {
        let rows = self.rows().await;
        (
            rows.iter().map(|r| r.requests).sum(),
            rows.iter().map(|r| r.input).sum(),
        )
    }

    async fn rows(&self) -> Vec<crate::store::analytics::UsageRaw> {
        self.store
            .analytics_usage_rows(0, i64::MAX, Audience::Owner, None)
            .await
            .unwrap()
    }

    fn key(&self) -> String {
        self.status.homes()[0].key.clone()
    }
}

fn resolver(verified: bool) -> Resolver {
    Arc::new(move |plan: &HomePlan| Resolved {
        canon: plan.raw.clone(),
        state: HomeState::Ok,
        exists: true,
        verified,
    })
}

fn resolver_state(state: HomeState) -> Resolver {
    Arc::new(move |plan: &HomePlan| Resolved {
        canon: plan.raw.clone(),
        state: state.clone(),
        exists: true,
        verified: false,
    })
}

/// A Claude assistant record. `id: None` has no dedupe key (it counts in
/// every read).
fn claude(id: Option<&str>, input: u64) -> String {
    let ids = id.map_or(String::new(), |id| {
        format!(r#","requestId":"req_{id}","message_id_marker":"{id}""#)
    });
    let message_id = id.map_or(String::new(), |id| format!(r#""id":"{id}","#));
    format!(
        r#"{{"type":"assistant","timestamp":"2026-10-20T10:00:01.000Z","sessionId":"s1"{ids},"message":{{{message_id}"model":"claude-opus-5-5","usage":{{"input_tokens":{input},"output_tokens":1}}}}}}"#
    ) + "\n"
}

const CLAUDE_FILE: &str = "projects/p1/s1.jsonl";

fn meta(id: &str, parent: Option<&str>) -> String {
    let parent = parent.map_or(String::new(), |p| format!(r#","parent_thread_id":"{p}""#));
    format!(
        r#"{{"timestamp":"2026-10-20T10:00:00.000Z","type":"session_meta","payload":{{"id":"{id}"{parent}}}}}"#
    ) + "\n"
}

fn codex_usage(model: &str, input: u64) -> String {
    let totals = format!(r#"{{"input_tokens":{input},"cached_input_tokens":0,"output_tokens":1}}"#);
    format!(
        r#"{{"timestamp":"2026-10-20T10:00:01.000Z","type":"turn_context","payload":{{"model":"{model}"}}}}
{{"timestamp":"2026-10-20T10:00:05.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{totals},"last_token_usage":{totals}}}}}}}
"#
    )
}

fn rollout(day: u32, id: &str) -> String {
    format!("sessions/2026/10/{day:02}/rollout-2026-10-{day:02}T10-00-00-{id}.jsonl")
}

#[tokio::test]
async fn a_pass_reads_new_lines_only() {
    let rig = Rig::new().await;
    rig.write(CLAUDE_FILE, &(claude(None, 10) + &claude(None, 20)));
    rig.pass(ProviderKind::Claude).await;
    assert_eq!(rig.totals().await, (2, 30));
    rig.append(CLAUDE_FILE, &claude(None, 5));
    rig.pass(ProviderKind::Claude).await;
    // Keyless records count at every read: a read from 0 would give 5 requests.
    assert_eq!(rig.totals().await, (3, 35));
    let rows = rig.store.analytics_files(rig.key()).await.unwrap();
    let size = std::fs::metadata(rig.home().join(CLAUDE_FILE))
        .unwrap()
        .len();
    assert_eq!(
        (rows.len(), rows[0].offset, rows[0].seen_size),
        (1, size, size)
    );
}

#[tokio::test]
async fn a_rewritten_file_is_read_again_without_double_counts() {
    let rig = Rig::new().await;
    rig.write(
        CLAUDE_FILE,
        &(claude(Some("m1"), 10) + &claude(Some("m2"), 20) + &claude(Some("m3"), 30)),
    );
    rig.pass(ProviderKind::Claude).await;
    assert_eq!(rig.totals().await, (3, 60));
    // The new file is shorter than the stored offset.
    rig.write(
        CLAUDE_FILE,
        &(claude(Some("m1"), 10) + &claude(Some("m4"), 40)),
    );
    rig.pass(ProviderKind::Claude).await;
    assert_eq!(rig.totals().await, (4, 100));
}

#[tokio::test]
async fn a_same_size_rewrite_is_read_again() {
    let rig = Rig::new().await;
    rig.write(
        CLAUDE_FILE,
        &(claude(Some("m1"), 10) + &claude(Some("m2"), 20)),
    );
    rig.pass(ProviderKind::Claude).await;
    assert_eq!(rig.totals().await, (2, 30));
    rig.write(
        CLAUDE_FILE,
        &(claude(Some("m1"), 10) + &claude(Some("m3"), 20)),
    );
    let file = std::fs::File::options()
        .write(true)
        .open(rig.home().join(CLAUDE_FILE))
        .unwrap();
    file.set_modified(std::time::SystemTime::now() + Duration::from_secs(60))
        .unwrap();
    rig.pass(ProviderKind::Claude).await;
    // m1 dedupes, m3 is new.
    assert_eq!(rig.totals().await, (3, 50));
}

#[tokio::test]
async fn an_unfinished_file_is_read_in_the_next_pass_without_new_bytes() {
    let rig = Rig::new().await;
    let line = claude(None, 10);
    rig.write(CLAUDE_FILE, &line.repeat(3));
    let small = PassConfig {
        bytes: line.len() as u64 + 1,
        ..PassConfig::default()
    };
    let first = rig
        .pass_as(ProviderKind::Claude, resolver(true), &small, rig.uid)
        .await;
    assert!(first.more);
    assert_eq!(rig.totals().await, (1, 10));
    // The file is unchanged; the rest is read anyway.
    let second = rig.pass(ProviderKind::Claude).await;
    assert!(!second.more);
    assert_eq!(rig.totals().await, (3, 30));
}

#[tokio::test]
async fn an_unfinished_last_line_waits_for_the_next_pass() {
    let rig = Rig::new().await;
    let line = claude(None, 10);
    let (head, tail) = line.trim_end().split_at(40);
    rig.write(CLAUDE_FILE, &(line.clone() + head));
    rig.pass(ProviderKind::Claude).await;
    assert_eq!(rig.totals().await, (1, 10));
    let rows = rig.store.analytics_files(rig.key()).await.unwrap();
    assert_eq!(rows[0].offset, line.len() as u64);
    rig.append(CLAUDE_FILE, &format!("{tail}\n"));
    rig.pass(ProviderKind::Claude).await;
    assert_eq!(rig.totals().await, (2, 20));
}

#[tokio::test]
async fn links_and_files_of_other_users_are_not_read() {
    let rig = Rig::new().await;
    rig.write(CLAUDE_FILE, &claude(None, 10));
    rig.write("outside/real.jsonl", &claude(None, 99));
    std::os::unix::fs::symlink(
        rig.home().join("outside/real.jsonl"),
        rig.home().join("projects/p1/link.jsonl"),
    )
    .unwrap();
    rig.pass(ProviderKind::Claude).await;
    assert_eq!(rig.totals().await, (1, 10));
    let rows = rig.store.analytics_files(rig.key()).await.unwrap();
    assert_eq!(rows.len(), 1);
    // A file that the station user does not own is skipped with no row.
    let other = Rig::new().await;
    other.write(CLAUDE_FILE, &claude(None, 10));
    other
        .pass_as(
            ProviderKind::Claude,
            resolver(true),
            &PassConfig::default(),
            other.uid + 1,
        )
        .await;
    assert_eq!(other.totals().await, (0, 0));
    assert!(
        other
            .store
            .analytics_files(other.key())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn nested_subagent_and_workflow_files_are_read_and_journal_is_not() {
    let rig = Rig::new().await;
    rig.write("projects/p1/s1.jsonl", &claude(None, 1));
    rig.write("projects/p1/s1/subagents/a.jsonl", &claude(None, 10));
    rig.write(
        "projects/p1/s1/subagents/workflows/w1/b.jsonl",
        &claude(None, 100),
    );
    rig.write(
        "projects/p1/s1/subagents/workflows/w1/journal.jsonl",
        &claude(None, 1000),
    );
    rig.write("projects/p1/s1/other/c.jsonl", &claude(None, 10_000));
    rig.pass(ProviderKind::Claude).await;
    assert_eq!(rig.totals().await, (3, 111));
}

#[tokio::test]
async fn codex_metas_are_read_before_usage() {
    let rig = Rig::new().await;
    // The grandchild sorts first, then the child, then the root.
    rig.write(
        &rollout(19, "grand"),
        &(meta("grand", Some("child")) + &codex_usage("gpt-6.1-sol", 7)),
    );
    rig.write(&rollout(20, "child"), &meta("child", Some("root")));
    rig.write(&rollout(21, "root"), &meta("root", None));
    rig.pass(ProviderKind::Codex).await;
    let rows = rig.rows().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].conversation, "root");
    assert_eq!(rows[0].input, 7);
}

#[tokio::test]
async fn an_unverifiable_home_is_read_and_marked() {
    let rig = Rig::new().await;
    rig.write(CLAUDE_FILE, &claude(None, 10));
    rig.pass_as(
        ProviderKind::Claude,
        resolver(false),
        &PassConfig::default(),
        rig.uid,
    )
    .await;
    assert_eq!(rig.totals().await, (1, 10));
    let homes = rig.status.homes();
    assert_eq!(homes.len(), 1);
    assert!(!homes[0].verified);
    assert!(homes[0].complete);
    assert_eq!(homes[0].state, SourceState::Ok);
    assert_eq!(
        homes[0].key,
        source_key(&super::scan::machine_id(), &rig.home())
    );
}

#[tokio::test]
async fn an_untrusted_home_is_unavailable() {
    let rig = Rig::new().await;
    rig.write(CLAUDE_FILE, &claude(None, 10));
    let untrusted = resolver_state(HomeState::Unavailable {
        reason: "home_not_trusted".to_owned(),
    });
    rig.pass_as(
        ProviderKind::Claude,
        untrusted,
        &PassConfig::default(),
        rig.uid,
    )
    .await;
    assert_eq!(rig.totals().await, (0, 0));
    let homes = rig.status.homes();
    assert_eq!(
        homes[0].state,
        SourceState::Unavailable {
            reason: "home_not_trusted".to_owned()
        }
    );
    assert!(!homes[0].complete);
    assert!(
        rig.store
            .analytics_files(rig.key())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn off_reads_nothing_and_reports_off() {
    let rig = Rig::new().await;
    rig.write(CLAUDE_FILE, &claude(None, 10));
    let off = PassConfig {
        enabled: false,
        ..PassConfig::default()
    };
    rig.pass_as(ProviderKind::Claude, resolver(true), &off, rig.uid)
        .await;
    assert_eq!(rig.totals().await, (0, 0));
    assert_eq!(rig.status.homes()[0].state, SourceState::Off);
    assert!(
        rig.store
            .analytics_files(rig.key())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn keeps_days_reads_cleanup_period_days() {
    for (settings, want) in [
        (Some(r#"{"cleanupPeriodDays": 90}"#), 90),
        (Some(r#"{"cleanupPeriodDays": 0}"#), 30),
        (Some(r#"{"cleanupPeriodDays": 40000}"#), 30),
        (Some(r#"{"cleanupPeriodDays": "7"}"#), 30),
        (Some("not json"), 30),
        (None, 30),
    ] {
        let rig = Rig::new().await;
        rig.write(CLAUDE_FILE, &claude(None, 1));
        if let Some(text) = settings {
            rig.write("settings.json", text);
        }
        rig.pass(ProviderKind::Claude).await;
        assert_eq!(rig.status.homes()[0].keeps_days, Some(want), "{settings:?}");
    }
    let rig = Rig::new().await;
    rig.write(&rollout(20, "r"), &meta("r", None));
    rig.pass(ProviderKind::Codex).await;
    assert_eq!(rig.status.homes()[0].keeps_days, None);
}

#[tokio::test]
async fn a_pass_stops_at_its_byte_budget_and_reports_pending_bytes() {
    let rig = Rig::new().await;
    let line = claude(None, 10);
    rig.write(CLAUDE_FILE, &line.repeat(3));
    let one_line = PassConfig {
        bytes: line.len() as u64,
        ..PassConfig::default()
    };
    let outcome = rig
        .pass_as(ProviderKind::Claude, resolver(true), &one_line, rig.uid)
        .await;
    assert!(outcome.more);
    assert_eq!(rig.totals().await, (1, 10));
    let home = &rig.status.homes()[0];
    assert_eq!(home.pending_bytes, 2 * line.len() as u64);
    assert!(!home.complete);
    let outcome = rig.pass(ProviderKind::Claude).await;
    assert!(!outcome.more);
    let home = &rig.status.homes()[0];
    assert_eq!(home.pending_bytes, 0);
    assert!(home.complete);
}

#[tokio::test(start_paused = true)]
async fn the_first_pass_waits_for_the_cursor() {
    // The cursor runs until a step returns false; a failed step is retried.
    let (_tx, mut rx) = watch::channel(false);
    let mut calls: Vec<&'static str> = Vec::new();
    let mut script = vec![Ok(true), Err(ScanError::Task), Ok(true), Ok(false)].into_iter();
    let caught = catch_up(
        || {
            let next = script.next().unwrap();
            calls.push(if next.is_ok() { "step" } else { "failed" });
            async move { next }
        },
        &mut rx,
        Duration::from_secs(30),
    )
    .await;
    assert!(caught);
    assert_eq!(calls, vec!["step", "failed", "step", "step"]);
    assert!(script.next().is_none());
}

#[tokio::test]
async fn the_scanner_task_runs_a_pass_after_the_cursor_and_stops_on_shutdown() {
    let rig = Rig::new().await;
    rig.write(CLAUDE_FILE, &claude(None, 10));
    let status = Arc::new(ScanStatus::new());
    let plans = rig.plans(ProviderKind::Claude);
    let raw = plans[0].raw.clone();
    let task_status = Arc::clone(&status);
    let store = rig.store.clone();
    let rx = rig.rx.clone();
    let uid = rig.uid;
    let task = tokio::spawn(async move {
        run_loop(
            store,
            move || {
                vec![HomePlan {
                    kind: ProviderKind::Claude,
                    instances: Vec::new(),
                    raw: raw.clone(),
                }]
            },
            || resolver(true),
            uid,
            PassConfig::default(),
            task_status,
            rx,
            Timing {
                every: Duration::from_millis(20),
                more: Duration::from_millis(20),
                retry: Duration::from_millis(20),
            },
        )
        .await;
    });
    for _ in 0..200 {
        if !status.homes().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(status.homes().len(), 1);
    assert_eq!(rig.totals().await, (1, 10));
    rig.tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn the_walk_stops_at_its_directory_budget() {
    let dir = tempfile::tempdir().unwrap();
    for n in 0..5 {
        let p: PathBuf = dir.path().join(format!("projects/p{n}"));
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("s.jsonl"), "{}\n").unwrap();
    }
    let root = fleetdeck_core::launch_dir::open_root(Path::new(dir.path())).unwrap();
    let all = walk_with(&root, ProviderKind::Claude, 100, 100);
    assert_eq!(all.files.len(), 5);
    assert!(!all.truncated);
    let few = walk_with(&root, ProviderKind::Claude, 3, 100);
    assert!(few.truncated);
    assert!(few.files.len() < 5);
    let capped = walk_with(&root, ProviderKind::Claude, 100, 2);
    assert!(capped.truncated);
    assert_eq!(capped.files.len(), 2);
}
