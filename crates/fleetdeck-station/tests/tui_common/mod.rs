//! Shared rig of the TUI handover tests (DL-140): the instance script, the
//! station and the helpers that `tui_handover` and `tui_handover_restart` use.

#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_core::config::Driver;
use fleetdeck_proto::command::{CommandOrigin, ReceiptState, SessionSpec};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::shell::{ShellChange, ShellEnvelope};
use fleetdeck_proto::{
    CommandId, Event, EventBody, Mutation, ProviderInstanceId, SessionCommand, SessionId,
    SessionSummary,
};
use fleetdeck_station::coordinator::Bindings;
use fleetdeck_station::manager::decide::decide as decide_session;
use fleetdeck_station::manager::resume::admit_resume;
use fleetdeck_station::manager::{Policy, PolicyHub};
use fleetdeck_station::shell::handover::spawn_exit_release;
use fleetdeck_station::shell::{ShellConfig, Shells, TerminalInstance, decide, dispatch};
use fleetdeck_station::store::{
    Caller, Decided, NewSession, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};
use fleetdeck_testkit::Det;

pub const WAIT: Duration = Duration::from_secs(15);

pub struct Rig {
    pub det: Det,
    pub db: PathBuf,
    pub root: PathBuf,
    pub binary: PathBuf,
    pub store: Store,
    pub shells: Arc<Shells>,
    pub policy: Policy,
    pub next: u32,
    pub exit_task: tokio::task::JoinHandle<()>,
    _dirs: Vec<tempfile::TempDir>,
}

pub fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

pub fn child_env() -> ChildEnv {
    ChildEnv::scrubbed([(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))])
}

pub fn shells_for(binary: &Path, store: &Store) -> (Arc<Shells>, tokio::task::JoinHandle<()>) {
    let instance = TerminalInstance {
        driver: Driver::Claude,
        binary: binary.to_path_buf(),
        env: child_env(),
    };
    let config = ShellConfig {
        program: None,
        instances: BTreeMap::from([(ProviderInstanceId::new("claude"), instance)]),
    };
    let shells = Arc::new(Shells::with_env(config, child_env()));
    let exits = shells.take_exits().expect("first take");
    (shells, spawn_exit_release(store.clone(), exits))
}

