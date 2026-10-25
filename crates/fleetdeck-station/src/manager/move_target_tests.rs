//! The target side of a move without the effect (FG-9, design §12.3 step 4,
//! "Abort", T4.2c): the `MoveIn` decision, the tombstone, `session.move_prepare`
//! and `session.move_discard`, through the session manager with real Git.

use std::path::PathBuf;
use std::sync::Arc;

use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, UserInput,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{
    FleetGitStart, GitSettings, LocalRemotes, ManifestFile, MoveDiscardEffect, MoveDiscardState,
    MoveFrom, MoveInState, MoveKind, MovePublication, PushMode, StartKind, TranscriptManifest,
    code, remote_identity,
};
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_proto::{CommandId, RejectCode};

use super::super::rig::{Rig, policy, rejected};
use crate::manager::{Adapters, SessionManager};
use crate::store::move_runs::Tombstone;
use crate::store::move_staging::StagingState;
use crate::store::session_holds::KIND_MOVE_IN;
use crate::store::{Caller, Decided};
use crate::workspace::PrivatePaths;
use crate::workspace::fleet_git::{Folder, Receiver, delete_folder_by_identity};

const NATIVE: &str = "11111111-2222-3333-4444-555555555555";
const OID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DISCARDED: &str = "move_discarded: This move was cancelled; it does not run again.";

struct Fx {
    rig: Rig,
    manager: Arc<SessionManager>,
}

impl Fx {
    /// A rig and a manager of the station "Bravo" (slug `bravo`) with a
    /// staging root.
    async fn new() -> Option<Self> {
        Self::build(true).await
    }

    /// As [`Self::new`]; `dir` gives the station its staging root.
    async fn build(dir: bool) -> Option<Self> {
        Self::build_with(dir, Adapters::new(), None).await
    }

