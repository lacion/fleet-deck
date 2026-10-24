//! The recovery at the station start (design §3.7.2) and the texts of the
//! quarantine. `recover` takes no session manager: these tests build none.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::process::CommandExt;
use std::time::Duration;

use fleetdeck_proto::cli_update::UpdateTrigger;

use super::super::run::tests::{FROM, Rig, TARGET, rig, test_config};
use super::*;
use crate::store::cli_update_journal::{Activation, NewJournal};

async fn recover_with(r: &Rig, limit: Duration) {
    let cfg = test_config();
    recover(&Recovery {
        store: &r.store,
        installer: &*r.installer,
        targets: &r.engine.targets,
        station_dir: &r.engine.station_dir,
        curl: &r.engine.curl,
        ids: &r.engine.ids,
        cfg: &cfg,
        limit,
    })
    .await;
}

/// A journal row `a1` whose stage is a real file. `activating` moves it to
/// the phase `activating`.
async fn journal(r: &Rig, activating: bool) -> std::path::PathBuf {
    let stage = r.dir.path().join("stage");
    std::fs::write(&stage, b"x").unwrap();
    let a = new_attempt(
        "a1",
        r.kind,
        UpdateTrigger::Owner,
        InstallMethod::Native,
        "stable",
        Some(FROM),
        Some(TARGET),
    );
    let j = NewJournal {
        provider: r.kind,
        method: InstallMethod::Native,
        from_version: FROM.to_owned(),
        target_version: TARGET.to_owned(),
        launcher: "/x".into(),
        dest: "/x".into(),
        stage_root: stage.clone(),
        rollback_sha256: Some("aa".repeat(32)),
    };
    r.store.cli_update_begin(a, j).await.unwrap();
    if activating {
        let act = Activation {
            rollback_path: r.dir.path().join("rollback"),
            rollback_sha256: Some("aa".repeat(32)),
            moved: Vec::new(),
        };
        r.store
            .cli_update_journal_activate(r.kind, act)
            .await
            .unwrap();
    }
    stage
}

async fn outcomes(r: &Rig) -> Vec<UpdateOutcome> {
    r.store
        .cli_update_attempts(0, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.outcome)
        .collect()
}

#[tokio::test]
async fn a_staging_row_ends_interrupted_and_its_stage_is_removed() {
    let r = rig(ProviderKind::Claude).await;
    let stage = journal(&r, false).await;
    recover_with(&r, Duration::from_secs(30)).await;
    assert!(!stage.exists());
    assert_eq!(r.journals().await, 0);
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::Interrupted);
    assert_eq!(last.to_version.as_deref(), Some(FROM));
    assert_eq!(r.quarantine().await, None);
}

#[tokio::test]
async fn an_activating_row_links_a_successor_and_the_quarantine_before_the_restore() {
    let r = rig(ProviderKind::Claude).await;
    journal(&r, true).await;
    *r.installer.restore_sleep.lock().unwrap() = Some(Duration::from_secs(3600));
    // The restore never ends: the limit stops the run (finding 3).
    recover_with(&r, Duration::from_millis(300)).await;
    let rows = r.store.cli_update_journals().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].attempt_id, "a1");
    assert_eq!(rows[0].marker, "a1");
    let last = r.last().await.unwrap();
    assert_eq!(last.attempt_id, rows[0].attempt_id);
    assert_eq!(last.trigger, UpdateTrigger::Recovery);
    assert_eq!(last.outcome, UpdateOutcome::Running);
    assert_eq!(last.from_version.as_deref(), Some(TARGET));
    assert_eq!(last.target_version.as_deref(), Some(FROM));
    assert_eq!(
        r.quarantine().await.as_deref(),
        Some(reason::RECOVERY_PENDING)
    );

    // A second recovery ends the running successor `interrupted`, links a
    // new one and restores.
    *r.installer.restore_sleep.lock().unwrap() = None;
    recover_with(&r, Duration::from_secs(30)).await;
    assert_eq!(r.journals().await, 0);
    assert_eq!(r.quarantine().await, None);
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::RolledBack);
    assert_eq!(last.trigger, UpdateTrigger::Recovery);
    let interrupted = outcomes(&r)
        .await
        .into_iter()
        .filter(|o| *o == UpdateOutcome::Interrupted)
        .count();
    assert_eq!(interrupted, 2);
    assert_eq!(r.version(), FROM);
}

