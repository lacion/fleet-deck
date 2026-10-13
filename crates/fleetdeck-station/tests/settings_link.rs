//! The station handlers of `settings.get`, `settings.set`,
//! `settings.secret_set` and `station.restart` over a loopback link
//! (DL-172): a temp config file and data directory, the real settings
//! holder, and a fake `Restart`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fleetdeck_core::childenv::ChildEnv;
use fleetdeck_core::config::StationConfig;
use fleetdeck_core::identity::Identity;
use fleetdeck_core::paths::Env;
use fleetdeck_core::restart::Restart;
use fleetdeck_link::control::Control;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LinkListener, LiveLink};
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec, StopMode,
};
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use fleetdeck_proto::settings::{RestartAnswer, RestartKind, SettingsSaved};
use fleetdeck_proto::shell::ShellChange;
use fleetdeck_proto::{CommandEnvelope, CommandId, IncarnationId, RejectCode};
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig};
use fleetdeck_providers::host::{DirectChildHost, StopPolicy};
use fleetdeck_station::link::{self, StationLink, StationPeers};
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::settings_svc::StationSettings;
use fleetdeck_station::shell::ShellConfig;
use fleetdeck_station::store::peers::PairedKey;
use fleetdeck_station::store::{Caller, Store, StoreConfig};
use fleetdeck_testkit::fake::SCENARIO_ENV;
use fleetdeck_testkit::{Det, FakeAgent};
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(10);
const CANARY: &str = "canary-inline-secret-7731";
const NEW_SECRET: &str = "canary-new-secret-5520";
const KEY: &str = "provider_instance.claude.env.ANTHROPIC_API_KEY";

#[derive(Debug, Default)]
struct FakeRestart(AtomicUsize);

