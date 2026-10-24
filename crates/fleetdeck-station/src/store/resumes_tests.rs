//! The termination evidence of a shim row and the exclusion of one native
//! conversation (DL-182, design `shim-resume.md` §1a, §3, §4).

use std::path::PathBuf;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::host::shim::wire::ProviderIdentity;
use fleetdeck_testkit::Det;

use super::*;
use crate::store::{NewSession, NewShim, OfflineApprovalPolicy, RunningFacts, ShimState, Store};
use crate::store::{StoreConfig, call};

async fn store(det: &Det) -> Store {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s
}

async fn session(s: &Store, id: &str, kind: ProviderKind, instance: &str, native: Option<&str>) {
    s.create_session(NewSession {
        session_id: id.into(),
        provider: kind,
        provider_instance: instance.into(),
        cwd: "/w".to_owned(),
        worktree: None,
        branch: None,
        mode: None,
        title: None,
        offline_approval_policy: OfflineApprovalPolicy::Wait,
    })
    .await
    .unwrap();
    if let Some(native) = native {
        let (id, native) = (id.to_owned(), native.to_owned());
        call(&s.inner.db, move |conn| {
            conn.execute(
                "UPDATE sessions SET native_session_id = ?2, state = 'exited' WHERE session_id = ?1",
                [id, native],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }
}

fn identity(pid: u32, boot: &str) -> ProviderIdentity {
    ProviderIdentity {
        pid,
        start_time: 5,
        boot_id: boot.to_owned(),
    }
}

/// One shim row of `session`, ended as `state`/`reason` with an inspection.
async fn row(
    s: &Store,
    session: &str,
    id: &str,
    state: ShimState,
    reason: &str,
    inspection: u32,
    native_target: Option<&str>,
) {
    s.shim_starting(NewShim {
        shim_id: id.to_owned(),
        session_id: session.into(),
        socket_path: PathBuf::from(format!("/run/x/{id}.sock")),
        launch_boot_id: Some("boot-1".to_owned()),
        native_target: native_target.map(str::to_owned),
    })
    .await
    .unwrap();
    if state != ShimState::NeverStarted {
        s.shim_running_with(
            id.to_owned(),
            identity(10, "boot-1"),
            RunningFacts::default(),
        )
        .await
        .unwrap();
    }
    s.shim_end(id.to_owned(), state, reason.to_owned(), inspection)
        .await
        .unwrap();
}

async fn blocked(
    s: &Store,
    kind: ProviderKind,
    native: &str,
    except: Option<&str>,
) -> Option<String> {
    blocked_for(s, kind, native, except, Purpose::Resume).await
}

async fn blocked_for(
    s: &Store,
    kind: ProviderKind,
    native: &str,
    except: Option<&str>,
    purpose: Purpose,
) -> Option<String> {
    s.execution_exclusion(
        None,
        kind,
        Some(native.to_owned()),
        except.map(SessionId::from),
        purpose,
    )
    .await
    .unwrap()
    .blocked
}

/// Sets the state of `session` (a session starts in `starting`).
async fn set_state(s: &Store, session: &str, state: &str) {
    let (session, state) = (session.to_owned(), state.to_owned());
    call(&s.inner.db, move |conn| {
        conn.execute(
            "UPDATE sessions SET state = ?2 WHERE session_id = ?1",
            [session, state],
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_row_proves_its_end_only_by_a_complete_clean_exit_a_never_started_launch_or_a_proof() {
    let det = Det::new();
    let s = store(&det).await;
    session(&s, "s1", ProviderKind::Claude, "claude", None).await;
    row(&s, "s1", "clean", ShimState::Exited, "exited", 1, None).await;
    row(
        &s,
        "s1",
        "never",
        ShimState::NeverStarted,
        "shim_never_started",
        0,
        None,
    )
    .await;
    row(&s, "s1", "legacy", ShimState::Exited, "exited", 0, None).await;
    row(
        &s,
        "s1",
        "left",
        ShimState::Exited,
        "exited_descendants_remaining",
        1,
        None,
    )
    .await;
    row(
        &s,
        "s1",
        "unverified",
        ShimState::Exited,
        "exit_unverified",
        1,
        None,
    )
    .await;
    row(
        &s,
        "s1",
        "lost",
        ShimState::Lost,
        "execution_unknown",
        0,
        None,
    )
    .await;
    let rows = s.shim_rows("s1".into()).await.unwrap();
    let mut proven: Vec<(&str, bool)> = rows
        .iter()
        .map(|r| (r.shim_id.as_str(), proves_end(r)))
        .collect();
    proven.sort_unstable();
    assert_eq!(
        proven,
        [
            ("clean", true),
            ("left", false),
            ("legacy", false),
            ("lost", false),
            ("never", true),
            ("unverified", false),
        ]
    );
    // A legacy clean exit reads as `exit_unverified` (test 19).
    let legacy = rows.iter().find(|r| r.shim_id == "legacy").unwrap();
    assert_eq!(effective_reason(legacy), "exit_unverified");
    assert_eq!(describe(legacy), "shim legacy is exited (exit_unverified)");
}

#[tokio::test]
async fn a_proof_next_to_a_row_proves_its_end_and_leaves_state_and_reason_alone() {
    let det = Det::new();
    let s = store(&det).await;
    session(&s, "s1", ProviderKind::Claude, "claude", None).await;
    row(&s, "s1", "a", ShimState::Lost, "execution_unknown", 0, None).await;
    row(&s, "s1", "b", ShimState::Exited, "exited", 0, None).await;
    let unresolved: Vec<String> = s
        .shims_unresolved(None)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.shim_id)
        .collect();
    assert_eq!(unresolved, ["a", "b"], "a legacy exit is unresolved too");
    assert!(
        s.shim_prove_reboot("a".to_owned(), "boot-2".to_owned())
            .await
            .unwrap()
    );
    assert!(
        s.shim_prove_reboot("b".to_owned(), "boot-2".to_owned())
            .await
            .unwrap()
    );
    assert!(
        !s.shim_prove_reboot("a".to_owned(), "boot-3".to_owned())
            .await
            .unwrap(),
        "a proof is recorded once"
    );
    let rows = s.shim_rows("s1".into()).await.unwrap();
    assert!(rows.iter().all(proves_end));
    let a = &rows[0];
    assert_eq!(
        (a.state, a.reason.as_deref(), a.proof.as_deref()),
        (
            ShimState::Lost,
            Some("execution_unknown"),
            Some(PROOF_REBOOT)
        )
    );
    assert_eq!(a.proof_boot_id.as_deref(), Some("boot-2"));
    assert!(s.shims_unresolved(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_open_row_is_never_unresolved_or_proven() {
    let det = Det::new();
    let s = store(&det).await;
    session(&s, "s1", ProviderKind::Claude, "claude", None).await;
    s.shim_starting(NewShim {
        shim_id: "open".to_owned(),
        session_id: "s1".into(),
        socket_path: PathBuf::from("/run/x/open.sock"),
        launch_boot_id: Some("boot-1".to_owned()),
        native_target: None,
    })
    .await
    .unwrap();
    assert!(s.shims_unresolved(None).await.unwrap().is_empty());
    assert!(
        !s.shim_prove_reboot("open".to_owned(), "boot-2".to_owned())
            .await
            .unwrap()
    );
    let rows = s.shim_rows("s1".into()).await.unwrap();
    assert!(!proves_end(&rows[0]));
}

#[tokio::test]
async fn the_exclusion_blocks_a_conversation_that_a_session_or_a_launch_still_holds() {
    let det = Det::new();
    let s = store(&det).await;
    // Session A (instance one) held N and lost its shim.
    session(&s, "a", ProviderKind::Codex, "codex-one", Some("N")).await;
    row(&s, "a", "a1", ShimState::Lost, "execution_unknown", 0, None).await;
    // Both instances may share one native storage (test 17): the other
    // instance of the same kind is blocked, a resume of A itself is the
    // business of its own rows.
    let why = blocked(&s, ProviderKind::Codex, "N", Some("b")).await;
    assert!(why.as_deref().is_some_and(|w| w.contains("a1")), "{why:?}");
    assert_eq!(blocked(&s, ProviderKind::Codex, "N", Some("a")).await, None);
    // Another conversation and another provider kind are free.
    assert_eq!(blocked(&s, ProviderKind::Codex, "M", None).await, None);
    assert_eq!(blocked(&s, ProviderKind::Claude, "N", None).await, None);
    // After the proof the conversation is free again.
    s.shim_prove_reboot("a1".to_owned(), "boot-2".to_owned())
        .await
        .unwrap();
    assert_eq!(blocked(&s, ProviderKind::Codex, "N", None).await, None);
    // A session that has not ended holds it, whatever its rows say.
    session(&s, "live", ProviderKind::Codex, "codex-two", Some("L")).await;
    call(&s.inner.db, |conn| {
        conn.execute(
            "UPDATE sessions SET state = 'running' WHERE session_id = 'live'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(blocked(&s, ProviderKind::Codex, "L", None).await.is_some());
}

#[tokio::test]
async fn a_launch_that_names_a_conversation_blocks_it_until_it_is_proven() {
    let det = Det::new();
    let s = store(&det).await;
    // A crash after the spawn of a resume of N and before `session.started`
    // (test 18): the session's own native id is unknown here, but the row
    // names the conversation.
    session(&s, "r", ProviderKind::Claude, "claude", None).await;
    row(
        &s,
        "r",
        "r1",
        ShimState::Lost,
        "execution_unknown",
        0,
        Some("N"),
    )
    .await;
    let why = blocked(&s, ProviderKind::Claude, "N", None).await;
    assert!(why.is_some_and(|w| w.contains("r1")));
    // A resume of another conversation is not blocked.
    assert_eq!(blocked(&s, ProviderKind::Claude, "OTHER", None).await, None);
    // A new conversation with no native target blocks nothing (it cannot
    // be N), and a row from before this build blocks nothing either.
    session(&s, "n", ProviderKind::Claude, "claude", None).await;
    row(&s, "n", "n1", ShimState::Lost, "execution_unknown", 0, None).await;
    assert_eq!(blocked(&s, ProviderKind::Claude, "OTHER", None).await, None);
    // The reconciliation of the owner frees the conversation.
    s.shim_prove_reboot("r1".to_owned(), "boot-2".to_owned())
        .await
        .unwrap();
    assert_eq!(blocked(&s, ProviderKind::Claude, "N", None).await, None);
}

#[tokio::test]
async fn the_exclusion_lists_the_rows_that_rest_on_a_reboot_proof() {
    let det = Det::new();
    let s = store(&det).await;
    session(&s, "a", ProviderKind::Claude, "claude", Some("N")).await;
    row(&s, "a", "a1", ShimState::Lost, "execution_unknown", 0, None).await;
    row(&s, "a", "a2", ShimState::Exited, "exited", 1, None).await;
    s.shim_prove_reboot("a1".to_owned(), "boot-2".to_owned())
        .await
        .unwrap();
    let found = s
        .execution_exclusion(
            Some("a".into()),
            ProviderKind::Claude,
            Some("N".to_owned()),
            Some("a".into()),
            Purpose::Resume,
        )
        .await
        .unwrap();
    assert_eq!(found.blocked, None);
    let ids: Vec<&str> = found
        .reboot_rows
        .iter()
        .map(|r| r.shim_id.as_str())
        .collect();
    assert_eq!(ids, ["a1"], "a clean exit needs no revalidation");
}

#[tokio::test]
async fn the_v23_columns_default_to_unverified_and_no_proof() {
    let det = Det::new();
    let s = store(&det).await;
    session(&s, "s1", ProviderKind::Claude, "claude", None).await;
    // A row written the way an older build wrote it.
    call(&s.inner.db, |conn| {
        conn.execute(
            "INSERT INTO shims (shim_id, session_id, incarnation, state, socket_path, reason,
                 created_at, updated_at)
             VALUES ('old', 's1', 'inc', 'exited', '/x', 'exited', 1, 1)",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let rows = s.shim_rows("s1".into()).await.unwrap();
    let old = &rows[0];
    assert_eq!(old.inspection_version, 0);
    assert_eq!(
        (
            old.proof.as_deref(),
            old.launch_boot_id.as_deref(),
            old.native_target.as_deref()
        ),
        (None, None, None)
    );
    assert!(!proves_end(old));
}

/// A fresh provider created conversation N, the station crashed before
/// `session.started` and the reattach failed: the session is `lost`, its
/// shim row is `lost`, and no native id is stored anywhere.
async fn lost_without_a_native_id(s: &Store, id: &str, shim: &str) {
    session(s, id, ProviderKind::Claude, "claude", None).await;
    row(s, id, shim, ShimState::Lost, "execution_unknown", 0, None).await;
    set_state(s, id, "lost").await;
}

#[tokio::test]
async fn an_import_is_refused_while_a_lost_run_with_an_unknown_native_id_is_unproven() {
    let det = Det::new();
    let s = store(&det).await;
    lost_without_a_native_id(&s, "u", "u1").await;
    // Any id may be the conversation of that run.
    for native in ["N", "ANY-OTHER"] {
        let why = blocked_for(&s, ProviderKind::Claude, native, None, Purpose::Import).await;
        assert!(
            why.as_deref()
                .is_some_and(|w| w.contains("session `u`") && w.contains("shim u1 is lost")),
            "{why:?}"
        );
    }
    // Another provider kind is not concerned.
    assert_eq!(
        blocked_for(&s, ProviderKind::Codex, "N", None, Purpose::Import).await,
        None
    );
    // A resume of the stored id of another session is not blocked by it.
    assert_eq!(blocked(&s, ProviderKind::Claude, "N", None).await, None);
    // The same check at dispatch: the import's own session is `except`, and
    // its own rows are the business of `own`.
    session(&s, "imp", ProviderKind::Claude, "claude", Some("N")).await;
    let at_dispatch = |purpose| {
        s.execution_exclusion(
            Some("imp".into()),
            ProviderKind::Claude,
            Some("N".to_owned()),
            Some("imp".into()),
            purpose,
        )
    };
    let found = at_dispatch(Purpose::Import).await.unwrap();
    assert!(
        found.blocked.as_deref().is_some_and(|w| w.contains("`u`")),
        "{found:?}"
    );
    assert_eq!(at_dispatch(Purpose::Resume).await.unwrap().blocked, None);
    // A proof of the owner frees the import.
    call(&s.inner.db, |conn| {
        conn.execute(
            "UPDATE shims SET proof = 'reconciled_by_owner' WHERE shim_id = 'u1'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        blocked_for(&s, ProviderKind::Claude, "N", None, Purpose::Import).await,
        None
    );
    assert_eq!(at_dispatch(Purpose::Import).await.unwrap().blocked, None);
}

#[tokio::test]
async fn a_live_session_with_an_unknown_native_id_blocks_an_import_until_it_stores_the_id() {
    let det = Det::new();
    let s = store(&det).await;
    session(&s, "fresh", ProviderKind::Claude, "claude", None).await;
    set_state(&s, "fresh", "running").await;
    let why = blocked_for(&s, ProviderKind::Claude, "N", None, Purpose::Import).await;
    assert_eq!(
        why.as_deref(),
        Some(
            "session `fresh` has a run whose conversation id is not known yet; an import waits \
             until that session reports its id or its run is proven ended"
        )
    );
    // A resume of another conversation, and an import of a free kind, pass.
    assert_eq!(blocked(&s, ProviderKind::Claude, "N", None).await, None);
    assert_eq!(
        blocked_for(&s, ProviderKind::Codex, "N", None, Purpose::Import).await,
        None
    );
    // The session itself is the one that continues the conversation.
    assert_eq!(
        blocked_for(
            &s,
            ProviderKind::Claude,
            "N",
            Some("fresh"),
            Purpose::Import
        )
        .await,
        None
    );
    // Once the native id is stored, the session is no longer unknown: it
    // blocks only the conversation it holds (rule (a)).
    call(&s.inner.db, |conn| {
        conn.execute(
            "UPDATE sessions SET native_session_id = 'HELD' WHERE session_id = 'fresh'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        blocked_for(&s, ProviderKind::Claude, "N", None, Purpose::Import).await,
        None
    );
    assert!(
        blocked_for(&s, ProviderKind::Claude, "HELD", None, Purpose::Import)
            .await
            .is_some()
    );
}

#[tokio::test]
async fn the_own_exclusion_blocks_on_an_unproven_row_and_lists_the_reboot_rows() {
    let det = Det::new();
    let s = store(&det).await;
    session(&s, "a", ProviderKind::Claude, "claude", Some("N")).await;
    row(&s, "a", "a1", ShimState::Lost, "execution_unknown", 0, None).await;
    let found = s.own_execution_exclusion("a".into()).await.unwrap();
    assert_eq!(
        found.blocked.as_deref(),
        Some("shim a1 is lost (execution_unknown)")
    );
    s.shim_prove_reboot("a1".to_owned(), "boot-2".to_owned())
        .await
        .unwrap();
    let found = s.own_execution_exclusion("a".into()).await.unwrap();
    assert_eq!(found.blocked, None);
    assert_eq!(found.reboot_rows.len(), 1);
    // A session without a row has nothing that may still run.
    let found = s.own_execution_exclusion("none".into()).await.unwrap();
    assert_eq!((found.blocked, found.reboot_rows.len()), (None, 0));
}

#[tokio::test]
async fn proves_end_accepts_a_station_proof() {
    let det = Det::new();
    let s = store(&det).await;
    session(&s, "s", ProviderKind::Claude, "claude", None).await;
    row(&s, "s", "a", ShimState::Exited, "exit_unverified", 1, None).await;
    let before = s.shim_rows("s".into()).await.unwrap();
    assert!(!proves_end(&before[0]));
    assert!(settle_eligible(&before[0]));
    assert!(s.shim_settle("s".into(), "a".to_owned()).await.unwrap());
    // The second call finds the row settled.
    assert!(!s.shim_settle("s".into(), "a".to_owned()).await.unwrap());
    let after = s.shim_rows("s".into()).await.unwrap();
    assert_eq!(after[0].station_proof.as_deref(), Some(PROOF_STATION));
    assert!(after[0].station_proof_at.is_some());
    assert!(proves_end(&after[0]));
    assert!(!settle_eligible(&after[0]));
    // A lost row and a row with descendants are never settled by the station.
    row(&s, "s", "b", ShimState::Lost, "execution_unknown", 0, None).await;
    row(
        &s,
        "s",
        "c",
        ShimState::Exited,
        "exited_descendants_remaining",
        1,
        None,
    )
    .await;
    for id in ["b", "c"] {
        assert!(
            !s.shim_settle("s".into(), id.to_owned()).await.unwrap(),
            "{id}"
        );
    }
    // A settle names the right session.
    row(&s, "s", "d", ShimState::Exited, "exit_unverified", 1, None).await;
    assert!(!s.shim_settle("other".into(), "d".to_owned()).await.unwrap());
}

#[tokio::test]
async fn unresolved_skips_a_settled_row() {
    let det = Det::new();
    let s = store(&det).await;
    session(&s, "s", ProviderKind::Claude, "claude", None).await;
    row(&s, "s", "a", ShimState::Exited, "exit_unverified", 1, None).await;
    row(&s, "s", "b", ShimState::Lost, "execution_unknown", 0, None).await;
    let ids = |rows: Vec<ShimRow>| rows.into_iter().map(|r| r.shim_id).collect::<Vec<_>>();
    assert_eq!(ids(s.shims_unresolved(None).await.unwrap()), ["a", "b"]);
    assert!(s.shim_settle("s".into(), "a".to_owned()).await.unwrap());
    assert_eq!(ids(s.shims_unresolved(None).await.unwrap()), ["b"]);
    assert!(
        !s.shim_prove_reboot("a".to_owned(), "boot-2".to_owned())
            .await
            .unwrap(),
        "the boot pass skips a settled row"
    );
}
