//! The `MoveIn` effect of the target (FG-9, design §12.3 steps 4 and 6, §12.4A
//! U1, §12.9 revision 3 and 4, T4.2d): the Git work, the import, the launch
//! admission, the commit by provider, the release, the confirm, the faults of
//! the fake providers, and the effect-aware discard. Real Git, the fake
//! providers of `fleetdeck-testkit`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::{CommandOrigin, ReceiptState, SessionCommand, UserInput};
use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::fleet_git::{
    ManifestFile, MoveDiscardState, MoveFrom, MoveInState, MoveKind, MovePublication,
    MoveReleaseState, PushState, TranscriptManifest, code,
};
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_testkit::FakeAgent;
use fleetdeck_testkit::fake::{
    RESUME_EXIT_AT_START, RESUME_FAULT_ENV, RESUME_MISSING, RESUME_OTHER_ID, SCENARIO_ENV,
    STDIN_LOG_ENV,
};
use fleetdeck_testkit::sim_claude::StdinLog;

use super::{Fx, NATIVE, sha256_hex, state};
use crate::manager::Adapters;
use crate::manager::land::move_in::MoveTimeouts;
use crate::store::EffectState;
use crate::store::move_runs::MoveInRow;
use crate::workspace::fleet_git::Receiver;

const STAMP_NAME: &str = "rollout-2026-10-17T10-11-12";

fn claude_adapter(
    log: &Path,
    fault: Option<&str>,
) -> Arc<dyn fleetdeck_providers::ProviderAdapter> {
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, "sim").unwrap();
    env.set(STDIN_LOG_ENV, log).unwrap();
    if let Some(fault) = fault {
        env.set(RESUME_FAULT_ENV, fault).unwrap();
    }
    let program = FakeAgent::Claude.locate().unwrap();
    let mut config = ClaudeAdapterConfig::new("claude".into(), program, env);
    config.stop = StopPolicy {
        grace: Duration::from_millis(500),
        term_grace: Duration::from_millis(300),
    };
    config.delivery_grace = Duration::from_millis(500);
    Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())))
}

fn codex_adapter(fault: Option<&str>) -> Arc<dyn fleetdeck_providers::ProviderAdapter> {
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, "sim").unwrap();
    if let Some(fault) = fault {
        env.set(RESUME_FAULT_ENV, fault).unwrap();
    }
    let program = FakeAgent::Codex.locate().unwrap();
    let mut config = CodexAdapterConfig::new("codex".into(), program, env);
    config.background_poll = Some(Duration::from_millis(50));
    config.stop = StopPolicy {
        grace: Duration::from_millis(500),
        term_grace: Duration::from_millis(300),
    };
    config.delivery_grace = Duration::from_millis(500);
    config.rollback_timeout = Duration::from_millis(500);
    Arc::new(CodexAdapter::new(config, Arc::new(DirectChildHost::new())))
}

/// The fixture of the effect: the station of `move_target_tests.rs` with the
/// fake providers, provider homes in a folder of their own, and the stdin log
/// of the fake Claude.
struct Mx {
    fx: Fx,
    _aux: tempfile::TempDir,
    claude_home: PathBuf,
    codex_home: PathBuf,
    log: PathBuf,
}

impl Mx {
    async fn new(claude_fault: Option<&str>, codex_fault: Option<&str>) -> Option<Self> {
        let aux = tempfile::tempdir().unwrap();
        let root = aux.path().canonicalize().unwrap();
        let (claude_home, codex_home) = (root.join("claude"), root.join("codex"));
        std::fs::create_dir_all(&claude_home).unwrap();
        std::fs::create_dir_all(&codex_home).unwrap();
        let log = root.join("claude.stdin");
        let adapters = Adapters::from([
            ("claude".into(), claude_adapter(&log, claude_fault)),
            ("codex".into(), codex_adapter(codex_fault)),
        ]);
        let mut claude_env = ChildEnv::from_process();
        claude_env.set("CLAUDE_CONFIG_DIR", &claude_home).unwrap();
        let mut codex_env = ChildEnv::from_process();
        codex_env.set("CODEX_HOME", &codex_home).unwrap();
        let envs = BTreeMap::from([("claude".into(), claude_env), ("codex".into(), codex_env)]);
        let fx = Fx::build_with(true, adapters, Some(envs)).await?;
        fx.manager.move_state().set_timeouts(MoveTimeouts {
            liveness: Duration::from_millis(300),
            resume: Duration::from_secs(10),
            first_input: Duration::from_secs(3),
        });
        Some(Self {
            fx,
            _aux: aux,
            claude_home,
            codex_home,
            log,
        })
    }

