//! The rig of the app call tests (B15 T5b): a fake bridge with scripted
//! provider answers, one session `s1` in a real Git folder (or without a
//! folder), a store, the call environment and the helpers that wait for a
//! state. Used by `calls_tests.rs`, `link/intake_apps_tests.rs` and
//! `manager/revert_run_apps_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_proto::apps::{AppCallParams, AppCallReply, AppCsp, AppRequest};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::apps::{AppBridge, AppError, AppTool, ToolAnswer};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::*;
use crate::manager::target::AppHost;
use crate::manager::turns::{Fences, SessionTurns};
use crate::manager::{Adapters, SessionTarget};
use crate::store::app_calls::NewAppCall;
use crate::store::apps::{NewApp, ToolAppEvent};
use crate::store::{NewSession, OfflineApprovalPolicy, StoreConfig};
use crate::workspace::{GitOptions, Limits, PrivatePaths};

pub(crate) const S1: &str = "s1";
pub(crate) const URI: &str = "ui://demo/counter.html";

pub(crate) fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

/// Polls `f` until it gives a value (at most 10 s).
pub(crate) async fn until<F, Fut, T>(what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// What the fake bridge answers to `call_tool`.
#[derive(Clone)]
pub(crate) enum Script {
    Ok(Value),
    /// A result that does not prove the end of the tool (`proves_end: false`).
    Unproven(Value),
    Err(AppError),
    /// Waits for the gate, then goes on with the next script.
    Gated(Arc<Notify>, Box<Script>),
}

/// A provider bridge with scripted answers.
pub(crate) struct Bridge {
    pub tools: Mutex<Vec<AppTool>>,
    pub script: Mutex<Script>,
    pub called: Mutex<Vec<(String, String, Value)>>,
    pub reads: AtomicUsize,
}

pub(crate) fn tool(
    server: &str,
    name: &str,
    read_only: bool,
    visibility: Option<&[&str]>,
) -> AppTool {
    AppTool {
        server: server.to_owned(),
        name: name.to_owned(),
        title: Some(name.to_uppercase()),
        read_only,
        visibility: visibility.map(|v| v.iter().map(|s| (*s).to_owned()).collect()),
        resource_uri: None,
    }
}

pub(crate) fn text(t: &str) -> Value {
    json!({"content": [{"type": "text", "text": t}]})
}

impl Bridge {
    fn new() -> Self {
        Self {
            tools: Mutex::new(vec![
                tool("demo", "show_counter", true, None),
                tool("demo", "bump", false, Some(&["app"])),
                tool("demo", "slow_bump", false, Some(&["app"])),
                tool("demo", "model_only", false, Some(&["model"])),
                tool("other", "other_only", false, Some(&["app"])),
            ]),
            script: Mutex::new(Script::Ok(text("ok"))),
            called: Mutex::new(Vec::new()),
            reads: AtomicUsize::new(0),
        }
    }

    pub(crate) fn set(&self, script: Script) {
        *self.script.lock().unwrap() = script;
    }

    pub(crate) fn calls(&self) -> usize {
        self.called.lock().unwrap().len()
    }
}

#[async_trait]
impl AppBridge for Bridge {
    async fn tools(&self) -> Result<Vec<AppTool>, AppError> {
        Ok(self.tools.lock().unwrap().clone())
    }

    async fn read_resource(&self, _server: &str, _uri: &str) -> Result<Value, AppError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"contents": []}))
    }

    async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<ToolAnswer, AppError> {
        self.called
            .lock()
            .unwrap()
            .push((server.to_owned(), tool.to_owned(), arguments));
        let mut script = self.script.lock().unwrap().clone();
        loop {
            match script {
                Script::Gated(gate, next) => {
                    gate.notified().await;
                    script = *next;
                }
                Script::Ok(result) => {
                    return Ok(ToolAnswer {
                        result,
                        proves_end: true,
                    });
                }
                Script::Unproven(result) => {
                    return Ok(ToolAnswer {
                        result,
                        proves_end: false,
                    });
                }
                Script::Err(e) => return Err(e),
            }
        }
    }
}

/// One session `s1` in a Git folder (or without a folder), with a fake bridge.
pub(crate) struct Rig {
    pub det: Det,
    pub _tmp: tempfile::TempDir,
    pub dir: PathBuf,
    pub store: Store,
    pub turns: Arc<SessionTurns>,
    pub bridge: Arc<Bridge>,
    pub env: AppEnv,
    pub host: AppHost,
}

pub(crate) fn proc(text: &str) -> crate::store::ProcessIncarnation {
    crate::store::ProcessIncarnation::from(text.to_owned())
}

impl Rig {
    /// A Codex session in a Git folder.
    pub(crate) async fn new() -> Self {
        Self::with(ProviderKind::Codex, true).await
    }

