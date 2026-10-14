//! Resume on the shim host (DL-182, design `shim-resume.md` §7): real
//! shims, the fake Claude and Codex agents. A resume starts a new shim and
//! a new provider process; the board shows `resumed`.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::{Event, RejectCode};
use fleetdeck_station::manager::hibernate;
use fleetdeck_station::store::ShimState;

mod shim_resume_rig;
use shim_resume_rig::*;

/// Design §7 test 1 (Claude) and 2 (Codex): a session that exited cleanly
/// under a shim resumes under a new shim; the board shows `resumed`; the
/// old row stays `exited`.
async fn a_clean_exit_resumes_under_a_new_shim(instance: &str, rig: &mut Rig) {
    let st = rig.station().await;
    let session = rig.ended(&st, instance).await;
    let before = starts(&st.store, &session).await;
    let native = before[0].1.clone();
    assert!(!native.is_empty());
    let old = st.store.shim_rows(session.clone()).await.unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].state, ShimState::Exited);
    assert_eq!(old[0].reason.as_deref(), Some("exited"));
    assert_eq!(old[0].inspection_version, 1, "a complete inspection");
    assert!(
        old[0].launch_boot_id.is_some(),
        "boot evidence of the launch"
    );
    assert_eq!(old[0].native_target, None, "a new conversation");

    rig.send(
        &st,
        SessionCommand::Resume {
            session_id: session.clone(),
        },
    )
    .await;
    prompt_when_live(rig, &st, &session, "again").await;
    wait_turns(&st.store, &session, 2).await;
    wait_state(&st.store, &session, SessionState::Running).await;
    let after = starts(&st.store, &session).await;
    assert_eq!(after.len(), 2, "{after:?}");
    assert_eq!(after[1].0, Some(true), "the board shows `resumed`");
    assert_eq!(after[1].1, native, "the same native conversation");
    let rows = st.store.shim_rows(session.clone()).await.unwrap();
    assert_eq!(rows.len(), 2, "a new shim, not a reattachment");
    assert_eq!(rows[0].shim_id, old[0].shim_id);
    assert_eq!(rows[0].state, ShimState::Exited, "the old row stays");
    assert_eq!(rows[1].state, ShimState::Running);
    assert_eq!(rows[1].native_target.as_deref(), Some(native.as_str()));
    assert_ne!(
        rows[0].provider_identity.as_ref().map(|i| i.pid),
        rows[1].provider_identity.as_ref().map(|i| i.pid)
    );
    st.manager
        .shutdown(fleetdeck_proto::command::StopMode::Graceful)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claude_session_that_exited_cleanly_resumes_under_a_new_shim() {
    let mut rig = Rig::new();
    a_clean_exit_resumes_under_a_new_shim("claude", &mut rig).await;
    // The resumed CLI got `--resume <id>` and the stored mode.
    let launch = std::fs::read_to_string(rig.launch_log()).unwrap();
    assert!(launch.lines().any(|l| l == "--resume"), "{launch}");
    assert!(launch.lines().any(|l| l == "--permission-mode"), "{launch}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_codex_session_that_exited_cleanly_resumes_its_thread_under_a_new_shim() {
    let mut rig = Rig::new();
    a_clean_exit_resumes_under_a_new_shim("codex", &mut rig).await;
    assert_eq!(rig.method_count("thread/start"), 1);
    assert_eq!(rig.method_count("thread/resume"), 1, "`thread/resume`");
    assert_eq!(rig.method_count("initialize"), 2, "a new process");
}

/// Design §7 tests 5 and 10 (first half): the input to a hibernated
/// session resumes it under a new shim, and the provider receives that
/// input once, as its first input.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_input_to_a_hibernated_session_resumes_it_under_a_new_shim_and_arrives_once() {
    const AFTER: u32 = 30;
    let mut rig = Rig::new();
    let st = rig.station().await;
    let session = rig.start(&st, "claude", "hello").await;
    wait_for(&st.store, &session, "the run completes", |e| {
        e.iter().any(|e| matches!(e, Event::RunCompleted { .. }))
    })
    .await;
    // The hibernation of a normal running shim session is admitted (14).
    rig.det.advance(std::time::Duration::from_millis(
        u64::from(AFTER) * 60_000 + 1,
    ));
    let policy = st.manager.policy();
    let swept = hibernate::sweep(
        &st.store,
        &policy,
        &fleetdeck_station::coordinator::Bindings::default(),
        AFTER,
    )
    .await
    .unwrap();
    assert_eq!(swept, 1, "a running shim session hibernates");
    wait_state(&st.store, &session, SessionState::Exited).await;
    let rows = wait_rows(&st.store, &session, "the row ends", |rows| {
        rows.len() == 1 && rows[0].state == ShimState::Exited
    })
    .await;
    assert_eq!(rows[0].reason.as_deref(), Some("exited"));

    let native = starts(&st.store, &session).await[0].1.clone();
    rig.send(
        &st,
        SessionCommand::Prompt {
            session_id: session.clone(),
            input: text("after hibernation"),
        },
    )
    .await;
    wait_turns(&st.store, &session, 2).await;
    let after = starts(&st.store, &session).await;
    assert_eq!(after.len(), 2, "{after:?}");
    assert_eq!(
        (after[1].0, after[1].1.as_str()),
        (Some(true), native.as_str())
    );
    assert_eq!(
        rig.log_count("claude", "after hibernation"),
        1,
        "the prompt reached the provider once"
    );
    let rows = st.store.shim_rows(session.clone()).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].state, ShimState::Exited);
    st.manager
        .shutdown(fleetdeck_proto::command::StopMode::Graceful)
        .await;
}

