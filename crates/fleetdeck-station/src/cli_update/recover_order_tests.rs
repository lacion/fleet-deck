//! The pre-PR review findings 2, 3 and 4 of B28 on the attempt task:
//! - 2: after the hold limit, no file system task of A2 runs on while the
//!   way back, the clean-up or the end of the hold happens;
//! - 3: a result that is not written keeps the journal row, the rollback
//!   copy and the stage, and a quarantine in memory;
//! - 4: the quarantine is in place before the hold ends.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::process::CommandExt;

use super::super::run::Limits;
use super::super::run::tests::{FROM, Rig, rig};
use super::*;

/// The write of an `updated` result fails (a database error).
const REFUSE_UPDATED: &str = "CREATE TRIGGER refuse_end BEFORE UPDATE OF outcome \
     ON cli_update_attempts WHEN NEW.outcome = 'updated' \
     BEGIN SELECT RAISE(ABORT, 'refused by the test'); END";
/// The write of a `rolled_back` result fails.
const REFUSE_ROLLED_BACK: &str = "CREATE TRIGGER refuse_end BEFORE UPDATE OF outcome \
     ON cli_update_attempts WHEN NEW.outcome = 'rolled_back' \
     BEGIN SELECT RAISE(ABORT, 'refused by the test'); END";

fn pos(events: &[String], name: &str) -> usize {
    events
        .iter()
        .position(|e| e == name)
        .unwrap_or_else(|| panic!("no event {name} in {events:?}"))
}

fn limits(r: &Rig, hold: Duration, settle: Duration) {
    *r.engine.limits.lock().unwrap() = Limits {
        stage: Duration::from_secs(60),
        hold,
        settle,
    };
}

/// Starts an attempt and waits until its task ended (the fence of a
/// foreign group takes about 5 s). Returns the attempt id.
async fn run_to_end(r: &Rig) -> String {
    let id = r.run(false, false).await.attempt_id.expect("started");
    for _ in 0..3000 {
        if r.engine.running_id().is_none() {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the attempt did not end");
}

fn kind_event(r: &Rig, what: &str) -> String {
    format!("{what} {:?}", r.kind)
}

// ---- Finding 4 ----------------------------------------------------------

#[tokio::test]
async fn a_process_left_in_the_activation_quarantines_before_the_hold_ends() {
    let r = rig(ProviderKind::Claude).await;
    *r.installer.activate_err.lock().unwrap() = Some(Failure::ProcessLeft);
    run_to_end(&r).await;
    let e = r.events();
    assert!(pos(&e, &kind_event(&r, "quarantine")) < pos(&e, "hold_end"));
    // The durable row was written before the in-memory mark.
    assert_eq!(r.quarantine().await.as_deref(), Some(reason::PROCESS_LEFT));
    assert_eq!(r.journals().await, 1);
}

#[tokio::test]
async fn a_hold_timeout_with_a_failed_fence_quarantines_before_the_hold_ends() {
    let r = rig(ProviderKind::Claude).await;
    limits(&r, Duration::from_millis(200), Duration::from_secs(10));
    *r.installer.activate_sleep.lock().unwrap() = Some(Duration::from_millis(600));
    // A group whose process carries no marker: the fence cannot end it.
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .unwrap();
    let pgid = i32::try_from(child.id()).unwrap();
    let id = r.run(false, false).await.attempt_id.expect("started");
    r.store.cli_update_journal_pgid(r.kind, pgid).await.unwrap();
    for _ in 0..3000 {
        if r.engine.running_id().is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    let e = r.events();
    assert!(pos(&e, "activate_end") < pos(&e, &kind_event(&r, "quarantine")));
    assert!(pos(&e, &kind_event(&r, "quarantine")) < pos(&e, "hold_end"));
    assert!(!e.contains(&"restore".to_owned()));
    let last = r.last().await.unwrap();
    assert_eq!(last.attempt_id, id);
    assert_eq!(last.reason.as_deref(), Some(reason::PROCESS_LEFT));
    assert_eq!(r.journals().await, 1);
}

// ---- Finding 2 ----------------------------------------------------------

#[tokio::test]
async fn a_hold_timeout_waits_for_the_blocking_activation_before_the_way_back() {
    let r = rig(ProviderKind::Claude).await;
    limits(&r, Duration::from_millis(100), Duration::from_secs(10));
    *r.installer.activate_sleep.lock().unwrap() = Some(Duration::from_millis(800));
    run_to_end(&r).await;
    let e = r.events();
    // A2 ended before the way back, and nothing of A2 came after it.
    assert_eq!(e.iter().filter(|x| *x == "activate_end").count(), 1);
    assert!(pos(&e, "activate_end") < pos(&e, "restore"));
    assert!(pos(&e, "restore") < pos(&e, "hold_end"));
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::RolledBack);
    assert_eq!(last.reason.as_deref(), Some(reason::TIMEOUT));
    assert_eq!(r.version(), FROM);
    assert_eq!(r.journals().await, 0);
}

#[tokio::test]
async fn a_blocking_activation_past_the_settle_limit_changes_nothing_more() {
    let r = rig(ProviderKind::Claude).await;
    limits(&r, Duration::from_millis(100), Duration::from_millis(100));
    *r.installer.activate_sleep.lock().unwrap() = Some(Duration::from_millis(1500));
    let id = run_to_end(&r).await;
    let e = r.events();
    // No way back and no clean-up while A2 may still rename.
    assert!(!e.contains(&"restore".to_owned()));
    assert!(!e.contains(&"activate_end".to_owned()));
    assert!(pos(&e, &kind_event(&r, "quarantine")) < pos(&e, "hold_end"));
    assert!(rollback_dir(&r.engine.station_dir, &id).exists());
    assert_eq!(r.journals().await, 1);
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::RollbackFailed);
    assert_eq!(last.reason.as_deref(), Some(reason::PROCESS_LEFT));
    assert_eq!(r.quarantine().await.as_deref(), Some(reason::PROCESS_LEFT));
    // The blocking task ends later; the journal row is still there for the
    // start recovery.
    assert!(r.installer.mutations.settle(Duration::from_secs(10)).await);
    assert_eq!(r.journals().await, 1);
}

// ---- Finding 3 ----------------------------------------------------------

#[tokio::test]
async fn an_unwritten_update_result_keeps_the_rollback_copy_and_quarantines() {
    let r = rig(ProviderKind::Claude).await;
    r.store
        .land_test_sql(REFUSE_UPDATED, Vec::new())
        .await
        .unwrap();
    let id = run_to_end(&r).await;
    let e = r.events();
    assert!(!e.contains(&kind_event(&r, "release")));
    assert!(pos(&e, &kind_event(&r, "quarantine")) < pos(&e, "hold_end"));
    assert!(rollback_dir(&r.engine.station_dir, &id).exists());
    let rows = r.store.cli_update_journals().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].phase, JournalPhase::Activating);
    assert_eq!(r.last().await.unwrap().outcome, UpdateOutcome::Running);
}

