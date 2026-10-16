//! Resume on the shim host (DL-182, design `shim-resume.md` §7): the
//! decisions, the platform gate, the verified reboot at the station start,
//! the revalidation at the dispatch and the exclusion of one conversation.
//! The sessions end under the direct host against the fake agents; their
//! shim rows are written the way a shim station leaves them. The real
//! shims are in `crates/fleetdeck-cli/tests/shim_resume*.rs`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleetdeck_proto::command::{CommandOrigin, ReceiptState, SessionCommand, StopMode, UserInput};
use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::{Event, RejectCode, SessionId};
use fleetdeck_providers::host::shim::host::{ShimHost, ShimHostConfig};
use fleetdeck_providers::host::shim::identity::identity_of;
use fleetdeck_providers::host::shim::wire::{ExitState, ProviderIdentity};

use super::shim_resume_rig::*;
use super::*;
use crate::coordinator::Bindings;
use crate::store::{
    Caller, EffectPayload, EffectState, NewShim, OfflineApprovalPolicy, RunningFacts, ShimState,
    StoreConfig,
};

// --- the exit report (design §1a, tests 19 and 21) ---------------------

fn exit(remaining: Option<u32>, inspection: u32) -> ExitState {
    ExitState {
        termination: "code:0".to_owned(),
        cause: "exited".to_owned(),
        stderr_tail: String::new(),
        descendants_remaining: remaining,
        final_offset: 0,
        inspection,
    }
}

#[test]
fn only_a_complete_inspection_with_no_member_proves_a_clean_exit() {
    use crate::manager::shims::{exit_reason, reason};
    assert_eq!(exit_reason(Some(&exit(Some(0), 1))), reason::EXITED);
    assert_eq!(
        exit_reason(Some(&exit(Some(2), 1))),
        reason::EXITED_DESCENDANTS_REMAINING
    );
    // An unavailable count is not a proof (design §1a).
    assert_eq!(exit_reason(Some(&exit(None, 1))), reason::EXIT_UNVERIFIED);
    // An older shim: its zero count is unverified; its other counts stay
    // descendants remaining; `u32::MAX` is how version 1 says "unknown".
    assert_eq!(
        exit_reason(Some(&exit(Some(0), 0))),
        reason::EXIT_UNVERIFIED
    );
    assert_eq!(
        exit_reason(Some(&exit(Some(u32::MAX), 0))),
        reason::EXITED_DESCENDANTS_REMAINING
    );
    assert_eq!(exit_reason(None), reason::EXIT_NOT_REPORTED);
}

// --- the platform gate (test 16) ---------------------------------------