impl Restart for FakeRestart {
    fn request(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct Rig {
    _det: Det,
    dir: tempfile::TempDir,
    config: PathBuf,
    store: PathBuf,
    manager: Arc<SessionManager>,
    ctx: Arc<StationLink>,
    listener: LinkListener,
    deck_node: NodeConfig,
    restart: Arc<FakeRestart>,
}

fn config_text(root: &Path, max_sessions: u32) -> String {
    format!(
        "name = \"box\"\nallowed_roots = [\"{}\"]\nmax_sessions = {max_sessions}\nallow_shell = true\n\n\
         [[provider_instance]]\nid = \"claude\"\ndriver = \"claude\"\nenv = {{ ANTHROPIC_API_KEY = \"{CANARY}\" }}\n",
        root.join("projects").display()
    )
}

fn fake_adapter() -> Arc<dyn fleetdeck_providers::ProviderAdapter> {
    let mut env = ChildEnv::from_process();
    env.set(SCENARIO_ENV, "sim").unwrap();
    let mut config =
        ClaudeAdapterConfig::new("claude".into(), FakeAgent::Claude.locate().unwrap(), env);
    config.stop = StopPolicy {
        grace: Duration::from_millis(500),
        term_grace: Duration::from_millis(300),
    };
    Arc::new(ClaudeAdapter::new(config, Arc::new(DirectChildHost::new())))
}

impl Rig {
    async fn new(max_sessions: u32) -> Self {
        Self::with_restart(max_sessions, true).await
    }

    async fn with_restart(max_sessions: u32, handle: bool) -> Self {
        let det = Det::new();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("projects/app")).unwrap();
        let cfg_dir = root.join("cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let config = cfg_dir.join("station.toml");
        std::fs::write(&config, config_text(&root, max_sessions)).unwrap();
        let store_dir = root.join("data/station/secrets");
        let env = Env {
            home: Some(root.clone()),
            ..Env::default()
        };
        let parsed = StationConfig::load_with_store(&config, &env, &store_dir).unwrap();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        let adapters = Adapters::from([("claude".into(), fake_adapter())]);
        let manager = Arc::new(
            SessionManager::start(
                store.clone(),
                Policy::from_config(&parsed),
                adapters,
                det.ids(),
            )
            .await
            .unwrap(),
        );
        let restart = Arc::new(FakeRestart::default());
        let handle: Option<Arc<dyn Restart>> =
            handle.then(|| Arc::clone(&restart) as Arc<dyn Restart>);
        manager.set_settings(Arc::new(StationSettings::new(
            &config, &store_dir, env, handle,
        )));
        let deck = Arc::new(Identity::generate().unwrap());
        store
            .pin_peer(PairedKey {
                key: deck.public_key(),
                role: Role::Deck,
                scopes: BTreeSet::from([Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin]),
                dial_url: None,
            })
            .await
            .unwrap();
        let peers = StationPeers::load(store.clone()).await.unwrap();
        let station_id = Arc::new(Identity::generate().unwrap());
        let node = NodeConfig::new(
            Arc::clone(&station_id),
            LocalHello::new(Role::Station, store.incarnation().clone()),
            Arc::new(Epochs::in_memory()),
        )
        .with_store(Arc::clone(&peers));
        peers.pin_all(&node.pins).await.unwrap();
        let listener = node::listen(node.clone(), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let deck_node = NodeConfig::new(
            Arc::clone(&deck),
            LocalHello::new(Role::Deck, IncarnationId::from("deck-inc")),
            Arc::new(Epochs::in_memory()),
        )
        .with_store(Grants::new());
        deck_node.pins.pin(station_id.public_key(), Role::Station);
        let ctx = Arc::new(StationLink {
            manager: Arc::clone(&manager),
            node,
            peers,
            name: "box".to_owned(),
            shells: Arc::new(fleetdeck_station::shell::Shells::new(ShellConfig::default())),
            coordinator: fleetdeck_station::coordinator::LinkForward::default(),
            paste_dir: None,
        });
        Self {
            _det: det,
            dir,
            config,
            store: store_dir,
            manager,
            ctx,
            listener,
            deck_node,
            restart,
        }
    }

    async fn connect(&mut self) -> LiveLink {
        let peer = self.ctx.node.identity.public_key();
        let (d, a) = tokio::time::timeout(T, async {
            tokio::join!(
                node::dial(&self.deck_node, self.listener.local_addr, peer),
                self.listener.next_link()
            )
        })
        .await
        .expect("link setup");
        let at_station = fleetdeck_link::start(&self.ctx.node, a.expect("listener"));
        tokio::spawn(link::serve(Arc::clone(&self.ctx), at_station));
        fleetdeck_link::start(&self.deck_node, d.unwrap())
    }

    fn toml(&self) -> String {
        std::fs::read_to_string(&self.config).unwrap()
    }

    fn workspace(&self) -> String {
        self.dir
            .path()
            .join("projects/app")
            .to_string_lossy()
            .into_owned()
    }

    async fn start_session(&self, id: &str) -> Result<(), RejectCode> {
        let envelope = CommandEnvelope {
            command_id: CommandId::from(id),
            target_incarnation: None,
            retry_until: self.manager.store().now_ms() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Local,
            command: SessionCommand::Start {
                spec: SessionSpec {
                    provider_instance: "claude".into(),
                    cwd: self.workspace(),
                    mode: Some("default".to_owned()),
                    model: None,
                    title: Some("Fix the build".to_owned()),
                    coordinator: None,
                },
                prompt: None,
                workspace: None,
                setup_cmd: None,
                batch: None,
                issue: None,
                origin_text: false,
            },
        };
        let decided = self
            .manager
            .submit(envelope, Caller::local())
            .await
            .unwrap();
        match decided.receipt.state {
            ReceiptState::Rejected => Err(decided.receipt.reject.unwrap().code),
            _ => Ok(()),
        }
    }

    async fn wait_live(&self, n: usize) {
        let deadline = tokio::time::Instant::now() + T;
        while self.manager.live_sessions() < n {
            assert!(tokio::time::Instant::now() < deadline, "no live session");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn call(c: &Control, method: &str, params: Value) -> Outcome {
    tokio::time::timeout(T, c.request(method, None, params))
        .await
        .expect("response")
        .unwrap()
}

fn ok(o: Outcome) -> Value {
    match o {
        Outcome::Ok(v) => v,
        Outcome::Err(e) => panic!("request failed: {e}"),
    }
}

fn err(o: Outcome) -> WireError {
    match o {
        Outcome::Err(e) => e,
        Outcome::Ok(v) => panic!("expected an error, got {v}"),
    }
}

async fn revision(c: &Control) -> String {
    let view = ok(call(c, "settings.get", json!({})).await);
    view["revision"].as_str().unwrap().to_owned()
}

fn set_params(rev: &str, changes: Value, elevated: bool) -> Value {
    json!({ "change": { "changes": changes, "revision": rev }, "elevated": elevated })
}

#[tokio::test]
async fn settings_get_hides_the_secrets() {
    let mut rig = Rig::new(2).await;
    let deck = rig.connect().await;
    let view = ok(call(&deck.control, "settings.get", json!({})).await);
    let text = view.to_string();
    assert!(!text.contains(CANARY), "{text}");
    assert_eq!(
        view["values"]["provider_instance"][0]["env"]["ANTHROPIC_API_KEY"],
        json!({ "set": true })
    );
    assert_eq!(view["values"]["max_sessions"], json!(2));
    assert!(view["fields"].as_array().is_some_and(|f| !f.is_empty()));
}

#[tokio::test]
async fn settings_set_of_max_sessions_changes_the_next_start_decision() {
    let mut rig = Rig::new(1).await;
    let deck = rig.connect().await;
    rig.start_session("c1").await.unwrap();
    rig.wait_live(1).await;
    assert_eq!(rig.start_session("c2").await, Err(RejectCode::Forbidden));

    let rev = revision(&deck.control).await;
    let saved: SettingsSaved = serde_json::from_value(ok(call(
        &deck.control,
        "settings.set",
        set_params(&rev, json!({ "max_sessions": 2 }), false),
    )
    .await))
    .unwrap();
    assert_eq!(saved.applied, vec!["max_sessions"]);
    assert!(saved.restart_needed.is_empty());
    assert_eq!(saved.values["max_sessions"], json!(2));
    assert_eq!(rig.manager.policy().max_sessions(), 2);
    assert!(rig.toml().contains("max_sessions = 2"), "{}", rig.toml());
    rig.start_session("c3").await.unwrap();
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn turning_allow_shell_off_refuses_a_new_shell() {
    let mut rig = Rig::new(2).await;
    let deck = rig.connect().await;
    let open = ShellChange::Open {
        shell_id: "sh-1".to_owned(),
        cwd: rig.workspace(),
        cols: 80,
        rows: 24,
    };
    rig.ctx
        .shells
        .check(&open, &rig.manager.policy(), &[])
        .unwrap();
    let rev = revision(&deck.control).await;
    ok(call(
        &deck.control,
        "settings.set",
        set_params(&rev, json!({ "allow_shell": false }), false),
    )
    .await);
    let reject = rig
        .ctx
        .shells
        .check(&open, &rig.manager.policy(), &[])
        .unwrap_err();
    assert_eq!(reject.code, RejectCode::Unsupported);
}

#[tokio::test]
async fn a_restart_key_is_listed_in_restart_needed_and_stays_unapplied() {
    let mut rig = Rig::new(2).await;
    let deck = rig.connect().await;
    let rev = revision(&deck.control).await;
    let saved: SettingsSaved = serde_json::from_value(ok(call(
        &deck.control,
        "settings.set",
        set_params(&rev, json!({ "shim_buffer_mib": 4 }), false),
    )
    .await))
    .unwrap();
    assert_eq!(saved.restart_needed, vec!["shim_buffer_mib"]);
    assert!(!saved.applied.contains(&"shim_buffer_mib".to_owned()));
    assert!(rig.toml().contains("shim_buffer_mib = 4"));
}

#[tokio::test]
async fn a_danger_key_without_elevation_gives_elevation_required_with_the_keys() {
    let mut rig = Rig::new(2).await;
    let deck = rig.connect().await;
    let rev = revision(&deck.control).await;
    let before = rig.toml();
    let e = err(call(
        &deck.control,
        "settings.set",
        set_params(&rev, json!({ "allow_setup_cmd": true }), false),
    )
    .await);
    assert_eq!(e.code, "elevation_required");
    assert_eq!(e.data.unwrap()["keys"], json!(["allow_setup_cmd"]));
    assert_eq!(rig.toml(), before, "the file is unchanged");
    assert!(!rig.manager.policy().allow_setup_cmd());
    // With the elevation the same change is saved and applied.
    ok(call(
        &deck.control,
        "settings.set",
        set_params(&rev, json!({ "allow_setup_cmd": true }), true),
    )
    .await);
    assert!(rig.manager.policy().allow_setup_cmd());
}

#[tokio::test]
async fn an_old_revision_gives_settings_changed() {
    let mut rig = Rig::new(2).await;
    let deck = rig.connect().await;
    let old = revision(&deck.control).await;
    ok(call(
        &deck.control,
        "settings.set",
        set_params(&old, json!({ "max_sessions": 3 }), false),
    )
    .await);
    let e = err(call(
        &deck.control,
        "settings.set",
        set_params(&old, json!({ "max_sessions": 4 }), false),
    )
    .await);
    assert_eq!(e.code, "settings_changed");
    assert_eq!(rig.manager.policy().max_sessions(), 3);
}

#[tokio::test]
async fn secret_set_writes_a_reference_and_the_toml_holds_no_value() {
    let mut rig = Rig::new(2).await;
    let deck = rig.connect().await;
    let rev = revision(&deck.control).await;
    let params = |elevated: bool| {
        json!({
            "secret": { "key": KEY, "value": NEW_SECRET },
            "revision": rev,
            "elevated": elevated,
        })
    };
    let e = err(call(&deck.control, "settings.secret_set", params(false)).await);
    assert_eq!(e.code, "elevation_required");
    assert!(!e.message.contains(NEW_SECRET));

    let answer = ok(call(&deck.control, "settings.secret_set", params(true)).await);
    assert!(!answer.to_string().contains(NEW_SECRET), "{answer}");
    let saved: SettingsSaved = serde_json::from_value(answer).unwrap();
    assert_eq!(saved.restart_needed, vec!["provider_instance"]);
    let text = rig.toml();
    assert!(
        !text.contains(NEW_SECRET) && !text.contains(CANARY),
        "{text}"
    );
    assert!(text.contains("{ secret = \"provider-claude-"), "{text}");
    let files: Vec<_> = std::fs::read_dir(&rig.store)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), NEW_SECRET);
    // The view still hides it.
    let view = ok(call(&deck.control, "settings.get", json!({})).await);
    assert!(!view.to_string().contains(NEW_SECRET));
}

#[tokio::test]
async fn a_secret_request_that_does_not_parse_never_quotes_the_value() {
    let mut rig = Rig::new(2).await;
    let deck = rig.connect().await;
    let rev = revision(&deck.control).await;
    let e = err(call(
        &deck.control,
        "settings.secret_set",
        json!({ "secret": { "key": KEY, "value": 424_242_131 }, "revision": rev, "elevated": true }),
    )
    .await);
    assert!(!format!("{e:?}").contains("424242131"), "{e:?}");
    assert!(rig.toml().contains(CANARY), "the file is unchanged");
}

#[tokio::test]
async fn a_restart_without_elevation_is_refused() {
    let mut rig = Rig::new(2).await;
    let deck = rig.connect().await;
    let e = err(call(
        &deck.control,
        "station.restart",
        json!({ "stop_direct_sessions": true, "elevated": false }),
    )
    .await);
    assert_eq!(e.code, "elevation_required");
    assert_eq!(e.data.unwrap()["keys"], json!(["restart"]));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(rig.restart.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_restart_lists_a_running_direct_session_and_stops_it_only_when_told() {
    let mut rig = Rig::new(2).await;
    let deck = rig.connect().await;
    rig.start_session("c1").await.unwrap();
    rig.wait_live(1).await;

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
    assert_eq!(answer.would_stop[0].title, "Fix the build");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(rig.restart.0.load(Ordering::SeqCst), 0);

    let answer: RestartAnswer = serde_json::from_value(ok(call(
        &deck.control,
        "station.restart",
        json!({ "stop_direct_sessions": true, "elevated": true }),
    )
    .await))
    .unwrap();
    assert!(answer.restarting && answer.would_stop.is_empty());
    let deadline = tokio::time::Instant::now() + T;
    while rig.restart.0.load(Ordering::SeqCst) == 0 {
        assert!(tokio::time::Instant::now() < deadline, "no restart request");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.restart.0.load(Ordering::SeqCst), 1);
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_station_without_a_restart_handle_answers_unsupported() {
    let mut rig = Rig::with_restart(2, false).await;
    let deck = rig.connect().await;
    let e = err(call(
        &deck.control,
        "station.restart",
        json!({ "stop_direct_sessions": true, "elevated": true }),
    )
    .await);
    assert_eq!(e.code, "unsupported");
}
