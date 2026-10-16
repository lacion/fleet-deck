//! The revert of a Claude session under a shim (PLAN §6.6; ADR-005;
//! DL-087): the Claude stream-json protocol has no conversation rollback,
//! so the capability set says `rollback: false`. The same `decide_revert`
//! as with the direct host stores an `Unsupported` rejection before any
//! file change. The session manager runs with the shim host, the fake
//! Claude CLI and a Git workspace.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, StopMode, UserInput,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{
    CommandEnvelope, CommandId, Event, EventBody, RejectCode, SessionId, TurnId,
};
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::host::shim::host::{ShimHost, ShimHostConfig};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_station::manager::target::ShimSettings;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::store::{
    Caller, Decided, EffectPayload, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};
use fleetdeck_testkit::Det;
use fleetdeck_testkit::fake::{FakeAgent, SCENARIO_ENV, STDIN_LOG_ENV};

mod shim_reaper;

const WAIT: Duration = Duration::from_secs(20);

fn git_output(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

struct Rig {
    tmp: tempfile::TempDir,
    det: Det,
    next: u32,
}

impl Drop for Rig {
    fn drop(&mut self) {
        shim_reaper::reap(&self.shims_dir());
    }
}

struct Station {
    store: Store,
    manager: SessionManager,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("projects/app")).unwrap();
        std::fs::create_dir(tmp.path().join("station")).unwrap();
        let ws = tmp.path().join("projects/app");
        git_output(&ws, &["init", "-q", "-b", "main"]);
        std::fs::write(ws.join("a.txt"), "one\n").unwrap();
        git_output(&ws, &["add", "."]);
        git_output(&ws, &["commit", "-q", "-m", "init"]);
        Self {
            tmp,
            det: Det::new(),
            next: 0,
        }
    }

    fn workspace(&self) -> PathBuf {
        std::fs::canonicalize(self.tmp.path().join("projects/app")).unwrap()
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.workspace().join("a.txt")).unwrap()
    }

    /// The refs and the head commit of the workspace repository.
    fn refs(&self) -> (String, String) {
        let ws = self.workspace();
        (
            git_output(&ws, &["for-each-ref", "--format=%(refname) %(objectname)"]),
            git_output(&ws, &["rev-parse", "HEAD"]),
        )
    }

    fn shims_dir(&self) -> PathBuf {
        self.tmp.path().join("station").join("shims")
    }

    fn log(&self) -> PathBuf {
        self.tmp.path().join("claude-stdin.log")
    }

    fn adapters(&self) -> Adapters {
        let mut env = ChildEnv::from_process();
        env.set(SCENARIO_ENV, "sim").unwrap();
        env.set(STDIN_LOG_ENV, self.log()).unwrap();
        let mut config =
            ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
        config.stop = StopPolicy {
            grace: Duration::from_millis(500),
            term_grace: Duration::from_millis(300),
        };
        Adapters::from([(
            "claude".into(),
            Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())))
                as Arc<dyn fleetdeck_providers::ProviderAdapter>,
        )])
    }

    async fn station(&self) -> Station {
        let store = Store::open(
            &self.tmp.path().join("station.db"),
            self.det.clock(),
            self.det.ids(),
            StoreConfig::default(),
        )
        .await
        .unwrap();
        let policy = Policy::new(
            vec![self.tmp.path().join("projects")],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
        );
        let mut config = ShimHostConfig::new(
            PathBuf::from(env!("CARGO_BIN_EXE_fleetdeck")),
            self.tmp.path().join("station"),
            store.incarnation().as_str().to_owned(),
        );
        config.runtime_dir = None;
        config.tmp_dir = self.tmp.path().to_owned();
        config.buffer_bytes = 1024 * 1024;
        let shims = ShimSettings {
            host: Arc::new(ShimHost::new(config)),
            in_flight_wait: Duration::from_secs(1),
            rollback_timeout: Duration::from_secs(5),
        };
        let manager = SessionManager::start_with_shims(
            store.clone(),
            policy,
            self.adapters(),
            self.det.ids(),
            shims,
            Arc::new(fleetdeck_station::workspace::PrivatePaths::default()),
        )
        .await
        .unwrap();
        Station { store, manager }
    }

    fn envelope(&mut self, command: SessionCommand) -> CommandEnvelope {
        self.next += 1;
        CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("cmd-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Local,
            command,
        }
    }

    async fn submit(&self, station: &Station, envelope: CommandEnvelope) -> Decided {
        station
            .manager
            .submit(envelope, Caller::local())
            .await
            .unwrap()
    }

    async fn send(&mut self, station: &Station, command: SessionCommand) -> Decided {
        let envelope = self.envelope(command);
        let decided = self.submit(station, envelope).await;
        assert_eq!(
            decided.receipt.state,
            ReceiptState::Accepted,
            "{:?}",
            decided.receipt
        );
        decided
    }

    /// A session with one finalized turn: the turn waits, `a.txt` changes
    /// while it waits, and the turn is interrupted. Returns the session and
    /// the turn.
    async fn one_changing_turn(&mut self, station: &Station) -> (SessionId, TurnId) {
        let decided = self
            .send(
                station,
                SessionCommand::Start {
                    spec: SessionSpec {
                        provider_instance: "claude".into(),
                        cwd: self.workspace().to_string_lossy().into_owned(),
                        mode: None,
                        model: None,
                        title: None,
                        coordinator: None,
                    },
                    prompt: Some(UserInput::new("wait edit".to_owned())),
                    workspace: None,
                    setup_cmd: None,
                    batch: None,
                    issue: None,
                    origin_text: false,
                },
            )
            .await;
        let session = decided.receipt.result.and_then(|r| r.session_id).unwrap();
        wait_for(&station.store, &session, "the working delta", |events| {
            events
                .iter()
                .any(|e| matches!(e, Event::MessageDelta { text, .. } if text == "working"))
        })
        .await;
        std::fs::write(self.workspace().join("a.txt"), "one\nedited\n").unwrap();
        self.send(
            station,
            SessionCommand::Interrupt {
                session_id: session.clone(),
            },
        )
        .await;
        wait_for(&station.store, &session, "turn.finalized", |events| {
            events
                .iter()
                .any(|e| matches!(e, Event::TurnFinalized { .. }))
        })
        .await;
        let turn = events(&station.store, &session)
            .await
            .iter()
            .find_map(|e| match e {
                Event::TurnFinalized { turn_id, .. } => Some(turn_id.clone()),
                _ => None,
            })
            .unwrap();
        (session, turn)
    }
}

