//! The attempts table of provider CLI updates (B28).

use fleetdeck_proto::cli_update::{InstallMethod, UpdateOutcome, UpdateTrigger};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_testkit::Det;

use super::super::tests::store;
use super::super::{Store, call};
use super::{AttemptEnd, NewAttempt};
use crate::store::cli_update_journal::NewJournal;

pub(in crate::store) fn attempt(id: &str, provider: ProviderKind) -> NewAttempt {
    NewAttempt {
        attempt_id: id.to_owned(),
        provider,
        trigger: UpdateTrigger::Owner,
        method: InstallMethod::Native,
        channel: "stable".to_owned(),
        from_version: Some("2.1.309".to_owned()),
        target_version: Some("2.1.310".to_owned()),
    }
}

pub(in crate::store) fn end(outcome: UpdateOutcome) -> AttemptEnd {
    AttemptEnd {
        outcome,
        reason: None,
        to_version: Some("2.1.310".to_owned()),
        target_version: None,
    }
}

fn journal(provider: ProviderKind) -> NewJournal {
    NewJournal {
        provider,
        method: InstallMethod::Native,
        from_version: "2.1.309".to_owned(),
        target_version: "2.1.310".to_owned(),
        launcher: "/home/o/.local/bin/claude".into(),
        dest: "/home/o/.local/bin/claude".into(),
        stage_root: "/home/o/.local/share/claude/versions/.fd-stage-x".into(),
        rollback_sha256: None,
    }
}

async fn seqs(s: &Store) -> Vec<u64> {
    s.cli_update_attempts(0, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.seq)
        .collect()
}

#[tokio::test]
async fn cli_update_insert_end_and_last() {
    let det = Det::new();
    let s = store(&det).await;
    assert_eq!(s.cli_update_last(ProviderKind::Claude).await.unwrap(), None);

    let seq = s
        .cli_update_insert(attempt("a1", ProviderKind::Claude), None)
        .await
        .unwrap();
    let last = s
        .cli_update_last(ProviderKind::Claude)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last.seq, seq);
    assert_eq!(last.outcome, UpdateOutcome::Running);
    assert_eq!(last.ended_at, None);

    s.cli_update_finish(
        "a1".to_owned(),
        end(UpdateOutcome::Updated),
        super::super::cli_update_journal::JournalAction::Delete,
        super::super::cli_update_journal::QuarantineAction::Keep,
    )
    .await
    .unwrap();
    let last = s
        .cli_update_last(ProviderKind::Claude)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last.outcome, UpdateOutcome::Updated);
    assert_eq!(last.to_version.as_deref(), Some("2.1.310"));
    assert!(last.ended_at.is_some());

    // A row inserted already ended has its end time at once.
    let seq2 = s
        .cli_update_insert(
            attempt("a2", ProviderKind::Codex),
            Some(end(UpdateOutcome::UpToDate)),
        )
        .await
        .unwrap();
    assert!(seq2 > seq);
    let codex = s
        .cli_update_last(ProviderKind::Codex)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(codex.outcome, UpdateOutcome::UpToDate);
    assert!(codex.ended_at.is_some());
    assert_eq!(
        s.cli_update_last(ProviderKind::Claude)
            .await
            .unwrap()
            .unwrap()
            .seq,
        seq
    );
}