#[tokio::test]
async fn a_recovery_past_its_limit_leaves_the_journal_and_the_quarantine() {
    let r = rig(ProviderKind::Claude).await;
    journal(&r, true).await;
    *r.installer.restore_sleep.lock().unwrap() = Some(Duration::from_secs(3600));
    recover_with(&r, Duration::from_millis(200)).await;
    assert_eq!(r.journals().await, 1);
    assert!(r.quarantine().await.is_some());
}

#[tokio::test]
async fn a_failed_restore_ends_rollback_failed_with_the_quarantine() {
    let r = rig(ProviderKind::Claude).await;
    journal(&r, true).await;
    *r.installer.restore_err.lock().unwrap() = Some(Failure::Failed(reason::ROLLBACK_FAILED));
    recover_with(&r, Duration::from_secs(30)).await;
    assert_eq!(r.journals().await, 0);
    assert_eq!(
        r.last().await.unwrap().outcome,
        UpdateOutcome::RollbackFailed
    );
    assert_eq!(
        r.quarantine().await.as_deref(),
        Some(reason::ROLLBACK_FAILED)
    );
}

#[tokio::test]
async fn a_fence_failure_keeps_the_journal_row() {
    let r = rig(ProviderKind::Claude).await;
    journal(&r, true).await;
    // A group with a process that carries no marker is never signalled, so
    // the fence cannot end it.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let pgid = i32::try_from(child.id()).unwrap();
    r.store.cli_update_journal_pgid(r.kind, pgid).await.unwrap();
    recover_with(&r, Duration::from_secs(60)).await;
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(r.journals().await, 1);
    let last = r.last().await.unwrap();
    assert_eq!(last.outcome, UpdateOutcome::RollbackFailed);
    assert_eq!(last.reason.as_deref(), Some(reason::PROCESS_LEFT));
    assert_eq!(r.quarantine().await.as_deref(), Some(reason::PROCESS_LEFT));
    assert!(!r.events().contains(&"restore".to_owned()));
}

#[tokio::test]
async fn a_running_row_without_a_journal_row_ends_interrupted() {
    let r = rig(ProviderKind::Claude).await;
    let a = new_attempt(
        "o1",
        r.kind,
        UpdateTrigger::Owner,
        InstallMethod::Native,
        "stable",
        None,
        None,
    );
    r.store.cli_update_insert(a, None).await.unwrap();
    recover_with(&r, Duration::from_secs(30)).await;
    assert_eq!(r.last().await.unwrap().outcome, UpdateOutcome::Interrupted);
}

#[tokio::test]
async fn recover_needs_no_manager_and_does_nothing_without_rows() {
    let r = rig(ProviderKind::Claude).await;
    recover_with(&r, Duration::from_secs(30)).await;
    assert_eq!(r.journals().await, 0);
    assert!(r.last().await.is_none());
    assert!(r.events().is_empty());
}

#[test]
fn the_texts_name_the_provider_and_the_reinstall_command() {
    let t = rollback_text(ProviderKind::Claude, InstallMethod::Native, "2.1.309");
    assert_eq!(
        t,
        "Claude failed after an update and the way back failed. Reinstall it on this station: claude install 2.1.309"
    );
    let t = rollback_text(ProviderKind::Codex, InstallMethod::Npm, "0.163.0");
    assert!(t.ends_with("npm install -g @openai/codex@0.163.0"));
    let t = rollback_text(ProviderKind::Codex, InstallMethod::Bun, "0.163.0");
    assert!(t.ends_with("bun add -g @openai/codex@0.163.0"));
    assert_eq!(
        pending_text(ProviderKind::Codex, InstallMethod::Npm, "0.163.0"),
        "the station could not finish an interrupted Codex update. Restart the station; if this stays, reinstall Codex: npm install -g @openai/codex@0.163.0"
    );
    assert_eq!(
        left_text(ProviderKind::Claude),
        "a process of a Claude update did not stop. Restart the station."
    );
}

#[test]
fn a_sweep_removes_the_stale_folders_and_keeps_the_named_ones() {
    let dir = tempfile::tempdir().unwrap();
    let versions = dir.path().join("versions");
    std::fs::create_dir_all(&versions).unwrap();
    std::fs::write(versions.join(".fd-stage-old"), b"x").unwrap();
    std::fs::write(versions.join(".fd-stage-keep"), b"x").unwrap();
    std::fs::write(versions.join("2.1.309"), b"x").unwrap();
    let tool = Tool::Native {
        link: dir.path().join("claude"),
        versions: versions.clone(),
        gpgv: None,
    };
    sweep(dir.path(), &tool, &["keep".to_owned()]);
    assert!(!versions.join(".fd-stage-old").exists());
    assert!(versions.join(".fd-stage-keep").exists());
    assert!(versions.join("2.1.309").exists());
}