async fn events(store: &Store, session: &SessionId) -> Vec<Event> {
    let ReadResult::Events { events, .. } = store.read_after(0, 10_000).await.unwrap() else {
        panic!("snapshot required")
    };
    events
        .into_iter()
        .filter(|e| &e.session_id == session)
        .filter_map(|e| match e.event {
            EventBody::Known(event) => Some(event),
            EventBody::Unknown { .. } => None,
        })
        .collect()
}

async fn wait_for(store: &Store, session: &SessionId, what: &str, pick: impl Fn(&[Event]) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        let found = events(store, session).await;
        if pick(&found) {
            return;
        }
        assert!(Instant::now() < deadline, "no {what}: {found:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claude_revert_on_the_shim_host_is_refused_like_on_the_direct_host() {
    let mut rig = Rig::new();
    let station = rig.station().await;
    let (session, turn) = rig.one_changing_turn(&station).await;
    // The Claude process runs under a real shim.
    assert!(
        std::fs::read_dir(rig.shims_dir()).unwrap().next().is_some(),
        "the session runs under a shim"
    );
    assert_eq!(rig.file(), "one\nedited\n");
    let refs_before = rig.refs();
    let events_before = events(&station.store, &session).await.len();

    let envelope = rig.envelope(SessionCommand::Revert {
        session_id: session.clone(),
        turn_id: turn,
    });
    let first = rig.submit(&station, envelope.clone()).await;
    assert_eq!(first.receipt.state, ReceiptState::Rejected, "{first:?}");
    assert!(!first.duplicate);
    let reject = first.receipt.reject.clone().unwrap();
    assert_eq!(reject.code, RejectCode::Unsupported, "{reject:?}");
    assert!(first.effects.is_empty(), "{:?}", first.effects);

    // No revert row, no revert effect, no file or ref change.
    let after = events(&station.store, &session).await;
    assert!(
        !after
            .iter()
            .any(|e| matches!(e, Event::RevertUpdated { .. })),
        "{after:?}"
    );
    assert_eq!(
        after.len(),
        events_before,
        "the rejection appended no event: {after:?}"
    );
    let effects = station
        .store
        .session_effects(session.clone())
        .await
        .unwrap();
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e.payload, EffectPayload::Revert { .. })),
        "{effects:?}"
    );
    assert_eq!(rig.file(), "one\nedited\n", "no file changed");
    assert_eq!(rig.refs(), refs_before, "no ref changed");

    // The same command envelope returns the same stored receipt.
    let again = rig.submit(&station, envelope).await;
    assert!(again.duplicate, "{again:?}");
    assert_eq!(again.receipt, first.receipt);
    assert_eq!(rig.file(), "one\nedited\n");
    assert_eq!(rig.refs(), refs_before);
    station.manager.shutdown(StopMode::Graceful).await;
}