#[tokio::test]
async fn cli_update_attempts_stop_before_a_running_row_and_page_by_after_seq() {
    let det = Det::new();
    let s = store(&det).await;
    for id in ["a1", "a2"] {
        s.cli_update_insert(
            attempt(id, ProviderKind::Claude),
            Some(end(UpdateOutcome::Updated)),
        )
        .await
        .unwrap();
    }
    s.cli_update_insert(attempt("a3", ProviderKind::Codex), None)
        .await
        .unwrap();
    s.cli_update_insert(
        attempt("a4", ProviderKind::Claude),
        Some(end(UpdateOutcome::Updated)),
    )
    .await
    .unwrap();

    let all = s.cli_update_attempts(0, 100).await.unwrap();
    let ids: Vec<_> = all.iter().map(|a| a.attempt_id.as_str()).collect();
    assert_eq!(ids, ["a1", "a2"]);

    let after_first = s.cli_update_attempts(all[0].seq, 100).await.unwrap();
    assert_eq!(after_first.len(), 1);
    assert_eq!(after_first[0].attempt_id, "a2");
    assert_eq!(s.cli_update_attempts(0, 1).await.unwrap().len(), 1);
    assert!(
        s.cli_update_attempts(all[1].seq, 100)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn cli_update_last_seq_ignores_running_rows() {
    let det = Det::new();
    let s = store(&det).await;
    assert_eq!(s.cli_update_last_seq().await.unwrap(), 0);
    s.cli_update_insert(attempt("a1", ProviderKind::Claude), None)
        .await
        .unwrap();
    assert_eq!(s.cli_update_last_seq().await.unwrap(), 0);
    let seq = s
        .cli_update_insert(
            attempt("a2", ProviderKind::Claude),
            Some(end(UpdateOutcome::Failed)),
        )
        .await
        .unwrap();
    s.cli_update_insert(attempt("a3", ProviderKind::Codex), None)
        .await
        .unwrap();
    assert_eq!(s.cli_update_last_seq().await.unwrap(), seq);
}

#[tokio::test]
async fn cli_update_prune_keeps_the_newest_and_never_a_named_row() {
    let det = Det::new();
    let s = store(&det).await;
    // a1 is named by a journal row, a2 by a quarantine row.
    s.cli_update_begin(
        attempt("a1", ProviderKind::Claude),
        journal(ProviderKind::Claude),
    )
    .await
    .unwrap();
    s.cli_update_insert(
        attempt("a2", ProviderKind::Codex),
        Some(end(UpdateOutcome::RollbackFailed)),
    )
    .await
    .unwrap();
    call(&s.inner.db, |conn| {
        conn.execute(
            "INSERT INTO cli_update_quarantine (provider, attempt_id, reason, text, since)
             VALUES ('codex', 'a2', 'rollback_failed', 't', 1)",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    for id in ["a3", "a4", "a5"] {
        s.cli_update_insert(
            attempt(id, ProviderKind::Claude),
            Some(end(UpdateOutcome::UpToDate)),
        )
        .await
        .unwrap();
    }

    assert_eq!(s.cli_update_prune(2).await.unwrap(), 1);
    let kept: Vec<String> = call(&s.inner.db, |conn| {
        let mut stmt = conn.prepare("SELECT attempt_id FROM cli_update_attempts ORDER BY seq")?;
        Ok(stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?)
    })
    .await
    .unwrap();
    assert_eq!(kept, ["a1", "a2", "a4", "a5"]);
}

#[tokio::test]
async fn cli_update_check_refuses_a_provider_that_is_not_claude_or_codex() {
    let det = Det::new();
    let s = store(&det).await;
    let err = s
        .cli_update_insert(attempt("p1", ProviderKind::Unknown), None)
        .await;
    assert!(err.is_err());
    let raw = call(&s.inner.db, |conn| {
        Ok(conn.execute(
            "INSERT INTO cli_update_attempts
               (attempt_id, provider, trigger, method, channel, outcome, started_at)
             VALUES ('p2', 'pi', 'owner', 'native', 'stable', 'running', 1)",
            [],
        ))
    })
    .await
    .unwrap();
    assert!(raw.is_err());
    assert!(seqs(&s).await.is_empty());
}

#[tokio::test]
async fn cli_update_interrupt_orphans_ends_a_running_row_without_a_journal_row() {
    let det = Det::new();
    let s = store(&det).await;
    s.cli_update_insert(attempt("orphan", ProviderKind::Codex), None)
        .await
        .unwrap();
    s.cli_update_begin(
        attempt("held", ProviderKind::Claude),
        journal(ProviderKind::Claude),
    )
    .await
    .unwrap();

    assert_eq!(s.cli_update_interrupt_orphans().await.unwrap(), 1);
    let orphan = s
        .cli_update_last(ProviderKind::Codex)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(orphan.outcome, UpdateOutcome::Interrupted);
    assert!(orphan.ended_at.is_some());
    let held = s
        .cli_update_last(ProviderKind::Claude)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(held.outcome, UpdateOutcome::Running);
    assert_eq!(s.cli_update_interrupt_orphans().await.unwrap(), 0);
}