    /// The publication of a source session: a commit on `fd/alpha/cs` that is
    /// on the remote and not in the home (the home plays the target).
    fn publication(&self) -> MovePublication {
        let rig = &self.fx.rig;
        let home = &rig.home;
        rig.git(home, &["checkout", "-b", "src"]);
        let oid = rig.commit(home, "b.txt", "two");
        rig.git(home, &["push", "origin", "src:refs/heads/fd/alpha/cs"]);
        rig.git(home, &["checkout", "main"]);
        rig.git(home, &["branch", "-D", "src"]);
        MovePublication {
            local_branch: "fd/alpha/cs".to_owned(),
            dest: rig.remote.dest.clone(),
            ref_name: "fd/alpha/cs".to_owned(),
            oid,
        }
    }

    fn command(
        &self,
        move_id: &str,
        kind: MoveKind,
        instance: &str,
        publication: &MovePublication,
        summary: Option<&str>,
    ) -> SessionCommand {
        SessionCommand::MoveIn {
            move_id: move_id.to_owned(),
            kind,
            provider_instance: instance.into(),
            title: Some("moved".to_owned()),
            callsign_hint: Some("cs".to_owned()),
            from: MoveFrom {
                station_id: "alpha".into(),
                session_id: "src".into(),
                publication: publication.clone(),
                summary: summary.map(str::to_owned),
            },
            fleet_git: Box::new(self.fx.start()),
        }
    }

    fn worktree(&self) -> PathBuf {
        self.fx.rig.root.join("work--fd-cs")
    }

    /// The staged upload of a Claude move.
    async fn upload_claude(&self, move_id: &str) {
        self.fx.uploaded(move_id).await;
    }

