//! The execution of `!` shell commands of Claude sessions and their results
//! in the model context (B8): a real `sh` command in a temp Git repo, with
//! `fake-claude sim` as the provider.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::command::{CommandOrigin, RETRY_WINDOW_MS, SessionCommand, StopMode};
use fleetdeck_proto::event::{Event, ProviderKind, ToolStatus};
use fleetdeck_proto::{CommandEnvelope, CommandId, EventBody, SessionId};

use super::shell_advance;
use crate::coordinator::Bindings;
use crate::store::shell_runs::ShellState;
use crate::store::{Caller, EffectPayload, OfflineApprovalPolicy, ReadResult, Store, StoreConfig};

#[path = "shell_run_rig.rs"]
pub(in crate::manager) mod rig;
use rig::*;

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
    rig.shutdown().await;
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
    rig.shutdown().await;
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
    rig.shutdown().await;
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
    rig.shutdown().await;
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
    rig.shutdown().await;
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
    rig.shutdown().await;
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
    rig.shutdown().await;
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
    let row = rig
        .row_is(&id, |r| r.state.is_terminal() && !r.advance_pending)
        .await;
    assert_eq!(row.state, ShellState::Done, "{row:?}");
    let free = tokio::time::timeout(Duration::from_secs(5), fence.lock()).await;
    assert!(free.is_ok(), "the fence is released at the end");
    drop(free); // the advance task and the shutdown need the fence
    rig.shutdown().await;
}

#[tokio::test]
async fn the_shutdown_cancels_a_running_command() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let id = rig.bang(&s, "echo $$ > pid.txt; sleep 30").await;
    let pid = read_pid(&rig.app().join("pid.txt")).await;
    let began = tokio::time::Instant::now();
    rig.shutdown().await;
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
                        timeout_ms: 600_000,
                    },
                )?
                .effect_id
                .as_str()
                .to_owned();
            d.shell_insert(&id, &owner, ProviderKind::Claude, "/w", "sleep 60")?;
            Ok(fleetdeck_proto::command::CommandResult {
                session_id: Some(owner.clone()),
                input_id: None,
                seq: None,
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
    rig.shutdown().await;
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
    rig.shutdown().await;
}

#[tokio::test]
async fn an_oversize_result_is_cut_and_every_named_result_reaches_the_model() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    // 30,000 `<` escape to 120,000 bytes: each result is cut to its third of 64 KiB (B8).
    let big = "head -c 30000 /dev/zero | tr '\\0' '<'";
    let ids = [
        rig.run(&s, big).await.effect_id,
        rig.run(&s, big).await.effect_id,
        rig.run(&s, big).await.effect_id,
    ];
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let context = rig.prompt_with(&s, "x", &refs).await;
    assert_eq!(context.len(), 3, "no named result is deferred");
    for result in &context {
        assert!(result.stdout.starts_with("[… "), "{}", &result.stdout[..40]);
        assert!(result.stdout.contains(" characters omitted]\n"));
        let size =
            serde_json::to_string(&fleetdeck_providers::claude::control::shell_blocks(result))
                .unwrap()
                .len();
        assert!(size <= 64 * 1024 / 3, "{size}");
    }
    for id in &ids {
        assert!(rig.row(id).await.included_in.is_some(), "{id}");
    }
    rig.shutdown().await;
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
    rig.shutdown().await;
}

#[path = "shell_run_race_tests.rs"]
mod race;
