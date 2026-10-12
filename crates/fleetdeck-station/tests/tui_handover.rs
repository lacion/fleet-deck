//! The station side of the TUI handover (DL-140): `shell.handover` admits an
//! ended session, holds it while the provider TUI runs in a PTY, and ends
//! the hold only when the process is gone; `shell.release` and the start
//! recovery clear holds that the station cannot see. The "provider" is a
//! shell script that is the instance binary.

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
use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::shell::{ShellChange, ShellEnvelope, ShellState};
use fleetdeck_proto::{
    CommandId, Event, EventBody, Mutation, ProviderInstanceId, SessionCommand, SessionId,
    SessionSummary,
};
use fleetdeck_station::manager::Policy;
use fleetdeck_station::manager::decide::decide as decide_session;
use fleetdeck_station::manager::resume::admit_resume;
use fleetdeck_station::shell::handover::{Liveness, liveness, recover_holds, spawn_exit_release};
use fleetdeck_station::shell::{ShellConfig, Shells, TerminalInstance, decide, dispatch};
use fleetdeck_station::store::{
    Caller, Decided, NewSession, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};
use fleetdeck_testkit::Det;

const WAIT: Duration = Duration::from_secs(15);

struct Rig {
    det: Det,
    db: PathBuf,
    root: PathBuf,
    binary: PathBuf,
    store: Store,
    shells: Arc<Shells>,
    policy: Policy,
    next: u32,
    exit_task: tokio::task::JoinHandle<()>,
    _dirs: Vec<tempfile::TempDir>,
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn child_env() -> ChildEnv {
    ChildEnv::scrubbed([(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))])
}

fn shells_for(binary: &Path, store: &Store) -> (Arc<Shells>, tokio::task::JoinHandle<()>) {
    let instance = TerminalInstance {
        driver: Driver::Claude,
        binary: binary.to_path_buf(),
        env: child_env(),
    };
    let config = ShellConfig {
        allow: true,
        max_shells: 4,
        program: None,
        instances: BTreeMap::from([(ProviderInstanceId::new("claude"), instance)]),
    };
    let shells = Arc::new(Shells::with_env(config, child_env()));
    let exits = shells.take_exits().expect("first take");
    (shells, spawn_exit_release(store.clone(), exits))
}

