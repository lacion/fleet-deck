//! The update hold, the provider refresh and the durable quarantine marks
//! (B28 design §3.7.1, §3.8, §3.8.1).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_proto::cli_update::{InstallMethod, UpdateOutcome, UpdateTrigger};
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, StopMode, UserInput,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::{Outcome, VersionStatus};
use fleetdeck_proto::{
    Capabilities, CommandEnvelope, CommandId, Event, EventBody, ProviderInstanceId,
};
use fleetdeck_providers::adapter::{AuthState, NativeSessionId, ProviderProbe};
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::host::DirectChildHost;
use fleetdeck_providers::{EventSink, LiveSession, ProviderAdapter, ProviderError};
use fleetdeck_testkit::fake::SCENARIO_ENV;
use fleetdeck_testkit::{Det, FakeAgent};

use crate::manager::{Adapters, Policy, SessionManager};
use crate::store::cli_update_journal::{JournalAction, NewJournal, QuarantineAction};
use crate::store::cli_updates::{AttemptEnd, NewAttempt};
use crate::store::{Caller, Dispatch, OfflineApprovalPolicy, ReadResult, Store, StoreConfig};
use crate::worker::EffectTarget;

const WAIT: Duration = Duration::from_secs(10);
const DENY_TEXT: &str = "this claude version is known to break the protocol";

/// An adapter with a settable probe version that counts probes and starts.
struct Counting {
    kind: ProviderKind,
    version: Mutex<Option<&'static str>>,
    probes: AtomicUsize,
    starts: AtomicUsize,
}

impl Counting {
    fn new(kind: ProviderKind, version: &'static str) -> Arc<Self> {
        Arc::new(Self {
            kind,
            version: Mutex::new(Some(version)),
            probes: AtomicUsize::new(0),
            starts: AtomicUsize::new(0),
        })
    }

    fn set_version(&self, version: &'static str) {
        *self.version.lock().unwrap() = Some(version);
    }
}

#[async_trait]
impl ProviderAdapter for Counting {
    fn kind(&self) -> ProviderKind {
        self.kind
    }
    fn capabilities(&self) -> Capabilities {
        fleetdeck_providers::claude::capabilities(false)
    }
    async fn probe(&self) -> fleetdeck_providers::adapter::Result<ProviderProbe> {
        self.probes.fetch_add(1, Ordering::SeqCst);
        Ok(ProviderProbe {
            kind: self.kind,
            version: self.version.lock().unwrap().map(str::to_owned),
            auth: AuthState::Unknown,
        })
    }
    async fn start(
        &self,
        _: SessionSpec,
        _: EventSink,
    ) -> fleetdeck_providers::adapter::Result<Box<dyn LiveSession>> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::SessionEnded)
    }
    async fn resume(
        &self,
        _: &NativeSessionId,
        _: SessionSpec,
        _: EventSink,
    ) -> fleetdeck_providers::adapter::Result<Box<dyn LiveSession>> {
        Err(ProviderError::SessionEnded)
    }
}

/// A station with a Claude instance `claude` and a Codex instance `codex`.
struct Rig {
    manager: Arc<SessionManager>,
    det: Det,
    root: tempfile::TempDir,
    cwd: String,
    claude: Arc<Counting>,
    codex: Arc<Counting>,
}

fn policy(root: &std::path::Path) -> Policy {
    Policy::new(
        vec![root.join("projects")],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([
            ("claude".into(), ProviderKind::Claude),
            ("codex".into(), ProviderKind::Codex),
        ]),
    )
}

