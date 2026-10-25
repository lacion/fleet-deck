//! The refusals of the usage recount (B20, design `usage-cost.md` §4.2
//! step 3): the scope, the provenance and the running checks.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::command::{CommandOrigin, ReceiptState};
use fleetdeck_proto::event::{ProviderKind, USAGE_SCHEME, Usage, UsageSource};
use fleetdeck_proto::{Event, RejectCode, SessionId};
use fleetdeck_providers::host::shim::wire::ProviderIdentity;

use super::rig::*;
use crate::coordinator::Bindings;
use crate::manager::decide;
use crate::manager::usage_recount;
use crate::store::transcripts::FirstEffect;
use crate::store::{Caller, NewSession, NewShim, OfflineApprovalPolicy, RunningFacts, ShimState};

#[tokio::test]
async fn a_new_rule_total_is_refused() {
    let mut fx = Fx::new().await;
    fx.store()
        .create_session(NewSession {
            session_id: "s1".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: CWD.to_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    let live = Usage {
        input_tokens: Some(5),
        scheme: Some(USAGE_SCHEME),
        ..Usage::default()
    };
    fx.store()
        .append(
            "s1".into(),
            vec![
                started(ProviderKind::Claude, Some(NATIVE)),
                usage_event(live),
                ended(),
            ],
        )
        .await
        .unwrap();
    fx.effect("s1", "start", false, "confirmed").await;
    let summary = fx.store().summary("s1".into()).await.unwrap().unwrap();
    assert_eq!(summary.usage.scheme, Some(USAGE_SCHEME));
    fx.refused("s1", RejectCode::InvalidState, "counted_since_upgrade")
        .await;
}

#[tokio::test]
async fn an_old_count_set_apart_is_refused() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    // A new-rule event moves the old count into `usage_legacy`.
    fx.append(
        "s1",
        usage_event(Usage {
            input_tokens: Some(5),
            scheme: Some(USAGE_SCHEME),
            ..Usage::default()
        }),
    )
    .await;
    let summary = fx.store().summary("s1".into()).await.unwrap().unwrap();
    assert!(summary.usage_legacy.is_some());
    fx.refused("s1", RejectCode::InvalidState, "counted_since_upgrade")
        .await;
}

#[tokio::test]
async fn an_imported_session_is_refused() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    fx.next_sql(
        "INSERT INTO session_transcripts (session_id, capture, captured_at)
         VALUES ('s1', 'import', 1)"
            .to_owned(),
    )
    .await;
    fx.refused("s1", RejectCode::InvalidState, "other_history")
        .await;
}

#[tokio::test]
async fn a_moved_session_is_refused() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    fx.append(
        "s1",
        Event::SessionMoved {
            move_id: "m1".to_owned(),
            to_station: "station-b".into(),
            to_session: "t1".into(),
        }
        .into(),
    )
    .await;
    fx.refused("s1", RejectCode::InvalidState, "other_history")
        .await;
}

#[tokio::test]
async fn a_shared_conversation_is_refused() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    fx.session("s2", ProviderKind::Claude, Some(NATIVE)).await;
    fx.refused("s1", RejectCode::InvalidState, "other_history")
        .await;
    // Another conversation does not count.
    fx.session("s3", ProviderKind::Claude, Some(OTHER)).await;
    let (_, _, source, _) = set_parts(fx.accepted("s3").await);
    assert_eq!(source, UsageSource::None);
}

#[tokio::test]
async fn an_older_import_without_a_transcript_row_is_refused() {
    let mut fx = Fx::new().await;
    fx.bare_session("s1", ProviderKind::Claude, Some(NATIVE))
        .await;
    fx.effect("s1", "resume", true, "confirmed").await;
    write_private(
        &fx.claude_file(),
        &format!("{}{}", claude_user(), cost_state(20, 0.2)),
    );
    fx.refused("s1", RejectCode::InvalidState, "origin_unproven")
        .await;
}

#[tokio::test]
async fn an_import_from_before_the_import_flag_is_refused() {
    let mut fx = Fx::new().await;
    fx.bare_session("s1", ProviderKind::Claude, Some(NATIVE))
        .await;
    fx.effect("s1", "resume", false, "confirmed").await;
    fx.refused("s1", RejectCode::InvalidState, "origin_unproven")
        .await;
}

#[tokio::test]
async fn a_session_without_a_start_effect_is_refused() {
    let mut fx = Fx::new().await;
    fx.bare_session("s1", ProviderKind::Claude, Some(NATIVE))
        .await;
    fx.refused("s1", RejectCode::InvalidState, "origin_unproven")
        .await;
}

#[tokio::test]
async fn a_pruned_start_with_a_later_resume_is_refused() {
    let mut fx = Fx::new().await;
    fx.bare_session("s1", ProviderKind::Claude, Some(NATIVE))
        .await;
    fx.effect("s1", "resume", false, "confirmed").await;
    fx.refused("s1", RejectCode::InvalidState, "origin_unproven")
        .await;
}

#[tokio::test]
async fn the_recount_and_the_history_share_the_first_effect_rule() {
    let mut fx = Fx::new().await;
    let cases = [
        ("a-start", Some(("start", false)), FirstEffect::Start),
        ("b-plain", Some(("resume", false)), FirstEffect::Unknown),
        ("c-import", Some(("resume", true)), FirstEffect::Import),
        ("d-none", None, FirstEffect::Unknown),
    ];
    for (id, effect, want) in cases {
        fx.bare_session(id, ProviderKind::Claude, None).await;
        if let Some((kind, import)) = effect {
            fx.effect(id, kind, import, "confirmed").await;
        }
        let history = fx
            .store()
            .history_facts(id.into())
            .await
            .unwrap()
            .unwrap()
            .first_effect;
        assert_eq!(history, want, "{id}");
        let recount = fx.recount_origin(id).await;
        assert_eq!(recount, want, "{id}");
    }
}

