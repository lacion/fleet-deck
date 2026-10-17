//! Auto-continue tests that start a whole `SessionManager` on a store with a
//! lost session (split from `auto_continue_tests.rs`).

use super::*;

#[tokio::test]
async fn a_coordinator_or_the_local_socket_cannot_enable_auto_continue() {
    let rig = Rig::new().await;
    let s = rig.session("grant", false, Work::Idle).await;
    let manager = SessionManager::start(
        rig.store.clone(),
        rig.policy(),
        adapters(),
        rig.det.ids(),
        Arc::new(crate::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    let send = |origin: CommandOrigin, enabled: bool, n: u32| CommandEnvelope {
        authority: None,
        command_id: CommandId::from(format!("set-{n}")),
        target_incarnation: None,
        retry_until: rig.store.now_ms() + RETRY_WINDOW_MS,
        origin,
        command: SessionCommand::SetAutoContinue {
            session_id: s.clone(),
            enabled,
        },
    };
    // The local socket, whatever origin it names, and a coordinator.
    for (n, origin) in [
        CommandOrigin::Local,
        CommandOrigin::Board,
        CommandOrigin::Coordinator,
        CommandOrigin::Mail,
    ]
    .into_iter()
    .enumerate()
    {
        let n = u32::try_from(n).unwrap();
        let d = manager
            .submit(send(origin, true, n), Caller::local())
            .await
            .unwrap();
        assert_eq!(d.receipt.state, ReceiptState::Rejected);
        assert_eq!(d.receipt.reject.unwrap().code, RejectCode::Forbidden);
    }
    // A coordinator origin is refused in the decision itself, also for a
    // paired caller.
    let id = s.clone();
    let d = rig
        .store
        .decide(
            send(CommandOrigin::Coordinator, true, 10),
            Caller::local(),
            move |d| {
                crate::manager::auto_continue::decide_set(d, CommandOrigin::Coordinator, &id, true)
            },
        )
        .await
        .unwrap();
    assert_eq!(d.receipt.reject.unwrap().code, RejectCode::Forbidden);
    // The owner's board command turns it on; anyone can turn it off.
    let id = s.clone();
    let d = rig
        .store
        .decide(
            send(CommandOrigin::Board, true, 11),
            Caller::local(),
            move |d| crate::manager::auto_continue::decide_set(d, CommandOrigin::Board, &id, true),
        )
        .await
        .unwrap();
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    assert!(
        rig.store
            .summary(s.clone())
            .await
            .unwrap()
            .unwrap()
            .auto_continue
    );
    let d = manager
        .submit(send(CommandOrigin::Local, false, 12), Caller::local())
        .await
        .unwrap();
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    assert!(
        !rig.store
            .summary(s.clone())
            .await
            .unwrap()
            .unwrap()
            .auto_continue
    );
    manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_continued_session_is_shown_as_resumed_with_a_continuation_input() {
    let rig = Rig::new().await;
    let s = rig.session("e2e", true, Work::Turn).await;
    let manager = SessionManager::start(
        rig.store.clone(),
        rig.policy(),
        adapters(),
        rig.det.ids(),
        Arc::new(crate::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let events = rig.events(&s).await;
        let resumed = events.iter().any(|e| {
            matches!(e, Event::SessionStarted { resumed: Some(true), provider_refs, .. }
                if provider_refs.get("session_id").and_then(|v| v.as_str()) == Some("native-e2e"))
        });
        let continued = events.iter().any(|e| {
            matches!(
                e,
                Event::RunStarted {
                    origin: RunOrigin::Continuation,
                    ..
                }
            )
        });
        let done = events.iter().any(|e| {
            matches!(
                e,
                Event::TurnCompleted {
                    status: TurnStatus::Done,
                    ..
                }
            )
        });
        if resumed && continued && done {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{events:#?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(rig.skips(&s).await.is_empty());
    manager.shutdown(StopMode::Kill).await;
}
