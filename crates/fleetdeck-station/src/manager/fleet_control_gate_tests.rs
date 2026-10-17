//! The authority gate of a fleet-control effect runs after every awaited
//! preparation, right before the provider write or launch (ADR-008 §6.2,
//! PRE-PR fixes 1 and 4), and a launch or a mode change sends the mode that
//! it really uses.

use fleetdeck_proto::mutation::LaunchMode;

use super::*;

fn spec_start(st: &Station) -> SessionCommand {
    SessionCommand::Start {
        spec: spec(&st.workspace(), "claude"),
        prompt: Some(text("hello")),
        workspace: None,
        setup_cmd: None,
        batch: None,
        issue: None,
        origin_text: false,
    }
}

// A revocation that happens while an input waits for the workspace fence
// reaches the gate: the deck is asked only after the fence, and the provider
// gets no write.
#[tokio::test]
async fn a_revocation_while_an_input_waits_for_the_workspace_fence_stops_the_write() {
    for kind in ["prompt", "follow_up"] {
        let mut st = Station::new(4).await;
        let s = idle_session(&mut st).await;
        let deck = FakeDeck::new(Reply::Valid);
        st.link_deck(&deck);
        let turns = st.manager.target.turns_of(&s).await.unwrap();
        let fence = turns.fence().await;
        let command = if kind == "prompt" {
            SessionCommand::Prompt {
                session_id: s.clone(),
                input: text("held"),
            }
        } else {
            follow_up(&s, "held")
        };
        let d = st.coord(command).await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            deck.asked().is_empty(),
            "{kind}: the gate waits for the fence"
        );
        // The deck revokes while the input waits.
        deck.set(Reply::Invalid);
        drop(fence);
        let retired = wait_retired(&st, &s, &d.receipt.command_id).await;
        assert_eq!(retired.reason.as_deref(), Some("authority_lost"), "{kind}");
        assert_eq!(deck.asked().len(), 1, "{kind}");
        assert_eq!(
            st.turns_done(&s).await,
            1,
            "{kind}: the provider got no write"
        );
        assert_eq!(st.errors(&s).await, ["authority_lost"], "{kind}");
        st.manager.shutdown(StopMode::Kill).await;
    }
}