#[tokio::test]
async fn a_live_session_is_refused() {
    let mut fx = Fx::new().await;
    fx.store()
        .create_session(NewSession {
            session_id: "s1".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: CWD.to_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    // Started and not ended.
    fx.append("s1", started(ProviderKind::Claude, Some(NATIVE)))
        .await;
    fx.append("s1", usage_event(old_count())).await;
    fx.effect("s1", "start", false, "confirmed").await;
    let message = fx
        .refused("s1", RejectCode::InvalidState, "session_running")
        .await;
    assert!(message.ends_with("It keeps its old count."), "{message}");
}

#[tokio::test]
async fn a_lost_row_without_proof_is_refused() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    fx.store()
        .shim_starting(NewShim {
            shim_id: "sh1".to_owned(),
            session_id: "s1".into(),
            socket_path: PathBuf::from("/run/x/sh1.sock"),
            launch_boot_id: Some("boot-1".to_owned()),
            native_target: None,
        })
        .await
        .unwrap();
    fx.store()
        .shim_running_with(
            "sh1".to_owned(),
            ProviderIdentity {
                pid: 10,
                start_time: 5,
                boot_id: "boot-1".to_owned(),
            },
            RunningFacts::default(),
        )
        .await
        .unwrap();
    fx.store()
        .shim_end(
            "sh1".to_owned(),
            ShimState::Lost,
            "adapter_checkpoint_unreadable".to_owned(),
            0,
        )
        .await
        .unwrap();
    // The row ends the execution in the log; the session itself is ended.
    let summary = fx.store().summary("s1".into()).await.unwrap().unwrap();
    assert!(matches!(
        summary.state,
        fleetdeck_proto::event::SessionState::Exited | fleetdeck_proto::event::SessionState::Lost
    ));
    fx.refused("s1", RejectCode::InvalidState, "session_running")
        .await;
}

#[tokio::test]
async fn a_queued_resume_is_refused() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    // A claimed row is an unsettled resume that the effect worker leaves
    // alone (a queued row of this shape would be dispatched).
    fx.effect("s1", "resume", false, "claimed").await;
    fx.refused("s1", RejectCode::InvalidState, "session_running")
        .await;
}

#[tokio::test]
async fn a_terminal_hold_is_refused() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    fx.append(
        "s1",
        Event::SessionTerminal {
            shell_id: Some("sh-1".to_owned()),
            pid: None,
            reason: None,
        }
        .into(),
    )
    .await;
    fx.refused("s1", RejectCode::InvalidState, "session_running")
        .await;
}

#[tokio::test]
async fn a_session_that_changed_is_refused() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    let envelope = fx.envelope("s1");
    let facts = fx.manager.recount_prepare(&envelope.command).await;
    // An event between the prepare and the decision.
    fx.append(
        "s1",
        Event::SessionDigest {
            text: "changed".to_owned(),
        }
        .into(),
    )
    .await;
    let session: SessionId = "s1".into();
    let decided = fx
        .store()
        .decide(envelope, Caller::local(), move |d| {
            usage_recount::decide(d, &session, facts)
        })
        .await
        .unwrap();
    assert_eq!(decided.receipt.state, ReceiptState::Rejected);
    let message = decided.receipt.reject.unwrap().message;
    assert!(message.starts_with("session_changed: "), "{message}");
    assert!(fx.sets().await.is_empty());
}

#[tokio::test]
async fn another_provider_is_refused() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Unknown, Some(NATIVE)).await;
    fx.refused("s1", RejectCode::Unsupported, "unsupported_provider")
        .await;
}

#[tokio::test]
async fn an_unknown_session_is_refused() {
    let mut fx = Fx::new().await;
    let decided = fx.recount("nope").await;
    assert_eq!(decided.receipt.state, ReceiptState::Rejected);
    assert_eq!(
        decided.receipt.reject.unwrap().code,
        RejectCode::UnknownSession
    );
}

#[tokio::test]
async fn the_recount_waits_for_the_session_lock() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    let session: SessionId = "s1".into();
    let guard = fx.manager.target.stops().locks.hold(&session).await;
    let envelope = fx.envelope("s1");
    let manager = Arc::clone(&fx.manager);
    let task = tokio::spawn(async move { manager.submit(envelope, Caller::local()).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!task.is_finished(), "the recount did not wait for the lock");
    assert!(fx.sets().await.is_empty());
    drop(guard);
    let decided = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    assert_eq!(fx.sets().await.len(), 1);
}

#[tokio::test]
async fn the_direct_decide_refuses_a_recount() {
    let mut fx = Fx::new().await;
    fx.session("s1", ProviderKind::Claude, Some(NATIVE)).await;
    let envelope = fx.envelope("s1");
    let command = envelope.command.clone();
    let policy = fx.manager.policy.current();
    let decided = fx
        .store()
        .decide(envelope, Caller::local(), move |d| {
            decide::decide(
                d,
                &policy,
                &Bindings::default(),
                CommandOrigin::Board,
                false,
                command,
            )
        })
        .await
        .unwrap();
    assert_eq!(decided.receipt.state, ReceiptState::Rejected);
    let message = decided.receipt.reject.unwrap().message;
    assert!(message.starts_with("usage_recount_direct: "), "{message}");
    assert!(fx.sets().await.is_empty());
}

#[test]
fn the_station_advertises_usage_recount() {
    use fleetdeck_proto::version::feature;
    assert!(feature::ALL.contains(&feature::USAGE_RECOUNT));
    assert!(!feature::REQUIRED.contains(&feature::USAGE_RECOUNT));
}