async fn open_store(det: &Det, db: &Path) -> Store {
    let store = Store::open(db, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
}

impl Rig {
    /// A station whose instance binary is a script with `body`.
    async fn new(body: &str) -> Self {
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
        );
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
    async fn restarted(mut self) -> Self {
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

    fn id(&mut self, what: &str) -> CommandId {
        self.next += 1;
        CommandId::from(format!("{what}-{}", self.next))
    }

    fn envelope(&mut self, what: &str, change: ShellChange) -> ShellEnvelope {
        ShellEnvelope {
            command_id: self.id(what),
            target_incarnation: self.store.incarnation().clone(),
            retry_until: self.store.now_ms() + 60_000,
            change,
        }
    }

    /// Decides one shell command in a receipt transaction.
    async fn shell(&mut self, what: &str, change: ShellChange) -> Decided {
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

    async fn handover(&mut self, shell: &str, session: &SessionId) -> Decided {
        let change = ShellChange::Handover {
            shell_id: shell.to_owned(),
            session_id: session.clone(),
            cols: 80,
            rows: 24,
        };
        self.shell("handover", change).await
    }

    async fn release(&mut self, shell: &str) -> Decided {
        let change = ShellChange::Release {
            shell_id: shell.to_owned(),
        };
        self.shell("release", change).await
    }

    async fn close(&mut self, shell: &str) -> Decided {
        let change = ShellChange::Close {
            shell_id: shell.to_owned(),
        };
        self.shell("close", change).await
    }

    /// A session command through the station's own decision function.
    async fn session(&mut self, command: SessionCommand) -> Decided {
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
                decide_session(d, &policy, CommandOrigin::Local, command)
            })
            .await
            .unwrap()
    }

    /// `admit_resume` itself, as the auto-continue calls it.
    async fn auto_continue(&mut self, session: &SessionId) -> Decided {
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

    async fn dispatch(&self) -> usize {
        dispatch(&self.store, &self.shells, &self.policy)
            .await
            .unwrap()
    }

    /// An ended session of the `claude` instance with the native session
    /// `native`.
    async fn ended(&self, session: &str, native: &str) -> SessionId {
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

    async fn started(&self, session: &str, native: &str) -> SessionId {
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

    async fn summary(&self, session: &SessionId) -> SessionSummary {
        self.store.summary(session.clone()).await.unwrap().unwrap()
    }

    /// The reasons of every `session.terminal` of `session`, oldest first.
    async fn reasons(&self, session: &SessionId) -> Vec<String> {
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

    async fn effect_state(&self, command: &CommandId) -> String {
        self.store
            .shell_effect(command.clone())
            .await
            .unwrap()
            .unwrap()
            .state
    }
}

fn rejected(d: &Decided) -> String {
    assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
    d.receipt.reject.clone().unwrap().message
}

fn accepted(d: &Decided) {
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
}

async fn eventually<F, Fut>(what: &str, mut check: F)
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

async fn released(rig: &Rig, session: &SessionId) -> bool {
    rig.summary(session).await.terminal.is_none()
}

fn import(native: &str, rig: &Rig) -> SessionCommand {
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

#[tokio::test]
async fn a_handover_of_an_ended_session_holds_it_and_starts_the_tui() {
    let mut rig = Rig::new("echo \"$@\" > args.txt\nexec cat").await;
    let s = rig.ended("s-a", "native-a").await;

    let d = rig.handover("tui-1", &s).await;
    accepted(&d);
    // The admission holds the session before any PTY I/O.
    let hold = rig.summary(&s).await.terminal.expect("held at admission");
    assert_eq!(hold.shell_id, "tui-1");
    assert_eq!(hold.pid, None);
    assert_eq!(hold.reason.as_deref(), Some("handover requested"));
    assert!(rig.shells.list().is_empty());

    assert_eq!(rig.dispatch().await, 1);
    let hold = rig.summary(&s).await.terminal.expect("still held");
    assert!(hold.pid.is_some(), "the pid is in the hold: {hold:?}");
    let shells = rig.shells.list();
    assert_eq!(shells.len(), 1);
    assert_eq!(shells[0].shell_id, "tui-1");
    assert_eq!(shells[0].session_id.as_ref(), Some(&s));
    assert_eq!(shells[0].state, ShellState::Running);
    assert_eq!(rig.effect_state(&d.receipt.command_id).await, "confirmed");

    // The TUI runs in the canonical workspace with the stored mode and model.
    let args = rig.root.join("args.txt");
    eventually("the TUI wrote its arguments", || async {
        args.exists() && std::fs::read_to_string(&args).is_ok_and(|t| t.ends_with('\n'))
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(&args).unwrap().trim(),
        "--resume native-a --permission-mode plan --model opus"
    );
    rig.shells.close_all();
}

#[tokio::test]
async fn a_resume_of_a_held_session_is_refused() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);

    let hold = "a terminal holds this session (shell `tui-1`); return it to board control first";
    let d = rig
        .session(SessionCommand::Resume {
            session_id: s.clone(),
        })
        .await;
    assert_eq!(rejected(&d), hold);
    // The auto-continue admits through the same function.
    let d = rig.auto_continue(&s).await;
    assert_eq!(rejected(&d), hold);
}

#[tokio::test]
async fn a_handover_of_a_running_session_is_refused() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.started("s-a", "native-a").await;
    rig.store
        .append(
            s.clone(),
            vec![
                Event::SessionStateChanged {
                    state: SessionState::Running,
                    detail: None,
                }
                .into(),
            ],
        )
        .await
        .unwrap();

    let d = rig.handover("tui-1", &s).await;
    assert_eq!(rejected(&d), "only an ended session can be resumed");
    assert!(rig.summary(&s).await.terminal.is_none());
}

#[tokio::test]
async fn a_second_handover_of_one_session_is_refused() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);

    let d = rig.handover("tui-2", &s).await;
    assert_eq!(
        rejected(&d),
        "a terminal holds this session (shell `tui-1`); return it to board control first"
    );
    assert_eq!(rig.summary(&s).await.terminal.unwrap().shell_id, "tui-1");
}

#[tokio::test]
async fn a_session_with_the_same_native_id_as_a_held_one_cannot_run_either() {
    let mut rig = Rig::new("exec cat").await;
    let a = rig.ended("s-a", "native-x").await;
    let b = rig.ended("s-b", "native-x").await;
    accepted(&rig.handover("tui-1", &a).await);
    let alias = "session `s-a` holds this native session in a terminal";

    let d = rig
        .session(SessionCommand::Resume {
            session_id: b.clone(),
        })
        .await;
    assert_eq!(rejected(&d), alias);
    let d = rig.handover("tui-2", &b).await;
    assert_eq!(rejected(&d), alias);
    let d = rig.session(import("native-x", &rig)).await;
    assert_eq!(rejected(&d), alias);
}

#[tokio::test]
async fn closing_a_tui_releases_the_hold_only_after_the_process_died() {
    // The TUI ignores SIGHUP, so only the 3 s kill of the process group
    // ends it.
    let mut rig = Rig::new("trap '' HUP\nwhile :; do sleep 1; done").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert_eq!(rig.dispatch().await, 1);

    accepted(&rig.close("tui-1").await);
    assert_eq!(rig.dispatch().await, 1);
    assert!(!rig.shells.exists("tui-1"));
    tokio::time::sleep(Duration::from_millis(700)).await;
    let hold = rig.summary(&s).await.terminal;
    assert!(hold.is_some(), "the process still runs: the hold stays");

    eventually("the hold ends with the process", || async {
        released(&rig, &s).await
    })
    .await;
    let reasons = rig.reasons(&s).await;
    let last = reasons.last().unwrap();
    assert!(
        last.starts_with("the terminal process exited"),
        "{reasons:?}"
    );
}

#[tokio::test]
async fn closing_a_tui_that_hangs_up_releases_its_hold() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert_eq!(rig.dispatch().await, 1);

    accepted(&rig.close("tui-1").await);
    assert_eq!(rig.dispatch().await, 1);
    eventually("the hold ends", || async { released(&rig, &s).await }).await;
    let reasons = rig.reasons(&s).await;
    assert!(
        reasons
            .last()
            .unwrap()
            .starts_with("the terminal process exited"),
        "{reasons:?}"
    );
}

#[tokio::test]
async fn a_tui_that_exits_by_itself_releases_the_hold_with_its_exit_code() {
    let mut rig = Rig::new("exit 3").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert_eq!(rig.dispatch().await, 1);

    eventually("the hold ends", || async { released(&rig, &s).await }).await;
    let reasons = rig.reasons(&s).await;
    assert_eq!(
        reasons.last().map(String::as_str),
        Some("the terminal process exited (code 3)"),
        "{reasons:?}"
    );
}

#[tokio::test]
async fn a_tui_that_does_not_start_releases_the_hold_and_fails_the_effect() {
    let mut rig = Rig::new("exec cat").await;
    std::fs::remove_file(&rig.binary).unwrap();
    let s = rig.ended("s-a", "native-a").await;
    let d = rig.handover("tui-1", &s).await;
    accepted(&d);

    assert_eq!(rig.dispatch().await, 1);
    assert_eq!(rig.effect_state(&d.receipt.command_id).await, "failed");
    assert!(rig.summary(&s).await.terminal.is_none());
    let reasons = rig.reasons(&s).await;
    assert!(
        reasons
            .last()
            .unwrap()
            .starts_with("the terminal did not start: "),
        "{reasons:?}"
    );
    assert!(rig.shells.list().is_empty());
}

#[tokio::test]
async fn a_release_is_refused_while_the_shell_lives() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert_eq!(rig.dispatch().await, 1);

    let d = rig.release("tui-1").await;
    assert_eq!(rejected(&d), "shell `tui-1` still runs; close it first");
    assert!(rig.summary(&s).await.terminal.is_some());
    rig.shells.close_all();
}

