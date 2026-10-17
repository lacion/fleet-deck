//! The source side of a session move (FG-9, design §12.2, §12.3 steps 1, 2
//! and 5, §12.4, §12.4A): the hold, the freeze, the export plan, the finish,
//! the abort and the undo, through the session manager with real Git.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, UserInput,
};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::fleet_git::{MoveStatusState, PushState};
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_proto::{CommandId, Event, RejectCode};

use super::super::rig::{Rig, ended, policy, rejected};
use crate::manager::fleet_git_cleanup::{CleanupCtx, run_once as cleanup_once};
use crate::manager::{Adapters, SessionManager};
use crate::shell::{ShellConfig, Shells};
use crate::store::move_runs::MoveOutState;
use crate::store::session_holds::{HELD, HELD_MOVE, KIND_LAND, KIND_MOVE};
use crate::store::{Caller, Decided, NewEvent};
use crate::workspace::PrivatePaths;

const NATIVE: &str = "11111111-2222-3333-4444-555555555555";
const NOT_PUSHED: &str = "move_not_ready: The branch is not pushed.";

struct Fx {
    rig: Rig,
    manager: Arc<SessionManager>,
    claude_home: PathBuf,
}

impl Fx {
    /// A rig and a manager on its store (started before any session, because
    /// its recovery ends the sessions that have no process).
    async fn new() -> Option<Self> {
        let rig = Rig::new(false).await?;
        let claude_home = rig.root.join("claude-home");
        let mut env = ChildEnv::empty();
        env.set("CLAUDE_CONFIG_DIR", &claude_home).unwrap();
        let policy = policy(&rig.root, false, true)
            .with_setup_envs(BTreeMap::from([("claude".into(), env)]));
        let manager = SessionManager::start(
            rig.store.clone(),
            policy,
            Adapters::new(),
            rig.det.ids(),
            Arc::new(PrivatePaths::default()),
        )
        .await
        .unwrap();
        Some(Self {
            rig,
            manager: Arc::new(manager),
            claude_home,
        })
    }

    async fn send(
        &self,
        id: &str,
        origin: CommandOrigin,
        authority: Option<CommandAuthority>,
        command: SessionCommand,
    ) -> Decided {
        let envelope = CommandEnvelope {
            authority,
            command_id: CommandId::from(id.to_owned()),
            target_incarnation: None,
            retry_until: self.rig.store.now_ms() + RETRY_WINDOW_MS,
            origin,
            command,
        };
        self.manager
            .submit(envelope, Caller::local())
            .await
            .unwrap()
    }

    async fn board(&self, id: &str, command: SessionCommand) -> Decided {
        self.send(id, CommandOrigin::Board, None, command).await
    }

    async fn accepted(&self, id: &str, command: SessionCommand) -> Decided {
        let decided = self.board(id, command).await;
        assert_eq!(
            decided.receipt.state,
            ReceiptState::Accepted,
            "{:?}",
            decided.receipt.reject
        );
        decided
    }

    async fn refused(&self, id: &str, command: SessionCommand) -> (RejectCode, String) {
        rejected(&self.board(id, command).await)
    }

    /// A running idle session with its own branch, its worktree and, when
    /// `native`, a transcript below the provider home of the source.
    async fn session(&self, id: &str, native: bool) -> (PathBuf, String) {
        let wt = self.rig.session(id, true).await;
        let mut refs = fleetdeck_proto::event::ProviderRefs::new();
        if native {
            refs.insert("session_id".to_owned(), serde_json::Value::from(NATIVE));
        }
        let started: NewEvent = Event::SessionStarted {
            provider: fleetdeck_proto::event::ProviderKind::Claude,
            provider_instance: "claude".into(),
            provider_version: None,
            model: None,
            cwd: wt.display().to_string(),
            mode: None,
            capabilities: Box::new(fleetdeck_proto::capability::Capabilities::minimal()),
            resumed: None,
            provider_refs: refs,
            title: None,
            branch: None,
            repo: None,
        }
        .into();
        self.rig
            .store
            .append(id.into(), vec![started])
            .await
            .unwrap();
        if native {
            let dir = self
                .claude_home
                .join("projects")
                .join(crate::history::locate::encode_cwd(
                    &wt.display().to_string(),
                ));
            std::fs::create_dir_all(&dir).unwrap();
            let line = format!("{{\"sessionId\":\"{NATIVE}\",\"type\":\"user\"}}\n");
            std::fs::write(dir.join(format!("{NATIVE}.jsonl")), line).unwrap();
        }
        let tip = self.mark_pushed(id, &wt).await;
        (wt, tip)
    }

