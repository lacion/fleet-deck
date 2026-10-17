//! The rig of the shell run tests (B8).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, StopMode, UserInput,
};
use fleetdeck_proto::event::{Event, ProviderKind, ToolStatus};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{CommandEnvelope, CommandId, EventBody, SessionId};
use fleetdeck_providers::ShellContext;
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_testkit::fake::SCENARIO_ENV;
use fleetdeck_testkit::{Det, FakeAgent};

use crate::manager::{Adapters, Policy, SessionManager};
use crate::store::shell_runs::ShellRow;
use crate::store::{
    Caller, Decided, EffectPayload, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};

pub(super) const WAIT: Duration = Duration::from_secs(15);

/// A clock that no other test process shares: the effect ids are the
/// markers of the commands, and the marker kill finds a process by them.
pub(super) fn unique_det() -> Det {
    let pid = u64::from(std::process::id());
    Det::at(Det::new().now_ms() + pid * 1_000)
}

pub(in crate::manager) struct Rig {
    pub(super) det: Det,
    pub(in crate::manager) manager: Arc<SessionManager>,
    pub(super) root: tempfile::TempDir,
    pub(super) deck: Caller,
    pub(super) next: u32,
}

pub(super) fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

impl Rig {
    pub(in crate::manager) async fn new() -> Self {
        let det = unique_det();
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("projects/app");
        std::fs::create_dir_all(&app).unwrap();
        git(&app, &["init", "-q"]);
        git(&app, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        let policy = Policy::new(
            vec![root.path().join("projects")],
            8,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
        );
        let mut env = ChildEnv::from_process();
        env.set(SCENARIO_ENV, "sim").unwrap();
        let mut config =
            ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
        config.stop = StopPolicy {
            grace: Duration::from_millis(500),
            term_grace: Duration::from_millis(300),
        };
        let adapters = Adapters::from([(
            "claude".into(),
            Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new()))) as Arc<_>,
        )]);
        let manager = SessionManager::start(
            store.clone(),
            policy,
            adapters,
            det.ids(),
            Arc::new(crate::workspace::PrivatePaths::default()),
        )
        .await
        .unwrap();
        let key = Identity::generate().unwrap().public_key();
        store
            .pin_peer(crate::store::peers::PairedKey {
                key,
                role: fleetdeck_link::tls::Role::Deck,
                scopes: [Scope::Read, Scope::Drive, Scope::Admin]
                    .into_iter()
                    .collect(),
                dial_url: None,
            })
            .await
            .unwrap();
        Self {
            det,
            manager: Arc::new(manager),
            root,
            deck: Caller::peer(key.fingerprint(), Scope::Admin),
            next: 0,
        }
    }

    pub(in crate::manager) fn app(&self) -> std::path::PathBuf {
        std::fs::canonicalize(self.root.path().join("projects/app")).unwrap()
    }

    pub(in crate::manager) fn app_root(&self) -> String {
        self.app().to_string_lossy().into_owned()
    }

    pub(super) async fn submit(
        &mut self,
        caller: Caller,
        origin: CommandOrigin,
        c: SessionCommand,
    ) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("run-cmd-{}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin,
            command: c,
        };
        self.manager.submit(envelope, caller).await.unwrap()
    }

    pub(super) async fn local(&mut self, command: SessionCommand) -> Decided {
        let d = self
            .submit(Caller::local(), CommandOrigin::Local, command)
            .await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
        d
    }

    pub(super) async fn events(&self) -> Vec<Event> {
        match self.manager.store().read_after(0, 100_000).await.unwrap() {
            ReadResult::Events { events, .. } => events
                .into_iter()
                .filter_map(|e| match e.event {
                    EventBody::Known(k) => Some(k),
                    EventBody::Unknown { .. } => None,
                })
                .collect(),
            ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
        }
    }

    pub(super) async fn until<T>(&self, what: &str, pick: impl Fn(&Event) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(found) = self.events().await.iter().find_map(&pick) {
                return found;
            }
            assert!(tokio::time::Instant::now() < deadline, "no {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub(super) async fn run_completed(&self, count: usize) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let n = self
                .events()
                .await
                .iter()
                .filter(|e| matches!(e, Event::RunCompleted { .. }))
                .count();
            if n >= count {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "no run.completed");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A session in the repo that finished its first run, effects settled.
    pub(in crate::manager) async fn session(&mut self) -> SessionId {
        let d = self
            .local(SessionCommand::Start {
                spec: SessionSpec {
                    provider_instance: "claude".into(),
                    cwd: self.app_root(),
                    mode: Some("default".to_owned()),
                    model: None,
                    title: None,
                    coordinator: None,
                    launch: None,
                },
                prompt: Some(UserInput::new("hello")),
                workspace: None,
                setup_cmd: None,
                batch: None,
                issue: None,
                origin_text: false,
                fleet_git: None,
            })
            .await;
        let s = d.receipt.result.unwrap().session_id.unwrap();
        self.run_completed(1).await;
        self.effects_settled(&s).await;
        s
    }

    pub(super) async fn effects_settled(&self, s: &SessionId) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let effects = self
                .manager
                .store()
                .session_effects(s.clone())
                .await
                .unwrap();
            if effects.iter().all(|e| {
                !matches!(
                    e.state,
                    crate::store::EffectState::Queued | crate::store::EffectState::Claimed
                )
            }) {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "{effects:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Submits `!command` as the board; returns the effect id.
    pub(in crate::manager) async fn bang(&mut self, s: &SessionId, command: &str) -> String {
        let deck = self.deck.clone();
        let d = self
            .submit(
                deck,
                CommandOrigin::Board,
                SessionCommand::ShellCommand {
                    session_id: s.clone(),
                    command: command.to_owned(),
                },
            )
            .await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
        d.effects[0].effect_id.as_str().to_owned()
    }

    pub(super) async fn row(&self, id: &str) -> ShellRow {
        self.manager
            .store()
            .shell_run(id.to_owned())
            .await
            .unwrap()
            .unwrap()
    }

    pub(in crate::manager) async fn row_is(
        &self,
        id: &str,
        pick: impl Fn(&ShellRow) -> bool,
    ) -> ShellRow {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let row = self.row(id).await;
            if pick(&row) {
                return row;
            }
            assert!(tokio::time::Instant::now() < deadline, "{row:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The manager's shutdown, at most 30 s: a hang fails the test.
    pub(in crate::manager) async fn shutdown(&self) {
        let ended = tokio::time::timeout(
            Duration::from_secs(30),
            self.manager.shutdown(StopMode::Kill),
        )
        .await;
        assert!(ended.is_ok(), "the shutdown did not end in 30 s");
    }

    /// Runs `command` to its end and waits for the advance.
    pub(in crate::manager) async fn run(&mut self, s: &SessionId, command: &str) -> ShellRow {
        let id = self.bang(s, command).await;
        let row = self
            .row_is(&id, |r| r.state.is_terminal() && !r.advance_pending)
            .await;
        assert_eq!(row.effect_id, id);
        row
    }

    pub(super) async fn interrupt(&mut self, s: &SessionId) {
        let deck = self.deck.clone();
        let d = self
            .submit(
                deck,
                CommandOrigin::Board,
                SessionCommand::Interrupt {
                    session_id: s.clone(),
                },
            )
            .await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    }

    pub(super) async fn prompt_with(
        &mut self,
        s: &SessionId,
        text: &str,
        ids: &[&str],
    ) -> Vec<ShellContext> {
        let mut input = UserInput::new(text);
        input.shell_results = ids.iter().map(|i| (*i).to_owned()).collect();
        let d = self
            .local(SessionCommand::Prompt {
                session_id: s.clone(),
                input,
            })
            .await;
        match &d.effects[0].payload {
            EffectPayload::Prompt { input, .. } => input.shell_context.clone(),
            EffectPayload::Resume { prompt, .. } => prompt.as_ref().unwrap().shell_context.clone(),
            other => panic!("{other:?}"),
        }
    }
}

/// True when no process `pid` lives (a zombie counts as gone).
pub(super) fn gone(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => {
            stat.rsplit(')')
                .next()
                .and_then(|rest| rest.split_whitespace().next())
                == Some("Z")
        }
    }
}

pub(super) async fn read_pid(path: &Path) -> i32 {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if let Some(pid) = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| t.trim().parse().ok())
        {
            return pid;
        }
        assert!(tokio::time::Instant::now() < deadline, "no pid file");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub(super) fn completed(events: &[Event], id: &str) -> Option<(ToolStatus, Option<i32>, String)> {
    events.iter().find_map(|e| match e {
        Event::ToolCompleted {
            node_id,
            status,
            exit_code,
            provider_refs,
            ..
        } if node_id.as_str() == format!("shell-{id}") => Some((
            *status,
            *exit_code,
            provider_refs["shell"].as_str().unwrap().to_owned(),
        )),
        _ => None,
    })
}
