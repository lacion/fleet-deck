//! The journal and the quarantine of provider CLI updates (B28).

use fleetdeck_proto::cli_update::{InstallMethod, UpdateOutcome};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_testkit::Det;

use super::super::cli_updates::cli_updates_tests::{attempt, end};
use super::super::{Store, StoreConfig};
use super::{Activation, JournalAction, JournalPhase, MovedEntry, NewJournal, QuarantineAction};

fn journal(provider: ProviderKind, method: InstallMethod) -> NewJournal {
    NewJournal {
        provider,
        method,
        from_version: "2.1.309".to_owned(),
        target_version: "2.1.310".to_owned(),
        launcher: "/home/o/.local/bin/claude".into(),
        dest: "/home/o/.local/bin/claude".into(),
        stage_root: "/home/o/.local/share/claude/versions/.fd-stage-a1".into(),
        rollback_sha256: Some("aa".repeat(32)),
    }
}

async fn open(path: &std::path::Path, det: &Det) -> Store {
    let s = Store::open(path, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s
}

fn quarantine(reason: &'static str) -> QuarantineAction {
    QuarantineAction::Set {
        reason,
        text: "the text".to_owned(),
    }
}

#[tokio::test]
async fn cli_update_begin_writes_both_rows_and_a_second_begin_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let det = Det::new();
    let s = open(&tmp.path().join("station.db"), &det).await;
    s.cli_update_begin(
        attempt("a1", ProviderKind::Claude),
        journal(ProviderKind::Claude, InstallMethod::Native),
    )
    .await
    .unwrap();
    let rows = s.cli_update_journals().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].attempt_id, "a1");
    assert_eq!(rows[0].marker, "a1");
    assert_eq!(rows[0].phase, JournalPhase::Staging);
    assert_eq!(rows[0].installer_pgid, None);

    let second = s
        .cli_update_begin(
            attempt("a2", ProviderKind::Claude),
            journal(ProviderKind::Claude, InstallMethod::Native),
        )
        .await;
    assert!(second.is_err());
    // The attempt row of the failed begin was rolled back with it.
    s.cli_update_finish(
        "a1".to_owned(),
        end(UpdateOutcome::Failed),
        JournalAction::Delete,
        QuarantineAction::Keep,
    )
    .await
    .unwrap();
    assert_eq!(s.cli_update_attempts(0, 100).await.unwrap().len(), 1);
}

#[tokio::test]
async fn cli_update_journal_activate_survives_a_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("station.db");
    let det = Det::new();
    {
        let s = open(&path, &det).await;
        s.cli_update_begin(
            attempt("a1", ProviderKind::Codex),
            journal(ProviderKind::Codex, InstallMethod::Npm),
        )
        .await
        .unwrap();
        s.cli_update_journal_pgid(ProviderKind::Codex, 4242)
            .await
            .unwrap();
        s.cli_update_journal_activate(
            ProviderKind::Codex,
            Activation {
                rollback_path: "/stage/old".into(),
                rollback_sha256: None,
                moved: vec![
                    MovedEntry {
                        name: "@openai".to_owned(),
                        digest: Some("d1".to_owned()),
                    },
                    MovedEntry {
                        name: "codex".to_owned(),
                        digest: None,
                    },
                ],
            },
        )
        .await
        .unwrap();
    }
    let s = open(&path, &det).await;
    let rows = s.cli_update_journals().await.unwrap();
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.phase, JournalPhase::Activating);
    assert_eq!(r.installer_pgid, Some(4242));
    assert_eq!(
        r.rollback_path.as_deref(),
        Some(std::path::Path::new("/stage/old"))
    );
    assert_eq!(r.rollback_sha256, None);
    assert_eq!(r.method, InstallMethod::Npm);
    assert_eq!(r.moved.len(), 2);
    assert_eq!(r.moved[0].name, "@openai");
    assert_eq!(r.moved[0].digest.as_deref(), Some("d1"));
    assert_eq!(r.moved[1].digest, None);
}

#[tokio::test]
async fn cli_update_journal_pgid_and_activate_need_a_journal_row() {
    let det = Det::new();
    let s = super::super::tests::store(&det).await;
    assert!(
        s.cli_update_journal_pgid(ProviderKind::Claude, 1)
            .await
            .is_err()
    );
    assert!(
        s.cli_update_journal_activate(
            ProviderKind::Claude,
            Activation {
                rollback_path: "/r".into(),
                rollback_sha256: None,
                moved: Vec::new()
            }
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn cli_update_recover_begin_links_a_successor_and_survives_a_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("station.db");
    let det = Det::new();
    {
        let s = open(&path, &det).await;
        s.cli_update_begin(
            attempt("a1", ProviderKind::Claude),
            journal(ProviderKind::Claude, InstallMethod::Native),
        )
        .await
        .unwrap();
        s.cli_update_journal_activate(
            ProviderKind::Claude,
            Activation {
                rollback_path: "/r/claude".into(),
                rollback_sha256: Some("bb".repeat(32)),
                moved: Vec::new(),
            },
        )
        .await
        .unwrap();
        let rec = s
            .cli_update_recover_begin(
                ProviderKind::Claude,
                attempt("s1", ProviderKind::Claude),
                "pending".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(rec.attempt_id, "s1");
        assert_eq!(rec.marker, "a1");
        assert_eq!(rec.phase, JournalPhase::Activating);
        assert_eq!(rec.rollback_sha256, Some("bb".repeat(32)));
    }
    let s = open(&path, &det).await;
    let rows = s.cli_update_journals().await.unwrap();
    assert_eq!(
        (rows[0].attempt_id.as_str(), rows[0].marker.as_str()),
        ("s1", "a1")
    );
    let q = s.cli_update_quarantines().await.unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].reason, "recovery_pending");
    assert_eq!(q[0].text, "pending");
    assert_eq!(q[0].attempt_id, "s1");
    // The old attempt ended `interrupted`; the successor runs.
    let old = s.cli_update_attempts(0, 100).await.unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].attempt_id, "a1");
    assert_eq!(old[0].outcome, UpdateOutcome::Interrupted);
    let last = s
        .cli_update_last(ProviderKind::Claude)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last.attempt_id, "s1");
    assert_eq!(last.outcome, UpdateOutcome::Running);
}