/// Design §7 test 9: a provider that cannot resume the native id gives
/// `start_failed` with a committed end; the next resume is not blocked by
/// the failed one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_provider_that_cannot_resume_the_native_id_fails_the_start_and_ends_its_row() {
    let mut rig = Rig::new();
    let session = {
        let st = rig.station().await;
        let session = rig.ended(&st, "claude").await;
        st.manager
            .shutdown(fleetdeck_proto::command::StopMode::Graceful)
            .await;
        session
    };
    // The same station directory, a CLI that ends at once.
    rig.claude_scenario = "exit:3".to_owned();
    let st = rig.station().await;
    rig.send(
        &st,
        SessionCommand::Resume {
            session_id: session.clone(),
        },
    )
    .await;
    wait_for(&st.store, &session, "start_failed", |e| {
        e.iter()
            .any(|e| matches!(e, Event::Error { message, .. } if message.contains("start_failed")))
    })
    .await;
    wait_state(&st.store, &session, SessionState::Exited).await;
    // The failed launch ended its row with the shim's own evidence, so it
    // does not block the next resume.
    let rows = wait_rows(&st.store, &session, "both rows end", |rows| {
        rows.len() == 2 && rows.iter().all(|r| r.state != ShimState::Running)
    })
    .await;
    assert!(rows[1].native_target.is_some());
    assert_eq!(
        st.manager
            .store()
            .shim_rows(session.clone())
            .await
            .unwrap()
            .iter()
            .filter(|r| r.state == ShimState::Starting)
            .count(),
        0
    );
    // Not blocked by the failed one: the decision admits a new resume.
    rig.claude_scenario = "sim".to_owned();
    let st2 = rig.station().await;
    let decided = rig
        .submit(
            &st2,
            SessionCommand::Resume {
                session_id: session.clone(),
            },
        )
        .await;
    assert!(
        decided.receipt.reject.is_none()
            || decided
                .receipt
                .reject
                .as_ref()
                .is_some_and(|r| r.code != RejectCode::Unsupported),
        "{:?}",
        decided.receipt
    );
}

/// Design §7 test 23 at the station: a launch whose journal holds a boot id
/// that is not the machine's (the station read another one) never starts a
/// provider. The shim refuses, the journal proves no spawn, and the row is
/// `never_started`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_launch_with_a_wrong_boot_id_in_its_journal_starts_no_provider() {
    let mut rig = Rig::new();
    rig.set_boot(Boot::Id("00000000-0000-4000-8000-0000000000bb".to_owned()));
    let st = rig.station().await;
    let session = rig.start(&st, "claude", "hello").await;
    wait_for(&st.store, &session, "the start fails", |e| {
        e.iter()
            .any(|e| matches!(e, Event::Error { code, .. } if code == "start_failed"))
    })
    .await;
    let rows = st.store.shim_rows(session.clone()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, ShimState::NeverStarted, "{rows:?}");
    assert_eq!(
        rows[0].launch_boot_id.as_deref(),
        Some("00000000-0000-4000-8000-0000000000bb")
    );
    assert_eq!(rig.log_count("claude", ""), 0, "no provider ran");
    st.manager
        .shutdown(fleetdeck_proto::command::StopMode::Graceful)
        .await;
}

/// An unreadable boot id gives no launch at all (the journal needs one).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_launch_without_a_readable_boot_id_does_not_start() {
    let mut rig = Rig::new();
    rig.set_boot(Boot::Unreadable);
    let st = rig.station().await;
    let session = rig.start(&st, "claude", "hello").await;
    wait_for(&st.store, &session, "the start fails", |e| {
        e.iter().any(|e| {
            matches!(e, Event::Error { code, message, .. }
                if code == "start_failed" && message.contains("the boot id cannot be read"))
        })
    })
    .await;
    assert!(st.store.shim_rows(session).await.unwrap().is_empty());
    st.manager
        .shutdown(fleetdeck_proto::command::StopMode::Graceful)
        .await;
}