    /// The staged upload of a Codex move.
    async fn upload_codex(&self, move_id: &str) {
        let body = format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{NATIVE}\"}}}}\n")
            .into_bytes();
        let manifest = TranscriptManifest {
            provider: ProviderKind::Codex,
            native_session_id: NATIVE.to_owned(),
            files: vec![ManifestFile {
                key: "rollout".to_owned(),
                rel: format!("sessions/2026/10/17/{STAMP_NAME}-{NATIVE}.jsonl"),
                size: body.len() as u64,
                sha256: sha256_hex(&body),
            }],
            total: body.len() as u64,
        };
        let mut receiver =
            Receiver::create(&self.fx.rig.root.join("moves"), move_id, &manifest).unwrap();
        receiver.feed(&body).unwrap();
        let folder = receiver.finish().unwrap();
        let store = &self.fx.rig.store;
        let at = (folder.path.display().to_string(), folder.dev, folder.ino);
        assert!(
            store
                .move_staging_insert(move_id.to_owned(), at, manifest)
                .await
                .unwrap()
        );
        assert!(
            store
                .move_staging_set(
                    move_id.to_owned(),
                    crate::store::move_staging::StagingState::Uploading,
                    crate::store::move_staging::StagingState::Ready,
                )
                .await
                .unwrap()
        );
    }

    async fn wait_state(&self, move_id: &str, want: MoveInState) -> MoveInRow {
        for _ in 0..1500 {
            let row = self.fx.row(move_id).await;
            if row.state == want {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "the move {move_id} did not reach {want:?}: {:?}",
            self.fx.row(move_id).await
        );
    }

    async fn wait_effect(&self, session: &str) -> EffectState {
        for _ in 0..1500 {
            let effects = self
                .fx
                .rig
                .store
                .session_effects(session.into())
                .await
                .unwrap();
            if let Some(e) = effects.iter().find(|e| e.payload.kind() == "move_in")
                && !matches!(e.state, EffectState::Queued | EffectState::Claimed)
            {
                return e.state;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the effect of {session} did not settle");
    }

    fn user_lines(&self) -> usize {
        StdinLog::read(&self.log).count("\"type\":\"user\"")
    }

    async fn prompt(&self, id: &str, session: &str, text: &str) {
        let command = SessionCommand::Prompt {
            session_id: session.into(),
            input: UserInput::new(text.to_owned()),
        };
        let decided = self.fx.send(id, CommandOrigin::Board, None, command).await;
        assert_eq!(
            decided.receipt.state,
            ReceiptState::Accepted,
            "{:?}",
            decided.receipt.reject
        );
    }

    async fn start_move(
        &self,
        kind: MoveKind,
        instance: &str,
        summary: Option<&str>,
    ) -> (String, MovePublication) {
        let publication = self.publication();
        let command = self.command("m1", kind, instance, &publication, summary);
        // `Fx::accepted` waits for the effect to end; this one lives on.
        let decided = self
            .fx
            .send("c1", CommandOrigin::Board, None, command)
            .await;
        assert_eq!(
            decided.receipt.state,
            ReceiptState::Accepted,
            "{:?}",
            decided.receipt.reject
        );
        (self.fx.session_of(&decided).await, publication)
    }
}

fn abandoned_text(row: &MoveInRow) -> String {
    row.message.clone().unwrap_or_default()
}

#[tokio::test]
async fn a_branch_only_claude_move_commits_after_liveness_releases_and_sends_the_held_prompt() {
    let Some(mx) = Mx::new(None, None).await else {
        return;
    };
    let (id, publication) = mx
        .start_move(MoveKind::BranchOnly, "claude", Some("fixed the login form"))
        .await;
    // The worktree is on the source branch at the published commit.
    let row = mx.wait_state("m1", MoveInState::Resumed).await;
    assert_eq!(row.provider, ProviderKind::Claude);
    let head = mx.fx.rig.git(&mx.worktree(), &["rev-parse", "HEAD"]);
    assert_eq!(head, publication.oid);
    let branch = mx
        .fx
        .rig
        .git(&mx.worktree(), &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(branch, "fd/alpha/cs");
    // The private ref is gone; the publication ref is the target's own name.
    assert!(
        mx.fx
            .rig
            .git(&mx.fx.rig.home, &["for-each-ref", "refs/fleetdeck/"])
            .is_empty()
    );
    let git = mx
        .fx
        .rig
        .store
        .git_session(id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    assert!(git.created_branch);
    assert_eq!(git.publish_ref.as_deref(), Some("fd/bravo/cs"));
    let push = mx
        .fx
        .rig
        .store
        .git_push(id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(push.state, PushState::Due);
    assert_eq!(push.last_pushed_oid, None);
    assert_eq!(mx.wait_effect(&id).await, EffectState::Confirmed);
    // The target holds all input: the held prompt is not sent before the
    // release, and the commit needed no input.
    assert_eq!(mx.user_lines(), 0);
    let hold = mx
        .fx
        .rig
        .store
        .session_hold(id.as_str().into())
        .await
        .unwrap();
    assert!(hold.is_some());
    let answer = mx.fx.manager.move_release("m1").await.unwrap();
    assert_eq!(answer.state, MoveReleaseState::Released);
    assert!(
        mx.fx
            .rig
            .store
            .session_hold(id.as_str().into())
            .await
            .unwrap()
            .is_none()
    );
    // A repeat answers the stored state.
    let again = mx.fx.manager.move_release("m1").await.unwrap();
    assert_eq!(again.state, MoveReleaseState::Released);
    // The held prompt goes out after the release: the first `session.started`
    // of a branch-only Claude session confirms the move.
    let row = mx.wait_state("m1", MoveInState::Confirmed).await;
    assert!(row.first_input_at.is_some());
    assert_eq!(mx.user_lines(), 1);
    let log = std::fs::read_to_string(&mx.log).unwrap();
    assert!(
        log.contains(
            "This session continues the work of the session \\\"moved\\\" from station alpha."
        ),
        "{log}"
    );
    assert!(log.contains("fixed the login form"), "{log}");
    assert!(
        log.contains("this station pushes it as fd/bravo/cs."),
        "{log}"
    );
    assert!(
        log.contains("<untrusted source=\\\"session:alpha/src\\\">"),
        "{log}"
    );
    mx.fx.shutdown().await;
}

#[tokio::test]
async fn a_held_prompt_without_a_summary_reads_no_summary_was_sent() {
    let Some(mx) = Mx::new(None, None).await else {
        return;
    };
    mx.start_move(MoveKind::BranchOnly, "claude", None).await;
    mx.wait_state("m1", MoveInState::Resumed).await;
    mx.fx.manager.move_release("m1").await.unwrap();
    mx.wait_state("m1", MoveInState::Confirmed).await;
    let log = std::fs::read_to_string(&mx.log).unwrap();
    assert!(log.contains("(no summary was sent)"), "{log}");
    mx.fx.shutdown().await;
}

#[tokio::test]
async fn a_claude_move_commits_after_five_idle_seconds_without_input() {
    let Some(mx) = Mx::new(None, None).await else {
        return;
    };
    mx.fx
        .manager
        .move_state()
        .set_timeouts(MoveTimeouts::default());
    mx.upload_claude("m1").await;
    let started = std::time::Instant::now();
    let (id, _) = mx.start_move(MoveKind::Native, "claude", None).await;
    mx.wait_state("m1", MoveInState::Launching).await;
    // The transcript went below the project folder of the new worktree.
    let placed = mx
        .claude_home
        .join("projects")
        .join(crate::history::locate::encode_cwd(
            &mx.worktree().to_string_lossy(),
        ))
        .join(format!("{NATIVE}.jsonl"));
    assert!(placed.is_file(), "{placed:?}");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(mx.fx.row("m1").await.state, MoveInState::Launching);
    mx.wait_state("m1", MoveInState::Resumed).await;
    assert!(
        started.elapsed() >= Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    // No user input reached the process.
    assert_eq!(mx.user_lines(), 0);
    let released = mx.fx.manager.move_release("m1").await.unwrap();
    assert_eq!(released.state, MoveReleaseState::Released);
    // One prompt starts the conversation with the expected id: confirmed.
    mx.prompt("p1", &id, "continue").await;
    let row = mx.wait_state("m1", MoveInState::Confirmed).await;
    assert!(row.first_input_at.is_some());
    assert_eq!(mx.user_lines(), 1);
    assert_eq!(
        mx.fx
            .rig
            .store
            .session_native_id(id.as_str().into())
            .await
            .unwrap()
            .as_deref(),
        Some(NATIVE)
    );
    mx.fx.shutdown().await;
}

#[tokio::test]
async fn claude_exit_at_start_abandons_with_move_resume_failed_and_keeps_the_files() {
    let Some(mx) = Mx::new(Some(RESUME_EXIT_AT_START), None).await else {
        return;
    };
    mx.upload_claude("m1").await;
    let (id, _) = mx.start_move(MoveKind::Native, "claude", None).await;
    let row = mx.wait_state("m1", MoveInState::Abandoned).await;
    assert_eq!(row.code.as_deref(), Some(code::MOVE_RESUME_FAILED));
    let text = abandoned_text(&row);
    assert!(
        text.starts_with("Claude could not resume the conversation on Bravo: "),
        "{text}"
    );
    assert!(text.ends_with("The session stays on alpha."), "{text}");
    // The hold stays, the files stay, the session is not writable.
    assert!(
        mx.fx
            .rig
            .store
            .session_hold(id.as_str().into())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(row.imported.len(), 1);
    assert!(Path::new(&row.imported[0].path).is_file());
    assert_eq!(mx.wait_effect(&id).await, EffectState::Confirmed);
    let prompt = SessionCommand::Prompt {
        session_id: id.as_str().into(),
        input: UserInput::new("hi".to_owned()),
    };
    assert_eq!(
        mx.fx.refused("p1", prompt).await,
        state("This copy of the conversation was not used. The session stays on alpha.")
    );
    mx.fx.shutdown().await;
}

#[tokio::test]
async fn claude_missing_conversation_fails_after_the_first_prompt_and_holds_again() {
    let Some(mx) = Mx::new(Some(RESUME_MISSING), None).await else {
        return;
    };
    mx.upload_claude("m1").await;
    let (id, _) = mx.start_move(MoveKind::Native, "claude", None).await;
    // The commit needs no input: the fake stays alive.
    mx.wait_state("m1", MoveInState::Resumed).await;
    mx.fx.manager.move_release("m1").await.unwrap();
    mx.prompt("p1", &id, "continue").await;
    let row = mx.wait_state("m1", MoveInState::ResumeFailed).await;
    assert_eq!(row.code.as_deref(), Some(code::MOVE_RESUME_FAILED));
    // The target is stopped, held again, and its push row stopped.
    let summary = mx.fx.summary(&id).await;
    assert!(matches!(
        summary.state,
        SessionState::Exited | SessionState::Lost
    ));
    let prompt = SessionCommand::Prompt {
        session_id: id.as_str().into(),
        input: UserInput::new("again".to_owned()),
    };
    assert_eq!(
        mx.fx.refused("p2", prompt).await,
        state(
            "This session could not resume the conversation. Undo the move to continue on alpha."
        )
    );
    let push = mx
        .fx
        .rig
        .store
        .git_push(id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(push.state, PushState::Stopped);
    assert_eq!(push.stop_code.as_deref(), Some("move_failed"));
    // The files are kept.
    assert_eq!(row.imported.len(), 1);
    assert!(Path::new(&row.imported[0].path).is_file());
    // Step U1: `failed`, the hold stays, a repeat answers `failed`.
    let failed = mx.fx.manager.move_fail("m1").await.unwrap();
    assert_eq!(failed.state, MoveInState::Failed);
    let again = mx.fx.manager.move_fail("m1").await.unwrap();
    assert_eq!(again.state, MoveInState::Failed);
    assert!(
        mx.fx
            .rig
            .store
            .session_hold(id.as_str().into())
            .await
            .unwrap()
            .is_some()
    );
    let prompt = SessionCommand::Prompt {
        session_id: id.as_str().into(),
        input: UserInput::new("third".to_owned()),
    };
    assert_eq!(
        mx.fx.refused("p3", prompt).await,
        state("This move was undone. The session continues on alpha.")
    );
    // A move that is not `resume_failed` answers its state.
    assert!(mx.fx.manager.move_release("m1").await.is_err());
    mx.fx.shutdown().await;
}

#[tokio::test]
async fn claude_other_id_fails_with_the_started_conversation_text() {
    let Some(mx) = Mx::new(Some(RESUME_OTHER_ID), None).await else {
        return;
    };
    mx.upload_claude("m1").await;
    let (id, _) = mx.start_move(MoveKind::Native, "claude", None).await;
    mx.wait_state("m1", MoveInState::Resumed).await;
    mx.fx.manager.move_release("m1").await.unwrap();
    mx.prompt("p1", &id, "continue").await;
    let row = mx.wait_state("m1", MoveInState::ResumeFailed).await;
    let text = abandoned_text(&row);
    assert!(
        text.starts_with("Claude started conversation sim-other-"),
        "{text}"
    );
    assert!(text.ends_with(&format!(", expected {NATIVE}.")), "{text}");
    mx.fx.shutdown().await;
}

#[tokio::test]
async fn a_codex_native_resume_commits_at_the_thread_answer_and_confirms_at_the_release() {
    let Some(mx) = Mx::new(None, None).await else {
        return;
    };
    mx.upload_codex("m1").await;
    let (id, _) = mx.start_move(MoveKind::Native, "codex", None).await;
    mx.wait_state("m1", MoveInState::Resumed).await;
    // The conversation is on the Codex home of the target.
    assert!(
        mx.codex_home
            .join(format!("sessions/2026/10/17/{STAMP_NAME}-{NATIVE}.jsonl"))
            .is_file()
    );
    // No turn started: the commit needed no input.
    let summary = mx.fx.summary(&id).await;
    assert!(summary.current_turn.is_none() && summary.current_run.is_none());
    let released = mx.fx.manager.move_release("m1").await.unwrap();
    assert_eq!(released.state, MoveReleaseState::Confirmed);
    assert_eq!(mx.fx.row("m1").await.state, MoveInState::Confirmed);
    mx.fx.shutdown().await;
}

#[tokio::test]
async fn a_codex_other_thread_id_abandons_with_identity_changed() {
    let Some(mx) = Mx::new(None, Some(RESUME_OTHER_ID)).await else {
        return;
    };
    mx.upload_codex("m1").await;
    let (id, _) = mx.start_move(MoveKind::Native, "codex", None).await;
    let row = mx.wait_state("m1", MoveInState::Abandoned).await;
    assert_eq!(row.code.as_deref(), Some(code::IDENTITY_CHANGED));
    assert_eq!(
        abandoned_text(&row),
        "Codex resumed another conversation on Bravo. The session stays on alpha."
    );
    assert!(
        mx.fx
            .rig
            .store
            .session_hold(id.as_str().into())
            .await
            .unwrap()
            .is_some()
    );
    mx.fx.shutdown().await;
}

#[tokio::test]
async fn a_coordinator_gate_refusal_stores_launch_refused_and_spawns_nothing() {
    let Some(mx) = Mx::new(None, None).await else {
        return;
    };
    let publication = mx.publication();
    let authority = CommandAuthority {
        grant_id: "g1".to_owned(),
        caller_station: "other".into(),
        caller_session: "caller".into(),
        generation: 1,
    };
    let decided = mx
        .fx
        .send(
            "c1",
            CommandOrigin::Coordinator,
            Some(authority),
            mx.command("m1", MoveKind::BranchOnly, "claude", &publication, None),
        )
        .await;
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    let id = mx.fx.session_of(&decided).await;
    // No deck answers the authority check: the gate does not confirm it.
    let row = mx.wait_state("m1", MoveInState::LaunchRefused).await;
    assert_eq!(row.code.as_deref(), Some(code::AUTHORITY_UNKNOWN));
    assert!(!mx.log.exists(), "no provider process was spawned");
    mx.wait_effect(&id).await;
    // The discard treats a refused launch like a staged move.
    let answer = mx.fx.manager.move_discard("m1").await.unwrap();
    assert_eq!(answer.state, MoveDiscardState::Discarded);
    assert_eq!(mx.fx.summary(&id).await.state, SessionState::Exited);
    mx.fx.shutdown().await;
}

#[tokio::test]
async fn a_publication_on_another_destination_stops_before_the_worktree_and_ends_no_session() {
    let Some(mx) = Mx::new(None, None).await else {
        return;
    };
    let mut publication = mx.publication();
    publication.dest = "git.example.com/other/repo".to_owned();
    let command = mx.command("m1", MoveKind::BranchOnly, "claude", &publication, None);
    let decided = mx.fx.accepted("c1", command).await;
    let id = mx.fx.session_of(&decided).await;
    let row = mx.fx.row("m1").await;
    assert_eq!(row.state, MoveInState::Staged);
    assert_eq!(row.code.as_deref(), Some(code::MOVE_PUBLICATION_MISMATCH));
    assert!(!mx.worktree().exists());
    assert_ne!(mx.fx.summary(&id).await.state, SessionState::Exited);
    let effects = mx
        .fx
        .rig
        .store
        .session_effects(id.as_str().into())
        .await
        .unwrap();
    assert_eq!(effects[0].state, EffectState::RetiredNotDispatched);
    let hold = mx
        .fx
        .rig
        .store
        .session_hold(id.as_str().into())
        .await
        .unwrap();
    assert!(hold.is_some());
    mx.fx.shutdown().await;
}

#[path = "move_in_discard_tests.rs"]
mod discard_effect;
