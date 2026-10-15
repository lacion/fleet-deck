//! `session.import` (M3 6-12, DL-308) against `fake-claude sim`: a new
//! driven session continues a native CLI session that this station never
//! ran, and every admission rule stores a rejection that spawns nothing.

use super::*;

/// The `session.started` events of `session`: `(resumed, native id)`.
async fn starts(st: &Station, session: &SessionId) -> Vec<(Option<bool>, Option<String>)> {
    st.events()
        .await
        .into_iter()
        .filter(|e| e.session_id == *session)
        .filter_map(|e| match e.event {
            EventBody::Known(Event::SessionStarted {
                resumed,
                provider_refs,
                ..
            }) => Some((
                resumed,
                provider_refs
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            )),
            _ => None,
        })
        .collect()
}

fn import(cwd: &str, instance: &str, native: &str, prompt: Option<&str>) -> SessionCommand {
    SessionCommand::Import {
        spec: spec(cwd, instance),
        native_session_id: native.to_owned(),
        prompt: prompt.map(text),
    }
}

#[tokio::test]
async fn an_imported_claude_session_continues_the_given_native_session() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let d = st
        .accepted(import(&ws, "claude", "cli-session-7", Some("go on")))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    let found = loop {
        let found = starts(&st, &s).await;
        if !found.is_empty() {
            break found;
        }
        assert!(tokio::time::Instant::now() < deadline, "no session.started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        found[0],
        (Some(true), Some("cli-session-7".to_owned())),
        "the board shows a resumed session with the imported native id"
    );
    let effects = st.settled(&s).await;
    assert!(
        effects.contains(&("resume", EffectState::Confirmed)),
        "{effects:?}"
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_import_is_refused_for_a_bad_id_a_foreign_workspace_or_a_native_session_in_use() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let elsewhere = st
        .root
        .path()
        .join("elsewhere")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        st.rejected(import(
            &ws,
            "claude",
            "--dangerously-skip-permissions",
            None
        ))
        .await,
        RejectCode::InvalidCommand,
        "an id that reads as a flag"
    );
    assert_eq!(
        st.rejected(import(&elsewhere, "claude", "cli-1", None))
            .await,
        RejectCode::Forbidden,
        "a workspace outside the allowed roots"
    );
    assert_eq!(
        st.rejected(import(&ws, "nope", "cli-1", None)).await,
        RejectCode::UnknownProvider
    );
    st.accepted(import(&ws, "claude", "cli-1", Some("hi")))
        .await;
    assert_eq!(
        st.rejected(import(&ws, "claude", "cli-1", None)).await,
        RejectCode::InvalidState,
        "one native session is driven by one session at a time"
    );
    st.manager.shutdown(StopMode::Kill).await;
}

