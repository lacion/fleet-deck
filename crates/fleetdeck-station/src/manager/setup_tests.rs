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
