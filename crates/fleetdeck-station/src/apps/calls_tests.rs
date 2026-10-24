//! The app tool calls (B15 T5b, design `docs/v2/design/mcp-apps.md` §4.6 and
//! §4.6.1): the checks of `session.app_call` in order, the call row and its
//! outcome table, the permits, the end proof and the station start. The rig
//! (`calls_rig_tests.rs`) gives a fake bridge and a real Git folder for the
//! fence and the generation. The revert cases are in
//! `manager/revert_run_apps_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::apps::{AppCallReply, AppRequest, MAX_APP_ARGS_BYTES, MAX_APP_REPLY_BYTES};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::apps::AppError;
use serde_json::json;
use tokio::sync::Notify;

use super::rig::{Rig, S1, Script, URI, proc, target_of, text, tool, until};
use super::*;
use crate::manager::target::HostEnd;
use crate::store::app_calls::{AppCallBarrier, NewAppCall};
use crate::store::apps::MAX_APPS_PER_SESSION;
use crate::store::{NewSession, NewShim, OfflineApprovalPolicy, ShimState};

fn code_of(r: Result<AppCallReply, AppFail>) -> &'static str {
    r.expect_err("a refused call").code
}

#[tokio::test]
async fn app_call_unknown_app() {
    let rig = Rig::new().await;
    assert_eq!(
        code_of(rig.call("nope", "bump", true).await),
        code::UNKNOWN_APP
    );
    assert_eq!(rig.bridge.calls(), 0);
}

#[tokio::test]
async fn app_call_gone_after_caps_is_app_gone() {
    let rig = Rig::new().await;
    let first = rig.app().await;
    for _ in 0..MAX_APPS_PER_SESSION {
        rig.app().await;
    }
    assert_eq!(
        code_of(rig.call(&first, "bump", true).await),
        code::APP_GONE
    );
    assert_eq!(rig.bridge.calls(), 0);
}