#[tokio::test]
async fn a_release_is_refused_while_the_handover_is_not_dispatched() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    let d = rig.handover("tui-1", &s).await;
    accepted(&d);
    assert_eq!(rig.effect_state(&d.receipt.command_id).await, "queued");

    let d = rig.release("tui-1").await;
    assert_eq!(
        rejected(&d),
        "shell `tui-1` has a pending change; try again when it settles"
    );
    assert!(rig.summary(&s).await.terminal.is_some(), "the hold stays");

    // Once the handover is done the TUI is the shell that runs.
    assert_eq!(rig.dispatch().await, 1);
    assert!(rig.summary(&s).await.terminal.unwrap().pid.is_some());
    rig.shells.close_all();
}

#[tokio::test]
async fn a_release_without_a_hold_is_refused() {
    let mut rig = Rig::new("exec cat").await;
    let d = rig.release("tui-9").await;
    assert_eq!(rejected(&d), "no session is held by shell `tui-9`");
}

#[tokio::test]
async fn after_a_restart_a_hold_with_a_dead_process_is_released() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    // A process id that is gone: a child that has ended and was reaped.
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let dead = gone.id();
    gone.wait().unwrap();
    assert!(
        rig.store
            .record_terminal_pid(s.clone(), "tui-1".to_owned(), dead)
            .await
            .unwrap()
    );

    let rig = rig.restarted().await;
    assert!(rig.summary(&s).await.terminal.is_some());
    recover_holds(&rig.store).await.unwrap();
    assert!(rig.summary(&s).await.terminal.is_none());
    assert_eq!(
        rig.reasons(&s).await.last().map(String::as_str),
        Some("the station restarted; the terminal process is gone")
    );
}