    pub(crate) async fn with(kind: ProviderKind, in_git: bool) -> Self {
        let det = Det::new();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("repo");
        std::fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.join("b.txt"), "two\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "init"]);
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
        store
            .create_session(NewSession {
                session_id: S1.into(),
                provider: kind,
                provider_instance: "p".into(),
                cwd: dir.display().to_string(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        let turns = SessionTurns::open(
            store.clone(),
            det.ids(),
            GitOptions::isolated(),
            Limits::default(),
            &Fences::default(),
            S1.into(),
            in_git.then_some(dir.as_path()),
            None,
            Arc::new(PrivatePaths::default()),
        )
        .await;
        turns.set_process(proc("proc-1"));
        let bridge = Arc::new(Bridge::new());
        let host = AppHost {
            bridge: Arc::clone(&bridge) as Arc<dyn AppBridge>,
            turns: Arc::clone(&turns),
            process: proc("proc-1"),
        };
        let env = AppEnv::new(store.clone(), det.ids());
        Self {
            det,
            _tmp: tmp,
            dir,
            store,
            turns,
            bridge,
            env,
            host,
        }
    }

    /// Stores one app of `server` and returns its id.
    pub(crate) async fn app_of(&self, server: &str) -> String {
        self.det.advance(Duration::from_millis(1));
        self.store
            .insert_app(
                NewApp {
                    session_id: S1.into(),
                    html: b"<html>x</html>".to_vec(),
                    csp: AppCsp::default(),
                    tool_input: Value::Null,
                    tool_result: None,
                },
                ToolAppEvent {
                    node_id: "n".into(),
                    server: server.to_owned(),
                    tool: "show_counter".to_owned(),
                    resource_uri: URI.to_owned(),
                    prefers_border: None,
                },
            )
            .await
            .unwrap()
    }

    pub(crate) async fn app(&self) -> String {
        self.app_of("demo").await
    }

    pub(crate) fn params(&self, app: &str, request: AppRequest, confirmed: bool) -> AppCallParams {
        AppCallParams {
            session_id: S1.into(),
            app_id: app.to_owned(),
            request,
            confirmed,
        }
    }

    pub(crate) fn tools_call(&self, app: &str, name: &str, confirmed: bool) -> AppCallParams {
        let request = AppRequest::ToolsCall {
            name: name.to_owned(),
            arguments: json!({"by": 1}),
        };
        self.params(app, request, confirmed)
    }

    pub(crate) async fn call(
        &self,
        app: &str,
        name: &str,
        confirmed: bool,
    ) -> Result<AppCallReply, AppFail> {
        let params = self.tools_call(app, name, confirmed);
        app_call(&self.env, Some(self.host.clone()), params).await
    }

    pub(crate) fn spawn_call(
        &self,
        app: &str,
        name: &str,
        confirmed: bool,
    ) -> JoinHandle<Result<AppCallReply, AppFail>> {
        let (env, host) = (self.env.clone(), self.host.clone());
        let params = self.tools_call(app, name, confirmed);
        tokio::spawn(async move { app_call(&env, Some(host), params).await })
    }

    /// The id of the oldest open row of the session (waits for it).
    pub(crate) async fn open_id(&self) -> String {
        until("an open call row", || async {
            let rows = self.store.app_calls_open(S1.into(), None).await.unwrap();
            rows.into_iter().next().map(|r| r.call_id)
        })
        .await
    }

    pub(crate) async fn row(&self, id: &str) -> AppCallRow {
        self.store.app_call(id.to_owned()).await.unwrap().unwrap()
    }

    /// Waits until the row is in `state`.
    pub(crate) async fn wait_state(&self, id: &str, state: CallState) -> AppCallRow {
        until("the call state", || async {
            let row = self.row(id).await;
            (row.state == state).then_some(row)
        })
        .await
    }

    /// Waits until the row is terminal and its advance ran.
    pub(crate) async fn wait_settled(&self, id: &str) -> AppCallRow {
        until("a settled call row", || async {
            let row = self.row(id).await;
            (row.state.is_terminal() && !row.advance_pending).then_some(row)
        })
        .await
    }

    /// Plants a row of `session` (state `running`), as an older incarnation
    /// when `incarnation` is given.
    pub(crate) async fn plant(&self, id: &str, process: &str, incarnation: Option<&str>) {
        self.store
            .app_call_begin(NewAppCall {
                call_id: id.to_owned(),
                session_id: S1.into(),
                app_id: format!("app-{id}"),
                worktree_root: self.turns.worktree_root().unwrap(),
                server: "demo".to_owned(),
                tool: "bump".to_owned(),
                read_only: false,
                process: process.to_owned(),
                incarnation: incarnation.map_or_else(
                    || self.store.incarnation().as_str().to_owned(),
                    str::to_owned,
                ),
            })
            .await
            .unwrap();
    }

    pub(crate) async fn plant_uncertain(&self, id: &str, process: &str) {
        self.plant(id, process, None).await;
        assert!(
            self.store
                .app_call_mark_uncertain(id.to_owned(), UncertainCause::ProviderError)
                .await
                .unwrap()
        );
    }
}

pub(crate) fn target_of(rig: &Rig) -> Arc<SessionTarget> {
    SessionTarget::new(rig.store.clone(), Adapters::new(), rig.det.ids()).shared()
}
