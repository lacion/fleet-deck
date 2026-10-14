//! The live policy (DL-172 §1.4): a publish changes what the next decision,
//! dispatch, tick or call reads, and a failed prepare changes nothing.

use fleetdeck_core::config::TrackerConfig;
use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::integrations::TrackerKind;

use super::*;
use crate::coordinator::Bindings;
use crate::manager::policy_hub::test_config;
use crate::store::Dispatch;
use crate::worker::EffectTarget;

fn start_command(cwd: &str) -> SessionCommand {
    SessionCommand::Start {
        spec: spec(cwd, "claude"),
        prompt: None,
        workspace: None,
        setup_cmd: None,
        batch: None,
        issue: None,
        origin_text: false,
    }
}

fn roots(st: &Station) -> PathBuf {
    st.root.path().join("projects")
}

fn linear(id: &str) -> TrackerConfig {
    TrackerConfig {
        id: id.to_owned(),
        kind: TrackerKind::Linear,
        site: None,
        email: None,
        token: fleetdeck_core::config::TokenSource::Env("LIN_TOKEN".to_owned()),
    }
}

#[tokio::test]
async fn a_publish_changes_max_sessions_and_the_version_counts_one_per_publish() {
    let mut st = Station::new(1).await;
    let ws = st.workspace();
    let hub = Arc::clone(st.manager.policy_hub());
    assert_eq!(hub.version(), 1);
    st.start(&ws, None).await;
    assert_eq!(st.rejected(start_command(&ws)).await, RejectCode::Forbidden);

    let prepared = st
        .manager
        .prepare(&test_config(&roots(&st), "max_sessions = 3\n"))
        .unwrap();
    // The first step publishes nothing.
    assert_eq!((hub.version(), st.manager.policy().max_sessions()), (1, 1));
    st.manager.publish(prepared);
    assert_eq!((hub.version(), st.manager.policy().max_sessions()), (2, 3));
    st.accepted(start_command(&ws)).await;

    let prepared = st
        .manager
        .prepare(&test_config(&roots(&st), "max_sessions = 4\n"))
        .unwrap();
    st.manager.publish(prepared);
    assert_eq!((hub.version(), st.manager.policy().max_sessions()), (3, 4));
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_restart_only_fields_keep_their_running_values() {
    let st = Station::new(2).await;
    let extra = "listen = [\"127.0.0.1:1\"]\nhost = \"shim\"\nshim_buffer_mib = 3\n\
                 [[provider_instance]]\nid = \"other\"\ndriver = \"claude\"\nbinary = \"/x\"\n";
    let config = test_config(&roots(&st), extra);
    let (integrations, bindings) = (st.manager.integrations(), st.manager.coordinator_bindings());
    let before = bindings.clone();
    st.manager.publish(st.manager.prepare(&config).unwrap());
    let policy = st.manager.policy();
    assert_eq!(
        policy.instance(&"claude".into()),
        Some(ProviderKind::Claude)
    );
    assert_eq!(policy.instance(&"codex".into()), Some(ProviderKind::Codex));
    assert_eq!(policy.instance(&"other".into()), None);
    // The runtime holders are the same ones: the trackers did not change.
    assert!(Arc::ptr_eq(&integrations, &st.manager.integrations()));
    bindings.set_endpoint("http://127.0.0.1:1/mcp".to_owned());
    assert_eq!(
        before.endpoint(),
        st.manager.coordinator_bindings().endpoint()
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_bad_tracker_returns_its_key_and_changes_nothing() {
    let st = Station::new(2).await;
    let mut config = test_config(&roots(&st), "max_sessions = 3\n");
    config.trackers = vec![linear("a"), linear("a")];
    let (key, message) = st.manager.prepare(&config).unwrap_err();
    assert_eq!(key, "trackers[1].id");
    assert!(!message.contains("LIN_TOKEN"), "{message}");
    // A Jira tracker without its site.
    let mut jira = linear("j");
    jira.kind = TrackerKind::Jira;
    config.trackers = vec![jira];
    assert_eq!(
        st.manager.prepare(&config).unwrap_err().0,
        "trackers[0].site"
    );
    // An id that the parser would refuse.
    config.trackers = vec![linear("Bad Id")];
    assert_eq!(st.manager.prepare(&config).unwrap_err().0, "trackers[0].id");
    assert_eq!(st.manager.policy_hub().version(), 1);
    assert_eq!(st.manager.policy().max_sessions(), 2);
    assert!(st.manager.integrations().trackers().is_empty());
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_publish_gives_the_integration_service_the_new_tracker() {
    let st = Station::new(2).await;
    let before = st.manager.integrations();
    assert!(before.trackers().is_empty());
    let extra = "[[trackers]]\nid = \"lin\"\nkind = \"linear\"\ntoken_env = \"LIN_TOKEN\"\n";
    let prepared = st
        .manager
        .prepare(&test_config(&roots(&st), extra))
        .unwrap();
    // Until the publish the service is the old one.
    assert!(st.manager.integrations().trackers().is_empty());
    st.manager.publish(prepared);
    let after = st.manager.integrations();
    assert!(!Arc::ptr_eq(&before, &after));
    let ids: Vec<_> = after.trackers().iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["lin"]);
    assert_eq!(st.manager.policy().trackers().len(), 1);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_hibernation_ticker_picks_up_a_new_interval_without_a_restart() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    st.wait_for("turn done", |e| {
        matches!(e, Event::TurnCompleted { .. }).then_some(())
    })
    .await;
    st.wait_for("run completed", |e| {
        matches!(e, Event::RunCompleted { .. }).then_some(())
    })
    .await;
    let (_stop, rx) = watch::channel(false);
    let ticker = tokio::spawn(hibernate::ticker(
        st.manager.store().clone(),
        Arc::clone(st.manager.policy_hub()),
        st.manager.coordinator_bindings().clone(),
        Duration::from_millis(20),
        rx,
    ));
    // Hibernation is off: the ticks stop nothing, however old the session.
    st.det.advance(Duration::from_secs(24 * 60 * 60));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(st.state(&s).await, SessionState::Running);

    let prepared = st
        .manager
        .prepare(&test_config(&roots(&st), "hibernate_after_min = 5\n"))
        .unwrap();
    st.manager.publish(prepared);
    st.wait_for("hibernated", |e| {
        matches!(e, Event::SessionHibernated { after_min } if *after_min == 5).then_some(())
    })
    .await;
    ticker.abort();
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_queued_start_is_refused_at_dispatch_when_the_roots_narrowed() {
    let det = Det::new();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("projects/app")).unwrap();
    std::fs::create_dir_all(root.path().join("elsewhere")).unwrap();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    let policy = Policy::new(
        vec![root.path().join("projects")],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
    );
    let hub = Arc::new(PolicyHub::new(policy.clone()));
    let target = SessionTarget::new(store.clone(), Adapters::new(), det.ids())
        .with_policy(Arc::clone(&hub))
        .with_private(Arc::new(crate::workspace::PrivatePaths::default()))
        .shared();
    let cwd = root
        .path()
        .join("projects/app")
        .to_string_lossy()
        .into_owned();
    let command = start_command(&cwd);
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from("cmd-1".to_owned()),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Local,
        command: command.clone(),
    };
    let decided = store
        .decide(envelope, Caller::local(), move |d| {
            decide::decide(
                d,
                &policy,
                &Bindings::default(),
                CommandOrigin::Local,
                command,
            )
        })
        .await
        .unwrap();
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    let session = decided.receipt.result.unwrap().session_id.unwrap();
    let effect = decided
        .effects
        .into_iter()
        .find(|e| e.payload.kind() == "start")
        .unwrap();

    // The owner narrows the roots after the commit and before the dispatch.
    let config = test_config(&root.path().join("elsewhere"), "");
    hub.swap(hub.current().reconfigured(&config));
    let dispatched = target.dispatch(&effect).await;
    assert!(
        matches!(&dispatched, Dispatch::NotDispatched(code) if code == "policy_changed"),
        "{dispatched:?}"
    );
    let reasons: Vec<String> = match store.read_after(0, 1000).await.unwrap() {
        ReadResult::Events { events, .. } => events
            .into_iter()
            .filter(|e| e.session_id == session)
            .filter_map(|e| match e.event {
                EventBody::Known(Event::Error { code, message, .. }) if code == "start_failed" => {
                    Some(message)
                }
                _ => None,
            })
            .collect(),
        ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
    };
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].contains("allowed roots"), "{reasons:?}");
}

#[test]
fn a_snapshot_refuses_a_start_outside_its_roots_or_with_a_setup_command_that_is_off() {
    let root = tempfile::tempdir().unwrap();
    let inside = root.path().join("projects");
    std::fs::create_dir_all(inside.join("app")).unwrap();
    let make = |allow_setup: bool| {
        Policy::new(
            vec![inside.clone()],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::new(),
        )
        .with_allow_setup_cmd(allow_setup)
    };
    let app = inside.join("app").to_string_lossy().into_owned();
    let out = root.path().to_string_lossy().into_owned();
    assert_eq!(make(true).start_refusal(&app, false, true), None);
    assert!(make(true).start_refusal(&out, false, false).is_some());
    assert!(make(false).start_refusal(&app, false, true).is_some());
    assert_eq!(make(false).start_refusal(&app, false, false), None);
    // A folder that the dispatch makes: its nearest existing parent decides.
    let new = inside.join("new/clone").to_string_lossy().into_owned();
    assert_eq!(make(true).start_refusal(&new, true, false), None);
    let new_out = root
        .path()
        .join("else/clone")
        .to_string_lossy()
        .into_owned();
    assert!(make(true).start_refusal(&new_out, true, false).is_some());
}