/// A station whose Claude instance has its own `config_home`. The provider
/// program appends a line to the transcript when it starts (DL-189). The
/// transcript of `native` is written at its exact path from `fixture`.
/// Returns the station, the workspace and the transcript path.
async fn station_with_history(
    home: &Path,
    native: &str,
    fixture: &[u8],
) -> (Station, String, PathBuf) {
    use fleetdeck_core::config::{Driver, ProviderInstance};
    use std::os::unix::fs::PermissionsExt;

    let det = Det::new();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("projects/app")).unwrap();
    let ws = root
        .path()
        .join("projects/app")
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let transcript = home
        .join("projects")
        .join(crate::history::locate::encode_cwd(&ws))
        .join(format!("{native}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, fixture).unwrap();
    std::fs::set_permissions(&transcript, std::fs::Permissions::from_mode(0o600)).unwrap();

    // The fake provider, behind a script that writes to the transcript first.
    let script = root.path().join("claude-wrapper");
    let line = r#"{"type":"user","uuid":"late","sessionId":"x","message":{"role":"user","content":"after the import"}}"#;
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{line}' >> '{}'\nexec '{}' \"$@\"\n",
            transcript.display(),
            FakeAgent::Claude.locate().unwrap().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut instance = ProviderInstance::default_for(Driver::Claude);
    instance.config_home = Some(home.to_path_buf());
    let env = ChildEnv::for_instance(ChildEnv::from_process(), &instance).unwrap();
    let mut adapter_env = env.clone();
    adapter_env.set(SCENARIO_ENV, "sim").unwrap();
    let mut config = ClaudeAdapterConfig::new("claude".into(), script, adapter_env);
    config.stop = StopPolicy {
        grace: Duration::from_millis(500),
        term_grace: Duration::from_millis(300),
    };
    config.delivery_grace = Duration::from_millis(500);
    let adapter: Arc<dyn fleetdeck_providers::ProviderAdapter> =
        Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())));

    let history = crate::history::History::default();
    history.set_dir(root.path().join("data/history"));
    let store =
        Store::open_in_memory_with_history(det.clock(), det.ids(), StoreConfig::default(), history)
            .await
            .unwrap();
    let policy = Policy::new(
        vec![root.path().join("projects")],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
    )
    .with_setup_envs(BTreeMap::from([("claude".into(), env)]));
    let manager = SessionManager::start(
        store,
        policy,
        Adapters::from([("claude".into(), adapter)]),
        det.ids(),
        Arc::new(crate::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();
    let station = Station {
        det,
        manager: Arc::new(manager),
        root,
        next: 0,
    };
    (station, ws, transcript)
}

#[tokio::test]
async fn an_import_snapshots_the_transcript_before_the_provider_starts_and_the_history_reads_it() {
    use std::os::unix::fs::PermissionsExt;

    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcripts");
    let fixture = std::fs::read(fixture_dir.join("claude-2.1.300-basic.jsonl")).unwrap();
    // The session id that the records of the fixture name.
    let native = "00e00a5d-d49a-4946-b5bc-fcdf51bd4493";
    let provider_home = tempfile::tempdir().unwrap();
    let home = provider_home.path().canonicalize().unwrap();
    let (mut st, ws, transcript) = station_with_history(&home, native, &fixture).await;

    let d = st
        .accepted(import(&ws, "claude", native, Some("go on")))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    let effects = st.settled(&s).await;
    assert!(
        effects.contains(&("resume", EffectState::Confirmed)),
        "{effects:?}"
    );

    // The row, the snapshot, its mode and its bytes.
    let row = st
        .manager
        .store()
        .transcript(s.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.capture, crate::store::transcripts::Capture::Import);
    let snapshot = PathBuf::from(row.snapshot.clone().unwrap());
    assert!(snapshot.starts_with(st.root.path().join("data/history")));
    assert_eq!(
        std::fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let held = std::fs::read(&snapshot).unwrap();
    assert_eq!(held, fixture);
    assert_eq!(row.prefix_size, Some(fixture.len() as u64));

    // The provider ran after the snapshot: it appended a line to the
    // transcript, and the snapshot does not hold it.
    let after = std::fs::read(&transcript).unwrap();
    assert!(
        after.len() > fixture.len(),
        "the fake provider did not write"
    );
    assert!(after.starts_with(&fixture));
    assert!(!String::from_utf8_lossy(&held).contains("after the import"));

    // `session.history` answers the expected items of the fixture.
    let answer = crate::history::answer(
        st.manager.store(),
        &st.manager.policy(),
        &st.manager.private_paths(),
        fleetdeck_proto::history::SessionHistoryQuery {
            session_id: s.clone(),
            before: None,
            limit: None,
            member: false,
        },
    )
    .await
    .unwrap();
    let expected: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture_dir.join("claude-2.1.300-basic.expected.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&answer.state).unwrap(),
        expected["state"]
    );
    let got: Vec<serde_json::Value> = answer
        .items
        .iter()
        .map(|i| serde_json::json!({"kind": i.kind, "id": i.id, "text": i.text, "tool": i.tool}))
        .collect();
    assert_eq!(serde_json::Value::Array(got), expected["items"]);
    st.manager.shutdown(StopMode::Kill).await;
}
