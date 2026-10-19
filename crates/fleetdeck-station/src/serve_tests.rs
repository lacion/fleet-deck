use super::*;
use crate::workspace::private_paths::private_paths_tests::private_of;

#[test]
fn effective_host_runs_the_direct_host_for_shim_on_macos_only() {
    use HostChoice::{Direct, Shim};
    assert_eq!(effective_host(Direct, true), Direct);
    assert_eq!(effective_host(Direct, false), Direct);
    assert_eq!(effective_host(Shim, true), Direct);
    assert_eq!(effective_host(Shim, false), Shim);
}

#[tokio::test]
async fn the_rescan_task_registers_new_credentials_and_stops_with_the_station() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let private = Arc::new(private_of(&home));
    let late = home.join(".ssh/id_late");
    std::fs::write(&late, "secret").unwrap();
    let meta = std::fs::metadata(&late).unwrap();
    assert!(!private.is_private_file(&meta));
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(rescan_credentials(
        Arc::clone(&private),
        std::time::Duration::from_millis(10),
        rx,
    ));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    while !private.is_private_file(&meta) {
        assert!(tokio::time::Instant::now() < deadline, "not registered");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn the_prune_task_prunes_the_history_and_stops_on_shutdown() {
    let det = fleetdeck_testkit::Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
        .create_session(crate::store::NewSession {
            session_id: "s1".into(),
            provider: fleetdeck_proto::event::ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: crate::store::OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    assert!(
        store
            .artifact_suggest("s1".into(), "a.md".to_owned())
            .await
            .unwrap()
    );
    assert!(
        !store
            .artifact_suggest("s1".into(), "a.md".to_owned())
            .await
            .unwrap()
    );
    det.advance(std::time::Duration::from_secs(91 * 24 * 60 * 60));
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(prune_history(
        store.clone(),
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(10),
        rx,
    ));
    // The old suggestion is gone, so the same path is new again.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !store
        .artifact_suggest("s1".into(), "a.md".to_owned())
        .await
        .unwrap()
    {
        assert!(tokio::time::Instant::now() < deadline, "not pruned");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn a_malformed_update_record_stops_the_live_marker() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("u1.json"), "{ not json").unwrap();
    let err = mark_live_or_fail(tmp.path()).unwrap_err();
    assert!(matches!(err, ServeError::Upgrade { .. }), "{err:?}");
    let text = err.to_string();
    assert!(text.contains("cannot be marked live"), "{text}");
    assert!(text.contains("u1.json"), "{text}");
    assert!(text.contains("start the station again"), "{text}");
    // No upgrades directory is not an error.
    mark_live_or_fail(&tmp.path().join("none")).unwrap();
}

#[tokio::test]
async fn the_station_does_not_open_its_database_when_the_live_marker_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = NodeDirs {
        config: tmp.path().join("config"),
        data: tmp.path().join("data"),
    };
    let upgrades = dirs.station().join("upgrades");
    std::fs::create_dir_all(&upgrades).unwrap();
    std::fs::write(upgrades.join("u1.json"), "{ not json").unwrap();
    let env = Env {
        home: Some(tmp.path().to_owned()),
        ..Env::default()
    };
    let config = StationConfig::load(&dirs.station_config(), &env).unwrap();
    let err = RunningStation::start(&dirs, &config, None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, ServeError::Upgrade { .. }), "{err:?}");
    assert!(!dirs.station_db().exists());
}
#[tokio::test]
async fn a_station_restart_marks_a_cloning_home_attempt_interrupted() {
    use crate::store::home_attempts::{BeginHome, NewHomeAttempt};
    use fleetdeck_proto::fleet_git::{HomeAttemptState, code};
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let dirs = NodeDirs {
        config: home.join("config"),
        data: home.join("data"),
    };
    ensure_private_dir(&dirs.station()).unwrap();
    let dest = home.join("projects").join("app");
    {
        // The earlier process: a clone started, then the station stopped.
        let det = fleetdeck_testkit::Det::new();
        let store = Store::open(
            &dirs.station_db(),
            det.clock(),
            det.ids(),
            StoreConfig::default(),
        )
        .await
        .unwrap();
        let new = NewHomeAttempt {
            attempt_id: "a1".to_owned(),
            project_id: "p1".to_owned(),
            generation: 1,
            request_digest: "d".to_owned(),
            retry_of: None,
            dest_path: dest.display().to_string(),
            url: "https://h.example/acme/app".to_owned(),
            remote_name: "origin".to_owned(),
        };
        assert!(matches!(
            store.home_begin(new).await.unwrap(),
            BeginHome::Created(_)
        ));
    }
    let env = Env {
        home: Some(home.clone()),
        ..Env::default()
    };
    let text = format!(
        "allowed_roots = [\"{}\"]\nhost = \"direct\"\n",
        home.display()
    );
    let config = StationConfig::parse(&text, &env).unwrap();
    let station = RunningStation::start(&dirs, &config, None, None)
        .await
        .unwrap();
    let row = station
        .manager
        .store()
        .home_attempt("p1".into(), "a1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, HomeAttemptState::Interrupted);
    assert_eq!(row.code.as_deref(), Some(code::CLONE_INTERRUPTED));
    assert!(row.message.unwrap().contains(&dest.display().to_string()));
    station.stop().await;
}