pub async fn open_store(det: &Det, db: &Path) -> Store {
    let store = Store::open(db, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
}

impl Rig {
    /// A station whose instance binary is a script with `body`.
    pub async fn new(body: &str) -> Self {
        let det = Det::new();
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(work.path()).unwrap();
        let binary = script(bin.path(), "claude", body);
        let db = state.path().join("station.db");
        let store = open_store(&det, &db).await;
        let policy = Policy::new(
            vec![root.clone()],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::from([(ProviderInstanceId::new("claude"), ProviderKind::Claude)]),
        )
        .with_shell(true, 4);
        let (shells, exit_task) = shells_for(&binary, &store);
        Self {
            shells,
            exit_task,
            det,
            db,
            root,
            binary,
            store,
            policy,
            next: 0,
            _dirs: vec![state, work, bin],
        }
    }

    /// The same station after a restart: a new process on the same database.
    pub async fn restarted(mut self) -> Self {
        self.shells.close_all();
        self.exit_task.abort();
        let Self { det, db, .. } = &self;
        let store = open_store(det, db).await;
        let (shells, exit_task) = shells_for(&self.binary, &store);
        self.store = store;
        self.shells = shells;
        self.exit_task = exit_task;
        self
    }

    pub fn id(&mut self, what: &str) -> CommandId {
        self.next += 1;
        CommandId::from(format!("{what}-{}", self.next))
    }

    pub fn envelope(&mut self, what: &str, change: ShellChange) -> ShellEnvelope {
        ShellEnvelope {
            command_id: self.id(what),
            target_incarnation: self.store.incarnation().clone(),
            retry_until: self.store.now_ms() + 60_000,
            change,
        }
    }

    /// Decides one shell command in a receipt transaction.
    pub async fn shell(&mut self, what: &str, change: ShellChange) -> Decided {
        let envelope = self.envelope(what, change);
        let (shells, policy) = (Arc::clone(&self.shells), self.policy.clone());
        self.store
            .decide_mutation(
                Mutation::Shell(envelope.clone()),
                Caller::local(),
                move |d| decide(d, &shells, &policy, &envelope),
            )
            .await
            .unwrap()
    }

    pub async fn handover(&mut self, shell: &str, session: &SessionId) -> Decided {
        let change = ShellChange::Handover {
            shell_id: shell.to_owned(),
            session_id: session.clone(),
            cols: 80,
            rows: 24,
        };
        self.shell("handover", change).await
    }

    pub async fn release(&mut self, shell: &str) -> Decided {
        let change = ShellChange::Release {
            shell_id: shell.to_owned(),
        };
        self.shell("release", change).await
    }

    pub async fn close(&mut self, shell: &str) -> Decided {
        let change = ShellChange::Close {
            shell_id: shell.to_owned(),
        };
        self.shell("close", change).await
    }

    /// A session command through the station's own decision function.
    pub async fn session(&mut self, command: SessionCommand) -> Decided {
        let id = self.id("session");
        let envelope = ShellEnvelope {
            command_id: id,
            target_incarnation: self.store.incarnation().clone(),
            retry_until: self.store.now_ms() + 60_000,
            change: ShellChange::Close {
                shell_id: "none".to_owned(),
            },
        };
        let policy = self.policy.clone();
        self.store
            .decide_mutation(Mutation::Shell(envelope), Caller::local(), move |d| {
                decide_session(
                    d,
                    &policy,
                    &Bindings::default(),
                    CommandOrigin::Local,
                    false,
                    command,
                )
            })
            .await
            .unwrap()
    }

    /// `admit_resume` itself, as the auto-continue calls it.
    pub async fn auto_continue(&mut self, session: &SessionId) -> Decided {
        let envelope = self.envelope(
            "auto",
            ShellChange::Close {
                shell_id: "none".to_owned(),
            },
        );
        let (policy, session) = (self.policy.clone(), session.clone());
        self.store
            .decide_mutation(Mutation::Shell(envelope), Caller::local(), move |d| {
                admit_resume(d, &policy, &session, None, None)
            })
            .await
            .unwrap()
    }

    pub async fn dispatch(&self) -> usize {
        dispatch(
            &self.store,
            &self.shells,
            &PolicyHub::new(self.policy.clone()),
        )
        .await
        .unwrap()
    }

    /// An ended session of the `claude` instance with the native session
    /// `native`.
    pub async fn ended(&self, session: &str, native: &str) -> SessionId {
        let id = self.started(session, native).await;
        self.store
            .append(
                id.clone(),
                vec![
                    Event::SessionEnded {
                        reason: "stopped".to_owned(),
                        exit_code: Some(0),
                    }
                    .into(),
                ],
            )
            .await
            .unwrap();
        id
    }

    pub async fn started(&self, session: &str, native: &str) -> SessionId {
        let id = SessionId::new(session);
        let cwd = self.root.to_string_lossy().into_owned();
        self.store
            .create_session(NewSession {
                session_id: id.clone(),
                provider: ProviderKind::Claude,
                provider_instance: ProviderInstanceId::new("claude"),
                cwd: cwd.clone(),
                worktree: None,
                branch: None,
                mode: Some("plan".to_owned()),
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        let refs = serde_json::json!({ "session_id": native });
        let started = Event::SessionStarted {
            provider: ProviderKind::Claude,
            provider_instance: ProviderInstanceId::new("claude"),
            provider_version: None,
            model: Some("opus".to_owned()),
            cwd,
            mode: Some("plan".to_owned()),
            capabilities: Box::new(fleetdeck_providers::claude::args::capabilities(false)),
            resumed: None,
            provider_refs: refs.as_object().unwrap().clone(),
            title: None,
            branch: None,
            repo: None,
        };
        self.store
            .append(id.clone(), vec![started.into()])
            .await
            .unwrap();
        id
    }

    pub async fn summary(&self, session: &SessionId) -> SessionSummary {
        self.store.summary(session.clone()).await.unwrap().unwrap()
    }

    /// The reasons of every `session.terminal` of `session`, oldest first.
    pub async fn reasons(&self, session: &SessionId) -> Vec<String> {
        let ReadResult::Events { events, .. } = self.store.read_after(0, 10_000).await.unwrap()
        else {
            panic!("snapshot required");
        };
        events
            .into_iter()
            .filter(|e| e.session_id == *session)
            .filter_map(|e| match e.event {
                EventBody::Known(Event::SessionTerminal { reason, .. }) => reason,
                _ => None,
            })
            .collect()
    }

    pub async fn effect_state(&self, command: &CommandId) -> String {
        self.store
            .shell_effect(command.clone())
            .await
            .unwrap()
            .unwrap()
            .state
    }
}

pub fn rejected(d: &Decided) -> String {
    assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
    d.receipt.reject.clone().unwrap().message
}

pub fn accepted(d: &Decided) {
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
}

pub async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let t0 = Instant::now();
    while !check().await {
        assert!(t0.elapsed() < WAIT, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub async fn released(rig: &Rig, session: &SessionId) -> bool {
    rig.summary(session).await.terminal.is_none()
}

pub fn import(native: &str, rig: &Rig) -> SessionCommand {
    SessionCommand::Import {
        spec: SessionSpec {
            provider_instance: ProviderInstanceId::new("claude"),
            cwd: rig.root.to_string_lossy().into_owned(),
            mode: None,
            model: None,
            title: None,
            coordinator: None,
        },
        native_session_id: native.to_owned(),
        prompt: None,
    }
}

pub const DEAF_TUI: &str = "trap '' HUP\ntouch ready\nwhile :; do sleep 1; done";
