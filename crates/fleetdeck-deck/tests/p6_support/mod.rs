//! The world of the P6 end-to-end proofs (R13 P6 task T7a): one real deck
//! (its own runtime, a database file), a caller station A without a desktop
//! (a real `StationNode` with the coordinator MCP endpoint), and a target
//! station B (a real station link with the fake desktop). Every link is a
//! real TLS link on loopback. The grant, the binding and the MCP credential
//! are the ones of an owner grant of the coordinator session `coord-1`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

pub mod board;
pub mod fake_deck;
pub mod fake_desktop;
pub mod input_rig;
pub mod input_rig_p7b;
pub mod mcp;
mod proofs_exposure;
pub mod recorder;
mod station_b;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_core::paths::Env;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::live::{DeckLive, LiveCell};
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_deck::store::{GrantClass, LinkFeature, NewGrant};
use fleetdeck_deck::{DeckNode, DeckStore, NodeError};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::StationId;
use fleetdeck_proto::command::GrantScope;
use fleetdeck_proto::coordinator_spec::InputScope;
use fleetdeck_proto::event::{Event, ProviderKind};
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::version::feature;
use fleetdeck_station::coordinator::{Binding, BindingState, ToolClass, mint};
use fleetdeck_station::link::StationNode;
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::shell::{ShellConfig, Shells};
use fleetdeck_station::store::{NewEvent, NewSession, OfflineApprovalPolicy, Store, StoreConfig};
use fleetdeck_station::workspace::PrivatePaths;
use serde_json::{Value, json};

pub use station_b::StationB;

use crate::common::board::{BoardClient, board_with_node};
use crate::settings_support::{Headers, elevate, owner, set_password};
use fake_desktop::Rig;
use mcp::Mcp;

pub const T: Duration = Duration::from_secs(10);
/// The coordinator session on A.
pub const CALLER: &str = "coord-1";
/// Its generated callsign.
pub const CALLSIGN: &str = "swift-fox";
/// The native identity of its binding: a sentinel (proof 26).
pub const NATIVE_SENTINEL: &str = "SENTINEL-NATIVE-ID-0e8f";

pub fn fast() -> BackoffPolicy {
    BackoffPolicy {
        base: Duration::from_millis(50),
        cap: Duration::from_millis(200),
        stable: Duration::from_secs(60),
    }
}

pub fn clock_ids() -> (SharedClock, SharedIds) {
    let clock: SharedClock = Arc::new(SystemClock);
    let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
    (clock, ids)
}