    /// The push row says that the tip of the branch is pushed.
    async fn mark_pushed(&self, id: &str, wt: &Path) -> String {
        let tip = self.rig.git(wt, &["rev-parse", "HEAD"]);
        let store = &self.rig.store;
        store
            .git_push_trigger(id.into(), tip.clone())
            .await
            .unwrap()
            .unwrap();
        store
            .git_push_open_attempt(id.into(), self.rig.url.clone())
            .await
            .unwrap();
        store.git_push_applied(id.into()).await.unwrap();
        tip
    }

    async fn end(&self, id: &str) {
        self.rig
            .store
            .append(id.into(), vec![ended()])
            .await
            .unwrap();
    }

    async fn summary(&self, id: &str) -> fleetdeck_proto::SessionSummary {
        self.rig.store.summary(id.into()).await.unwrap().unwrap()
    }

    async fn push_state(&self, id: &str) -> (PushState, Option<String>) {
        let row = self.rig.store.git_push(id.into()).await.unwrap().unwrap();
        (row.state, row.stop_code)
    }
}

fn out(session: &str, move_id: &str) -> SessionCommand {
    SessionCommand::MoveOut {
        session_id: session.into(),
        move_id: move_id.to_owned(),
        to_station: "station-b".into(),
    }
}

fn finish(session: &str, move_id: &str) -> SessionCommand {
    SessionCommand::MoveFinish {
        session_id: session.into(),
        move_id: move_id.to_owned(),
        to_station: "station-b".into(),
        to_session: "t1".into(),
    }
}

fn undo(session: &str, move_id: &str) -> SessionCommand {
    SessionCommand::MoveUndo {
        session_id: session.into(),
        move_id: move_id.to_owned(),
    }
}

fn state(text: &str) -> (RejectCode, String) {
    (RejectCode::InvalidState, text.to_owned())
}