    /// As [`Self::build`] with provider adapters and, when given, the setup
    /// environments of the policy (they name the provider homes).
    async fn build_with(
        dir: bool,
        adapters: Adapters,
        envs: Option<
            std::collections::BTreeMap<
                fleetdeck_proto::ProviderInstanceId,
                fleetdeck_core::ChildEnv,
            >,
        >,
    ) -> Option<Self> {
        let rig = Rig::new(false).await?;
        let mut policy = policy(&rig.root, false, true).with_name("Bravo");
        if let Some(envs) = envs {
            policy = policy.with_setup_envs(envs);
        }
        let manager = SessionManager::start(
            rig.store.clone(),
            policy,
            adapters,
            rig.det.ids(),
            Arc::new(PrivatePaths::default()),
        )
        .await
        .unwrap();
        if dir {
            manager.set_moves_dir(rig.root.join("moves"));
        }
        Some(Self {
            rig,
            manager: Arc::new(manager),
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

    async fn accepted(&self, id: &str, command: SessionCommand) -> Decided {
        let decided = self.send(id, CommandOrigin::Board, None, command).await;
        assert_eq!(
            decided.receipt.state,
            ReceiptState::Accepted,
            "{:?}",
            decided.receipt.reject
        );
        // The decision queues the `MoveIn` effect (T4.2d). These tests have no
        // provider adapter, so the effect ends at once with `unknown_provider`;
        // wait for it, so that no test races the effect task.
        for effect in &decided.effects {
            for _ in 0..200 {
                let state = self
                    .rig
                    .store
                    .effect(effect.effect_id.clone())
                    .await
                    .unwrap()
                    .map(|e| e.state);
                if !matches!(
                    state,
                    Some(crate::store::EffectState::Queued | crate::store::EffectState::Claimed)
                ) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        decided
    }

    async fn refused(&self, id: &str, command: SessionCommand) -> (RejectCode, String) {
        rejected(&self.send(id, CommandOrigin::Board, None, command).await)
    }

    fn publication(&self) -> MovePublication {
        MovePublication {
            local_branch: "fd/alpha/cs".to_owned(),
            dest: self.rig.remote.dest.clone(),
            ref_name: "fd/alpha/cs".to_owned(),
            oid: OID.to_owned(),
        }
    }

    fn start(&self) -> FleetGitStart {
        FleetGitStart {
            kind: StartKind::Worktree,
            project_id: "p1".to_owned(),
            project_name: "app".to_owned(),
            settings: GitSettings {
                sync: true,
                push: PushMode::OnTurn,
                ..GitSettings::default()
            },
            settings_revision: 1,
            home_generation: 1,
            home_path: self.rig.home.display().to_string(),
            remote: remote_identity(&self.rig.url, LocalRemotes::Allow).unwrap(),
            slug: "bravo".to_owned(),
            forge: None,
            clone_url: self.rig.url.clone(),
        }
    }

    fn move_in(&self, move_id: &str, kind: MoveKind) -> SessionCommand {
        SessionCommand::MoveIn {
            move_id: move_id.to_owned(),
            kind,
            provider_instance: "claude".into(),
            title: Some("moved".to_owned()),
            callsign_hint: Some("cs".to_owned()),
            from: MoveFrom {
                station_id: "alpha".into(),
                session_id: "src".into(),
                publication: self.publication(),
                summary: None,
            },
            fleet_git: Box::new(self.start()),
        }
    }

    /// The session that a decision made (the receipt names it).
    async fn session_of(&self, decided: &Decided) -> String {
        decided
            .receipt
            .result
            .as_ref()
            .and_then(|r| r.session_id.clone())
            .map(|s| s.to_string())
            .expect("the receipt names the session")
    }

    /// A native upload that arrived: a staging folder with the files, its
    /// record in `ready`, and the manifest.
    async fn uploaded(&self, move_id: &str) -> (TranscriptManifest, Folder, Vec<u8>) {
        let body = format!("{{\"sessionId\":\"{NATIVE}\",\"type\":\"user\"}}\n").into_bytes();
        let manifest = TranscriptManifest {
            provider: ProviderKind::Claude,
            native_session_id: NATIVE.to_owned(),
            files: vec![ManifestFile {
                key: "main".to_owned(),
                rel: format!("projects/-src-/{NATIVE}.jsonl"),
                size: body.len() as u64,
                sha256: sha256_hex(&body),
            }],
            total: body.len() as u64,
        };
        let mut receiver =
            Receiver::create(&self.rig.root.join("moves"), move_id, &manifest).unwrap();
        receiver.feed(&body).unwrap();
        let folder = receiver.finish().unwrap();
        let store = &self.rig.store;
        let at = (folder.path.display().to_string(), folder.dev, folder.ino);
        assert!(
            store
                .move_staging_insert(move_id.to_owned(), at, manifest.clone())
                .await
                .unwrap()
        );
        assert!(
            store
                .move_staging_set(
                    move_id.to_owned(),
                    StagingState::Uploading,
                    StagingState::Ready
                )
                .await
                .unwrap()
        );
        (manifest, folder, body)
    }

    async fn advance(&self, move_id: &str, from: MoveInState, to: MoveInState) {
        assert!(
            self.rig
                .store
                .move_in_advance(move_id.to_owned(), from, to, None, Tombstone::Refuse)
                .await
                .unwrap()
        );
    }

    async fn row(&self, move_id: &str) -> crate::store::move_runs::MoveInRow {
        self.rig
            .store
            .move_in_run(move_id.to_owned())
            .await
            .unwrap()
            .unwrap()
    }

    async fn summary(&self, id: &str) -> fleetdeck_proto::SessionSummary {
        self.rig.store.summary(id.into()).await.unwrap().unwrap()
    }

    async fn shutdown(&self) {
        self.manager
            .shutdown(fleetdeck_proto::command::StopMode::Kill)
            .await;
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn state(text: &str) -> (RejectCode, String) {
    (RejectCode::InvalidState, text.to_owned())
}

fn invalid(text: &str) -> (RejectCode, String) {
    (
        RejectCode::InvalidCommand,
        format!("invalid_params: {text}"),
    )
}

#[tokio::test]
async fn the_move_in_decision_makes_the_session_the_hold_the_staged_row_and_created_branch_0() {
    let Some(fx) = Fx::new().await else { return };
    let decided = fx
        .accepted("c1", fx.move_in("m1", MoveKind::BranchOnly))
        .await;
    // The decision queues the `MoveIn` effect (T4.2d).
    assert_eq!(decided.effects.len(), 1);
    assert_eq!(decided.effects[0].payload.kind(), "move_in");
    let id = fx.session_of(&decided).await;
    let summary = fx.summary(&id).await;
    let path = fx.rig.root.join("work--fd-cs");
    assert_eq!(summary.cwd.as_deref(), Some(path.to_str().unwrap()));
    assert_eq!(summary.branch.as_deref(), Some("fd/alpha/cs"));
    assert_eq!(summary.title.as_deref(), Some("moved"));
    assert_eq!(summary.callsign.as_deref(), Some("cs"));
    let info = summary.move_in.expect("the staged event");
    assert_eq!(
        (info.move_id.as_str(), info.state),
        ("m1", MoveInState::Staged)
    );
    // The branch is the source's; the publication ref and the push row wait.
    let git = fx
        .rig
        .store
        .git_session(id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(git.kind, "worktree");
    assert_eq!(git.slug.as_deref(), Some("bravo"));
    assert_eq!(git.branch.as_deref(), Some("fd/alpha/cs"));
    assert!(!git.created_branch);
    assert_eq!(git.publish_ref, None);
    assert!(
        fx.rig
            .store
            .git_push(id.as_str().into())
            .await
            .unwrap()
            .is_none()
    );
    let hold = fx
        .rig
        .store
        .session_hold(id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (hold.kind.as_str(), hold.holder_id.as_str()),
        (KIND_MOVE_IN, "m1")
    );
    let row = fx.row("m1").await;
    assert_eq!(row.state, MoveInState::Staged);
    assert_eq!(row.command_id, "c1");
    assert_eq!(row.session_id.as_str(), id);
    assert_eq!(row.provider, ProviderKind::Claude);
    assert_eq!(row.native_session_id, None);
    assert_eq!(row.source_station.as_deref(), Some("alpha"));
    assert!(row.authority.is_none());
    // The same command id again changes nothing.
    let again = fx
        .send(
            "c1",
            CommandOrigin::Board,
            None,
            fx.move_in("m1", MoveKind::BranchOnly),
        )
        .await;
    assert!(again.duplicate);
    fx.shutdown().await;
}

#[tokio::test]
async fn the_hold_text_names_the_source_station() {
    let Some(fx) = Fx::new().await else { return };
    let decided = fx
        .accepted("c1", fx.move_in("m1", MoveKind::BranchOnly))
        .await;
    let id = fx.session_of(&decided).await;
    let prompt = SessionCommand::Prompt {
        session_id: id.as_str().into(),
        input: UserInput::new("hi".to_owned()),
    };
    assert_eq!(
        fx.refused("p1", prompt).await,
        state("This session is still arriving from alpha.")
    );
    fx.shutdown().await;
}

#[tokio::test]
async fn a_native_move_in_copies_the_staging_and_the_native_id_and_needs_a_ready_upload() {
    let Some(fx) = Fx::new().await else { return };
    // No upload: refused.
    assert_eq!(
        fx.refused("c0", fx.move_in("m1", MoveKind::Native)).await,
        state("move_checksum: The conversation files did not arrive complete. Try again.")
    );
    let (_, folder, _) = fx.uploaded("m1").await;
    let decided = fx.accepted("c1", fx.move_in("m1", MoveKind::Native)).await;
    let id = fx.session_of(&decided).await;
    let row = fx.row("m1").await;
    assert_eq!(row.native_session_id.as_deref(), Some(NATIVE));
    let staging = row.staging.expect("the staging identity is copied");
    assert_eq!(
        (staging.path.as_str(), staging.dev, staging.ino),
        (folder.path.to_str().unwrap(), folder.dev, folder.ino)
    );
    assert_eq!(row.session_id.as_str(), id);
    // The provider of the instance must be the provider of the manifest.
    let (_, _, _) = fx.uploaded("m2").await;
    fx.rig
        .store
        .move_staging_set("m2".into(), StagingState::Ready, StagingState::Deleted)
        .await
        .unwrap();
    fx.shutdown().await;
}

#[tokio::test]
async fn a_tombstoned_move_id_gets_a_rejected_receipt_move_discarded_and_no_session_row() {
    let Some(fx) = Fx::new().await else { return };
    let answer = fx.manager.move_discard("m1").await.unwrap();
    assert_eq!(
        (answer.state, answer.effect),
        (MoveDiscardState::Discarded, Some(MoveDiscardEffect::None))
    );
    assert_eq!(
        fx.refused("c1", fx.move_in("m1", MoveKind::BranchOnly))
            .await,
        state(DISCARDED)
    );
    assert!(
        fx.rig
            .store
            .move_in_run("m1".into())
            .await
            .unwrap()
            .is_none()
    );
    assert!(fx.rig.store.snapshot().await.unwrap().sessions.is_empty());
    fx.shutdown().await;
}

#[tokio::test]
async fn the_decision_refuses_what_the_design_names() {
    let Some(fx) = Fx::new().await else { return };
    fx.accepted("c1", fx.move_in("m1", MoveKind::BranchOnly))
        .await;
    // A move id that exists.
    assert_eq!(
        fx.refused("c2", fx.move_in("m1", MoveKind::BranchOnly))
            .await,
        invalid("This move id exists already.")
    );
    // A bad move id.
    assert_eq!(
        fx.refused("c3", fx.move_in("bad id", MoveKind::BranchOnly))
            .await,
        invalid("The move id is not valid.")
    );
    // A kind that this station does not know.
    assert_eq!(
        fx.refused("c4", fx.move_in("m4", MoveKind::Unknown)).await,
        state("unsupported_mode: This station does not know this move kind. Update the station.")
    );
    // Values of the source that are not valid.
    let changes: [fn(&mut MovePublication); 5] = [
        |p| p.local_branch = "main".to_owned(),
        |p| p.ref_name = "fd/alpha/a b".to_owned(),
        |p| p.oid = "HEAD".to_owned(),
        |p| p.dest = String::new(),
        |p| p.local_branch = "fd/alpha/../x".to_owned(),
    ];
    for (n, change) in changes.into_iter().enumerate() {
        let SessionCommand::MoveIn {
            move_id,
            kind,
            provider_instance,
            title,
            callsign_hint,
            mut from,
            fleet_git,
        } = fx.move_in(&format!("v{n}"), MoveKind::BranchOnly)
        else {
            unreachable!()
        };
        change(&mut from.publication);
        let command = SessionCommand::MoveIn {
            move_id,
            kind,
            provider_instance,
            title,
            callsign_hint,
            from,
            fleet_git,
        };
        let (code, text) = fx.refused(&format!("bad{n}"), command).await;
        assert_eq!(code, RejectCode::InvalidCommand, "{n}");
        assert!(text.starts_with("invalid_params: "), "{n}: {text}");
    }
    // An in-place start and a snapshot that does not match.
    let SessionCommand::MoveIn {
        move_id,
        kind,
        provider_instance,
        title,
        callsign_hint,
        from,
        mut fleet_git,
    } = fx.move_in("w1", MoveKind::BranchOnly)
    else {
        unreachable!()
    };
    let rebuild = |fleet_git: Box<FleetGitStart>| SessionCommand::MoveIn {
        move_id: move_id.clone(),
        kind,
        provider_instance: provider_instance.clone(),
        title: title.clone(),
        callsign_hint: callsign_hint.clone(),
        from: from.clone(),
        fleet_git,
    };
    fleet_git.kind = StartKind::InPlace;
    assert_eq!(
        fx.refused("w2", rebuild(fleet_git.clone())).await.0,
        RejectCode::InvalidCommand
    );
    fleet_git.kind = StartKind::Worktree;
    fleet_git.slug = "charlie".to_owned();
    assert_eq!(
        fx.refused("w3", rebuild(fleet_git.clone())).await,
        state("slug_mismatch: The station name changed. Try again in one minute.")
    );
    fleet_git.slug = "bravo".to_owned();
    fleet_git.home_generation = 9;
    assert_eq!(
        fx.refused("w4", rebuild(fleet_git)).await,
        state("stale_generation: The home of this project changed. Start again.")
    );
    // Only the board and a coordinator move a session in.
    for (n, origin) in [CommandOrigin::Local, CommandOrigin::Mail]
        .into_iter()
        .enumerate()
    {
        let decided = fx
            .send(
                &format!("o{n}"),
                origin,
                None,
                fx.move_in("m-origin", MoveKind::BranchOnly),
            )
            .await;
        assert_eq!(rejected(&decided).0, RejectCode::Forbidden);
    }
    fx.shutdown().await;
}

#[tokio::test]
async fn a_coordinator_move_in_stores_its_authority_and_needs_fleet_control() {
    let Some(fx) = Fx::new().await else { return };
    let authority = CommandAuthority {
        grant_id: "g1".to_owned(),
        caller_station: "other".into(),
        caller_session: "caller".into(),
        generation: 1,
    };
    let decided = fx
        .send(
            "c1",
            CommandOrigin::Coordinator,
            Some(authority.clone()),
            fx.move_in("m1", MoveKind::BranchOnly),
        )
        .await;
    assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    assert_eq!(fx.row("m1").await.authority, Some(authority.clone()));
    // Without fleet control on the station the coordinator is refused.
    fx.manager
        .policy_hub()
        .swap(policy(&fx.rig.root, false, false).with_name("Bravo"));
    let decided = fx
        .send(
            "c2",
            CommandOrigin::Coordinator,
            Some(authority),
            fx.move_in("m2", MoveKind::BranchOnly),
        )
        .await;
    assert_eq!(rejected(&decided).0, RejectCode::Forbidden);
    fx.shutdown().await;
}

#[tokio::test]
async fn a_move_in_during_a_restart_or_a_drain_is_refused() {
    let Some(fx) = Fx::new().await else { return };
    fx.manager.begin_restart();
    let (code, _) = fx
        .refused("c1", fx.move_in("m1", MoveKind::BranchOnly))
        .await;
    assert_eq!(code, RejectCode::InvalidState);
    fx.manager.end_restart();
    fx.accepted("c2", fx.move_in("m2", MoveKind::BranchOnly))
        .await;
    fx.manager
        .drain(std::time::Duration::from_millis(300))
        .await
        .unwrap();
    let (code, text) = fx
        .refused("c3", fx.move_in("m3", MoveKind::BranchOnly))
        .await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(text.contains("draining"), "{text}");
    assert!(
        fx.rig
            .store
            .move_in_run("m1".into())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fx.rig
            .store
            .move_in_run("m3".into())
            .await
            .unwrap()
            .is_none()
    );
    fx.shutdown().await;
}

#[tokio::test]
async fn a_prepare_is_checked_and_a_tombstoned_id_answers_move_discarded() {
    let Some(fx) = Fx::new().await else { return };
    let (manifest, folder, _) = fx.uploaded("m0").await;
    delete_folder_by_identity(&folder.path, folder.dev, folder.ino);
    assert!(
        fx.manager
            .move_prepare("m1", manifest.clone())
            .await
            .is_ok()
    );
    assert_eq!(
        fx.manager
            .move_prepare("bad id", manifest.clone())
            .await
            .unwrap_err()
            .code,
        "invalid_params"
    );
    let mut bad = manifest.clone();
    bad.total += 1;
    assert_eq!(
        fx.manager.move_prepare("m2", bad).await.unwrap_err().code,
        "invalid_params"
    );
    fx.manager.move_discard("m3").await.unwrap();
    let err = fx.manager.move_prepare("m3", manifest).await.unwrap_err();
    assert_eq!(err.code, code::MOVE_DISCARDED);
    assert_eq!(
        err.message,
        "This move was cancelled; it does not run again."
    );
    fx.shutdown().await;
    // A station without a staging root answers `unsupported`.
    let Some(bare) = Fx::build(false).await else {
        return;
    };
    assert_eq!(
        bare.manager
            .move_prepare("m4", manifest_for_prepare())
            .await
            .unwrap_err()
            .code,
        "unsupported"
    );
    bare.shutdown().await;
}

#[tokio::test]
async fn the_tombstone_survives_a_restart() {
    use fleetdeck_testkit::Det;
    let Some(rig) = Rig::new(false).await else {
        return;
    };
    let det = Det::new();
    let path: PathBuf = rig.root.join("station.db");
    let open = || async {
        crate::store::Store::open(
            &path,
            det.clock(),
            det.ids(),
            crate::store::StoreConfig::default(),
        )
        .await
        .unwrap()
    };
    let first = SessionManager::start(
        open().await,
        policy(&rig.root, false, true).with_name("Bravo"),
        Adapters::new(),
        det.ids(),
        Arc::new(PrivatePaths::default()),
    )
    .await
    .unwrap();
    first.move_discard("m1").await.unwrap();
    first
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
    drop(first);
    // A new station process on the same database.
    let second = SessionManager::start(
        open().await,
        policy(&rig.root, false, true).with_name("Bravo"),
        Adapters::new(),
        det.ids(),
        Arc::new(PrivatePaths::default()),
    )
    .await
    .unwrap();
    second.set_moves_dir(rig.root.join("moves"));
    assert!(second.store().move_tombstoned("m1".into()).await.unwrap());
    let err = second
        .move_prepare("m1", manifest_for_prepare())
        .await
        .unwrap_err();
    assert_eq!(err.code, code::MOVE_DISCARDED);
    let answer = second.move_discard("m1").await.unwrap();
    assert_eq!(answer.state, MoveDiscardState::Discarded);
    second
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

fn manifest_for_prepare() -> TranscriptManifest {
    let body = b"x";
    TranscriptManifest {
        provider: ProviderKind::Claude,
        native_session_id: NATIVE.to_owned(),
        files: vec![ManifestFile {
            key: "main".to_owned(),
            rel: format!("projects/p/{NATIVE}.jsonl"),
            size: 1,
            sha256: sha256_hex(body),
        }],
        total: 1,
    }
}

#[path = "move_discard_tests.rs"]
mod discard;

#[path = "move_in_tests.rs"]
mod launch;