#[tokio::test]
async fn an_unwritten_rolled_back_result_keeps_the_journal_and_the_rollback_copy() {
    let r = rig(ProviderKind::Claude).await;
    r.store
        .land_test_sql(REFUSE_ROLLED_BACK, Vec::new())
        .await
        .unwrap();
    *r.installer.activate_err.lock().unwrap() = Some(Failure::Failed(reason::VERSION_MISMATCH));
    let id = run_to_end(&r).await;
    let e = r.events();
    assert!(pos(&e, "restore") < pos(&e, &kind_event(&r, "quarantine")));
    assert!(pos(&e, &kind_event(&r, "quarantine")) < pos(&e, "hold_end"));
    assert!(rollback_dir(&r.engine.station_dir, &id).exists());
    assert_eq!(r.journals().await, 1);
}

#[tokio::test]
async fn an_unwritten_recovery_result_keeps_the_stage_and_the_quarantine_row() {
    let r = rig(ProviderKind::Claude).await;
    let stage = r.dir.path().join("stage");
    std::fs::write(&stage, b"x").unwrap();
    let a = new_attempt(
        "a1",
        r.kind,
        UpdateTrigger::Owner,
        InstallMethod::Native,
        "stable",
        Some(FROM),
        Some(super::super::run::tests::TARGET),
    );
    let j = crate::store::cli_update_journal::NewJournal {
        provider: r.kind,
        method: InstallMethod::Native,
        from_version: FROM.to_owned(),
        target_version: super::super::run::tests::TARGET.to_owned(),
        launcher: "/x".into(),
        dest: "/x".into(),
        stage_root: stage.clone(),
        rollback_sha256: Some("aa".repeat(32)),
    };
    r.store.cli_update_begin(a, j).await.unwrap();
    let act = crate::store::cli_update_journal::Activation {
        rollback_path: r.dir.path().join("rollback"),
        rollback_sha256: Some("aa".repeat(32)),
        moved: Vec::new(),
    };
    r.store
        .cli_update_journal_activate(r.kind, act)
        .await
        .unwrap();
    r.store
        .land_test_sql(REFUSE_ROLLED_BACK, Vec::new())
        .await
        .unwrap();
    let cfg = super::super::run::tests::test_config();
    recover(&Recovery {
        store: &r.store,
        installer: &*r.installer,
        targets: &r.engine.targets,
        station_dir: &r.engine.station_dir,
        curl: &r.engine.curl,
        ids: &r.engine.ids,
        cfg: &cfg,
        limit: Duration::from_secs(30),
    })
    .await;
    assert!(r.events().contains(&"restore".to_owned()));
    assert!(stage.exists());
    assert_eq!(r.journals().await, 1);
    assert_eq!(
        r.quarantine().await.as_deref(),
        Some(reason::RECOVERY_PENDING)
    );
}
