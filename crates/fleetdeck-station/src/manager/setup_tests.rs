//! The setup command of a session start (DL-148): the decision refusals, the
//! command that runs before the provider starts, and a failure that ends the
//! start with the exit text and the output tail.

use fleetdeck_testkit::fake::STDIN_LOG_ENV;
use fleetdeck_testkit::sim_claude::StdinLog;

use super::*;
use crate::store::{Dispatch, EffectPayload};

/// A station whose policy allows (or refuses) setup commands, with the
/// environment of the `claude` instance.
async fn station(allow: bool) -> Station {
    station_with(allow, None).await
}

/// [`station`] whose fake provider logs its stdin to `log`.
async fn station_with(allow: bool, log: Option<&std::path::Path>) -> Station {
    let det = Det::new();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("projects/app")).unwrap();
    // Random ids: the effect id is the setup marker, and the deterministic ids
    // of another test process would give it the same marker.
    let ids: fleetdeck_core::SharedIds = Arc::new(fleetdeck_core::UlidSource::new(det.clock()));
    let store = Store::open_in_memory(det.clock(), Arc::clone(&ids), StoreConfig::default())
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
    let provider = match log {
        None => adapter(FakeAgent::Claude.locate().unwrap()),
        Some(log) => {
            let mut env = ChildEnv::from_process();
            env.set(SCENARIO_ENV, "sim").unwrap();
            env.set(STDIN_LOG_ENV, log.to_str().unwrap()).unwrap();
            let program = FakeAgent::Claude.locate().unwrap();
            let mut config = ClaudeAdapterConfig::new("claude".into(), program, env);
            config.stop = StopPolicy {
                grace: Duration::from_millis(500),
                term_grace: Duration::from_millis(300),
            };
            config.delivery_grace = Duration::from_millis(500);
            Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())))
        }
    };
    let adapters = Adapters::from([("claude".into(), provider)]);
    let manager = SessionManager::start(store, policy, adapters, ids, Arc::default())
        .await
        .unwrap();
    Station {
        det,
        manager: Arc::new(manager),
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
        batch: None,
        issue: None,
        origin_text: false,
    }
}