#[tokio::test]
async fn an_ended_pushed_session_is_held_with_its_publication() {
    let Some(fx) = Fx::new().await else { return };
    let (_, tip) = fx.session("s1", true).await;
    fx.end("s1").await;
    let decided = fx.accepted("c1", out("s1", "m1")).await;
    // Nothing runs, so nothing is stopped.
    assert!(decided.effects.is_empty());
    let row = fx
        .rig
        .store
        .move_out_run("m1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, MoveOutState::Holding);
    assert_eq!(row.command_id, "c1");
    assert_eq!(row.to_station, "station-b");
    assert_eq!(row.session_id.as_str(), "s1");
    assert_eq!(row.publication.local_branch, "fd/alpha/s1");
    assert_eq!(row.publication.ref_name, "fd/alpha/s1");
    assert_eq!(row.publication.dest, fx.rig.remote.dest);
    assert_eq!(row.publication.oid, tip);
    let hold = fx
        .rig
        .store
        .session_hold("s1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (hold.kind.as_str(), hold.holder_id.as_str()),
        (KIND_MOVE, "m1")
    );
    // The same command id again changes nothing.
    let again = fx.board("c1", out("s1", "m1")).await;
    assert!(again.duplicate);
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn a_live_idle_session_is_held_and_gets_a_graceful_stop() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("s1", true).await;
    fx.rig
        .store
        .process_started("s1".into(), "p1".into(), None)
        .await
        .unwrap();
    let decided = fx.accepted("c1", out("s1", "m1")).await;
    assert_eq!(decided.effects.len(), 1);
    assert_eq!(decided.effects[0].payload.kind(), "stop");
    assert!(
        fx.rig
            .store
            .session_hold("s1".into())
            .await
            .unwrap()
            .is_some()
    );
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn the_decision_refuses_what_the_design_names() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("ok", true).await;
    // A busy session (a start that has no idle state yet).
    fx.rig.session("busy", true).await;
    // A session whose branch is not pushed.
    fx.rig.session("np", false).await;
    // A session with a newer tip than the pushed one.
    let (wt, _) = fx.session("newer", true).await;
    let newer = fx.rig.commit(&wt, "more.txt", "more");
    fx.rig
        .store
        .git_push_trigger("newer".into(), newer)
        .await
        .unwrap();
    // A session without a branch of its own.
    fx.rig.record("plain", false, None).await;
    // A session that a land holds.
    fx.session("landed", true).await;
    fx.rig
        .store
        .session_hold_insert("landed".into(), KIND_LAND, "land-1".to_owned())
        .await
        .unwrap();

    let busy = state("move_not_ready: A turn is running.");
    assert_eq!(fx.refused("a", out("busy", "m-busy")).await, busy);
    assert_eq!(fx.refused("b", out("np", "m-np")).await, state(NOT_PUSHED));
    assert_eq!(
        fx.refused("c", out("newer", "m-newer")).await,
        state(NOT_PUSHED)
    );
    assert_eq!(
        fx.refused("d", out("plain", "m-plain")).await,
        state(NOT_PUSHED)
    );
    assert_eq!(fx.refused("e", out("landed", "m-land")).await, state(HELD));
    assert_eq!(
        fx.refused("f", out("nobody", "m-none")).await,
        (RejectCode::UnknownSession, "no such session".to_owned())
    );
    assert_eq!(
        fx.refused("g", out("ok", "bad id")).await,
        (
            RejectCode::InvalidCommand,
            "invalid_params: The move id is not valid.".to_owned()
        )
    );
    // The origin: only the board and a coordinator move a session.
    for (n, origin) in [CommandOrigin::Local, CommandOrigin::Mail]
        .into_iter()
        .enumerate()
    {
        let decided = fx
            .send(&format!("o{n}"), origin, None, out("ok", "m-origin"))
            .await;
        assert_eq!(rejected(&decided).0, RejectCode::Forbidden);
    }
    // A second move of the same session, and a move id that exists.
    fx.accepted("h", out("ok", "m-ok")).await;
    assert_eq!(
        fx.refused("i", out("ok", "m-two")).await,
        state("move_not_ready: The session is already moving.")
    );
    fx.session("other", true).await;
    assert_eq!(
        fx.refused("j", out("other", "m-ok")).await,
        (
            RejectCode::InvalidCommand,
            "invalid_params: This move id exists already.".to_owned()
        )
    );
    // A coordinator move needs fleet control on this station.
    fx.manager
        .policy_hub()
        .swap(policy(&fx.rig.root, false, false));
    let authority = CommandAuthority {
        grant_id: "g1".to_owned(),
        caller_station: "other".into(),
        caller_session: "caller".into(),
        generation: 1,
    };
    let decided = fx
        .send(
            "k",
            CommandOrigin::Coordinator,
            Some(authority),
            out("other", "m-coord"),
        )
        .await;
    assert_eq!(rejected(&decided).0, RejectCode::Forbidden);
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn the_hold_refuses_input_with_its_own_text_and_a_stop_goes_through() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("s1", true).await;
    fx.accepted("c1", out("s1", "m1")).await;
    let input = || UserInput::new("hi".to_owned());
    let commands = [
        SessionCommand::Prompt {
            session_id: "s1".into(),
            input: input(),
        },
        SessionCommand::Steer {
            session_id: "s1".into(),
            input: input(),
        },
        SessionCommand::FollowUp {
            session_id: "s1".into(),
            input: input(),
        },
        SessionCommand::WorktreeTrash {
            session_id: "s1".into(),
        },
    ];
    for (n, command) in commands.into_iter().enumerate() {
        let method = command.method();
        let got = fx.refused(&format!("x{n}"), command).await;
        assert_eq!(got, state(HELD_MOVE), "{method}");
    }
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn finish_makes_the_session_moved_stops_the_push_row_and_ends_the_hold() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("s1", true).await;
    fx.end("s1").await;
    fx.accepted("c1", out("s1", "m1")).await;
    fx.accepted("c2", finish("s1", "m1")).await;
    let summary = fx.summary("s1").await;
    let moved = summary.moved_to.unwrap();
    assert_eq!(
        (
            moved.move_id.as_str(),
            moved.station_id.as_str(),
            moved.session_id.as_str()
        ),
        ("m1", "station-b", "t1")
    );
    assert_eq!(summary.state, SessionState::Exited);
    assert_eq!(
        fx.push_state("s1").await,
        (PushState::Stopped, Some("moved".to_owned()))
    );
    assert!(
        fx.rig
            .store
            .session_hold("s1".into())
            .await
            .unwrap()
            .is_none()
    );
    let row = fx
        .rig
        .store
        .move_out_run("m1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, MoveOutState::Finished);
    // A moved session refuses input, a land and a move, but not the trash.
    let moved_text = "The session moved to station-b. Open it there.".to_owned();
    for (n, command) in [
        SessionCommand::Prompt {
            session_id: "s1".into(),
            input: UserInput::new("hi".to_owned()),
        },
        out("s1", "m2"),
        Rig::land_command("s1", "land-1", fx.rig.plan("s1")),
    ]
    .into_iter()
    .enumerate()
    {
        let method = command.method();
        assert_eq!(
            fx.refused(&format!("y{n}"), command).await,
            state(&moved_text),
            "{method}"
        );
    }
    let trash = fx
        .board(
            "trash",
            SessionCommand::WorktreeTrash {
                session_id: "s1".into(),
            },
        )
        .await;
    if let Some(reject) = &trash.receipt.reject {
        assert_ne!(
            reject.message, moved_text,
            "the trash is not refused as moved"
        );
    }
    // The cleanup loop skips a moved session: no state, no trash.
    let options = fx.rig.options();
    let ctx = CleanupCtx {
        store: fx.rig.store.clone(),
        policy: Arc::clone(&fx.rig.hub),
        shells: Arc::new(Shells::new(ShellConfig::default())),
        options: Arc::new(move || options.clone()),
    };
    let long_after = fx.rig.store.now_ms() + 100 * 3_600_000;
    assert!(cleanup_once(&ctx, long_after).await.is_empty());
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn a_repeated_finish_is_accepted_and_a_finish_of_an_aborted_move_is_refused() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("s1", true).await;
    fx.end("s1").await;
    fx.accepted("c1", out("s1", "m1")).await;
    fx.accepted("c2", finish("s1", "m1")).await;
    // Another command id for the same move: accepted, nothing changes.
    let events_before = fx.rig.store.head();
    fx.accepted("c3", finish("s1", "m1")).await;
    assert_eq!(fx.rig.store.head(), events_before);
    // A finish for a move of another session.
    fx.session("s2", true).await;
    fx.end("s2").await;
    fx.accepted("c4", out("s2", "m2")).await;
    assert_eq!(
        fx.refused("c5", finish("s1", "m2")).await,
        (
            RejectCode::InvalidCommand,
            "invalid_params: The move does not belong to this session.".to_owned()
        )
    );
    // An aborted move.
    fx.manager.move_abort("m2").await.unwrap();
    assert_eq!(
        fx.refused("c6", finish("s2", "m2")).await,
        state("The move was cancelled.")
    );
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn a_finish_without_a_row_applies_only_when_nothing_runs() {
    let Some(fx) = Fx::new().await else { return };
    // The record is lost, but the session is idle: the move ends.
    fx.session("s1", true).await;
    fx.end("s1").await;
    fx.accepted("c1", finish("s1", "m-lost")).await;
    assert!(fx.summary("s1").await.moved_to.is_some());
    // A move id that no longer matches: refused.
    assert_eq!(
        fx.refused("c2", finish("s1", "m-other")).await,
        state("The session moved in another move.")
    );
    // A session that runs a turn: the receipt is rejected.
    fx.session("s2", true).await;
    let turn: NewEvent = Event::RunStarted {
        run_id: "r1".into(),
        input_summary: "work".to_owned(),
        origin: fleetdeck_proto::event::RunOrigin::Board,
        author: None,
        input_id: None,
        input_text: None,
    }
    .into();
    fx.rig.store.append("s2".into(), vec![turn]).await.unwrap();
    assert!(fx.summary("s2").await.current_run.is_some());
    assert_eq!(
        fx.refused("c3", finish("s2", "m-lost2")).await,
        state("The source session runs again. Stop it, then Fleet Deck finishes the move.")
    );
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn undo_makes_the_session_resumable_again_and_restores_the_push_row() {
    let Some(fx) = Fx::new().await else { return };
    let (wt, _) = fx.session("s1", true).await;
    fx.end("s1").await;
    fx.accepted("c1", out("s1", "m1")).await;
    fx.accepted("c2", finish("s1", "m1")).await;
    // Another move id, and a session that never moved.
    assert_eq!(
        fx.refused("c3", undo("s1", "m-other")).await,
        state("This session did not move in this move.")
    );
    fx.accepted("c4", undo("s1", "m1")).await;
    assert!(fx.summary("s1").await.moved_to.is_none());
    let row = fx
        .rig
        .store
        .move_out_run("m1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, MoveOutState::Undone);
    // The tip is the pushed commit: the row is `pushed`.
    assert_eq!(fx.push_state("s1").await, (PushState::Pushed, None));
    // A session that can be held and moved again.
    fx.accepted("c5", out("s1", "m2")).await;
    fx.manager.move_abort("m2").await.unwrap();
    // After a newer tip the restored row is due.
    fx.accepted("c6", out("s1", "m3")).await;
    fx.accepted("c7", finish("s1", "m3")).await;
    let newer = fx.rig.commit(&wt, "later.txt", "later");
    fx.rig
        .store
        .git_push_trigger("s1".into(), newer)
        .await
        .unwrap();
    assert_eq!(
        fx.push_state("s1").await,
        (PushState::Stopped, Some("moved".to_owned()))
    );
    fx.accepted("c8", undo("s1", "m3")).await;
    assert_eq!(fx.push_state("s1").await, (PushState::Due, None));
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn abort_releases_the_hold_and_is_idempotent_and_a_finished_move_stays() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("s1", true).await;
    fx.end("s1").await;
    fx.accepted("c1", out("s1", "m1")).await;
    fx.manager.move_abort("m1").await.unwrap();
    assert!(
        fx.rig
            .store
            .session_hold("s1".into())
            .await
            .unwrap()
            .is_none()
    );
    let row = fx
        .rig
        .store
        .move_out_run("m1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, MoveOutState::Aborted);
    // Again, and an unknown id: ok.
    fx.manager.move_abort("m1").await.unwrap();
    fx.manager.move_abort("nobody").await.unwrap();
    // The session is as before: it can be held again.
    fx.accepted("c2", out("s1", "m2")).await;
    fx.accepted("c3", finish("s1", "m2")).await;
    fx.manager.move_abort("m2").await.unwrap();
    let row = fx
        .rig
        .store
        .move_out_run("m2".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, MoveOutState::Finished, "the commit point passed");
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn move_status_answers_the_source_state_and_unknown() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("s1", true).await;
    fx.end("s1").await;
    let unknown = fx.manager.move_status("m1").await.unwrap();
    assert_eq!(unknown.state, MoveStatusState::Unknown);
    assert_eq!(unknown.session_id, None);
    fx.accepted("c1", out("s1", "m1")).await;
    let holding = fx.manager.move_status("m1").await.unwrap();
    assert_eq!(holding.state, MoveStatusState::Holding);
    assert_eq!(holding.session_id.unwrap().as_str(), "s1");
    fx.accepted("c2", finish("s1", "m1")).await;
    assert_eq!(
        fx.manager.move_status("m1").await.unwrap().state,
        MoveStatusState::Finished
    );
    fx.accepted("c3", undo("s1", "m1")).await;
    assert_eq!(
        fx.manager.move_status("m1").await.unwrap().state,
        MoveStatusState::Undone
    );
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn the_export_plan_waits_for_the_stop_checks_the_tip_and_lists_the_files() {
    let Some(fx) = Fx::new().await else { return };
    let (wt, tip) = fx.session("s1", true).await;
    fx.rig
        .store
        .process_started("s1".into(), "p1".into(), None)
        .await
        .unwrap();
    fx.accepted("c1", out("s1", "m1")).await;
    // The process still runs: the plan waits, then says so.
    let err = fx.manager.move_export_plan("m1").await.unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        ("invalid_state", "The session is still stopping.")
    );
    fx.rig
        .store
        .process_ended("p1".into(), "test".to_owned())
        .await
        .unwrap();
    let (publication, manifest, instance) = fx.manager.move_export_plan("m1").await.unwrap();
    assert_eq!(publication.oid, tip);
    assert_eq!(
        instance.as_ref().map(ToString::to_string).as_deref(),
        Some("claude")
    );
    let manifest = manifest.unwrap();
    assert_eq!(manifest.native_session_id, NATIVE);
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(manifest.files[0].key, "main");
    // A commit that is not pushed: the move would lose it.
    fx.rig.commit(&wt, "unpushed.txt", "unpushed");
    let err = fx.manager.move_export_plan("m1").await.unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        ("invalid_state", "The branch is not pushed.")
    );
    // The files stream (open files) does not check the tip.
    assert!(fx.manager.move_export_open("m1").await.unwrap().is_some());
    // After the abort no move holds the session.
    fx.manager.move_abort("m1").await.unwrap();
    let err = fx.manager.move_export_plan("m1").await.unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        ("invalid_state", "No move holds this session.")
    );
    assert!(fx.manager.move_export_open("m1").await.is_err());
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn a_session_without_a_native_id_has_a_branch_only_plan_and_a_missing_file_is_an_error() {
    let Some(fx) = Fx::new().await else { return };
    fx.session("s1", false).await;
    fx.end("s1").await;
    fx.accepted("c1", out("s1", "m1")).await;
    let (publication, manifest, instance) = fx.manager.move_export_plan("m1").await.unwrap();
    assert_eq!(publication.local_branch, "fd/alpha/s1");
    assert!(manifest.is_none());
    // A branch-only plan answers the instance too (the summary may have none).
    assert_eq!(
        instance.as_ref().map(ToString::to_string).as_deref(),
        Some("claude")
    );
    assert!(fx.manager.move_export_open("m1").await.unwrap().is_none());
    // A native session whose transcript is gone.
    fx.session("s2", true).await;
    fx.end("s2").await;
    let file = fx
        .claude_home
        .join("projects")
        .join(crate::history::locate::encode_cwd(
            &fx.rig.wt("s2").display().to_string(),
        ))
        .join(format!("{NATIVE}.jsonl"));
    std::fs::remove_file(file).unwrap();
    fx.accepted("c2", out("s2", "m2")).await;
    let err = fx.manager.move_export_plan("m2").await.unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str()),
        (
            "invalid_state",
            "The conversation files of this session were not found."
        )
    );
    fx.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[path = "move_prepr_tests.rs"]
mod prepr;
