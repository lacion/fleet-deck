//! The settings tests of the PRE-PR r2 fixes: the credential files that a
//! save registers, and the queued starts of a restart (child of
//! `settings_link.rs`, which has the rig).

use std::path::Path;
use std::sync::atomic::Ordering;

use fleetdeck_core::paths::NodeDirs;
use fleetdeck_proto::CommandEnvelope;
use fleetdeck_proto::CommandId;
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, SessionCommand, SessionSpec, StopMode,
};
use fleetdeck_proto::event::{Event, EventBody, ProviderKind, SessionState};
use fleetdeck_proto::settings::{RestartAnswer, RestartKind};
use fleetdeck_station::store::{
    Caller, EffectPayload, EffectState, NewSession, OfflineApprovalPolicy,
};
use fleetdeck_station::workspace::{PrivateEnv, PrivatePaths};
use serde_json::json;

use super::{KEY, NEW_SECRET, Rig, call, ok, revision, set_params};

impl Rig {
    /// A start that is accepted and stays queued: the session row and its
    /// start effect, written while the restart freeze holds the worker
    /// (the state of a start that was accepted just before the freeze).
    async fn queue_start(&self, id: &str) {
        let store = self.manager.store();
        store
            .create_session(NewSession {
                session_id: id.into(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: self.workspace(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        let envelope = CommandEnvelope {
            command_id: CommandId::from(format!("queue-{id}")),
            target_incarnation: None,
            retry_until: store.now_ms() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Local,
            command: SessionCommand::Resume {
                session_id: id.into(),
            },
        };
        let payload = EffectPayload::Start {
            spec: SessionSpec {
                provider_instance: "claude".into(),
                cwd: self.workspace(),
                mode: Some("default".to_owned()),
                model: None,
                title: None,
                coordinator: None,
            },
            prompt: None,
            workspace: None,
            setup_cmd: None,
        };
        let session = id.to_owned();
        store
            .decide(envelope, Caller::local(), move |d| {
                d.enqueue(&session.as_str().into(), payload)?;
                Ok(fleetdeck_proto::command::CommandResult {
                    session_id: Some(session.as_str().into()),
                    input_id: None,
                })
            })
            .await
            .unwrap();
    }
}

/// The identity check of the file routes, for a file that is opened by a
/// workspace name.
fn refused(private: &PrivatePaths, link: &Path) -> bool {
    let meta = std::fs::symlink_metadata(link).unwrap();
    private.is_private_opened(&meta, "link.txt")
}

#[tokio::test]
async fn a_save_and_a_secret_set_register_the_files_that_they_replace() {
    use std::os::unix::fs::MetadataExt;
    let mut rig = Rig::new(2).await;
    let deck = rig.connect().await;
    let projects = rig.dir.path().join("projects/app");
    let secret_params = |rev: &str, value: &str| {
        json!({
            "secret": { "key": KEY, "value": value },
            "revision": rev,
            "elevated": true,
        })
    };
    // A secret, linked into the workspace, then replaced by a second one.
    let rev = revision(&deck.control).await;
    ok(call(
        &deck.control,
        "settings.secret_set",
        secret_params(&rev, NEW_SECRET),
    )
    .await);
    let first: Vec<_> = std::fs::read_dir(&rig.store)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(first.len(), 1, "{first:?}");
    let secret_link = projects.join("secret-link.txt");
    std::fs::hard_link(&first[0], &secret_link).unwrap();
    // The station.toml, linked after the first save, then saved again.
    let config_link = projects.join("config-link.txt");
    std::fs::hard_link(&rig.config, &config_link).unwrap();
    let rev = revision(&deck.control).await;
    ok(call(
        &deck.control,
        "settings.secret_set",
        secret_params(&rev, "canary-other-secret-1"),
    )
    .await);
    let rev = revision(&deck.control).await;
    ok(call(
        &deck.control,
        "settings.set",
        set_params(&rev, json!({ "max_sessions": 3 }), false),
    )
    .await);
    // The originals are gone: each link is the only name of its old inode.
    for link in [&secret_link, &config_link] {
        assert_eq!(std::fs::metadata(link).unwrap().nlink(), 1, "{link:?}");
        assert!(refused(&rig.private, link), "{link:?}");
    }
    assert_ne!(
        std::fs::metadata(&rig.config).unwrap().ino(),
        std::fs::metadata(&config_link).unwrap().ino()
    );
    // The new files are registered too: a link to them, made after the
    // originals are replaced again, stays refused.
    let new_config = projects.join("config-link-2.txt");
    std::fs::hard_link(&rig.config, &new_config).unwrap();
    let rev = revision(&deck.control).await;
    ok(call(
        &deck.control,
        "settings.set",
        set_params(&rev, json!({ "max_sessions": 2 }), false),
    )
    .await);
    assert_eq!(std::fs::metadata(&new_config).unwrap().nlink(), 1);
    assert!(refused(&rig.private, &new_config));
    // A station restart: the registry file is loaded again.
    let again = PrivatePaths::new(
        &NodeDirs {
            config: rig.config.parent().unwrap().to_path_buf(),
            data: rig.dir.path().join("data"),
        },
        Some(rig.dir.path()),
        None,
        &PrivateEnv::default(),
    );
    for link in [&secret_link, &config_link, &new_config] {
        assert!(refused(&again, link), "{link:?}");
    }
    // The control: a file that is not a credential is not refused.
    let plain = projects.join("plain.txt");
    std::fs::write(&plain, "x").unwrap();
    assert!(!refused(&again, &plain));
}

#[tokio::test]
async fn a_queued_start_is_listed_and_a_refusal_lets_it_run() {
    let mut rig = Rig::new(2).await;
    let deck = rig.connect().await;
    // The freeze holds the worker: the accepted start stays queued.
    rig.manager.begin_restart();
    rig.queue_start("held-1").await;
    let answer: RestartAnswer = serde_json::from_value(ok(call(
        &deck.control,
        "station.restart",
        json!({ "stop_direct_sessions": false, "elevated": true }),
    )
    .await))
    .unwrap();
    assert!(!answer.restarting);
    assert_eq!(answer.would_stop.len(), 1, "{answer:?}");
    assert_eq!(answer.would_stop[0].kind, RestartKind::Session);
    assert_eq!(answer.would_stop[0].id, "held-1");
    assert!(
        answer.would_stop[0].title.starts_with("Queued start of "),
        "{answer:?}"
    );
    assert_eq!(rig.restart.0.load(Ordering::SeqCst), 0);
    // The refusal unfroze: the held effect runs.
    assert!(!rig.manager.restart_state().restarting());
    rig.wait_live(1).await;
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_restart_with_consent_fails_a_queued_start_and_the_recovery_finds_nothing_ambiguous() {
    let mut rig = Rig::new(2).await;
    let deck = rig.connect().await;
    rig.manager.begin_restart();
    rig.queue_start("held-2").await;
    let answer: RestartAnswer = serde_json::from_value(ok(call(
        &deck.control,
        "station.restart",
        json!({ "stop_direct_sessions": true, "elevated": true }),
    )
    .await))
    .unwrap();
    assert!(answer.restarting && answer.would_stop.is_empty());
    let store = rig.manager.store().clone();
    // The session ended as a failed start, with the reason, before the
    // restart committed.
    let events = match store.read_after(0, 10_000).await.unwrap() {
        fleetdeck_station::store::ReadResult::Events { events, .. } => events,
        other => panic!("{other:?}"),
    };
    let reasons: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.event {
            EventBody::Known(Event::Error { code, message, .. }) if code == "start_failed" => {
                Some(message.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        reasons,
        ["the station restarted before this session started"]
    );
    let summary = store.summary("held-2".into()).await.unwrap().unwrap();
    assert_eq!(summary.state, SessionState::Exited);
    assert_eq!(rig.manager.live_sessions(), 0);
    // The effect is retired already: none stays queued next to the ended
    // session.
    let effects = store.session_effects("held-2".into()).await.unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].state, EffectState::RetiredNotDispatched);
    assert_eq!(effects[0].reason.as_deref(), Some("station_restart"));
    // The shutdown, then the recovery at the next start.
    rig.manager.shutdown(StopMode::Kill).await;
    let report = store.recover(Vec::new()).await.unwrap();
    assert_eq!(report.lost_sessions, 0, "{report:?}");
    let summary = store.summary("held-2".into()).await.unwrap().unwrap();
    assert_eq!(summary.state, SessionState::Exited);
    let effects = store.session_effects("held-2".into()).await.unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].state, EffectState::RetiredNotDispatched);
    assert_eq!(rig.manager.live_sessions(), 0);
}