#[tokio::test]
async fn a_resume_import_and_hibernated_input_on_a_platform_without_a_reader_are_unsupported() {
    let mut rig = Rig::direct(Some(false)).await;
    let session = rig.ended("claude").await;
    let (code, message) = rig.refused(resume(&session)).await;
    assert_eq!(code, RejectCode::Unsupported);
    assert_eq!(
        message,
        "Import and resume need Linux on a shim host. On this station, set Host to direct in its settings."
    );
    let (code, message) = rig.refused(import(&rig, "claude", "native-x")).await;
    assert_eq!(code, RejectCode::Unsupported);
    assert_eq!(
        message,
        "Import and resume need Linux on a shim host. On this station, set Host to direct in its settings."
    );
    // The input of a hibernated session resumes it, so it is refused too.
    let live = rig.start("claude").await;
    // Events of the first turn can still move `updated_at` after the start:
    // advance and sweep again until the session is idle for the interval.
    let (store, policy) = (rig.store(), rig.manager.policy());
    let mut swept = 0;
    for _ in 0..50 {
        rig.det
            .advance(Duration::from_millis(u64::from(AFTER) * 60_000 + 1));
        swept = hibernate::sweep(&store, &policy, &Bindings::default(), AFTER)
            .await
            .unwrap();
        if swept > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(swept >= 1);
    rig.wait_state(&live, SessionState::Exited).await;
    let (code, message) = rig
        .refused(SessionCommand::Prompt {
            session_id: live.clone(),
            input: UserInput::new("again".to_owned()),
        })
        .await;
    assert_eq!(code, RejectCode::Unsupported);
    assert_eq!(
        message,
        "Import and resume need Linux on a shim host. On this station, set Host to direct in its settings."
    );
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_station_on_the_direct_host_or_a_supported_platform_resumes() {
    let mut rig = Rig::direct(Some(true)).await;
    let session = rig.ended("claude").await;
    rig.accepted(resume(&session)).await;
    rig.manager.shutdown(StopMode::Kill).await;
}

// --- the decision (test 4) ---------------------------------------------

#[tokio::test]
async fn a_session_whose_newest_row_has_descendants_remaining_is_not_resumable() {
    let mut rig = Rig::direct(None).await;
    let session = rig.ended("claude").await;
    let id = dead_identity(&real_boot());
    rig.row(
        &session,
        "a",
        ShimState::Exited,
        "exited",
        1,
        Some(id.clone()),
    )
    .await;
    rig.row(
        &session,
        "b",
        ShimState::Exited,
        "exited_descendants_remaining",
        1,
        Some(id),
    )
    .await;
    let (code, message) = rig.refused(resume(&session)).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(
        message.contains("not_resumable") && message.contains("exited_descendants_remaining"),
        "{message}"
    );
    rig.manager.shutdown(StopMode::Kill).await;
}

// --- the verified reboot (tests 6, 7, 13, 22) --------------------------

const LOST_REASONS: [(ShimState, &str); 8] = [
    (ShimState::Lost, "execution_unknown"),
    (ShimState::Lost, "attach_refused"),
    (ShimState::Lost, "identity_mismatch"),
    (ShimState::Lost, "exit_not_reported"),
    (ShimState::Lost, "adapter_checkpoint_corrupt: missing field"),
    (ShimState::Lost, "driver_failed"),
    (ShimState::Exited, "exited_descendants_remaining"),
    (ShimState::Exited, "exit_unverified"),
];

#[tokio::test]
async fn a_different_boot_id_proves_every_unresolved_row_and_the_session_becomes_resumable() {
    let mut rig = Rig::direct(None).await;
    for (n, (state, why)) in LOST_REASONS.into_iter().enumerate() {
        let session = rig.lost("claude").await;
        rig.row(
            &session,
            &format!("row-{n}"),
            state,
            why,
            1,
            Some(dead_identity("boot-old")),
        )
        .await;
        let (code, _) = rig.refused(resume(&session)).await;
        assert_eq!(code, RejectCode::InvalidState, "{why}");
    }
    // Same boot id, unreadable and empty: no proof (fail closed).
    rig.boot.set(Some("boot-old"));
    assert_eq!(
        shims::prove_reboots(&rig.store(), &rig.host).await.unwrap(),
        0
    );
    rig.boot.set(None);
    assert_eq!(
        shims::prove_reboots(&rig.store(), &rig.host).await.unwrap(),
        0
    );
    rig.boot.set(Some(""));
    assert_eq!(
        shims::prove_reboots(&rig.store(), &rig.host).await.unwrap(),
        0
    );
    assert_eq!(rig.store().shims_unresolved(None).await.unwrap().len(), 8);
    // A reboot proves them all, whatever the diagnostic reason.
    rig.boot.set(Some("boot-new"));
    assert_eq!(
        shims::prove_reboots(&rig.store(), &rig.host).await.unwrap(),
        8
    );
    let sessions: Vec<SessionId> = rig
        .store()
        .shims_unresolved(None)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.session_id)
        .collect();
    assert!(sessions.is_empty());
    for (n, (state, why)) in LOST_REASONS.into_iter().enumerate() {
        let row = rig.store().shim(format!("row-{n}")).await.unwrap().unwrap();
        // The state and the reason stay; the proof is next to them.
        assert_eq!((row.state, row.reason.as_deref()), (state, Some(why)));
        assert_eq!(row.proof.as_deref(), Some("ended_by_reboot"));
        assert_eq!(row.proof_boot_id.as_deref(), Some("boot-new"));
        // The session stays `lost`, and it resumes.
        assert_eq!(rig.state(&row.session_id).await, SessionState::Lost);
        let d = rig.send(resume(&row.session_id)).await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{why}");
    }
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn auto_continue_picks_a_session_that_a_reboot_proof_made_resumable() {
    use fleetdeck_proto::event::RunOrigin;
    let mut rig = Rig::direct(None).await;
    let session = rig.ended("claude").await;
    // The turn that the restart interrupted comes after the owner's stop.
    rig.det.advance(Duration::from_secs(1));
    let store = rig.store();
    let run = Event::RunStarted {
        author: None,
        input_id: None,
        run_id: "run-w".into(),
        input_summary: "work".to_owned(),
        origin: RunOrigin::Board,
    };
    let turn = Event::TurnStarted {
        run_id: "run-w".into(),
        turn_id: "turn-w".into(),
        provider_refs: serde_json::Map::new(),
    };
    let on = Event::AutoContinueChanged { enabled: true };
    store
        .append(session.clone(), vec![on.into(), run.into(), turn.into()])
        .await
        .unwrap();
    let head = store.head();
    let lost = Event::SessionStateChanged {
        state: SessionState::Lost,
        detail: Some("test".to_owned()),
    };
    store
        .append(session.clone(), vec![lost.into()])
        .await
        .unwrap();
    rig.row(
        &session,
        "row-0",
        ShimState::Lost,
        "execution_unknown",
        0,
        Some(dead_identity("boot-old")),
    )
    .await;
    assert_eq!(
        shims::prove_reboots(&rig.store(), &rig.host).await.unwrap(),
        1
    );
    // A shim station continues it with the proof (DL-188 step 3).
    let policy = (*rig.manager.policy()).clone().on_shim_host();
    assert_eq!(
        crate::manager::auto_continue::evaluate(&store, &policy, head)
            .await
            .unwrap(),
        1
    );
    let effects = store.session_effects(session.clone()).await.unwrap();
    let origins: Vec<_> = effects
        .iter()
        .filter_map(|e| match &e.payload {
            EffectPayload::Resume {
                continue_by: Some(_),
                prompt: Some(p),
                ..
            } => Some(p.origin),
            _ => None,
        })
        .collect();
    assert_eq!(origins, vec![RunOrigin::Continuation], "{effects:?}");
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_launch_without_an_identity_gets_a_reboot_proof_from_its_launch_boot_id() {
    let mut rig = Rig::direct(None).await;
    let session = rig.lost("claude").await;
    let native = rig.native(&session).await;
    // Power loss after the spawn and before the identity commit (test 22):
    // the row has the boot id of the launch journal and no identity, and it
    // names the conversation that it continues.
    rig.row_for(
        &session,
        "ghost",
        ShimState::Lost,
        "execution_unknown",
        0,
        None,
        Some("boot-old"),
        Some(&native),
    )
    .await;
    std::fs::create_dir_all(rig.host.config().shims_dir()).unwrap();
    // The same boot: it stays unresolved; a resume and an import of the
    // conversation are refused.
    rig.boot.set(Some("boot-old"));
    assert_eq!(
        shims::prove_reboots(&rig.store(), &rig.host).await.unwrap(),
        0
    );
    assert_eq!(
        rig.refused(resume(&session)).await.0,
        RejectCode::InvalidState
    );
    assert_eq!(
        rig.refused(import(&rig, "claude", &native)).await.0,
        RejectCode::InvalidState
    );
    // After a reboot the launch is fenced, the proof applies, and both are
    // available again across instances.
    rig.boot.set(Some("boot-new"));
    assert_eq!(
        shims::prove_reboots(&rig.store(), &rig.host).await.unwrap(),
        1
    );
    let ghost = rig.store().shim("ghost".to_owned()).await.unwrap().unwrap();
    assert_eq!(ghost.proof.as_deref(), Some("ended_by_reboot"));
    rig.accepted(resume(&session)).await;
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_row_without_any_boot_evidence_never_gets_a_reboot_proof() {
    let mut rig = Rig::direct(None).await;
    let session = rig.lost("claude").await;
    // A row of an older build: no identity and no launch boot id.
    rig.store()
        .shim_starting(NewShim {
            shim_id: "old".to_owned(),
            session_id: session.clone(),
            socket_path: PathBuf::from("/nonexistent/old.s"),
            launch_boot_id: None,
            native_target: None,
        })
        .await
        .unwrap();
    rig.store()
        .shim_end(
            "old".to_owned(),
            ShimState::Lost,
            "execution_unknown".to_owned(),
            0,
        )
        .await
        .unwrap();
    rig.boot.set(Some("boot-new"));
    assert_eq!(
        shims::prove_reboots(&rig.store(), &rig.host).await.unwrap(),
        0
    );
    assert_eq!(
        rig.refused(resume(&session)).await.0,
        RejectCode::InvalidState
    );
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_station_start_proves_reboots_before_it_admits_a_command() {
    let mut rig = Rig::direct(None).await;
    let session = rig.lost("claude").await;
    rig.row(
        &session,
        "row-0",
        ShimState::Lost,
        "execution_unknown",
        0,
        Some(dead_identity("boot-old")),
    )
    .await;
    // A bare store, as at a station start: recovery first, then commands.
    let store = Store::open_in_memory(rig.det.clock(), rig.det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store
        .create_session(crate::store::NewSession {
            session_id: "s-old".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    store
        .shim_starting(NewShim {
            shim_id: "row-1".to_owned(),
            session_id: "s-old".into(),
            socket_path: PathBuf::from("/nonexistent/row-1.s"),
            launch_boot_id: Some("boot-old".to_owned()),
            native_target: None,
        })
        .await
        .unwrap();
    store
        .shim_running_with(
            "row-1".to_owned(),
            dead_identity("boot-old"),
            RunningFacts::default(),
        )
        .await
        .unwrap();
    // The socket does not exist: recovery finds the shim gone, the row
    // `lost` (`execution_unknown`), and the boot pass proves it.
    let host = ShimHost::new({
        let mut config = ShimHostConfig::new(
            PathBuf::from("/nonexistent/fleetdeck"),
            rig.tmp.path().join("station"),
            store.incarnation().as_str().to_owned(),
        );
        config.boot_id_reader = rig.boot.reader();
        config
    });
    std::fs::create_dir_all(host.config().shims_dir()).unwrap();
    rig.boot.set(Some("boot-new"));
    assert!(!store.is_recovered());
    let configs = durable_configs(&adapters());
    crate::manager::shim_station::recover_station(
        &store,
        &host,
        &configs,
        Duration::from_millis(50),
    )
    .await
    .unwrap();
    assert!(store.is_recovered());
    let row = store.shim("row-1".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.state, ShimState::Lost);
    assert_eq!(row.reason.as_deref(), Some("execution_unknown"));
    assert_eq!(row.proof.as_deref(), Some("ended_by_reboot"));
    rig.manager.shutdown(StopMode::Kill).await;
}

// --- revalidation at launch (test 12) ----------------------------------

#[test]
fn a_reboot_proof_holds_only_while_the_boot_id_differs_and_no_process_has_the_identity() {
    fn row(identity: ProviderIdentity) -> crate::store::ShimRow {
        crate::store::ShimRow {
            shim_id: "r".to_owned(),
            session_id: "s".into(),
            incarnation: "i".to_owned(),
            state: ShimState::Lost,
            generation: 1,
            socket_path: PathBuf::from("/x"),
            provider_identity: Some(identity),
            cursor: 0,
            next_write_seq: 1,
            reason: Some("execution_unknown".to_owned()),
            inspection_version: 0,
            shim_pid: None,
            proof: Some("ended_by_reboot".to_owned()),
            proof_boot_id: Some("boot-2".to_owned()),
            launch_boot_id: None,
            native_target: None,
            created_at: 1,
        }
    }
    let boot = Boot(Arc::new(Mutex::new(Some("boot-2".to_owned()))));
    let dead = [row(dead_identity("boot-1"))];
    assert_eq!(shims::revalidate(&boot.reader(), &dead), None);
    // A memory restore brings the old boot id back.
    boot.set(Some("boot-1"));
    assert!(shims::revalidate(&boot.reader(), &dead).is_some());
    boot.set(None);
    assert!(shims::revalidate(&boot.reader(), &dead).is_some());
    // A live process with the stored identity.
    boot.set(Some("boot-2"));
    let mut alive = identity_of(std::process::id()).unwrap();
    alive.boot_id = "boot-1".to_owned();
    let found = shims::revalidate(&boot.reader(), &[row(alive)]);
    assert!(found.is_some_and(|w| w.contains("alive")));
    // Nothing to check: nothing refuses.
    assert_eq!(shims::revalidate(&boot.reader(), &[]), None);
}

/// A shim station with `session` (a lost row, proven by a reboot) ready for
/// a resume whose dispatch a test holds.
async fn held_resume(rig: &mut Rig, identity: ProviderIdentity) -> (SessionId, SessionId) {
    let session = rig.lost("claude").await;
    rig.row(
        &session,
        "row-0",
        ShimState::Lost,
        "execution_unknown",
        0,
        Some(identity),
    )
    .await;
    rig.boot.set(Some("boot-new"));
    // The station start proves the reboot before it admits a command.
    rig.as_shim_station().await;
    let row = rig.store().shim("row-0".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.proof.as_deref(), Some("ended_by_reboot"));
    let manager = Arc::clone(&rig.manager);
    let hook = manager.target.resume_dispatch_hook();
    hook.arm();
    rig.accepted(resume(&session)).await;
    hook.reached.notified().await;
    (session.clone(), session)
}

async fn assert_not_dispatched(rig: &Rig, session: &SessionId, rows_before: usize) {
    rig.wait_until("start_failed", |e| {
        e.iter().any(|e| {
            matches!(e, Event::Error { code, message, .. }
                if code == "start_failed" && message.contains("not proven ended"))
        })
    })
    .await;
    rig.wait_state(session, SessionState::Exited).await;
    let effects = rig.store().session_effects(session.clone()).await.unwrap();
    let last = effects.last().unwrap();
    assert_eq!(last.payload.kind(), "resume");
    assert_eq!(last.state, EffectState::RetiredNotDispatched, "{last:?}");
    assert_eq!(last.reason.as_deref(), Some("execution_not_ended"));
    assert_eq!(
        rig.rows(session).await.len(),
        rows_before,
        "no shim launched"
    );
}

#[tokio::test]
async fn a_row_that_appears_between_the_decision_and_the_dispatch_stops_the_resume() {
    let mut rig = Rig::direct(None).await;
    let (session, _) = held_resume(&mut rig, dead_identity("boot-old")).await;
    // Test 3: the decision passed; now an unresolved row appears.
    rig.row(
        &session,
        "late",
        ShimState::Lost,
        "execution_unknown",
        0,
        None,
    )
    .await;
    rig.manager
        .target
        .resume_dispatch_hook()
        .release
        .notify_one();
    assert_not_dispatched(&rig, &session, 2).await;
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_memory_restore_that_brings_back_the_old_boot_id_stops_the_resume() {
    let mut rig = Rig::direct(None).await;
    let (session, _) = held_resume(&mut rig, dead_identity("boot-old")).await;
    let row = rig.store().shim("row-0".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.proof.as_deref(), Some("ended_by_reboot"));
    rig.boot.set(Some("boot-old"));
    rig.manager
        .target
        .resume_dispatch_hook()
        .release
        .notify_one();
    assert_not_dispatched(&rig, &session, 1).await;
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_live_process_with_the_stored_identity_stops_a_resume_even_after_a_reboot_proof() {
    let mut rig = Rig::direct(None).await;
    let mut alive = identity_of(std::process::id()).unwrap();
    alive.boot_id = "boot-old".to_owned();
    let (session, _) = held_resume(&mut rig, alive).await;
    rig.manager
        .target
        .resume_dispatch_hook()
        .release
        .notify_one();
    assert_not_dispatched(&rig, &session, 1).await;
    rig.manager.shutdown(StopMode::Kill).await;
}

// --- one conversation, one execution (tests 11, 17, 18) -----------------

#[tokio::test]
async fn an_import_of_a_conversation_with_an_unresolved_row_is_refused_until_proof_and_then_holds_it()
 {
    let mut rig = Rig::direct(None).await;
    let original = rig.ended("claude").await;
    let native = rig.native(&original).await;
    rig.row(
        &original,
        "r1",
        ShimState::Lost,
        "execution_unknown",
        0,
        Some(dead_identity(&real_boot())),
    )
    .await;
    let (code, message) = rig.refused(import(&rig, "claude", &native)).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(message.contains("r1"), "{message}");
    // The owner's proof frees the conversation; the import is accepted.
    assert!(
        rig.store()
            .shim_prove_reboot("r1".to_owned(), "boot-new".to_owned())
            .await
            .unwrap()
    );
    let d = rig.accepted(import(&rig, "claude", &native)).await;
    let imported = d.receipt.result.unwrap().session_id.unwrap();
    // The import's session holds the conversation: the original does not
    // resume (it would run a second process on it).
    let (code, message) = rig.refused(resume(&original)).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(message.contains(imported.as_str()), "{message}");
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn two_admissions_for_one_native_conversation_pass_exactly_once() {
    let mut rig = Rig::direct(None).await;
    let first = rig.envelope(CommandOrigin::Local, import(&rig, "codex", "thread-race"));
    let second = rig.envelope(CommandOrigin::Local, import(&rig, "codex-b", "thread-race"));
    let (a, b) = tokio::join!(
        rig.manager.submit(first, Caller::local()),
        rig.manager.submit(second, Caller::local())
    );
    let states = [a.unwrap().receipt.state, b.unwrap().receipt.state];
    assert_eq!(
        states
            .iter()
            .filter(|s| **s == ReceiptState::Accepted)
            .count(),
        1,
        "{states:?}"
    );
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_codex_import_through_another_instance_waits_for_the_proof_of_the_first() {
    let mut rig = Rig::direct(None).await;
    // Two instances may share one `CODEX_HOME`: the thread is the same.
    let first = rig.ended("codex").await;
    let thread = rig.native(&first).await;
    rig.row(
        &first,
        "a1",
        ShimState::Lost,
        "execution_unknown",
        0,
        Some(dead_identity(&real_boot())),
    )
    .await;
    let (code, _) = rig.refused(import(&rig, "codex-b", &thread)).await;
    assert_eq!(code, RejectCode::InvalidState);
    rig.store()
        .shim_prove_reboot("a1".to_owned(), "boot-new".to_owned())
        .await
        .unwrap();
    rig.accepted(import(&rig, "codex-b", &thread)).await;
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_launch_that_named_a_conversation_blocks_it_but_no_other_conversation() {
    let mut rig = Rig::direct(None).await;
    let crashed = rig.ended("claude").await;
    let other = rig.ended("claude").await;
    // The crash was after the spawn of a resume of "conv-N" and before
    // `session.started` (test 18): the row names it, the session's own
    // native id is another one.
    rig.row_for(
        &crashed,
        "r1",
        ShimState::Lost,
        "execution_unknown",
        0,
        Some(dead_identity(&real_boot())),
        None,
        Some("conv-N"),
    )
    .await;
    let (code, message) = rig.refused(import(&rig, "claude", "conv-N")).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(message.contains("r1"), "{message}");
    // A resume of another conversation, and an import of another one, pass.
    rig.accepted(resume(&other)).await;
    rig.accepted(import(&rig, "claude", "conv-O")).await;
    // A new conversation that crashed (no target) blocks nothing.
    rig.row_for(
        &other,
        "r2",
        ShimState::Lost,
        "execution_unknown",
        0,
        None,
        Some(&real_boot()),
        None,
    )
    .await;
    rig.accepted(import(&rig, "claude", "conv-P")).await;
    rig.manager.shutdown(StopMode::Kill).await;
}