/// Polls `check` until it is true or the test times out.
pub async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + T;
    while !check().await {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The deck: its own runtime (a restart drops the runtime, which ends every
/// task of the deck at once, as a crash does), the database file and the live
/// switches.
pub struct Deck {
    rt: Option<tokio::runtime::Runtime>,
    pub store: DeckStore,
    pub node: Arc<DeckNode>,
    pub live: LiveCell,
    pub addr: std::net::SocketAddr,
}

fn deck_live(desktop_control: bool) -> DeckLive {
    DeckLive {
        name: "deck".into(),
        conflict_mail: false,
        fleet_control: true,
        desktop_control,
        tailscale_cert: false,
        tailscale_bin: None,
        knobs: fleetdeck_deck::knobs::DeckKnobs::default(),
        worker_grant: fleetdeck_core::config_worker_grant::WorkerGrantConfig {
            enabled: false,
            ..fleetdeck_core::config_worker_grant::WorkerGrantConfig::default()
        },
    }
}

impl Deck {
    /// Opens the deck on `db`, listening on `listen`, with `fleet_control`
    /// and `desktop_control` on.
    pub async fn start(
        db: &Path,
        identity: &Arc<Identity>,
        listen: std::net::SocketAddr,
        pin: Vec<NewStation>,
    ) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (db, identity) = (db.to_owned(), Arc::clone(identity));
        let (store, node) = rt
            .spawn(async move {
                let (clock, ids) = clock_ids();
                let store = DeckStore::open(&db, clock, ids).await.unwrap();
                for station in pin {
                    // A restart finds the station pinned already.
                    let _ = store.pin_station(station).await;
                }
                let mut tries = 0;
                let node = loop {
                    match DeckNode::start(store.clone(), Arc::clone(&identity), &[listen], fast())
                        .await
                    {
                        Ok(node) => break node,
                        Err(NodeError::Listen { .. }) if tries < 100 => {
                            tries += 1;
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                        Err(e) => panic!("deck node: {e}"),
                    }
                };
                (store, node)
            })
            .await
            .unwrap();
        store.set_fleet_control(true);
        let live = LiveCell::new(deck_live(true));
        assert!(node.link().desktop_live.set(live.clone()).is_ok());
        let addr = node.listen_addrs()[0];
        Self {
            rt: Some(rt),
            store,
            node,
            live,
            addr,
        }
    }

    /// Turns the deck setting `desktop_control` on or off.
    pub fn desktop_control(&self, on: bool) {
        self.live.publish(deck_live(on));
    }

    /// Ends every task of the deck at once.
    pub fn crash(mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }

    /// The board of the deck and an owner login.
    pub async fn board(&self) -> (BoardClient, Headers) {
        let c = board_with_node(Arc::clone(&self.node)).await;
        let h = owner(&c).await;
        (c, h)
    }

    /// The board of the deck, an owner password, an owner login and an
    /// elevation (the deletion of an artifact and the grant need it).
    pub async fn board_elevated(&self) -> (BoardClient, Headers, String) {
        let c = board_with_node(Arc::clone(&self.node)).await;
        set_password(&c).await;
        let h = owner(&c).await;
        let elevation = elevate(&c, &h).await;
        (c, h, elevation)
    }
}

impl Drop for Deck {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

/// The caller station A.
pub struct StationA {
    pub store: Store,
    pub manager: Arc<SessionManager>,
    pub node: Arc<StationNode>,
}

/// The settings of a station as `station.toml` values.
pub fn set_policy(manager: &SessionManager, fleet: bool, desktop: bool, capture: bool) {
    let env = Env {
        home: Some("/home/test".into()),
        ..Env::default()
    };
    let text = format!(
        "allow_fleet_control = {fleet}\nallow_desktop = {desktop}\nallow_desktop_capture = {capture}\n"
    );
    let config = fleetdeck_core::config::StationConfig::parse(&text, &env).unwrap();
    let prepared = manager.prepare(&config).unwrap();
    manager.publish(prepared);
}

/// The settings of a station as `station.toml` values, with
/// `allow_desktop_input` (R13 P7).
pub fn set_policy_input(
    manager: &SessionManager,
    (fleet, desktop, capture, input): (bool, bool, bool, bool),
) {
    set_policy_p7b(manager, (fleet, desktop, capture, input), (false, false));
}

/// As [`set_policy_input`], with `allow_desktop_click` and
/// `allow_desktop_input_any` (R13 P7b): `(click, any)`.
pub fn set_policy_p7b(
    manager: &SessionManager,
    (fleet, desktop, capture, input): (bool, bool, bool, bool),
    (click, any): (bool, bool),
) {
    let env = Env {
        home: Some("/home/test".into()),
        ..Env::default()
    };
    let text = format!(
        "allow_fleet_control = {fleet}\nallow_desktop = {desktop}\nallow_desktop_capture = {capture}\nallow_desktop_input = {input}\nallow_desktop_click = {click}\nallow_desktop_input_any = {any}\n"
    );
    let config = fleetdeck_core::config::StationConfig::parse(&text, &env).unwrap();
    let prepared = manager.prepare(&config).unwrap();
    manager.publish(prepared);
}

async fn station_a(dir: &Path) -> StationA {
    let (clock, ids) = clock_ids();
    std::fs::create_dir_all(dir).unwrap();
    let store = Store::open(
        &dir.join("station.db"),
        clock,
        Arc::clone(&ids),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    let policy = Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new())
        .with_allow_fleet_control(true);
    let manager = Arc::new(
        SessionManager::start(
            store.clone(),
            policy,
            Adapters::new(),
            ids,
            Arc::new(PrivatePaths::default()),
        )
        .await
        .unwrap(),
    );
    for id in [CALLER, "w1"] {
        store
            .create_session(NewSession {
                session_id: id.into(),
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
    }
    let named: NewEvent = Event::SessionNamed {
        callsign: CALLSIGN.to_owned(),
        worktree: None,
    }
    .into();
    store.append(CALLER.into(), vec![named]).await.unwrap();
    let node = StationNode::start_with_dirs(
        Arc::clone(&manager),
        Arc::new(Identity::generate().unwrap()),
        "alpha".to_owned(),
        &[],
        fast(),
        Arc::new(Shells::new(ShellConfig::default())),
        None,
        Some(dir.join("artifacts")),
    )
    .await
    .unwrap();
    StationA {
        store,
        manager,
        node,
    }
}

/// The ID of the station `name`, once the deck has it.
async fn station_id(deck: &Deck, name: &str) -> StationId {
    let store = deck.store.clone();
    let found: std::sync::Mutex<Option<StationId>> = std::sync::Mutex::default();
    eventually("a station record", || async {
        let record = store.find_station(name.to_owned()).await.unwrap();
        let some = record.is_some();
        *found.lock().unwrap() = record.map(|r| r.station_id);
        some
    })
    .await;
    let id = found.lock().unwrap().take();
    id.expect("a station")
}

/// The world of a proof.
pub struct World {
    pub tmp: tempfile::TempDir,
    pub identity: Arc<Identity>,
    slot: Option<Deck>,
    pub a: StationA,
    pub b: StationB,
    pub a_id: StationId,
    pub b_id: StationId,
    pub grant_id: String,
    pub binding: CoordinatorBinding,
    pub mcp: Mcp,
    /// The recording cap and the recording seconds cap of the grant, if the
    /// world has the recording allowance.
    recording: Option<(u32, u32)>,
    /// The input cap and the character cap of the grant, if the world has the
    /// input allowance (R13 P7).
    input: Option<(u32, u32)>,
    /// The click allowance and the scope `any_window` of the grant (R13 P7b).
    click: bool,
    any_window: bool,
}

/// What a world starts with.
pub struct Setup {
    /// Features that B leaves out of its hello.
    pub b_without: Vec<&'static str>,
    /// Whether the grant exists (a test of the grant creation makes its own).
    pub grant: bool,
    /// The recording cap and the recording seconds cap of the grant. `Some`
    /// gives the grant the recording allowance and the binding the
    /// `recording` class; `None` is a screenshot-only grant.
    pub recording: Option<(u32, u32)>,
    /// The input cap and the character cap of the grant (R13 P7). `Some`
    /// gives the grant the classes `desktop`, `capture` and `input`, the
    /// allowance `allow_input` with the scope `own_windows` and the two caps,
    /// gives the binding on A the classes `Input` and `InputEnabled`, and
    /// turns `allow_desktop_input` on for B. `None` is a grant without input.
    pub input: Option<(u32, u32)>,
    /// R13 P7b: the grant has `allow_click`, the binding on A has the class
    /// `ClickEnabled`, and B turns `allow_desktop_click` on. Needs `input`.
    pub click: bool,
    /// R13 P7b: the grant has the scope `any_window`, the binding on A has the
    /// class `InputAny`, and B turns `allow_desktop_input_any` on. Needs
    /// `input`.
    pub any_window: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            b_without: Vec::new(),
            grant: true,
            recording: None,
            input: None,
            click: false,
            any_window: false,
        }
    }
}

impl World {
    pub async fn start() -> Self {
        Self::with(Setup::default()).await
    }

    pub async fn with(setup: Setup) -> Self {
        mcp::record_logs();
        let tmp = tempfile::tempdir().unwrap();
        let identity = Arc::new(Identity::generate().unwrap());
        let rig = Rig::new();
        fleetdeck_station::desktop::install(Arc::clone(&rig.desktop));
        let b = StationB::new(&tmp.path().join("b"), rig, &identity).await;
        if setup.input.is_some() {
            input_rig::arm(&b.rig);
            if setup.click || setup.any_window {
                input_rig_p7b::arm(&b.rig);
            }
        }
        let pin = vec![NewStation {
            key: b.identity.public_key(),
            name: "beta".to_owned(),
            scopes: fleetdeck_deck::link::runtime::STATION_SCOPES
                .into_iter()
                .collect(),
            dial_url: None,
        }];
        let deck = Deck::start(
            &tmp.path().join("deck.db"),
            &identity,
            "127.0.0.1:0".parse().unwrap(),
            pin,
        )
        .await;
        let a = station_a(&tmp.path().join("a")).await;
        let invitation = a.node.invite("127.0.0.1:0".parse().unwrap()).await.unwrap();
        deck.node.add_station(&invitation, None).await.unwrap();
        b.link(deck.addr, identity.public_key(), &setup.b_without)
            .await;
        let a_id = station_id(&deck, "alpha").await;
        let b_id = station_id(&deck, "beta").await;
        let mut world = Self {
            tmp,
            identity,
            slot: Some(deck),
            a,
            b,
            a_id,
            b_id,
            grant_id: String::new(),
            binding: CoordinatorBinding {
                binding_id: String::new(),
                grant_id: String::new(),
                generation: 0,
                provider_instance: "claude".into(),
                native_id: String::new(),
            },
            mcp: Mcp {
                url: String::new(),
                token: String::new(),
            },
            recording: setup.recording,
            input: setup.input,
            click: setup.click,
            any_window: setup.any_window,
        };
        world.wait_linked(&setup.b_without).await;
        if setup.grant {
            world.grant_and_bind().await;
        }
        world
    }

    /// Waits until the deck has both stations, each with a snapshot.
    async fn wait_linked(&self, without: &[&str]) {
        for id in [&self.a_id, &self.b_id] {
            let (store, id) = (self.deck().store.clone(), id.clone());
            eventually("a linked station", || {
                let (store, id) = (store.clone(), id.clone());
                async move {
                    store.is_reachable(&id)
                        && store
                            .station(id)
                            .await
                            .unwrap()
                            .is_some_and(|s| s.incarnation.is_some())
                }
            })
            .await;
        }
        // The feature sets of both links are stored at the link start.
        let (store, b) = (self.deck().store.clone(), self.b_id.clone());
        let missing: Vec<&str> = without.to_vec();
        let mut wanted = vec![feature::DESKTOP_MCP, feature::CAPTURE_MCP];
        if self.recording.is_some() {
            wanted.extend([
                feature::DESKTOP_RECORDING,
                feature::ARTIFACTS_VIDEO,
                feature::CAPTURE_RECORDING_MCP,
            ]);
        }
        if self.input.is_some() && !missing.contains(&feature::DESKTOP_INPUT_MCP) {
            wanted.push(feature::DESKTOP_INPUT_MCP);
        }
        if self.click && !missing.contains(&feature::DESKTOP_CLICK_MCP) {
            wanted.push(feature::DESKTOP_CLICK_MCP);
        }
        if self.any_window && !missing.contains(&feature::DESKTOP_INPUT_ANY) {
            wanted.push(feature::DESKTOP_INPUT_ANY);
        }
        eventually("the features of the links", || {
            let (store, b, missing, wanted) =
                (store.clone(), b.clone(), missing.clone(), wanted.clone());
            async move {
                wanted.iter().all(|f| {
                    let state = store.link_feature(&b, f);
                    if missing.contains(f) {
                        state == LinkFeature::Without
                    } else {
                        state == LinkFeature::Negotiated
                    }
                })
            }
        })
        .await;
    }

    /// The owner grant of `coord-1` (classes `read`, `mail`, `desktop`,
    /// `capture`; stations A and B), its binding on the deck and the binding
    /// with its credential on A.
    pub async fn grant_and_bind(&mut self) {
        let mut spec = NewGrant::new(self.a_id.clone(), CALLER.into());
        spec.classes = [
            GrantClass::Read,
            GrantClass::Mail,
            GrantClass::Desktop,
            GrantClass::Capture,
        ]
        .into_iter()
        .collect();
        spec.stations = BTreeSet::from([self.a_id.clone(), self.b_id.clone()]);
        if let Some((count, seconds)) = self.recording {
            spec.recording_cap = count;
            spec.recording_second_cap = seconds;
        }
        if let Some((count, characters)) = self.input {
            spec.classes.insert(GrantClass::Input);
            spec.allow_input = true;
            spec.input_scope = Some(if self.any_window {
                InputScope::AnyWindow
            } else {
                InputScope::OwnWindows
            });
            spec.allow_click = self.click;
            spec.input_cap = count;
            spec.input_character_cap = characters;
        }
        // A second grant of another session keeps the `coordinate` scope of
        // both stations when the grant of the caller ends, so that a refusal
        // after a revocation is the answer of the handler and not the refusal
        // of the link layer (the same as the T1b world).
        let mut keeper = NewGrant::new(self.a_id.clone(), "w1".into());
        keeper.stations = BTreeSet::from([self.a_id.clone(), self.b_id.clone()]);
        self.deck().node.create_grant(keeper).await.unwrap();
        let grant = self.deck().node.create_grant(spec).await.unwrap();
        let binding = CoordinatorBinding {
            binding_id: "b-1".to_owned(),
            grant_id: grant.grant_id.clone(),
            generation: 1,
            provider_instance: "claude".into(),
            native_id: NATIVE_SENTINEL.to_owned(),
        };
        assert!(self.deck().store.bind_grant(binding.clone()).await.unwrap());
        let mut classes: BTreeSet<ToolClass> = [
            ToolClass::Read,
            ToolClass::Mail,
            ToolClass::Desktop,
            ToolClass::Capture,
        ]
        .into_iter()
        .collect();
        if self.recording.is_some() {
            classes.insert(ToolClass::Recording);
        }
        if self.input.is_some() {
            classes.insert(ToolClass::Input);
            classes.insert(ToolClass::InputEnabled);
            if self.click {
                classes.insert(ToolClass::ClickEnabled);
            }
            if self.any_window {
                classes.insert(ToolClass::InputAny);
            }
            // The station B of an input world allows desktop input.
            self.set_b_input(true, true, true, true);
        }
        let (token, digest) = mint().unwrap();
        self.a.node.coordinator_bindings().insert(
            digest,
            Binding {
                binding_id: binding.binding_id.clone(),
                grant_id: grant.grant_id.clone(),
                session_id: CALLER.into(),
                provider_instance: "claude".to_owned(),
                native_id: Some(NATIVE_SENTINEL.to_owned()),
                generation: 1,
                classes,
                state: BindingState::Active,
                control_skill: "skill".to_owned(),
                scope: GrantScope::Lineage,
            },
        );
        self.grant_id = grant.grant_id;
        self.binding = binding;
        self.mcp = Mcp {
            url: self.a.node.coordinator_url(),
            token: token.expose().to_owned(),
        };
    }

    /// The `station` argument of a tool call for B.
    pub fn to_b(&self) -> Value {
        json!(self.b_id.as_str())
    }

    /// The settings of B: `allow_fleet_control`, `allow_desktop`,
    /// `allow_desktop_capture`.
    pub fn set_b(&self, fleet: bool, desktop: bool, capture: bool) {
        set_policy(&self.b.manager, fleet, desktop, capture);
    }

    /// The settings of B with `allow_desktop_input` (R13 P7): fleet control,
    /// desktop, capture, input.
    pub fn set_b_input(&self, fleet: bool, desktop: bool, capture: bool, input: bool) {
        set_policy_p7b(
            &self.b.manager,
            (fleet, desktop, capture, input),
            (self.click, self.any_window),
        );
    }

    /// The settings of B with every P7 gate on and the two P7b gates as told
    /// (R13 P7b): `allow_desktop_click`, `allow_desktop_input_any`.
    pub fn set_b_gates(&self, click: bool, any: bool) {
        set_policy_p7b(&self.b.manager, (true, true, true, true), (click, any));
    }

    /// The setting `allow_fleet_control` of A.
    pub fn set_a_fleet(&self, on: bool) {
        set_policy(&self.a.manager, on, false, false);
    }

    /// Every audit row of the deck as one text.
    pub async fn audit_text(&self) -> String {
        self.deck()
            .store
            .audit_log(500)
            .await
            .unwrap()
            .into_iter()
            .map(|e| {
                format!(
                    "{}|{}|{}|{}",
                    e.actor,
                    e.action,
                    e.target.unwrap_or_default(),
                    e.detail.unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The deck restarts: the old runtime ends, a new deck opens the same
    /// database on the same address, and the stations link again.
    pub async fn restart_deck(&mut self) {
        let old = self.slot.take().expect("a deck");
        let addr = old.addr;
        old.crash();
        let fresh = Deck::start(
            &self.tmp.path().join("deck.db"),
            &self.identity,
            addr,
            Vec::new(),
        )
        .await;
        self.slot = Some(fresh);
        let (a, b) = (self.a_id.clone(), self.b_id.clone());
        let store = self.deck().store.clone();
        for id in [a, b] {
            let (store, id) = (store.clone(), id);
            eventually("a station links to the new deck", || {
                let (store, id) = (store.clone(), id.clone());
                async move {
                    store.is_reachable(&id)
                        && store.link_feature(&id, feature::DESKTOP_MCP) == LinkFeature::Negotiated
                }
            })
            .await;
        }
    }

    /// The deck.
    pub fn deck(&self) -> &Deck {
        self.slot.as_ref().expect("a deck")
    }
}

/// The path of the database file of a deck.
pub fn db_of(dir: &Path) -> PathBuf {
    dir.join("deck.db")
}