async fn send_as(st: &mut Station, origin: CommandOrigin, command: SessionCommand) -> RejectCode {
    st.next += 1;
    let envelope = CommandEnvelope {
        authority: None,
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

/// A stop effect without a process incarnation, as stored during a setup command.
fn unbound_stop(session: &SessionId, how: StopMode) -> crate::store::Effect {
    crate::store::Effect {
        effect_id: crate::store::EffectId::from("stop-test".to_owned()),
        command_id: None,
        session_id: session.clone(),
        process_incarnation: None,
        payload: EffectPayload::Stop {
            how,
            authority: None,
        },
        state: EffectState::Claimed,
        fence_token: None,
        reason: None,
    }
}

async fn session_of(st: &mut Station, cmd: &str) -> SessionId {
    let ws = st.workspace();
    st.accepted(start_with_setup(&ws, cmd))
        .await
        .receipt
        .result
        .unwrap()
        .session_id
        .unwrap()
}

async fn wait_ended(st: &Station, session: &SessionId) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while st.state(session).await != SessionState::Exited {
        assert!(tokio::time::Instant::now() < deadline, "not ended");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_no_live(st: &Station) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while st.manager.live_sessions() > 0 {
        assert!(tokio::time::Instant::now() < deadline, "a provider is live");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_stop_before_the_provider_spawn_spawns_nothing() {
    use crate::worker::EffectTarget;
    let log = tempfile::tempdir().unwrap();
    let log = log.path().join("stdin.log");
    let mut st = station_with(true, Some(&log)).await;
    let target = Arc::clone(&st.manager.target);
    let hook = &target.hooks().start;
    hook.arm();
    let a = session_of(&mut st, "true").await;
    // The task left its setup and waits before the provider start.
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .unwrap();
    let stop = unbound_stop(&a, StopMode::Kill);
    assert_eq!(target.dispatch(&stop).await, Dispatch::Confirmed);
    hook.release.notify_one();
    wait_ended(&st, &a).await;
    assert!(StdinLog::read(&log).pids.is_empty(), "no provider spawned");
    assert!(st.manager.store().setup_runs().await.unwrap().is_empty());
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_stop_after_the_spawn_does_not_send_the_first_prompt_and_ends_the_session() {
    use crate::worker::EffectTarget;
    let log = tempfile::tempdir().unwrap();
    let log = log.path().join("stdin.log");
    let mut st = station_with(true, Some(&log)).await;
    let target = Arc::clone(&st.manager.target);
    let hook = &target.hooks().input;
    hook.arm();
    let a = session_of(&mut st, "true").await;
    // The provider is spawned; the first prompt is not sent yet.
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .unwrap();
    let stop = unbound_stop(&a, StopMode::Graceful);
    assert_eq!(target.dispatch(&stop).await, Dispatch::Confirmed);
    hook.release.notify_one();
    wait_ended(&st, &a).await;
    wait_no_live(&st).await;
    let stdin = StdinLog::read(&log);
    assert_eq!(stdin.pids.len(), 1, "the provider was spawned");
    assert_eq!(stdin.count("hello"), 0, "{:?}", stdin.lines);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_stop_that_comes_after_the_start_stops_the_live_session() {
    use crate::worker::EffectTarget;
    let mut st = station(true).await;
    let a = session_of(&mut st, "true").await;
    st.wait_for("started", |e| {
        matches!(e, Event::SessionStarted { .. }).then_some(())
    })
    .await;
    // The task has settled and left the registry once the effect is settled.
    st.settled(&a).await;
    let stop = unbound_stop(&a, StopMode::Kill);
    let dispatch = st.manager.target.dispatch(&stop).await;
    assert!(
        matches!(dispatch, Dispatch::Sent | Dispatch::Confirmed),
        "{dispatch:?}"
    );
    wait_ended(&st, &a).await;
    wait_no_live(&st).await;
    st.manager.shutdown(StopMode::Kill).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_shutdown_during_the_provider_start_waits_and_leaves_no_provider() {
    let log = tempfile::tempdir().unwrap();
    let log = log.path().join("stdin.log");
    let mut st = station_with(true, Some(&log)).await;
    let target = Arc::clone(&st.manager.target);
    let hook = &target.hooks().input;
    hook.arm();
    let a = session_of(&mut st, "true").await;
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .unwrap();
    let release = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        hook.release.notify_one();
    };
    tokio::join!(st.manager.shutdown(StopMode::Kill), release);
    // The shutdown returned: the provider is already gone, with no polling.
    let stdin = StdinLog::read(&log);
    assert_eq!(stdin.pids.len(), 1, "the provider was spawned");
    let proc = format!("/proc/{}", stdin.pids[0]);
    assert!(!std::path::Path::new(&proc).exists(), "the provider runs");
    assert_eq!(stdin.count("hello"), 0, "{:?}", stdin.lines);
    wait_ended(&st, &a).await;
}

#[tokio::test]
async fn a_shutdown_before_the_provider_spawn_spawns_nothing() {
    let log = tempfile::tempdir().unwrap();
    let log = log.path().join("stdin.log");
    let mut st = station_with(true, Some(&log)).await;
    let target = Arc::clone(&st.manager.target);
    let hook = &target.hooks().start;
    hook.arm();
    let a = session_of(&mut st, "true").await;
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .unwrap();
    let release = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        hook.release.notify_one();
    };
    tokio::join!(st.manager.shutdown(StopMode::Kill), release);
    assert!(StdinLog::read(&log).pids.is_empty(), "no provider spawned");
    wait_ended(&st, &a).await;
}

#[tokio::test]
async fn without_a_stop_the_first_prompt_reaches_the_provider_after_the_setup() {
    let log = tempfile::tempdir().unwrap();
    let log = log.path().join("stdin.log");
    let mut st = station_with(true, Some(&log)).await;
    session_of(&mut st, "true").await;
    st.wait_for("turn", |e| {
        matches!(e, Event::TurnCompleted { .. }).then_some(())
    })
    .await;
    assert_eq!(StdinLog::read(&log).count("hello"), 1);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_stop_during_the_checkpoint_work_does_not_send_the_first_prompt() {
    use crate::worker::EffectTarget;
    let log = tempfile::tempdir().unwrap();
    let log = log.path().join("stdin.log");
    let mut st = station_with(true, Some(&log)).await;
    let target = Arc::clone(&st.manager.target);
    let hook = &target.hooks().checkpoint;
    hook.arm();
    let a = session_of(&mut st, "true").await;
    // The fence and the checkpoint work are done; the delivery is next.
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .unwrap();
    let stop = unbound_stop(&a, StopMode::Graceful);
    assert_eq!(target.dispatch(&stop).await, Dispatch::Confirmed);
    hook.release.notify_one();
    wait_ended(&st, &a).await;
    wait_no_live(&st).await;
    let stdin = StdinLog::read(&log);
    assert_eq!(stdin.pids.len(), 1, "the provider was spawned");
    assert_eq!(stdin.count("hello"), 0, "{:?}", stdin.lines);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_stop_between_the_provider_registration_and_the_task_end_stops_the_provider() {
    use crate::worker::EffectTarget;
    let log = tempfile::tempdir().unwrap();
    let log = log.path().join("stdin.log");
    let mut st = station_with(true, Some(&log)).await;
    let target = Arc::clone(&st.manager.target);
    let hook = &target.hooks().end;
    hook.arm();
    let a = session_of(&mut st, "true").await;
    // The provider is registered and the effect is settled; the task is
    // about to decide its end.
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .unwrap();
    assert_eq!(st.manager.live_sessions(), 1);
    let stop = unbound_stop(&a, StopMode::Kill);
    assert_eq!(target.dispatch(&stop).await, Dispatch::Confirmed);
    hook.release.notify_one();
    wait_ended(&st, &a).await;
    wait_no_live(&st).await;
    st.manager.shutdown(StopMode::Kill).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_restart_during_the_setup_command_lists_the_setup() {
    use crate::shell::{ShellConfig, Shells};
    use fleetdeck_proto::settings::RestartKind;
    let mut st = station(true).await;
    let a = session_of(&mut st, "sleep 30").await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while st.manager.store().setup_runs().await.unwrap().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no setup row");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    st.manager.begin_restart();
    // The claim of the worker ended with the dispatch: the admission does
    // not wait for the setup command.
    assert!(st.manager.settle_starts(Duration::from_millis(500)).await);
    let listed = st
        .manager
        .restart_would_stop(&Shells::new(ShellConfig::default()))
        .await
        .unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].kind, RestartKind::Setup);
    assert_eq!(listed[0].id, a.as_str());
    st.manager.end_restart();
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_restart_in_the_gap_after_the_setup_waits_and_then_lists_the_session() {
    use crate::shell::{ShellConfig, Shells};
    let mut st = station(true).await;
    let target = Arc::clone(&st.manager.target);
    let hook = &target.hooks().start;
    hook.arm();
    session_of(&mut st, "true").await;
    // The command ended and its row is gone; the provider is not registered.
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .unwrap();
    assert!(st.manager.store().setup_runs().await.unwrap().is_empty());
    st.manager.begin_restart();
    let shells = Shells::new(ShellConfig::default());
    assert!(
        st.manager
            .restart_would_stop(&shells)
            .await
            .unwrap()
            .is_empty(),
        "nothing is listed in the gap: only the wait protects it"
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(300),
            st.manager.settle_starts(Duration::from_secs(5))
        )
        .await
        .is_err(),
        "the admission waits for the deferred start"
    );
    hook.release.notify_one();
    assert!(st.manager.settle_starts(WAIT).await);
    let listed = st.manager.restart_would_stop(&shells).await.unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    st.manager.end_restart();
    st.manager.shutdown(StopMode::Kill).await;
}

/// The `station.restart` handler on this station, with a counting
/// `Restart` and a short settle wait.
fn settings_of(
    st: &Station,
    settle: Duration,
) -> (
    crate::settings_svc::StationSettings,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    #[derive(Debug, Default)]
    struct Counter(Arc<std::sync::atomic::AtomicUsize>);
    impl fleetdeck_core::restart::Restart for Counter {
        fn request(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let dir = st.root.path();
    let settings = crate::settings_svc::StationSettings::new(
        &dir.join("cfg/station.toml"),
        &dir.join("data/secrets"),
        fleetdeck_core::paths::Env::default(),
        Some(Arc::new(Counter(Arc::clone(&count)))),
        Arc::new(crate::workspace::PrivatePaths::default()),
    )
    .with_settle(settle);
    (settings, count)
}

/// Holds a setup start after its command ended, starts the restart
/// admission, and holds the admission in its settle wait until the task
/// waits for the handoff lock. Returns the admission, the submit guard that
/// holds the settle wait, and the session.
async fn setup_ends_during_admission(
    st: &mut Station,
    settings: Arc<crate::settings_svc::StationSettings>,
    stop_direct_sessions: bool,
) -> (
    tokio::task::JoinHandle<fleetdeck_proto::link::Outcome>,
    crate::manager::restart_gate::InFlight,
    SessionId,
) {
    use crate::shell::{ShellConfig, Shells};
    let target = Arc::clone(&st.manager.target);
    let hook = &target.hooks().handoff;
    hook.arm();
    let a = session_of(st, "true").await;
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .unwrap();
    // The command ended; its row stays until the handoff.
    assert_eq!(st.manager.store().setup_runs().await.unwrap().len(), 1);
    let held = st.manager.restart_state().enter_submit();
    let manager = Arc::clone(&st.manager);
    let admission = tokio::spawn(async move {
        let shells = Shells::new(ShellConfig::default());
        settings
            .restart(
                &manager,
                &shells,
                serde_json::json!({
                    "stop_direct_sessions": stop_direct_sessions,
                    "elevated": true
                }),
            )
            .await
    });
    let deadline = tokio::time::Instant::now() + WAIT;
    while !st.manager.restart_state().restarting() {
        assert!(tokio::time::Instant::now() < deadline, "no admission");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The task now reaches the handoff while the admission holds the lock.
    hook.release.notify_one();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        st.manager.store().setup_runs().await.unwrap().len(),
        1,
        "the row stays while the admission holds the lock"
    );
    (admission, held, a)
}

fn answer_of(outcome: fleetdeck_proto::link::Outcome) -> fleetdeck_proto::settings::RestartAnswer {
    match outcome {
        fleetdeck_proto::link::Outcome::Ok(v) => serde_json::from_value(v).unwrap(),
        other => panic!("{other:?}"),
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_setup_that_ends_during_the_admission_is_listed_and_starts_after_a_refusal() {
    use fleetdeck_proto::settings::RestartKind;
    let mut st = station(true).await;
    let (settings, count) = settings_of(&st, Duration::from_secs(5));
    let (admission, held, a) =
        setup_ends_during_admission(&mut st, Arc::new(settings), false).await;
    drop(held);
    let answer = answer_of(
        tokio::time::timeout(WAIT, admission)
            .await
            .unwrap()
            .unwrap(),
    );
    assert!(!answer.restarting);
    assert_eq!(answer.would_stop.len(), 1, "{:?}", answer.would_stop);
    assert_eq!(answer.would_stop[0].kind, RestartKind::Setup);
    assert_eq!(answer.would_stop[0].id, a.as_str());
    // The refusal cleared the freeze: the task takes its claim and starts.
    let deadline = tokio::time::Instant::now() + WAIT;
    while st.manager.live_sessions() == 0 {
        assert!(tokio::time::Instant::now() < deadline, "no provider start");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(st.manager.store().setup_runs().await.unwrap().is_empty());
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
    st.manager.shutdown(StopMode::Kill).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_setup_that_ends_during_a_committed_admission_ends_failed_and_never_starts() {
    let mut st = station(true).await;
    let (settings, _count) = settings_of(&st, Duration::from_secs(5));
    let (admission, held, a) = setup_ends_during_admission(&mut st, Arc::new(settings), true).await;
    drop(held);
    let answer = answer_of(
        tokio::time::timeout(WAIT, admission)
            .await
            .unwrap()
            .unwrap(),
    );
    assert!(answer.restarting);
    wait_ended(&st, &a).await;
    assert_eq!(st.manager.live_sessions(), 0, "no provider was started");
    assert!(st.manager.store().setup_runs().await.unwrap().is_empty());
    let reasons: Vec<String> = st
        .events()
        .await
        .into_iter()
        .filter_map(|e| match e.event {
            EventBody::Known(Event::Error { code, message, .. }) if code == "start_failed" => {
                Some(message)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        reasons,
        ["the station restarted before this session started"]
    );
    let effects = st.manager.store().session_effects(a.clone()).await.unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].state, EffectState::RetiredNotDispatched);
    assert_eq!(effects[0].reason.as_deref(), Some("station_restart"));
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn ending_a_claimed_start_for_a_restart_is_a_no_op_unless_it_is_still_claimed() {
    let mut st = station(false).await;
    let cwd = st.workspace();
    let d = st
        .accepted(SessionCommand::Start {
            spec: spec(&cwd, "claude"),
            prompt: None,
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
        })
        .await;
    let session = d.receipt.result.unwrap().session_id.unwrap();
    let store = st.manager.store().clone();
    let deadline = tokio::time::Instant::now() + WAIT;
    let effect = loop {
        let found = store.session_effects(session.clone()).await.unwrap();
        if let Some(e) = found
            .into_iter()
            .find(|e| e.state == EffectState::Confirmed)
        {
            break e;
        }
        assert!(tokio::time::Instant::now() < deadline, "start not settled");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let fence = effect.fence_token.unwrap();
    let before = st.events().await.len();
    // A settled effect is left alone: nothing is stored.
    let done = store
        .fail_claimed_start(effect.effect_id.clone(), fence, "x".to_owned())
        .await
        .unwrap();
    assert!(!done);
    // An unknown effect too.
    let done = store
        .fail_claimed_start("no-such-effect".into(), fence, "x".to_owned())
        .await
        .unwrap();
    assert!(!done);
    assert_eq!(st.events().await.len(), before);
    assert_eq!(
        store.effect(effect.effect_id).await.unwrap().unwrap().state,
        EffectState::Confirmed
    );
    st.manager.shutdown(StopMode::Kill).await;
}