#[tokio::test]
async fn after_a_restart_a_hold_with_a_live_process_stays_until_the_owner_releases_it() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    let mut alive = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = alive.id();
    assert!(
        rig.store
            .record_terminal_pid(s.clone(), "tui-1".to_owned(), pid)
            .await
            .unwrap()
    );

    let mut rig = rig.restarted().await;
    recover_holds(&rig.store).await.unwrap();
    let hold = rig.summary(&s).await.terminal.expect("the hold stays");
    assert_eq!(hold.pid, Some(pid));
    assert_eq!(
        hold.reason,
        Some(format!(
            "the station restarted; process {pid} may still use this session"
        ))
    );
    // It is still held: nothing resumes it or hands it over again.
    let d = rig.handover("tui-2", &s).await;
    assert!(rejected(&d).starts_with("a terminal holds this session (shell `tui-1`)"));

    let d = rig.release("tui-1").await;
    accepted(&d);
    assert!(rig.summary(&s).await.terminal.is_none());
    assert_eq!(
        rig.reasons(&s).await.last().map(String::as_str),
        Some("the owner released the terminal hold")
    );
    alive.kill().unwrap();
    alive.wait().unwrap();
}

#[tokio::test]
async fn after_a_restart_a_handover_that_never_ran_releases_its_hold() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    let d = rig.handover("tui-1", &s).await;
    accepted(&d);
    assert_eq!(rig.effect_state(&d.receipt.command_id).await, "queued");

    let rig = rig.restarted().await;
    assert_eq!(
        rig.effect_state(&d.receipt.command_id).await,
        "retired_not_dispatched"
    );
    recover_holds(&rig.store).await.unwrap();
    assert!(rig.summary(&s).await.terminal.is_none());
    assert_eq!(
        rig.reasons(&s).await.last().map(String::as_str),
        Some("the station restarted before the terminal started")
    );
}

#[tokio::test]
async fn after_a_restart_a_handover_that_may_have_started_keeps_its_hold() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    // Claimed, and the station stopped before it knew the outcome.
    assert!(rig.store.claim_shell_effect().await.unwrap().is_some());

    let rig = rig.restarted().await;
    recover_holds(&rig.store).await.unwrap();
    let hold = rig.summary(&s).await.terminal.expect("the hold stays");
    assert_eq!(hold.pid, None);
    assert_eq!(
        hold.reason.as_deref(),
        Some("the station restarted; a terminal process may still use this session")
    );
}

#[tokio::test]
async fn a_handover_of_an_instance_without_a_binary_is_refused() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    rig.shells = Arc::new(Shells::with_env(ShellConfig::default(), child_env()));

    let d = rig.handover("tui-1", &s).await;
    assert_eq!(
        rejected(&d),
        "provider instance `claude` has no binary for a terminal"
    );
    assert!(rig.summary(&s).await.terminal.is_none());
    let _ = &rig.det;
}

/// A TUI that ignores SIGHUP and says (a file in its cwd) when it does.
const DEAF_TUI: &str = "trap '' HUP\ntouch ready\nwhile :; do sleep 1; done";

#[tokio::test]
async fn a_release_is_refused_while_a_closed_tui_still_runs() {
    let mut rig = Rig::new(DEAF_TUI).await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert_eq!(rig.dispatch().await, 1);
    let ready = rig.root.join("ready");
    eventually("the TUI ignores the hangup", || async { ready.exists() }).await;

    accepted(&rig.close("tui-1").await);
    assert_eq!(rig.dispatch().await, 1);
    assert!(!rig.shells.exists("tui-1"));

    let d = rig.release("tui-1").await;
    assert_eq!(
        rejected(&d),
        "shell `tui-1` is closing and its process still runs; try again when it has stopped"
    );
    assert!(rig.summary(&s).await.terminal.is_some(), "the hold stays");

    eventually("the hold ends with the process", || async {
        released(&rig, &s).await
    })
    .await;
    let reasons = rig.reasons(&s).await;
    assert!(
        reasons
            .last()
            .unwrap()
            .starts_with("the terminal process exited"),
        "{reasons:?}"
    );
}