// The same for a start that waits for its preparation: the deck is asked
// right before the launch, and a revocation up to then stops the launch.
#[tokio::test]
async fn a_revocation_while_a_start_waits_for_its_preparation_stops_the_launch() {
    let mut st = Station::new(4).await;
    st.switch(true);
    let deck = FakeDeck::new(Reply::Valid);
    st.link_deck(&deck);
    let target = Arc::clone(&st.manager.target);
    let hook = target.gate_hook();
    hook.arm();
    let command = spec_start(&st);
    let d = st.coord(command).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    let s = d.receipt.result.unwrap().session_id.unwrap();
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .expect("the start reaches the gate");
    assert!(deck.asked().is_empty(), "nothing is asked before the gate");
    deck.set(Reply::Invalid);
    hook.release.notify_one();
    st.wait_for("the failed start", |e| match e {
        Event::SessionEnded { reason, .. } if reason == "start_failed" => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(st.errors(&s).await[0], "authority_lost");
    assert_eq!(deck.asked().len(), 1);
    assert_eq!(
        starts_of(&st, &s).await,
        0,
        "the provider was never launched"
    );
    st.manager.shutdown(StopMode::Kill).await;
}

/// A session that ran one turn and ended, with the switch on.
async fn ended_session(st: &mut Station) -> SessionId {
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    st.wait_turns(&s, 1).await;
    st.accepted(SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Graceful,
    })
    .await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while st.state(&s).await != SessionState::Exited {
        assert!(tokio::time::Instant::now() < deadline, "the session ends");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    st.settled(&s).await;
    st.switch(true);
    s
}

async fn starts_of(st: &Station, session: &SessionId) -> usize {
    st.events()
        .await
        .into_iter()
        .filter(|e| e.session_id == *session)
        .filter(|e| matches!(e.event, EventBody::Known(Event::SessionStarted { .. })))
        .count()
}

// A resume sends the provider kind and the stored mode; a set_mode sends the
// new mode; an input sends no launch.
#[tokio::test]
async fn a_launch_or_a_mode_change_sends_the_mode_that_it_really_uses() {
    let mut st = Station::new(4).await;
    let s = ended_session(&mut st).await;
    let deck = FakeDeck::new(Reply::Valid);
    st.link_deck(&deck);
    let stored = st
        .manager
        .store()
        .summary(s.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.mode.as_deref(), Some("default"));
    let d = st
        .coord(SessionCommand::Resume {
            session_id: s.clone(),
        })
        .await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    st.wait_asked(&deck, 1).await;
    assert_eq!(
        deck.asked()[0].launch,
        Some(LaunchMode {
            provider: ProviderKind::Claude,
            mode: Some("default".to_owned()),
        })
    );
    // The resumed session takes a mode change and an input.
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let d = st
            .coord(SessionCommand::SetMode {
                session_id: s.clone(),
                mode: "plan".to_owned(),
            })
            .await;
        if d.receipt.state == ReceiptState::Accepted {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{:?}", d.receipt);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    st.wait_asked(&deck, 2).await;
    assert_eq!(
        deck.asked()[1].launch,
        Some(LaunchMode {
            provider: ProviderKind::Claude,
            mode: Some("plan".to_owned()),
        })
    );
    let d = st.coord(follow_up(&s, "more")).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    st.wait_asked(&deck, 3).await;
    assert_eq!(deck.asked()[2].launch, None, "an input launches nothing");
    st.manager.shutdown(StopMode::Kill).await;
}

// A deck answer `mode_not_allowed` refuses the resume like any invalid answer.
#[tokio::test]
async fn a_mode_not_allowed_answer_refuses_the_resume() {
    let mut st = Station::new(4).await;
    let s = ended_session(&mut st).await;
    let deck = FakeDeck::new(Reply::ModeNotAllowed);
    st.link_deck(&deck);
    let started_before = starts_of(&st, &s).await;
    let d = st
        .coord(SessionCommand::Resume {
            session_id: s.clone(),
        })
        .await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    wait_retired(&st, &s, &d.receipt.command_id).await;
    assert!(deck.asked()[0].launch.is_some());
    assert!(st.errors(&s).await.contains(&"authority_lost".to_owned()));
    assert_eq!(st.state(&s).await, SessionState::Exited);
    assert_eq!(starts_of(&st, &s).await, started_before, "no new process");
    st.manager.shutdown(StopMode::Kill).await;
}

/// A station on the shim host whose shim executable does not exist: a launch
/// that reaches `host.start` fails, so a test sees a gate that missed.
async fn shim_station() -> Station {
    use fleetdeck_providers::host::shim::host::{ShimHost, ShimHostConfig};

    let det = Det::new();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("projects/app")).unwrap();
    std::fs::create_dir_all(root.path().join("station")).unwrap();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let policy = Policy::new(
        vec![root.path().join("projects")],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
    );
    let adapters = Adapters::from([(
        "claude".into(),
        adapter(FakeAgent::Claude.locate().unwrap()),
    )]);
    let mut config = ShimHostConfig::new(
        PathBuf::from("/nonexistent/fleetdeck"),
        root.path().join("station"),
        store.incarnation().as_str().to_owned(),
    );
    config.runtime_dir = None;
    config.tmp_dir = root.path().to_owned();
    let host = ShimHost::new(config);
    let shims = crate::manager::target::ShimSettings::new(Arc::new(host));
    let manager = SessionManager::start_with_shims(
        store,
        policy,
        adapters,
        det.ids(),
        shims,
        Arc::new(crate::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    Station {
        det,
        manager: Arc::new(manager),
        root,
        next: 0,
    }
}

// The gate of a durable launch runs after the `starting` row, right before
// the shim starts (the test holds it there). A revocation up to then stops
// the launch: no shim process starts, the row is `never_started`, and the
// start failed with `authority_lost`.
#[tokio::test]
async fn a_revocation_after_the_starting_row_stops_the_shim_launch() {
    use crate::store::ShimState;

    let mut st = shim_station().await;
    st.switch(true);
    let deck = FakeDeck::new(Reply::Valid);
    st.link_deck(&deck);
    let manager = Arc::clone(&st.manager);
    let hook = manager.gate_hook();
    hook.arm();
    let command = spec_start(&st);
    let d = st.coord(command).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    let s = d.receipt.result.unwrap().session_id.unwrap();
    let reached = tokio::time::timeout(WAIT, hook.reached.notified()).await;
    assert!(
        reached.is_ok(),
        "the launch reaches the gate: {:#?}",
        st.events().await
    );
    let rows = st.manager.store().shim_rows(s.clone()).await.unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].state, ShimState::Starting);
    assert!(deck.asked().is_empty(), "nothing is asked before the gate");
    deck.set(Reply::Invalid);
    hook.release.notify_one();
    st.wait_for("the failed start", |e| match e {
        Event::SessionEnded { reason, .. } if reason == "start_failed" => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(st.errors(&s).await, ["authority_lost"]);
    let rows = st.manager.store().shim_rows(s.clone()).await.unwrap();
    assert_eq!(rows[0].state, ShimState::NeverStarted, "{rows:?}");
    assert_eq!(rows[0].reason.as_deref(), Some("shim_never_started"));
    assert_eq!(starts_of(&st, &s).await, 0);
    st.manager.shutdown(StopMode::Kill).await;
}

// Every gate of a launching effect sends its launch mode: both gates of a
// start (the launch and the first input) and the mode change; an input sends
// none.
#[tokio::test]
async fn the_first_input_gate_of_a_start_sends_the_launch_mode() {
    let mut st = Station::new(4).await;
    st.switch(true);
    let deck = FakeDeck::new(Reply::Valid);
    st.link_deck(&deck);
    let command = spec_start(&st);
    let d = st.coord(command).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    let s = d.receipt.result.unwrap().session_id.unwrap();
    st.wait_asked(&deck, 2).await;
    let launch = Some(LaunchMode {
        provider: ProviderKind::Claude,
        mode: Some("default".to_owned()),
    });
    assert_eq!(deck.asked()[0].launch, launch, "the launch gate");
    assert_eq!(deck.asked()[1].launch, launch, "the first-input gate");
    st.wait_turns(&s, 1).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let d = st
            .coord(SessionCommand::SetMode {
                session_id: s.clone(),
                mode: "plan".to_owned(),
            })
            .await;
        if d.receipt.state == ReceiptState::Accepted {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{:?}", d.receipt);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    st.wait_asked(&deck, 3).await;
    assert_eq!(
        deck.asked()[2].launch,
        Some(LaunchMode {
            provider: ProviderKind::Claude,
            mode: Some("plan".to_owned()),
        })
    );
    let d = st.coord(follow_up(&s, "more")).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    st.wait_asked(&deck, 4).await;
    assert_eq!(deck.asked()[3].launch, None, "an input launches nothing");
    st.manager.shutdown(StopMode::Kill).await;
}
