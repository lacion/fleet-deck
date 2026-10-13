//! The setup command of a session start (DL-148): the decision refusals, the
//! command that runs before the provider starts, and a failure that ends the
//! start with the exit text and the output tail.

use super::*;

/// A station whose policy allows (or refuses) setup commands, with the
/// environment of the `claude` instance.
async fn station(allow: bool) -> Station {
    let det = Det::new();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("projects/app")).unwrap();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let policy = Policy::new(
        vec![root.path().join("projects")],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
    )
    .with_allow_setup_cmd(allow)
    .with_setup_envs(BTreeMap::from([(
        "claude".into(),
        ChildEnv::from_process(),
    )]));
    let adapters = Adapters::from([(
        "claude".into(),
        adapter(FakeAgent::Claude.locate().unwrap()),
    )]);
    let manager = SessionManager::start(store, policy, adapters, det.ids())
        .await
        .unwrap();
    Station {
        det,
        manager,
        root,
        next: 0,
    }
}

fn start_with_setup(cwd: &str, cmd: &str) -> SessionCommand {
    SessionCommand::Start {
        spec: spec(cwd, "claude"),
        prompt: Some(text("hello")),
        workspace: None,
        setup_cmd: Some(cmd.to_owned()),
    }
}

async fn send_as(st: &mut Station, origin: CommandOrigin, command: SessionCommand) -> RejectCode {
    st.next += 1;
    let envelope = CommandEnvelope {
        command_id: CommandId::from(format!("origin-{}", st.next)),
        target_incarnation: None,
        retry_until: i64::try_from(st.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin,
        command,
    };
    let d = st.manager.submit(envelope, Caller::local()).await.unwrap();
    assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
    d.receipt.reject.unwrap().code
}

async fn rejection(st: &mut Station, command: SessionCommand) -> (RejectCode, String) {
    let d = st.send(command).await;
    assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
    let r = d.receipt.reject.unwrap();
    (r.code, r.message)
}

#[tokio::test]
async fn a_station_with_the_policy_off_refuses_a_setup_command() {
    let mut st = station(false).await;
    let ws = st.workspace();
    let (code, message) = rejection(&mut st, start_with_setup(&ws, "true")).await;
    assert_eq!(code, RejectCode::Forbidden);
    assert_eq!(
        message,
        "setup commands are off on this station (allow_setup_cmd = false)"
    );
    // Nothing was created.
    assert!(st.events().await.is_empty());
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_coordinator_start_cannot_carry_a_setup_command() {
    let mut st = station(true).await;
    let ws = st.workspace();
    let command = start_with_setup(&ws, "true");
    assert_eq!(
        send_as(&mut st, CommandOrigin::Coordinator, command).await,
        RejectCode::Forbidden
    );
    assert!(st.events().await.is_empty());
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_empty_too_long_or_nul_setup_command_is_invalid() {
    let mut st = station(true).await;
    let ws = st.workspace();
    let message = "the setup command must be 1 to 4096 characters without NUL";
    for cmd in ["", "  \n\t ", &"x".repeat(4097), "echo\0hi"] {
        let (code, text) = rejection(&mut st, start_with_setup(&ws, cmd)).await;
        assert_eq!(code, RejectCode::InvalidCommand, "{cmd:?}");
        assert_eq!(text, message);
    }
    assert!(st.events().await.is_empty());
    // The longest command is accepted.
    let d = st
        .accepted(start_with_setup(&ws, &format!("#{}", "x".repeat(4095))))
        .await;
    assert!(d.receipt.result.unwrap().session_id.is_some());
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_setup_command_runs_before_the_provider_starts() {
    let mut st = station(true).await;
    let ws = st.workspace();
    st.accepted(start_with_setup(&ws, "echo hi > setup.txt"))
        .await;
    st.wait_for("started", |e| {
        matches!(e, Event::SessionStarted { .. }).then_some(())
    })
    .await;
    let file = std::path::Path::new(&ws).join("setup.txt");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "hi\n");
    // The row of the run ended with the run.
    assert!(st.manager.store().setup_runs().await.unwrap().is_empty());
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_failing_setup_command_ends_the_start_with_its_exit_and_output() {
    let mut st = station(true).await;
    let ws = st.workspace();
    let d = st
        .accepted(start_with_setup(
            &ws,
            "echo out-line; echo err-line >&2; exit 3",
        ))
        .await;
    let session = d.receipt.result.unwrap().session_id.unwrap();
    let message = st
        .wait_for("start failure", |e| match e {
            Event::Error { code, message, .. } if code == "start_failed" => Some(message.clone()),
            _ => None,
        })
        .await;
    assert!(
        message.starts_with("the setup command failed (exit 3)\n\n"),
        "{message}"
    );
    assert!(message.contains("out-line"), "{message}");
    assert!(message.contains("err-line"), "{message}");
    st.wait_for("session ended", |e| {
        matches!(e, Event::SessionEnded { reason, .. } if reason == "start_failed").then_some(())
    })
    .await;
    assert!(
        !st.events()
            .await
            .iter()
            .any(|e| matches!(&e.event, EventBody::Known(Event::SessionStarted { .. }))),
        "the provider never started"
    );
    assert!(st.manager.store().setup_runs().await.unwrap().is_empty());
    let summary = st.manager.store().summary(session).await.unwrap().unwrap();
    assert_eq!(summary.state, SessionState::Exited);
    st.manager.shutdown(StopMode::Kill).await;
}

/// Whether any process of the station's user carries the marker (Linux).
#[cfg(target_os = "linux")]
fn marked(effect_id: &str) -> bool {
    let marker = format!("{}={effect_id}", crate::workspace::setup::MARKER_VAR);
    std::fs::read_dir("/proc").unwrap().flatten().any(|e| {
        std::fs::read(e.path().join("environ"))
            .is_ok_and(|b| b.split(|c| *c == 0).any(|v| v == marker.as_bytes()))
    })
}

/// The effect id of the start of `session`.
async fn start_effect(st: &Station, session: &SessionId) -> String {
    st.manager
        .store()
        .session_effects(session.clone())
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.payload.kind() == "start")
        .unwrap()
        .effect_id
        .as_str()
        .to_owned()
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_slow_setup_does_not_block_other_sessions_and_a_stop_ends_it() {
    let mut st = station(true).await;
    let ws_a = st.workspace();
    let a = st
        .accepted(start_with_setup(&ws_a, "sleep 30"))
        .await
        .receipt
        .result
        .unwrap()
        .session_id
        .unwrap();
    // The row of A exists while its command runs.
    let deadline = tokio::time::Instant::now() + WAIT;
    while st.manager.store().setup_runs().await.unwrap().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no setup row");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let effect_a = start_effect(&st, &a).await;
    assert!(marked(&effect_a), "the setup of A runs");
    // B starts and gets its first prompt answered while A still waits.
    let ws_b = st.workspace();
    let b = st.start(&ws_b, Some("hello b")).await;
    let reply = tokio::time::timeout(WAIT, async {
        loop {
            let found = st.events().await.iter().any(|e| {
                e.session_id == b
                    && matches!(&e.event, EventBody::Known(Event::TurnCompleted { .. }))
            });
            if found {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(reply.is_ok(), "B was blocked by the setup of A");
    // A stop of A cancels its setup and ends the session.
    st.accepted(SessionCommand::Stop {
        session_id: a.clone(),
        how: StopMode::Graceful,
    })
    .await;
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let ended = st.events().await.iter().any(|e| {
            e.session_id == a
                && matches!(
                    &e.event,
                    EventBody::Known(Event::Error { code, message, .. })
                        if code == "start_failed"
                            && message.starts_with("the session was stopped during its setup command")
                )
        });
        if ended {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "A was not stopped");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(st.manager.store().setup_runs().await.unwrap().is_empty());
    assert!(!marked(&effect_a), "no process of the setup is left");
    assert_eq!(st.state(&a).await, SessionState::Exited);
    st.settled(&a).await;
    st.manager.shutdown(StopMode::Kill).await;
}