#[tokio::test]
async fn after_a_restart_a_hold_whose_process_cannot_be_checked_stays() {
    // Pid 1 is a process that a non-root user cannot signal (`EPERM`).
    if rustix::process::geteuid().is_root() || liveness(1) != Liveness::Unknown {
        eprintln!("skipped: pid 1 can be probed by this user");
        return;
    }
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert!(
        rig.store
            .record_terminal_pid(s.clone(), "tui-1".to_owned(), 1)
            .await
            .unwrap()
    );

    let rig = rig.restarted().await;
    recover_holds(&rig.store).await.unwrap();
    let hold = rig.summary(&s).await.terminal.expect("the hold stays");
    assert_eq!(hold.pid, Some(1));
    assert_eq!(
        rig.reasons(&s).await.last().map(String::as_str),
        Some("the station restarted; it cannot check process 1, so the hold stays")
    );
}

#[tokio::test]
async fn station_stop_kills_a_tui_that_ignores_the_hangup() {
    let mut rig = Rig::new(DEAF_TUI).await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert_eq!(rig.dispatch().await, 1);
    let ready = rig.root.join("ready");
    eventually("the TUI ignores the hangup", || async { ready.exists() }).await;
    let pid = rig.summary(&s).await.terminal.unwrap().pid.unwrap();
    assert_ne!(liveness(pid), Liveness::Gone);

    let shells = Arc::clone(&rig.shells);
    tokio::task::spawn_blocking(move || shells.close_all())
        .await
        .unwrap();
    // The pump reaps the zombie right after close_all saw it.
    eventually("the TUI process is gone", || async {
        liveness(pid) != Liveness::Running
    })
    .await;
}

#[tokio::test]
async fn a_closing_tui_keeps_its_shell_id_reserved() {
    let mut rig = Rig::new(DEAF_TUI).await;
    let a = rig.ended("s-a", "native-a").await;
    let b = rig.ended("s-b", "native-b").await;
    accepted(&rig.handover("tui-1", &a).await);
    assert_eq!(rig.dispatch().await, 1);
    let ready = rig.root.join("ready");
    eventually("the TUI ignores the hangup", || async { ready.exists() }).await;

    accepted(&rig.close("tui-1").await);
    assert_eq!(rig.dispatch().await, 1);
    assert!(!rig.shells.exists("tui-1"));

    // The id of the closing TUI is not free for another session.
    let d = rig.handover("tui-1", &b).await;
    assert_eq!(rejected(&d), "shell `tui-1` exists");
    assert!(rig.summary(&b).await.terminal.is_none());
    let d = rig.release("tui-1").await;
    assert_eq!(
        rejected(&d),
        "shell `tui-1` is closing and its process still runs; try again when it has stopped"
    );
    assert!(rig.summary(&a).await.terminal.is_some(), "the hold stays");

    eventually("the hold ends with the process", || async {
        released(&rig, &a).await
    })
    .await;
    let reasons = rig.reasons(&a).await;
    assert!(
        reasons
            .last()
            .unwrap()
            .starts_with("the terminal process exited"),
        "{reasons:?}"
    );
}

#[tokio::test]
async fn after_a_restart_a_failed_handover_with_a_hold_keeps_it() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    let d = rig.handover("tui-1", &s).await;
    accepted(&d);
    // Claimed and finished as failed, but the hold was not released: the
    // start was uncertain.
    let effect = rig.store.claim_shell_effect().await.unwrap().unwrap();
    rig.store
        .finish_shell_effect(effect.effect_id, Err("shell task: panic".into()))
        .await
        .unwrap();
    assert_eq!(rig.effect_state(&d.receipt.command_id).await, "failed");

    let rig = rig.restarted().await;
    recover_holds(&rig.store).await.unwrap();
    let hold = rig.summary(&s).await.terminal.expect("the hold stays");
    assert_eq!(hold.pid, None);
    assert_eq!(
        hold.reason.as_deref(),
        Some("the station restarted; a terminal process may still use this session")
    );
}