async fn rig_with(claude_version: &'static str, store: Option<Store>) -> Rig {
    let det = Det::new();
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("projects/app");
    std::fs::create_dir_all(&cwd).unwrap();
    let store = match store {
        Some(s) => s,
        None => Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap(),
    };
    let claude = Counting::new(ProviderKind::Claude, claude_version);
    let codex = Counting::new(ProviderKind::Codex, "0.50.0");
    let adapters = Adapters::from([
        (
            "claude".into(),
            Arc::clone(&claude) as Arc<dyn ProviderAdapter>,
        ),
        (
            "codex".into(),
            Arc::clone(&codex) as Arc<dyn ProviderAdapter>,
        ),
    ]);
    let manager = SessionManager::start(
        store,
        policy(root.path()),
        adapters,
        det.ids(),
        Arc::new(crate::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    Rig {
        manager: Arc::new(manager),
        det,
        root,
        cwd: cwd.to_string_lossy().into_owned(),
        claude,
        codex,
    }
}

async fn rig() -> Rig {
    rig_with(fleetdeck_providers::claude::TESTED_MIN_VERSION, None).await
}

fn start_envelope(det: &Det, cwd: &str, id: &str) -> CommandEnvelope {
    CommandEnvelope {
        authority: None,
        command_id: CommandId::from(id.to_owned()),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Local,
        command: SessionCommand::Start {
            spec: SessionSpec {
                effort: None,
                provider_instance: "claude".into(),
                cwd: cwd.to_owned(),
                mode: Some("default".to_owned()),
                model: None,
                title: None,
                coordinator: None,
                launch: None,
                extra_env: None,
            },
            prompt: Some(UserInput::new("hello".to_owned())),
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
            fleet_git: None,
        },
    }
}

async fn submit_start(manager: &SessionManager, det: &Det, cwd: &str, id: &str) {
    let decided = manager
        .submit(start_envelope(det, cwd, id), Caller::local())
        .await
        .unwrap();
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
}

/// The message of the first `start_failed` error, once it exists.
async fn start_failed_message(manager: &SessionManager) -> String {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let ReadResult::Events { events, .. } =
            manager.store().read_after(0, 100_000).await.unwrap()
        else {
            panic!("snapshot required");
        };
        let found = events.iter().find_map(|e| match &e.event {
            EventBody::Known(Event::Error { code, message, .. }) if code == "start_failed" => {
                Some(message.clone())
            }
            _ => None,
        });
        if let Some(message) = found {
            return message;
        }
        assert!(tokio::time::Instant::now() < deadline, "no start_failed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !check() {
        assert!(tokio::time::Instant::now() < deadline, "timeout: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn claude_id() -> ProviderInstanceId {
    "claude".into()
}

#[tokio::test]
async fn begin_update_fails_while_restarting() {
    let rig = rig().await;
    rig.manager.begin_restart();
    let refused = rig
        .manager
        .begin_provider_update(ProviderKind::Claude)
        .await;
    assert_eq!(refused.err(), Some("station_restarting"));
    assert_eq!(rig.manager.restart_state().updating(), None);
    rig.manager.end_restart();
    let hold = rig
        .manager
        .begin_provider_update(ProviderKind::Claude)
        .await
        .unwrap();
    assert_eq!(
        rig.manager.restart_state().updating(),
        Some(ProviderKind::Claude)
    );
    let second = rig.manager.begin_provider_update(ProviderKind::Codex).await;
    assert_eq!(second.err(), Some("another_update"));
    drop(hold);
    assert_eq!(rig.manager.restart_state().updating(), None);
}

#[tokio::test]
async fn a_restart_admission_after_begin_update_is_refused_and_clears_restarting() {
    #[derive(Debug, Default)]
    struct Counter(Arc<AtomicUsize>);
    impl fleetdeck_core::restart::Restart for Counter {
        fn request(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let rig = rig().await;
    let requests = Arc::new(AtomicUsize::new(0));
    let dir = rig.root.path();
    let settings = crate::settings_svc::StationSettings::new(
        &dir.join("cfg/station.toml"),
        &dir.join("data/secrets"),
        fleetdeck_core::paths::Env::default(),
        Some(Arc::new(Counter(Arc::clone(&requests)))),
        Arc::new(crate::workspace::PrivatePaths::default()),
    );
    let hold = rig
        .manager
        .begin_provider_update(ProviderKind::Codex)
        .await
        .unwrap();
    let shells = crate::shell::Shells::new(crate::shell::ShellConfig::default());
    let outcome = settings
        .restart(
            &rig.manager,
            &shells,
            serde_json::json!({ "stop_direct_sessions": true, "elevated": true }),
        )
        .await;
    let Outcome::Err(e) = outcome else {
        panic!("the admission must be refused");
    };
    assert_eq!(e.code, "invalid_state");
    assert_eq!(
        e.message,
        "a provider update runs on this station; restart in a few minutes"
    );
    assert!(!rig.manager.restart_state().restarting());
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    drop(hold);
}

#[tokio::test]
async fn a_queued_start_is_held_during_an_update_and_runs_after_the_hold_drops() {
    let rig = rig().await;
    let hold = rig
        .manager
        .begin_provider_update(ProviderKind::Codex)
        .await
        .unwrap();
    // Every provider waits, not only the updating kind.
    submit_start(&rig.manager, &rig.det, &rig.cwd, "cmd-1").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(rig.claude.starts.load(Ordering::SeqCst), 0);
    assert_eq!(
        rig.manager
            .store()
            .queued_start_effects()
            .await
            .unwrap()
            .len(),
        1
    );
    drop(hold);
    wait_for("the start reaches the adapter", || {
        rig.claude.starts.load(Ordering::SeqCst) == 1
    })
    .await;
}

#[tokio::test]
async fn live_sessions_of_lists_only_the_kind() {
    let rig = rig().await;
    // A real Claude session under the fake agent.
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, "sim").unwrap();
    let config =
        ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
    let real: Arc<dyn ProviderAdapter> =
        Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())));
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let manager = SessionManager::start(
        store,
        policy(rig.root.path()),
        Adapters::from([
            ("claude".into(), real),
            (
                "codex".into(),
                Arc::clone(&rig.codex) as Arc<dyn ProviderAdapter>,
            ),
        ]),
        det.ids(),
        Arc::new(crate::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    assert!(
        manager
            .live_sessions_of(ProviderKind::Claude)
            .await
            .is_empty()
    );
    submit_start(&manager, &det, &rig.cwd, "cmd-1").await;
    let deadline = tokio::time::Instant::now() + WAIT;
    'poll: loop {
        for (id, _) in manager.target.live_pids() {
            let summary = manager.store().summary(id).await.unwrap();
            if summary.is_some_and(|s| s.provider.is_some()) {
                break 'poll;
            }
        }
        assert!(tokio::time::Instant::now() < deadline, "no live session");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let claude = manager.live_sessions_of(ProviderKind::Claude).await;
    assert_eq!(claude.len(), 1);
    assert!(
        manager
            .live_sessions_of(ProviderKind::Codex)
            .await
            .is_empty()
    );
    manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_deferred_start_of_the_updating_kind_fails_with_provider_updating() {
    let rig = rig().await;
    let hold = rig
        .manager
        .begin_provider_update(ProviderKind::Claude)
        .await
        .unwrap();
    assert_eq!(
        rig.manager.target.updating_reason(&claude_id()).as_deref(),
        Some("the station was updating claude; start the session again")
    );
    assert_eq!(rig.manager.target.updating_reason(&"codex".into()), None);
    // The worker does not claim the start; a deferred task dispatches it.
    submit_start(&rig.manager, &rig.det, &rig.cwd, "cmd-1").await;
    let queued = rig.manager.store().queued_start_effects().await.unwrap();
    assert_eq!(queued.len(), 1);
    let dispatch = rig.manager.target.dispatch(&queued[0]).await;
    assert_eq!(
        dispatch,
        Dispatch::NotDispatched("provider_updating".into())
    );
    assert_eq!(rig.claude.starts.load(Ordering::SeqCst), 0);
    assert_eq!(
        start_failed_message(&rig.manager).await,
        "the station was updating claude; start the session again"
    );
    drop(hold);
}

#[tokio::test]
async fn begin_provider_update_reports_starts_in_flight_after_the_settle_bound() {
    let rig = rig().await;
    let submit = rig.manager.restart_state().enter_submit();
    let started = std::time::Instant::now();
    let refused = rig
        .manager
        .begin_provider_update_within(ProviderKind::Claude, Duration::from_millis(50))
        .await;
    assert_eq!(refused.err(), Some("starts_in_flight"));
    assert!(started.elapsed() < Duration::from_secs(5));
    // Nothing stays held after the error.
    assert_eq!(rig.manager.restart_state().updating(), None);
    drop(submit);
    let hold = rig
        .manager
        .begin_provider_update_within(ProviderKind::Claude, Duration::from_millis(50))
        .await;
    assert!(hold.is_ok());
}

async fn write_quarantine(store: &Store, text: &str) {
    store
        .cli_update_begin(
            NewAttempt {
                attempt_id: "a1".to_owned(),
                provider: ProviderKind::Claude,
                trigger: UpdateTrigger::Owner,
                method: InstallMethod::Native,
                channel: "stable".to_owned(),
                from_version: Some("2.1.309".to_owned()),
                target_version: Some("2.1.310".to_owned()),
            },
            NewJournal {
                provider: ProviderKind::Claude,
                method: InstallMethod::Native,
                from_version: "2.1.309".to_owned(),
                target_version: "2.1.310".to_owned(),
                launcher: "/home/o/.local/bin/claude".into(),
                dest: "/home/o/.local/bin/claude".into(),
                stage_root: "/home/o/.local/share/claude/versions/.fd-stage-a1".into(),
                rollback_sha256: None,
            },
        )
        .await
        .unwrap();
    store
        .cli_update_finish(
            "a1".to_owned(),
            AttemptEnd {
                outcome: UpdateOutcome::RollbackFailed,
                reason: Some("rollback_failed".to_owned()),
                to_version: None,
                target_version: None,
            },
            JournalAction::Delete,
            QuarantineAction::Set {
                reason: "rollback_failed",
                text: text.to_owned(),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_quarantine_row_refuses_starts_after_a_restart() {
    let text = "claude failed after an update and the way back failed. Reinstall it on this station: claude install 2.1.309";
    let first = rig().await;
    let store = first.manager.store().clone();
    // A start that waits in the queue when the first station process ends.
    let hold = first
        .manager
        .begin_provider_update(ProviderKind::Claude)
        .await
        .unwrap();
    submit_start(&first.manager, &first.det, &first.cwd, "cmd-1").await;
    first.manager.shutdown(StopMode::Kill).await;
    drop(hold);
    write_quarantine(&store, text).await;
    let second = rig_with(fleetdeck_providers::claude::TESTED_MIN_VERSION, Some(store)).await;
    // The restart retires the queued start without a dispatch (the effect
    // recovery); a new start is refused with the quarantine text (§3.8.1).
    submit_start(&second.manager, &second.det, &second.cwd, "cmd-2").await;
    assert_eq!(start_failed_message(&second.manager).await, text);
    assert_eq!(second.claude.starts.load(Ordering::SeqCst), 0);
    assert_eq!(first.claude.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refresh_providers_replaces_the_cache_and_clears_a_deny_mark() {
    let rig = rig_with("2.1.300", None).await;
    let before = rig.manager.providers().await;
    // `KNOWN_BROKEN` is empty today: the test sets the deny mark itself.
    rig.manager
        .target
        .mark_broken(&claude_id(), ProviderKind::Claude);
    assert_eq!(
        rig.manager.target.broken_reason(&claude_id()).as_deref(),
        Some(DENY_TEXT)
    );
    rig.claude
        .set_version(fleetdeck_providers::claude::TESTED_MIN_VERSION);
    // The cache still holds the old report until the refresh.
    assert_eq!(rig.manager.providers().await, before);
    let probes = rig.claude.probes.load(Ordering::SeqCst);
    let fresh = rig.manager.refresh_providers().await;
    assert_eq!(rig.claude.probes.load(Ordering::SeqCst), probes + 1);
    let claude = fresh
        .iter()
        .find(|p| p.instance.as_str() == "claude")
        .unwrap();
    assert_eq!(
        claude.version.as_deref(),
        Some(fleetdeck_providers::claude::TESTED_MIN_VERSION)
    );
    assert_eq!(claude.version_status, VersionStatus::Ok);
    assert_ne!(fresh, before);
    assert_eq!(rig.manager.providers().await, fresh);
    assert_eq!(rig.claude.probes.load(Ordering::SeqCst), probes + 1);
    assert_eq!(rig.manager.target.broken_reason(&claude_id()), None);
}

#[tokio::test]
async fn refresh_providers_does_not_clear_a_quarantine() {
    let rig = rig().await;
    let _ = rig.manager.providers().await;
    rig.manager
        .target
        .quarantine(ProviderKind::Claude, "quarantined text");
    let _ = rig.manager.refresh_providers().await;
    assert_eq!(
        rig.manager.target.broken_reason(&claude_id()).as_deref(),
        Some("quarantined text")
    );
    // A new result does not clear it (a deny mark never replaces it either:
    // `set_denied` keeps a `Quarantined` mark).
    rig.claude.set_version("2.1.300");
    let _ = rig.manager.refresh_providers().await;
    assert_eq!(
        rig.manager.target.broken_reason(&claude_id()).as_deref(),
        Some("quarantined text")
    );
    // The codex instance is of another kind and stays free.
    assert_eq!(rig.manager.target.broken_reason(&"codex".into()), None);
}

#[tokio::test]
async fn a_deny_mark_does_not_replace_a_quarantine() {
    let rig = rig().await;
    rig.manager.target.quarantine(ProviderKind::Claude, "q");
    rig.manager
        .target
        .mark_broken(&claude_id(), ProviderKind::Claude);
    assert_eq!(
        rig.manager.target.broken_reason(&claude_id()).as_deref(),
        Some("q")
    );
}

#[tokio::test]
async fn release_quarantine_keeps_a_deny_mark() {
    let rig = rig().await;
    let _ = rig.manager.providers().await;
    // `KNOWN_BROKEN` is empty today: the test sets the deny mark itself.
    rig.manager
        .target
        .mark_broken(&claude_id(), ProviderKind::Claude);
    // No quarantine: the deny mark stays.
    rig.manager.target.release_quarantine(ProviderKind::Claude);
    assert_eq!(
        rig.manager.target.broken_reason(&claude_id()).as_deref(),
        Some(DENY_TEXT)
    );
    // A quarantine replaces the deny mark; its release leaves no mark, and
    // the next refresh sets only the mark that its result calls for (none
    // for a version that is not on the deny list).
    rig.manager.target.quarantine(ProviderKind::Claude, "q");
    assert_eq!(
        rig.manager.target.broken_reason(&claude_id()).as_deref(),
        Some("q")
    );
    rig.manager.target.release_quarantine(ProviderKind::Claude);
    assert_eq!(rig.manager.target.broken_reason(&claude_id()), None);
    let _ = rig.manager.refresh_providers().await;
    assert_eq!(rig.manager.target.broken_reason(&claude_id()), None);
}

#[tokio::test]
async fn refresh_providers_replaces_the_handoff_versions() {
    let rig = rig_with("2.1.300", None).await;
    let _ = rig.manager.providers().await;
    assert_eq!(
        rig.manager.coordinator.versions(ProviderKind::Claude),
        ["2.1.300"]
    );
    rig.claude
        .set_version(fleetdeck_providers::claude::TESTED_MIN_VERSION);
    let _ = rig.manager.refresh_providers().await;
    assert_eq!(
        rig.manager.coordinator.versions(ProviderKind::Claude),
        [fleetdeck_providers::claude::TESTED_MIN_VERSION]
    );
    assert_eq!(
        rig.manager.coordinator.versions(ProviderKind::Codex),
        ["0.50.0"]
    );
}