#[tokio::test]
async fn app_call_session_stopped_never_starts() {
    let rig = Rig::new().await;
    let app = rig.app().await;
    let params = rig.tools_call(&app, "bump", true);
    let err = app_call(&rig.env, None, params).await.unwrap_err();
    assert_eq!(err.code, code::SESSION_STOPPED);
    assert_eq!(
        err.message,
        "The session that made this app is not running. Resume it to use the app."
    );
    assert_eq!(rig.bridge.calls(), 0);
    assert!(
        rig.store
            .app_calls_open(S1.into(), None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn app_call_refuses_app_only_tool_of_other_server_and_hidden_tool() {
    let rig = Rig::new().await;
    let app = rig.app().await;
    // A tool that only another server has, a tool hidden from apps, and an
    // unknown tool.
    for name in ["other_only", "model_only", "nothing"] {
        let err = rig.call(&app, name, true).await.unwrap_err();
        assert_eq!(err.code, code::TOOL_NOT_CALLABLE, "{name}");
        assert_eq!(err.message, "This app cannot call that tool.");
    }
    assert_eq!(rig.bridge.calls(), 0);
    assert!(
        rig.store
            .app_calls_open(S1.into(), None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn app_call_refuses_an_ambiguous_claude_pair() {
    let rig = Rig::with(ProviderKind::Claude, true).await;
    *rig.bridge.tools.lock().unwrap() =
        vec![tool("a", "b__c", true, None), tool("a__b", "c", true, None)];
    // Both pairs give the Claude name `mcp__a__b__c`: neither can be called.
    let first = rig.app_of("a").await;
    let second = rig.app_of("a__b").await;
    assert_eq!(
        code_of(rig.call(&first, "b__c", true).await),
        code::TOOL_NOT_CALLABLE
    );
    assert_eq!(
        code_of(rig.call(&second, "c", true).await),
        code::TOOL_NOT_CALLABLE
    );
    assert_eq!(rig.bridge.calls(), 0);
    // The same tools under Codex are addressed by the pair itself.
    let codex = Rig::new().await;
    *codex.bridge.tools.lock().unwrap() = vec![tool("a", "b__c", true, None)];
    let app = codex.app_of("a").await;
    assert!(codex.call(&app, "b__c", true).await.is_ok());
}

#[tokio::test]
async fn app_call_non_read_only_needs_confirmation_then_runs() {
    let rig = Rig::new().await;
    let app = rig.app().await;
    let reply = rig.call(&app, "bump", false).await.unwrap();
    assert_eq!(
        reply,
        AppCallReply::Confirm {
            tool: "bump".to_owned(),
            title: Some("BUMP".to_owned())
        }
    );
    assert_eq!(rig.bridge.calls(), 0);
    assert!(
        rig.store
            .app_calls_open(S1.into(), None)
            .await
            .unwrap()
            .is_empty()
    );
    let gate = Arc::new(Notify::new());
    rig.bridge.set(Script::Gated(
        Arc::clone(&gate),
        Box::new(Script::Ok(text("bumped"))),
    ));
    let handle = rig.spawn_call(&app, "bump", true);
    let id = rig.open_id().await;
    let row = rig.row(&id).await;
    assert!(!row.read_only);
    assert_eq!(row.generation, Some(1));
    assert_eq!(row.process, "proc-1");
    gate.notify_waiters();
    assert_eq!(
        handle.await.unwrap().unwrap(),
        AppCallReply::Done {
            result: text("bumped")
        }
    );
    let row = rig.wait_settled(&id).await;
    assert_eq!(row.state, CallState::Done);
    assert_eq!(
        rig.bridge.called.lock().unwrap()[0],
        ("demo".to_owned(), "bump".to_owned(), json!({"by": 1}))
    );
}

#[tokio::test]
async fn app_call_read_only_runs_at_once() {
    let rig = Rig::new().await;
    let app = rig.app().await;
    let gate = Arc::new(Notify::new());
    rig.bridge.set(Script::Gated(
        Arc::clone(&gate),
        Box::new(Script::Ok(text("2"))),
    ));
    let handle = rig.spawn_call(&app, "show_counter", false);
    let id = rig.open_id().await;
    let row = rig.row(&id).await;
    assert!(row.read_only);
    assert_eq!(row.generation, None);
    gate.notify_waiters();
    assert_eq!(
        handle.await.unwrap().unwrap(),
        AppCallReply::Done { result: text("2") }
    );
    let row = rig.wait_settled(&id).await;
    assert_eq!(row.state, CallState::Done);
    assert!(!row.advance_pending);
}

#[tokio::test]
async fn app_call_resources_read_needs_ui_scheme() {
    let rig = Rig::new().await;
    let app = rig.app().await;
    let read = |uri: String| {
        let params = rig.params(&app, AppRequest::ResourcesRead { uri }, false);
        app_call(&rig.env, Some(rig.host.clone()), params)
    };
    for bad in [
        "https://x/y".to_owned(),
        format!("ui://{}", "a".repeat(5000)),
    ] {
        assert_eq!(code_of(read(bad).await), code::INVALID_PARAMS);
    }
    assert_eq!(rig.bridge.reads.load(Ordering::SeqCst), 0);
    assert!(read(URI.to_owned()).await.is_ok());
    assert_eq!(rig.bridge.reads.load(Ordering::SeqCst), 1);
    // A read makes no row.
    assert!(
        rig.store
            .app_calls_open(S1.into(), None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn app_call_rate_and_concurrency_busy() {
    let rig = Rig::new().await;
    let app = rig.app().await;
    let gate = Arc::new(Notify::new());
    rig.bridge.set(Script::Gated(
        Arc::clone(&gate),
        Box::new(Script::Ok(text("x"))),
    ));
    let first = rig.spawn_call(&app, "bump", true);
    let second = rig.spawn_call(&app, "bump", true);
    until("two tasks", || async {
        (rig.env.limits.tasks(&S1.into()) == 2).then_some(())
    })
    .await;
    let busy = rig.call(&app, "bump", true).await.unwrap_err();
    assert_eq!(busy.code, code::BUSY);
    assert_eq!(
        busy.message,
        "The app sends too many requests. Wait a moment."
    );
    gate.notify_waiters();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    until("the permits", || async {
        (rig.env.limits.tasks(&S1.into()) == 0).then_some(())
    })
    .await;
    // 30 calls in a minute; the 31st is busy (2 already counted above).
    rig.bridge.set(Script::Ok(text("x")));
    for n in 0..(CALLS_PER_MINUTE - 2) {
        // The task of the call before holds its permit until it settled.
        until("the permits", || async {
            (rig.env.limits.tasks(&S1.into()) == 0).then_some(())
        })
        .await;
        rig.call(&app, "show_counter", false)
            .await
            .unwrap_or_else(|e| panic!("call {n}: {e}"));
    }
    assert_eq!(
        code_of(rig.call(&app, "show_counter", false).await),
        code::BUSY
    );
}

#[tokio::test]
async fn app_call_too_large_and_timeout() {
    let rig = Rig::new().await;
    let app = rig.app().await;
    let big_args = AppRequest::ToolsCall {
        name: "bump".to_owned(),
        arguments: json!({"x": "a".repeat(MAX_APP_ARGS_BYTES + 1)}),
    };
    let params = rig.params(&app, big_args, true);
    let err = app_call(&rig.env, Some(rig.host.clone()), params)
        .await
        .unwrap_err();
    assert_eq!(err.code, code::INVALID_PARAMS);
    let not_object = AppRequest::ToolsCall {
        name: "bump".to_owned(),
        arguments: json!([1]),
    };
    let params = rig.params(&app, not_object, true);
    let err = app_call(&rig.env, Some(rig.host.clone()), params)
        .await
        .unwrap_err();
    assert_eq!(err.code, code::INVALID_PARAMS);
    assert_eq!(rig.bridge.calls(), 0);
    // An answer over the reply cap is `too_large`; the row is still settled.
    rig.bridge
        .set(Script::Ok(text(&"a".repeat(MAX_APP_REPLY_BYTES + 1))));
    let err = rig.call(&app, "show_counter", false).await.unwrap_err();
    assert_eq!(err.code, code::TOO_LARGE);
    assert_eq!(err.message, "The answer is too large for the app.");
}

#[tokio::test]
async fn app_call_timeout_keeps_row_and_permit_until_the_answer() {
    let mut rig = Rig::new().await;
    rig.env.reply_wait = Duration::from_millis(200);
    let app = rig.app().await;
    let gate = Arc::new(Notify::new());
    rig.bridge.set(Script::Gated(
        Arc::clone(&gate),
        Box::new(Script::Ok(text("late"))),
    ));
    for _ in 0..2 {
        let err = rig.call(&app, "bump", true).await.unwrap_err();
        assert_eq!(err.code, code::TIMEOUT);
        assert_eq!(
            err.message,
            "The tool did not answer in 60 s. It may still run. A revert of this folder waits \
             until it ends."
        );
    }
    let rows = rig.store.app_calls_open(S1.into(), None).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.state == CallState::Running));
    assert_eq!(rig.env.limits.tasks(&S1.into()), 2);
    // A third call is busy while both tasks wait.
    assert_eq!(code_of(rig.call(&app, "bump", true).await), code::BUSY);
    // The late answer settles the rows, advances the generation and frees the
    // permits.
    gate.notify_waiters();
    for row in rows {
        let row = rig.wait_settled(&row.call_id).await;
        assert_eq!(row.state, CallState::Done);
    }
    until("the permits", || async {
        (rig.env.limits.tasks(&S1.into()) == 0).then_some(())
    })
    .await;
}

#[tokio::test]
async fn app_call_busy_when_the_fence_is_held() {
    let mut rig = Rig::new().await;
    rig.env.fence_wait = Duration::from_millis(150);
    let app = rig.app().await;
    let fence = rig.turns.fence().await;
    let err = rig.call(&app, "bump", true).await.unwrap_err();
    drop(fence);
    assert_eq!(err.code, code::BUSY);
    assert_eq!(
        err.message,
        "The session folder is busy (a revert or a checkpoint runs). Try again in a moment."
    );
    assert_eq!(rig.bridge.calls(), 0);
    assert!(
        rig.store
            .app_calls_open(S1.into(), None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(rig.env.limits.tasks(&S1.into()), 0);
}

#[tokio::test]
async fn app_call_without_worktree_has_no_row() {
    let rig = Rig::with(ProviderKind::Codex, false).await;
    assert!(rig.turns.worktree_root().is_none());
    let app = rig.app().await;
    let reply = rig.call(&app, "bump", true).await.unwrap();
    assert_eq!(reply, AppCallReply::Done { result: text("ok") });
    assert_eq!(rig.bridge.calls(), 1);
    assert!(
        rig.store
            .app_calls_open(S1.into(), None)
            .await
            .unwrap()
            .is_empty()
    );
    until("the permit", || async {
        (rig.env.limits.tasks(&S1.into()) == 0).then_some(())
    })
    .await;
}

#[tokio::test]
async fn app_call_process_end_settles_ended() {
    let rig = Rig::new().await;
    let app = rig.app().await;
    let gate = Arc::new(Notify::new());
    rig.bridge.set(Script::Gated(
        Arc::clone(&gate),
        Box::new(Script::Err(AppError::Ended)),
    ));
    let handle = rig.spawn_call(&app, "bump", true);
    let id = rig.open_id().await;
    gate.notify_waiters();
    assert_eq!(
        handle.await.unwrap().unwrap_err().code,
        code::SESSION_STOPPED
    );
    let row = rig.wait_settled(&id).await;
    assert_eq!(row.state, CallState::Ended);
}

#[tokio::test]
async fn app_call_provider_error_of_a_read_only_tool_settles_failed() {
    let rig = Rig::new().await;
    let app = rig.app().await;
    let gate = Arc::new(Notify::new());
    let boom = Script::Err(AppError::Answered("boom\u{7}".to_owned()));
    rig.bridge
        .set(Script::Gated(Arc::clone(&gate), Box::new(boom)));
    let handle = rig.spawn_call(&app, "show_counter", false);
    let id = rig.open_id().await;
    gate.notify_waiters();
    let err = handle.await.unwrap().unwrap_err();
    assert_eq!(err.code, code::PROVIDER_ERROR);
    assert_eq!(err.message, "The provider reported an error: boom");
    let row = rig.wait_settled(&id).await;
    assert_eq!(row.state, CallState::Failed);
    assert!(!row.advance_pending);
}

#[tokio::test]
async fn app_call_is_error_result_without_proof_is_uncertain() {
    let rig = Rig::new().await;
    let app = rig.app().await;
    // A non-read-only row: the app gets the result, the row is `uncertain`.
    let gate = Arc::new(Notify::new());
    let result = json!({"isError": true, "content": []});
    rig.bridge.set(Script::Gated(
        Arc::clone(&gate),
        Box::new(Script::Unproven(result.clone())),
    ));
    let handle = rig.spawn_call(&app, "bump", true);
    let id = rig.open_id().await;
    gate.notify_waiters();
    assert_eq!(
        handle.await.unwrap().unwrap(),
        AppCallReply::Done {
            result: result.clone()
        }
    );
    let row = rig.wait_state(&id, CallState::Uncertain).await;
    assert_eq!(row.cause, Some(UncertainCause::ProviderError));
    // A read-only row with the same answer is `done`.
    let gate = Arc::new(Notify::new());
    rig.bridge.set(Script::Gated(
        Arc::clone(&gate),
        Box::new(Script::Unproven(result)),
    ));
    let handle = rig.spawn_call(&app, "show_counter", false);
    until("the second row", || async {
        let rows = rig.store.app_calls_open(S1.into(), None).await.unwrap();
        rows.into_iter()
            .find(|r| r.call_id != id)
            .map(|r| r.call_id)
    })
    .await;
    gate.notify_waiters();
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn app_call_busy_with_eight_uncertain_rows() {
    let rig = Rig::new().await;
    let app = rig.app().await;
    for n in 0..MAX_UNCERTAIN_PER_SESSION {
        rig.plant_uncertain(&format!("u{n}"), "proc-1").await;
    }
    let err = rig.call(&app, "show_counter", false).await.unwrap_err();
    assert_eq!(err.code, code::BUSY);
    assert_eq!(
        err.message,
        "Too many app actions of this session may still run. Mark them as stopped, or stop the \
         session."
    );
    assert_eq!(rig.bridge.calls(), 0);
    // A read of a resource is not a tool call.
    let params = rig.params(
        &app,
        AppRequest::ResourcesRead {
            uri: URI.to_owned(),
        },
        false,
    );
    assert!(
        app_call(&rig.env, Some(rig.host.clone()), params)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn process_end_settles_uncertain_calls_of_that_process_only() {
    let rig = Rig::new().await;
    rig.plant_uncertain("old", "proc-1").await;
    rig.plant_uncertain("new", "proc-2").await;
    let session: SessionId = S1.into();
    assert_eq!(
        process_ended(&rig.store, &session, "proc-1").await.unwrap(),
        1
    );
    assert_eq!(rig.row("old").await.state, CallState::Ended);
    assert_eq!(rig.row("new").await.state, CallState::Uncertain);
    // A detach settles nothing and records no exit.
    let target = target_of(&rig);
    target.register_app_host(&session, rig.host.clone());
    assert!(target.drop_app_host(&session, &proc("proc-1"), HostEnd::Detached));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.row("new").await.state, CallState::Uncertain);
    assert!(!target.app_env().exits.has(&session, "proc-2"));
    // The observed exit of the process settles its rows and is recorded.
    target.drop_app_host(&session, &proc("proc-2"), HostEnd::Exited);
    rig.wait_state("new", CallState::Ended).await;
    assert!(target.app_env().exits.has(&session, "proc-2"));
}

/// A non-read-only call of `proc-1` whose provider answer is an error, paused
/// right after the answer; `end` happens while it is paused. Returns the row
/// state after the resume.
async fn paused_call_after(end: Option<(HostEnd, &str)>) -> CallState {
    let rig = Rig::new().await;
    let target = target_of(&rig);
    let session: SessionId = S1.into();
    target.register_app_host(&session, rig.host.clone());
    let (reached, resume) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let mut env = target.app_env();
    env.hooks.before_record = Some((Arc::clone(&reached), Arc::clone(&resume)));
    let app = rig.app().await;
    rig.bridge.set(Script::Err(AppError::Answered(
        "MCP server demo tool slow_bump timed out".to_owned(),
    )));
    let params = rig.tools_call(&app, "slow_bump", true);
    let host = target.app_host(&session).unwrap();
    let handle = tokio::spawn(async move { app_call(&env, Some(host), params).await });
    reached.notified().await;
    let id = rig.open_id().await;
    assert_eq!(rig.row(&id).await.state, CallState::Running);
    if let Some((how, process)) = end {
        target.drop_app_host(&session, &proc(process), how);
        // The `process_ended` task finds no `uncertain` row yet.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(rig.row(&id).await.state, CallState::Running);
    }
    resume.notify_one();
    let err = handle.await.unwrap().unwrap_err();
    assert_eq!(err.code, code::PROVIDER_ERROR);
    // The task settles or marks the row after the resume.
    until("the row leaves running", || async {
        let row = rig.row(&id).await;
        (row.state != CallState::Running).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    rig.row(&id).await.state
}

#[tokio::test]
async fn process_end_settles_a_call_marked_uncertain_after_the_exit() {
    // Only the recorded exit of that process ends the row.
    assert_eq!(
        paused_call_after(Some((HostEnd::Exited, "proc-1"))).await,
        CallState::Ended
    );
    // A detach leaves it `uncertain`; so does the exit of another process.
    assert_eq!(
        paused_call_after(Some((HostEnd::Detached, "proc-1"))).await,
        CallState::Uncertain
    );
    assert_eq!(
        paused_call_after(Some((HostEnd::Exited, "proc-2"))).await,
        CallState::Uncertain
    );
    assert_eq!(paused_call_after(None).await, CallState::Uncertain);
}

#[tokio::test]
async fn start_up_marks_unproven_calls_uncertain_and_settles_proven_ones() {
    let rig = Rig::new().await;
    // A direct-host row of an older station process: `uncertain`.
    rig.plant("direct", "proc-1", Some("older")).await;
    // A row of a session under a shim whose newest row proves the end: `ended`.
    rig.store
        .create_session(NewSession {
            session_id: "s2".into(),
            provider: ProviderKind::Codex,
            provider_instance: "p".into(),
            cwd: rig.dir.display().to_string(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    rig.store
        .shim_starting(NewShim {
            shim_id: "shim-1".to_owned(),
            session_id: "s2".into(),
            socket_path: PathBuf::from("/run/x/shim-1.sock"),
            launch_boot_id: Some("boot-1".to_owned()),
            native_target: None,
        })
        .await
        .unwrap();
    rig.store
        .shim_end(
            "shim-1".to_owned(),
            ShimState::NeverStarted,
            "shim_never_started".to_owned(),
            0,
        )
        .await
        .unwrap();
    rig.store
        .app_call_begin(NewAppCall {
            call_id: "shimmed".to_owned(),
            session_id: "s2".into(),
            app_id: "app-s2".to_owned(),
            worktree_root: rig.turns.worktree_root().unwrap(),
            server: "demo".to_owned(),
            tool: "bump".to_owned(),
            read_only: false,
            process: "proc-9".to_owned(),
            station_run: "older".to_owned(),
        })
        .await
        .unwrap();
    // A row of this station process is not touched.
    rig.plant("current", "proc-1", None).await;
    start_up(&rig.store).await.unwrap();
    let direct = rig.row("direct").await;
    assert_eq!(direct.state, CallState::Uncertain);
    assert_eq!(direct.cause, Some(UncertainCause::StationRestart));
    assert_eq!(rig.row("shimmed").await.state, CallState::Ended);
    assert!(rig.row("shimmed").await.advance_pending);
    assert_eq!(rig.row("current").await.state, CallState::Running);
    // The folder barrier holds while a row is unsettled, and the advance of
    // the settled one is still pending.
    assert_eq!(
        rig.store
            .app_call_barrier(rig.turns.worktree_root().unwrap(), "no-turn".to_owned())
            .await
            .unwrap(),
        Some(AppCallBarrier::Running)
    );
    // A later start-up changes no `uncertain` row.
    start_up(&rig.store).await.unwrap();
    assert_eq!(
        rig.row("direct").await.cause,
        Some(UncertainCause::StationRestart)
    );
}

#[tokio::test]
async fn settle_proven_ends_uncertain_calls_of_an_ended_shim() {
    let rig = Rig::new().await;
    rig.plant_uncertain("u1", "proc-1").await;
    // Without a shim row nothing is proven.
    settle_proven(&rig.store).await.unwrap();
    assert_eq!(rig.row("u1").await.state, CallState::Uncertain);
    rig.store
        .shim_starting(NewShim {
            shim_id: "shim-1".to_owned(),
            session_id: S1.into(),
            socket_path: PathBuf::from("/run/x/shim-1.sock"),
            launch_boot_id: Some("boot-1".to_owned()),
            native_target: None,
        })
        .await
        .unwrap();
    // A shim that still starts proves nothing.
    settle_proven(&rig.store).await.unwrap();
    assert_eq!(rig.row("u1").await.state, CallState::Uncertain);
    rig.store
        .shim_end(
            "shim-1".to_owned(),
            ShimState::NeverStarted,
            "shim_never_started".to_owned(),
            0,
        )
        .await
        .unwrap();
    settle_proven(&rig.store).await.unwrap();
    assert_eq!(rig.row("u1").await.state, CallState::Ended);
}