#[tokio::test]
async fn cli_update_recover_begin_twice_ends_the_first_successor_and_links_a_new_one() {
    let det = Det::new();
    let s = super::super::tests::store(&det).await;
    s.cli_update_begin(
        attempt("a1", ProviderKind::Claude),
        journal(ProviderKind::Claude, InstallMethod::Native),
    )
    .await
    .unwrap();
    for id in ["s1", "s2"] {
        s.cli_update_recover_begin(
            ProviderKind::Claude,
            attempt(id, ProviderKind::Claude),
            "pending".to_owned(),
        )
        .await
        .unwrap();
    }
    let rows = s.cli_update_attempts(0, 100).await.unwrap();
    let outcomes: Vec<_> = rows
        .iter()
        .map(|a| (a.attempt_id.as_str(), a.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("a1", UpdateOutcome::Interrupted),
            ("s1", UpdateOutcome::Interrupted)
        ]
    );
    let j = s.cli_update_journals().await.unwrap();
    assert_eq!(j[0].attempt_id, "s2");
    assert_eq!(j[0].marker, "a1");
    assert_eq!(
        s.cli_update_quarantines().await.unwrap()[0].attempt_id,
        "s2"
    );
    // A provider without a journal row cannot begin a recovery.
    assert!(
        s.cli_update_recover_begin(
            ProviderKind::Codex,
            attempt("s3", ProviderKind::Codex),
            "x".to_owned()
        )
        .await
        .is_err()
    );
    assert_eq!(s.cli_update_attempts(0, 100).await.unwrap().len(), 2);
}

#[tokio::test]
async fn cli_update_finish_applies_the_journal_and_quarantine_actions() {
    let det = Det::new();
    let s = super::super::tests::store(&det).await;
    s.cli_update_begin(
        attempt("a1", ProviderKind::Claude),
        journal(ProviderKind::Claude, InstallMethod::Native),
    )
    .await
    .unwrap();

    // Keep the journal row and set the quarantine row.
    s.cli_update_finish(
        "a1".to_owned(),
        end(UpdateOutcome::RollbackFailed),
        JournalAction::Keep,
        quarantine("process_left"),
    )
    .await
    .unwrap();
    assert_eq!(s.cli_update_journals().await.unwrap().len(), 1);
    let q = s.cli_update_quarantines().await.unwrap();
    assert_eq!(
        (q[0].provider, q[0].reason.as_str()),
        (ProviderKind::Claude, "process_left")
    );

    // Delete the journal row and keep the quarantine row.
    s.cli_update_insert(attempt("a2", ProviderKind::Claude), None)
        .await
        .unwrap();
    s.cli_update_finish(
        "a1".to_owned(),
        end(UpdateOutcome::RollbackFailed),
        JournalAction::Delete,
        QuarantineAction::Keep,
    )
    .await
    .unwrap();
    assert!(s.cli_update_journals().await.unwrap().is_empty());
    assert_eq!(s.cli_update_quarantines().await.unwrap().len(), 1);

    // Replace the quarantine reason, then clear it.
    s.cli_update_finish(
        "a2".to_owned(),
        end(UpdateOutcome::RollbackFailed),
        JournalAction::Keep,
        quarantine("rollback_failed"),
    )
    .await
    .unwrap();
    assert_eq!(
        s.cli_update_quarantines().await.unwrap()[0].reason,
        "rollback_failed"
    );
    s.cli_update_insert(attempt("a3", ProviderKind::Claude), None)
        .await
        .unwrap();
    s.cli_update_finish(
        "a3".to_owned(),
        end(UpdateOutcome::Updated),
        JournalAction::Keep,
        QuarantineAction::Clear,
    )
    .await
    .unwrap();
    assert!(s.cli_update_quarantines().await.unwrap().is_empty());

    // An unknown attempt id writes nothing.
    assert!(
        s.cli_update_finish(
            "nope".to_owned(),
            end(UpdateOutcome::Updated),
            JournalAction::Delete,
            QuarantineAction::Clear
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn cli_update_verified_writes_the_row_and_clears_the_quarantine_together() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("station.db");
    let det = Det::new();
    {
        let s = open(&path, &det).await;
        s.cli_update_insert(attempt("a1", ProviderKind::Claude), None)
            .await
            .unwrap();
        s.cli_update_finish(
            "a1".to_owned(),
            end(UpdateOutcome::RollbackFailed),
            JournalAction::Delete,
            quarantine("rollback_failed"),
        )
        .await
        .unwrap();
        // A quarantine row survives a reopen.
    }
    let s = open(&path, &det).await;
    assert_eq!(s.cli_update_quarantines().await.unwrap().len(), 1);
    // The Codex quarantine would stay: only the provider of the row clears.
    let seq = s
        .cli_update_verified(
            attempt("v1", ProviderKind::Claude),
            end(UpdateOutcome::Updated),
        )
        .await
        .unwrap();
    let last = s
        .cli_update_last(ProviderKind::Claude)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last.seq, seq);
    assert_eq!(last.outcome, UpdateOutcome::Verified);
    assert!(last.ended_at.is_some());
    assert!(s.cli_update_quarantines().await.unwrap().is_empty());
}
