//! The restart freeze (PRE-PR r1 finding 3): new sessions are refused, the
//! queued starts stay queued, and the admission waits for the starts in
//! flight and then lists them.

use super::*;
use crate::manager::restart_gate;
use crate::shell::{ShellConfig, Shells};
use crate::store::EffectPayload;

fn start_command(cwd: &str) -> SessionCommand {
    SessionCommand::Start {
        spec: spec(cwd, "claude"),
        prompt: None,
        workspace: None,
        setup_cmd: None,
        batch: None,
        issue: None,
        origin_text: false,
        fleet_git: None,
    }
}

#[tokio::test]
async fn a_start_submitted_while_restarting_is_refused_and_works_again_after_a_refusal() {
    let mut st = Station::new(4).await;
    let cwd = st.workspace();
    st.manager.begin_restart();
    assert!(st.manager.restart_state().restarting());
    let d = st.send(start_command(&cwd)).await;
    assert_eq!(d.receipt.state, ReceiptState::Rejected);
    let reject = d.receipt.reject.unwrap();
    assert_eq!(reject.code, RejectCode::InvalidState);
    assert_eq!(reject.message, "the station is restarting");
    for command in [
        SessionCommand::Import {
            spec: spec(&cwd, "claude"),
            native_session_id: "native-1".into(),
            prompt: None,
            transcript: None,
        },
        SessionCommand::Resume {
            session_id: "s1".into(),
        },
    ] {
        assert_eq!(st.rejected(command).await, RejectCode::InvalidState);
    }
    // After a refusal the flag is clear and a start works.
    st.manager.end_restart();
    assert!(!st.manager.restart_state().restarting());
    st.accepted(start_command(&cwd)).await;
    st.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn a_committed_restart_keeps_the_freeze() {
    let st = Station::new(4).await;
    st.manager.begin_restart();
    st.manager.commit_restart();
    st.manager.end_restart();
    assert!(st.manager.restart_state().restarting());
}

#[tokio::test]
async fn a_queued_start_is_not_claimed_while_held_and_is_claimed_after() {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
        .create_session(crate::store::NewSession {
            session_id: "s1".into(),
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
        .process_started("s1".into(), "proc-1".into(), None)
        .await
        .unwrap();
    let fence = store.register_worker().await.unwrap();
    for (id, payload) in [
        (
            "q-start",
            EffectPayload::Start {
                spec: spec("/w", "claude"),
                prompt: None,
                workspace: None,
                setup_cmd: None,
                authority: None,
                fleet_git: None,
            },
        ),
        ("q-interrupt", EffectPayload::Interrupt { authority: None }),
    ] {
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(id),
            target_incarnation: None,
            retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Local,
            command: SessionCommand::Interrupt {
                session_id: "s1".into(),
            },
        };
        store
            .decide(envelope, crate::store::Caller::local(), move |d| {
                d.enqueue(&"s1".into(), payload)?;
                Ok(fleetdeck_proto::command::CommandResult {
                    session_id: Some("s1".into()),
                    input_id: None,
                })
            })
            .await
            .unwrap();
    }
    // Held: neither the start nor the later effect of its session is
    // claimed; both stay queued.
    assert!(store.claim_next_held(fence, true).await.unwrap().is_none());
    let queued = store.session_effects("s1".into()).await.unwrap();
    assert!(queued.iter().all(|e| e.state == EffectState::Queued));
    // Not held: the start comes first.
    let first = store.claim_next_held(fence, false).await.unwrap().unwrap();
    assert_eq!(first.payload.kind(), "start");
}

#[tokio::test]
async fn the_admission_waits_for_a_dispatch_in_flight_and_then_lists_the_session() {
    let mut st = Station::new(4).await;
    let cwd = st.workspace();
    let target = Arc::clone(&st.manager.target);
    // The start is held after the provider spawn, before the session is
    // registered on the target.
    let hook = &target.hooks().input;
    hook.arm();
    st.accepted(start_command(&cwd)).await;
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .unwrap();
    st.manager.begin_restart();
    let shells = Shells::new(ShellConfig::default());
    assert!(
        st.manager
            .restart_would_stop(&shells)
            .await
            .unwrap()
            .is_empty(),
        "the start is not live yet: a check now would miss it"
    );
    let settled = tokio::time::timeout(
        Duration::from_millis(300),
        st.manager.settle_starts(Duration::from_secs(5)),
    )
    .await;
    assert!(settled.is_err(), "the admission waits for the dispatch");
    hook.release.notify_one();
    assert!(st.manager.settle_starts(WAIT).await);
    let listed = st.manager.restart_would_stop(&shells).await.unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    st.manager.end_restart();
    st.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn a_submit_in_flight_is_waited_for_and_a_timeout_reports_false() {
    let st = Station::new(4).await;
    let state = Arc::clone(st.manager.restart_state());
    let held = state.enter_submit();
    assert!(!st.manager.settle_starts(Duration::from_millis(120)).await);
    drop(held);
    assert!(st.manager.settle_starts(Duration::from_millis(120)).await);
    let _ = restart_gate::RESTARTING;
}
