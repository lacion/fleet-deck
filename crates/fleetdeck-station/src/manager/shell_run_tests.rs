//! The execution of `!` shell commands of Claude sessions and their results
//! in the model context (B8): a real `sh` command in a temp Git repo, with
//! `fake-claude sim` as the provider.

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

use super::{Adapters, Policy, SessionManager, shell_advance};
use crate::coordinator::Bindings;
use crate::store::shell_runs::{ShellRow, ShellState};
use crate::store::{
    Caller, Decided, EffectPayload, OfflineApprovalPolicy, ReadResult, Store, StoreConfig,
};

const WAIT: Duration = Duration::from_secs(15);

/// A clock that no other test process shares: the effect ids are the
/// markers of the commands, and the marker kill finds a process by them.
fn unique_det() -> Det {
    let pid = u64::from(std::process::id());
    Det::at(Det::new().now_ms() + pid * 1_000)
}

struct Rig {
    det: Det,
    manager: Arc<SessionManager>,
    root: tempfile::TempDir,
    deck: Caller,
    next: u32,
}

fn git(dir: &Path, args: &[&str]) {
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
    async fn new() -> Self {
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

    fn app(&self) -> std::path::PathBuf {
        std::fs::canonicalize(self.root.path().join("projects/app")).unwrap()
    }

    fn app_root(&self) -> String {
        self.app().to_string_lossy().into_owned()
    }

    async fn submit(
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

    async fn local(&mut self, command: SessionCommand) -> Decided {
        let d = self
            .submit(Caller::local(), CommandOrigin::Local, command)
            .await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
        d
    }

    async fn events(&self) -> Vec<Event> {
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

    async fn until<T>(&self, what: &str, pick: impl Fn(&Event) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(found) = self.events().await.iter().find_map(&pick) {
                return found;
            }
            assert!(tokio::time::Instant::now() < deadline, "no {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn run_completed(&self, count: usize) {
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
    async fn session(&mut self) -> SessionId {
        let d = self
            .local(SessionCommand::Start {
                spec: SessionSpec {
                    provider_instance: "claude".into(),
                    cwd: self.app_root(),
                    mode: Some("default".to_owned()),
                    model: None,
                    title: None,
                    coordinator: None,
                },
                prompt: Some(UserInput::new("hello")),
                workspace: None,
                setup_cmd: None,
                batch: None,
                issue: None,
                origin_text: false,
            })
            .await;
        let s = d.receipt.result.unwrap().session_id.unwrap();
        self.run_completed(1).await;
        self.effects_settled(&s).await;
        s
    }

    async fn effects_settled(&self, s: &SessionId) {
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
    async fn bang(&mut self, s: &SessionId, command: &str) -> String {
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

    async fn row(&self, id: &str) -> ShellRow {
        self.manager
            .store()
            .shell_run(id.to_owned())
            .await
            .unwrap()
            .unwrap()
    }

    async fn row_is(&self, id: &str, pick: impl Fn(&ShellRow) -> bool) -> ShellRow {
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

    /// Runs `command` to its end and waits for the advance.
    async fn run(&mut self, s: &SessionId, command: &str) -> ShellRow {
        let id = self.bang(s, command).await;
        let row = self
            .row_is(&id, |r| r.state.is_terminal() && !r.advance_pending)
            .await;
        assert_eq!(row.effect_id, id);
        row
    }

    async fn interrupt(&mut self, s: &SessionId) {
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

    async fn prompt_with(&mut self, s: &SessionId, text: &str, ids: &[&str]) -> Vec<ShellContext> {
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
fn gone(pid: i32) -> bool {
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

async fn read_pid(path: &Path) -> i32 {
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

fn completed(events: &[Event], id: &str) -> Option<(ToolStatus, Option<i32>, String)> {
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

#[tokio::test]
async fn a_command_runs_in_the_session_folder_with_the_marker_and_no_input() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let row = rig
        .run(&s, "pwd; echo \"id=$FLEETDECK_SHELL_ID\"; cat; echo end")
        .await;
    assert_eq!(row.state, ShellState::Done);
    assert_eq!(row.exit_code, Some(0));
    let out = row.stdout_tail.unwrap();
    assert!(out.starts_with(&format!("{}\n", rig.app_root())), "{out}");
    assert!(out.contains(&format!("id={}", row.effect_id)), "{out}");
    assert!(out.ends_with("end\n"), "stdin is closed: {out}");
    let events = rig.events().await;
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ToolStarted { node_id, title, input_summary, .. }
            if node_id.as_str() == format!("shell-{}", row.effect_id)
                && title == "Shell command"
                && input_summary.starts_with("pwd;")
    )));
    assert_eq!(
        completed(&events, &row.effect_id),
        Some((ToolStatus::Ok, Some(0), "done".to_owned()))
    );
    // The effect is confirmed, the generation advanced at the start and at
    // the settlement.
    let effects = rig
        .manager
        .store()
        .session_effects(s.clone())
        .await
        .unwrap();
    let shell = effects
        .iter()
        .find(|e| e.effect_id.as_str() == row.effect_id);
    assert_eq!(shell.unwrap().state, crate::store::EffectState::Confirmed);
    assert_eq!(row.generation, Some(1));
    let generation = rig
        .manager
        .store()
        .root_generation(rig.app_root())
        .await
        .unwrap();
    assert_eq!(generation, 2);
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_exit_code_decides_done_or_failed() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let ok = rig.run(&s, "exit 0").await;
    assert_eq!((ok.state, ok.exit_code), (ShellState::Done, Some(0)));
    let bad = rig.run(&s, "echo oops >&2; exit 3").await;
    assert_eq!((bad.state, bad.exit_code), (ShellState::Failed, Some(3)));
    assert_eq!(bad.stderr_tail.as_deref(), Some("oops\n"));
    let events = rig.events().await;
    assert_eq!(
        completed(&events, &bad.effect_id),
        Some((ToolStatus::Failed, Some(3), "done".to_owned()))
    );
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_tails_keep_30000_characters_and_the_event_16_kib() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let row = rig
        .run(
            &s,
            "head -c 40000 /dev/zero | tr '\\0' a; echo err >&2; echo more-err >&2",
        )
        .await;
    assert_eq!(row.stdout_tail.as_ref().unwrap().chars().count(), 30_000);
    assert_eq!(row.stderr_tail.as_deref(), Some("err\nmore-err\n"));
    let events = rig.events().await;
    let (chunk, truncated) = events
        .iter()
        .find_map(|e| match e {
            Event::ToolOutput {
                node_id,
                chunk,
                truncated,
            } if node_id.as_str() == format!("shell-{}", row.effect_id) => {
                Some((chunk.clone(), *truncated))
            }
            _ => None,
        })
        .unwrap();
    assert!(truncated);
    assert_eq!(chunk.len(), 16 * 1024);
    let (out, err) = chunk.split_once("\n--- stderr ---\n").unwrap();
    assert!(out.chars().all(|c| c == 'a'));
    assert_eq!(err, "err\nmore-err\n");
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_cancel_stored_before_the_run_settles_the_row_without_a_command() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    // The task waits for the folder fence: the cancel comes first.
    let fence = rig
        .manager
        .target
        .fences()
        .for_worktree(Path::new(&rig.app_root()));
    let held = fence.lock().await;
    let id = rig.bang(&s, "touch ran.txt").await;
    rig.interrupt(&s).await;
    drop(held);
    let row = rig
        .row_is(&id, |r| r.state.is_terminal() && !r.advance_pending)
        .await;
    assert_eq!(row.state, ShellState::Cancelled);
    assert!(row.cancel_requested);
    assert!(!rig.app().join("ran.txt").exists());
    assert!(!rig.events().await.iter().any(|e| matches!(
        e,
        Event::ToolStarted { node_id, .. } if node_id.as_str() == format!("shell-{id}")
    )));
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_cancel_while_the_command_runs_ends_it_and_its_group_within_5_seconds() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let pid_file = rig.app().join("pid.txt");
    let id = rig.bang(&s, "echo $$ > pid.txt; sleep 30").await;
    let pid = read_pid(&pid_file).await;
    rig.until("tool.started", |e| {
        matches!(e, Event::ToolStarted { node_id, .. } if node_id.as_str() == format!("shell-{id}"))
            .then_some(())
    })
    .await;
    let began = tokio::time::Instant::now();
    rig.interrupt(&s).await;
    let row = rig.row_is(&id, |r| r.state.is_terminal()).await;
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );
    assert_eq!(row.state, ShellState::Cancelled);
    assert!(gone(pid), "the shell of the group is gone");
    rig.until("tool.completed", |e| {
        completed(std::slice::from_ref(e), &id).map(|c| c.2)
    })
    .await;
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_timeout_ends_the_command_with_the_state_timeout() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    rig.manager
        .target
        .set_shell_timeout(Duration::from_millis(300));
    let id = rig.bang(&s, "sleep 30").await;
    let row = rig.row_is(&id, |r| r.state.is_terminal()).await;
    assert_eq!(row.state, ShellState::Timeout);
    let label = rig
        .until("tool.completed", |e| {
            completed(std::slice::from_ref(e), &id).map(|c| c.2)
        })
        .await;
    assert_eq!(label, "timeout");
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_background_child_is_killed_with_its_group() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let began = tokio::time::Instant::now();
    let row = rig.run(&s, "sleep 60 & echo $! > bg.pid").await;
    assert_eq!(row.state, ShellState::Done);
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "no wait for the sleep"
    );
    let pid = read_pid(&rig.app().join("bg.pid")).await;
    assert!(gone(pid), "the background sleep is gone");
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_folder_fence_is_held_during_the_run_and_released_after() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let fence = rig
        .manager
        .target
        .fences()
        .for_worktree(Path::new(&rig.app_root()));
    let id = rig.bang(&s, "echo $$ > pid.txt; sleep 2").await;
    read_pid(&rig.app().join("pid.txt")).await;
    assert!(fence.try_lock().is_err(), "a capture waits for the command");
    let row = rig.row_is(&id, |r| r.state.is_terminal()).await;
    assert_eq!(row.state, ShellState::Done, "{row:?}");
    let free = tokio::time::timeout(Duration::from_secs(5), fence.lock()).await;
    assert!(free.is_ok(), "the fence is released at the end");
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_shutdown_cancels_a_running_command() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let id = rig.bang(&s, "echo $$ > pid.txt; sleep 30").await;
    let pid = read_pid(&rig.app().join("pid.txt")).await;
    let began = tokio::time::Instant::now();
    rig.manager.shutdown(StopMode::Kill).await;
    assert!(began.elapsed() < Duration::from_secs(10));
    assert_eq!(rig.row(&id).await.state, ShellState::Cancelled);
    assert!(gone(pid));
}

#[tokio::test]
async fn the_start_up_settles_a_running_row_unknown_after_killing_its_processes() {
    let det = unique_det();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    let s = SessionId::from("s1");
    store
        .create_session(crate::store::NewSession {
            session_id: s.clone(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    store
        .process_started(s.clone(), "proc-1".into(), Some(1))
        .await
        .unwrap();
    // A shell effect that the last worker claimed, and a `running` row.
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from("plant-running"),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Local,
        command: SessionCommand::Interrupt {
            session_id: s.clone(),
        },
    };
    let owner = s.clone();
    store
        .decide(envelope, Caller::local(), move |d| {
            let id = d
                .enqueue(
                    &owner,
                    EffectPayload::ShellCommand {
                        command: "sleep 60".to_owned(),
                        provider: ProviderKind::Claude,
                        worktree_root: "/w".to_owned(),
                    },
                )?
                .effect_id
                .as_str()
                .to_owned();
            d.shell_insert(&id, &owner, ProviderKind::Claude, "/w", "sleep 60")?;
            Ok(fleetdeck_proto::command::CommandResult {
                session_id: Some(owner.clone()),
                input_id: None,
            })
        })
        .await
        .unwrap();
    let fence = store.register_worker().await.unwrap();
    let id = store
        .claim_next(fence)
        .await
        .unwrap()
        .unwrap()
        .effect_id
        .as_str()
        .to_owned();
    assert!(store.shell_set_running(id.clone()).await.unwrap());
    let mut child = tokio::process::Command::new("/bin/sh")
        .args(["-c", "exec sleep 60"])
        .env(shell_advance::MARKER_VAR, &id)
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = i32::try_from(child.id().unwrap()).unwrap();
    // The next process: its worker fence replaces the old one.
    store.register_worker().await.unwrap();
    let fences = crate::manager::turns::Fences::default();
    shell_advance::start_up(&store, &fences).await.unwrap();
    let row = store.shell_run(id.clone()).await.unwrap().unwrap();
    assert_eq!(row.state, ShellState::Unknown);
    assert!(!row.advance_pending, "the advance ran");
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    assert!(gone(pid));
    let events = match store.read_after(0, 1000).await.unwrap() {
        ReadResult::Events { events, .. } => events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(k) => Some(k),
                EventBody::Unknown { .. } => None,
            })
            .collect::<Vec<_>>(),
        ReadResult::SnapshotRequired { .. } => panic!("snapshot required"),
    };
    assert_eq!(
        completed(&events, &id),
        Some((ToolStatus::Failed, None, "unknown".to_owned()))
    );
    // The terminal row carries the unknown end into the next prompt.
    assert!(store.shell_admission_open().await.unwrap());
}

#[tokio::test]
async fn a_prompt_carries_its_results_once_and_in_order() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let a = rig.run(&s, "echo '<a&b>'").await;
    let b = rig.run(&s, "echo bad >&2; exit 2").await;
    let c = rig.run(&s, "true").await;
    // An unknown id, and a repeated id, are dropped.
    let context = rig
        .prompt_with(
            &s,
            "look",
            &[&b.effect_id, "nope", &a.effect_id, &b.effect_id],
        )
        .await;
    assert_eq!(context.len(), 2);
    assert_eq!(context[0].command, "echo bad >&2; exit 2");
    assert_eq!(context[0].end, "exit 2");
    assert_eq!(context[0].stderr, "bad\n");
    assert_eq!(context[1].end, "exit 0");
    assert_eq!(context[1].stdout, "<a&b>\n");
    rig.run_completed(2).await;
    assert!(rig.row(&a.effect_id).await.included_in.is_some());
    assert!(rig.row(&b.effect_id).await.included_in.is_some());
    // A second prompt that names the same ids gets nothing; the third result
    // that nobody named stays attachable.
    let again = rig
        .prompt_with(&s, "again", &[&a.effect_id, &b.effect_id])
        .await;
    assert!(again.is_empty());
    assert!(rig.row(&c.effect_id).await.included_in.is_none());
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_end_text_names_how_the_command_ended() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    rig.manager
        .target
        .set_shell_timeout(Duration::from_millis(300));
    let slow = rig.run(&s, "sleep 30").await;
    assert_eq!(slow.state, ShellState::Timeout);
    let context = rig.prompt_with(&s, "x", &[&slow.effect_id]).await;
    assert_eq!(context[0].end, "timed out");
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_oversize_result_is_cut_and_the_results_over_64_kib_wait() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    // 30,000 `<` escape to 120,000 bytes: each result is cut to 32 KiB.
    let big = "head -c 30000 /dev/zero | tr '\\0' '<'";
    let ids = [
        rig.run(&s, big).await.effect_id,
        rig.run(&s, big).await.effect_id,
        rig.run(&s, big).await.effect_id,
    ];
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let context = rig.prompt_with(&s, "x", &refs).await;
    assert_eq!(context.len(), 2, "the third result does not fit 64 KiB");
    for result in &context {
        assert!(result.stdout.starts_with("[… "), "{}", &result.stdout[..40]);
        assert!(result.stdout.contains(" characters omitted]\n"));
        let size =
            serde_json::to_string(&fleetdeck_providers::claude::control::shell_blocks(result))
                .unwrap()
                .len();
        assert!(size <= 32 * 1024, "{size}");
    }
    assert!(rig.row(&ids[2]).await.included_in.is_none());
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_resume_first_path_carries_the_context() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let row = rig.run(&s, "echo kept").await;
    // The session hibernates; its next input resumes it with the results.
    rig.det.advance(Duration::from_secs(31 * 60));
    let store = rig.manager.store().clone();
    let policy = rig.manager.policy().clone();
    let swept = super::hibernate::sweep(&store, &policy, &Bindings::default(), 30)
        .await
        .unwrap();
    assert_eq!(swept, 1);
    rig.until("session ended", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    rig.effects_settled(&s).await;
    let context = rig.prompt_with(&s, "back", &[&row.effect_id]).await;
    assert_eq!(context.len(), 1);
    assert_eq!(context[0].stdout, "kept\n");
    assert!(rig.row(&row.effect_id).await.included_in.is_some());
    rig.manager.shutdown(StopMode::Kill).await;
}

#[path = "shell_run_race_tests.rs"]
mod race;
