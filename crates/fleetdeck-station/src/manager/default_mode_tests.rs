//! The default mode of a station (B27 §3.5): a start or an import without a
//! mode gets `[default_mode]`; a start from issue text and a start with
//! command authority never do; a bypass or full-access default waits for the
//! restart that makes the adapters offer it; `station.info` reports the
//! effective default.

use super::*;
use crate::manager::default_mode::DefaultModeState;

fn start_cmd(cwd: &str, instance: &str, mode: Option<&str>) -> SessionCommand {
    let mut s = spec(cwd, instance);
    s.mode = mode.map(str::to_owned);
    SessionCommand::Start {
        spec: s,
        prompt: None,
        workspace: None,
        setup_cmd: None,
        batch: None,
        issue: None,
        origin_text: false,
        fleet_git: None,
    }
}

async fn mode_of(st: &Station, session: &SessionId) -> Option<String> {
    let snapshot = st.manager.store().snapshot().await.unwrap();
    snapshot
        .sessions
        .into_iter()
        .find(|s| &s.session_id == session)
        .and_then(|s| s.mode)
}

/// Applies `extra` of `station.toml` with the live change of the station.
fn apply(st: &Station, extra: &str) {
    let config = test_config(&st.root.path().join("projects"), extra);
    let prepared = st.manager.prepare(&config).unwrap();
    st.manager.publish(prepared);
}

/// A start without a mode: the mode that the station stores.
async fn stored(st: &mut Station, instance: &str) -> Option<String> {
    let ws = st.workspace();
    let d = st.accepted(start_cmd(&ws, instance, None)).await;
    let id = d.receipt.result.unwrap().session_id.unwrap();
    mode_of(st, &id).await
}

#[tokio::test]
async fn a_start_without_a_mode_gets_the_station_default() {
    let mut st = Station::new(8).await;
    apply(&st, "[default_mode]\nclaude = \"acceptEdits\"\n");
    assert_eq!(
        stored(&mut st, "claude").await.as_deref(),
        Some("acceptEdits")
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_start_with_a_mode_keeps_it() {
    let mut st = Station::new(8).await;
    apply(&st, "[default_mode]\nclaude = \"acceptEdits\"\n");
    let ws = st.workspace();
    let d = st.accepted(start_cmd(&ws, "claude", Some("plan"))).await;
    let id = d.receipt.result.unwrap().session_id.unwrap();
    assert_eq!(mode_of(&st, &id).await.as_deref(), Some("plan"));
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_start_from_issue_text_ignores_the_default() {
    let mut st = Station::new(8).await;
    apply(&st, "[default_mode]\nclaude = \"auto\"\n");
    let ws = st.workspace();
    let mut command = start_cmd(&ws, "claude", None);
    if let SessionCommand::Start { origin_text, .. } = &mut command {
        *origin_text = true;
    }
    let d = st.accepted(command).await;
    let id = d.receipt.result.unwrap().session_id.unwrap();
    assert_eq!(mode_of(&st, &id).await.as_deref(), Some("default"));
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_codex_default_is_used() {
    let mut st = Station::new(8).await;
    apply(
        &st,
        "[default_mode]\ncodex = \"on-request/workspace-write\"\n",
    );
    assert_eq!(
        stored(&mut st, "codex").await.as_deref(),
        Some("on-request/workspace-write")
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_import_without_a_mode_gets_the_default() {
    let mut st = Station::new(8).await;
    apply(&st, "[default_mode]\nclaude = \"acceptEdits\"\n");
    let mut s = spec(&st.workspace(), "claude");
    s.mode = None;
    let d = st
        .accepted(SessionCommand::Import {
            spec: s,
            native_session_id: "cli-session-9".to_owned(),
            prompt: None,
            transcript: None,
        })
        .await;
    let id = d.receipt.result.unwrap().session_id.unwrap();
    assert_eq!(mode_of(&st, &id).await.as_deref(), Some("acceptEdits"));
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn station_info_reports_the_default_mode() {
    let st = Station::new(8).await;
    let reported = |providers: Vec<fleetdeck_proto::link::StationProvider>| {
        providers
            .into_iter()
            .map(|p| (p.instance.to_string(), p.default_mode))
            .collect::<BTreeMap<_, _>>()
    };
    let unset = reported(st.manager.providers().await);
    assert_eq!(unset["claude"].as_deref(), Some("default"));
    assert_eq!(unset["codex"].as_deref(), Some("untrusted/workspace-write"));
    apply(
        &st,
        "[default_mode]\nclaude = \"plan\"\ncodex = \"on-request/read-only\"\n",
    );
    let set = reported(st.manager.providers().await);
    assert_eq!(set["claude"].as_deref(), Some("plan"));
    assert_eq!(set["codex"].as_deref(), Some("on-request/read-only"));
    // A bypass default that the adapters do not offer yet: the provider default.
    apply(
        &st,
        "allow_bypass_permissions = true\n[default_mode]\nclaude = \"bypassPermissions\"\n",
    );
    let deferred = reported(st.manager.providers().await);
    assert_eq!(deferred["claude"].as_deref(), Some("default"));
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_live_settings_change_updates_the_default() {
    let mut st = Station::new(8).await;
    apply(&st, "[default_mode]\nclaude = \"acceptEdits\"\n");
    assert_eq!(
        stored(&mut st, "claude").await.as_deref(),
        Some("acceptEdits")
    );
    apply(&st, "[default_mode]\nclaude = \"plan\"\n");
    assert_eq!(stored(&mut st, "claude").await.as_deref(), Some("plan"));
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_bypass_default_without_the_restart_runs_supervised() {
    let mut st = Station::new(8).await;
    apply(
        &st,
        "allow_bypass_permissions = true\n[default_mode]\nclaude = \"bypassPermissions\"\n",
    );
    assert_eq!(stored(&mut st, "claude").await.as_deref(), Some("default"));
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_codex_full_access_default_without_the_restart_runs_supervised() {
    let mut st = Station::new(8).await;
    apply(
        &st,
        "allow_bypass_permissions = true\n[default_mode]\ncodex = \"never/danger-full-access\"\n",
    );
    assert_eq!(
        stored(&mut st, "codex").await.as_deref(),
        Some("untrusted/workspace-write")
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_bypass_default_after_the_restart_is_used() {
    let mut st = Station::new(8).await;
    // The state of a process that started with `allow_bypass_permissions`.
    let hub = st.manager.policy_hub();
    let mut policy = hub.current().as_ref().clone();
    policy.default_modes = DefaultModeState {
        configured: fleetdeck_core::config::DefaultModes {
            claude: Some("bypassPermissions".to_owned()),
            codex: None,
        },
        bypass_offered: true,
    };
    hub.swap(policy);
    assert_eq!(
        stored(&mut st, "claude").await.as_deref(),
        Some("bypassPermissions")
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_start_with_authority_never_gets_the_default() {
    let mut st = Station::new(8).await;
    st.switch(true);
    let deck = FakeDeck::new(Reply::Valid);
    st.link_deck(&deck);
    apply(
        &st,
        "allow_fleet_control = true\n[default_mode]\nclaude = \"auto\"\n",
    );
    let ws = st.workspace();
    let d = st.coord(start_cmd(&ws, "claude", None)).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    let id = d.receipt.result.unwrap().session_id.unwrap();
    assert_ne!(mode_of(&st, &id).await.as_deref(), Some("auto"));
    st.manager.shutdown(StopMode::Kill).await;
}
