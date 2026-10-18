//! The world of the P5b end-to-end proofs (R13 P5b task P1): one real deck
//! (its own runtime and a database file), one real station B (store, manager,
//! artifact service, link runtime) with the fake desktop and the fake
//! `wf-recorder` behind the `shim-exec` seam, and a board client with an
//! owner login. Every link is a real TLS link on loopback.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

pub mod mp4;
pub mod rig;
mod station_b;
pub mod wire;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_deck::DeckNode;
use fleetdeck_deck::DeckStore;
use fleetdeck_deck::NodeError;
use fleetdeck_deck::desktop_capture::recording::RecordingRow as DeckRow;
use fleetdeck_deck::store::LinkFeature;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::StationId;
use fleetdeck_proto::capture::CAPTURE_ID_HEADER;
use fleetdeck_proto::recording::RecordingPhase;
use fleetdeck_proto::version::feature;
use fleetdeck_station::store::capture_ops::RecordingRow;
use serde_json::{Value, json};

use crate::common::board::{BoardClient, board_with_node};
use crate::settings_support::{Headers, elevate, owner, set_password};
use rig::{RecorderRig, recorder_rig};
pub use station_b::StationB;
use wire::{Client, Pairs, Reply};

pub const T: Duration = Duration::from_secs(10);
/// A valid capture ID.
pub const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
/// The mode of `DP-1` of the fake desktop.
pub const WIDTH: u16 = 3440;
pub const HEIGHT: u16 = 1440;
/// The header that declares `artifacts_video.v1`.
pub const VIDEO: (&str, &str) = ("X-FleetDeck-Features", "artifacts_video.v1");

/// The features that an owner recording needs on the link (design §3.1).
const FEATURES: [&str; 6] = [
    feature::DESKTOP,
    feature::DESKTOP_AUTHORITY,
    feature::DESKTOP_CAPTURE,
    feature::ARTIFACTS_DELETE,
    feature::ARTIFACTS_VIDEO,
    feature::DESKTOP_RECORDING,
];

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
/// task of the deck at once, as a crash does) and the database file.
pub struct Deck {
    rt: Option<tokio::runtime::Runtime>,
    pub store: DeckStore,
    pub node: Arc<DeckNode>,
    pub addr: std::net::SocketAddr,
}

impl Deck {
    /// Opens the deck on `db`, listening on `listen`, with `pin` pinned.
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
        let addr = node.listen_addrs()[0];
        Self {
            rt: Some(rt),
            store,
            node,
            addr,
        }
    }

    /// Ends every task of the deck at once.
    pub fn crash(mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

impl Drop for Deck {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

/// What a world starts with.
#[derive(Default)]
pub struct Setup {
    /// An owner password and an elevation (the deletion of an artifact needs
    /// them).
    pub password: bool,
}

/// The world of a proof.
pub struct World {
    pub tmp: tempfile::TempDir,
    pub identity: Arc<Identity>,
    slot: Option<Deck>,
    pub b: StationB,
    pub b_id: StationId,
    pub dir_b: PathBuf,
    board: BoardClient,
    /// Boards of deck instances that ended: their servers stay up, so that a
    /// request in flight is not cut by the test.
    old: Vec<BoardClient>,
    pub client: Client,
    h: Headers,
    password: bool,
}

impl World {
    pub async fn start() -> Self {
        Self::with(Setup::default()).await
    }

    pub async fn with(setup: Setup) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let identity = Arc::new(Identity::generate().unwrap());
        let rig = recorder_rig();
        // The file that the fake recorder writes for a request of `n`
        // seconds is a valid clip of `n` seconds; a test replaces it.
        for n in 1..=12 {
            rig.fixture(n, &mp4::clip(WIDTH, HEIGHT, n));
        }
        rig.default_fixture(&mp4::clip(WIDTH, HEIGHT, 2));
        fleetdeck_station::desktop::install(Arc::clone(&rig.desktop));
        let dir_b = tmp.path().join("b");
        let b = StationB::new(&dir_b, rig, &identity).await;
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
        b.link(deck.addr, identity.public_key(), &[]).await;
        let (board, client, h) = attach(&deck, setup.password).await;
        let b_id = station_id(&deck).await;
        let world = Self {
            tmp,
            identity,
            slot: Some(deck),
            b,
            b_id,
            dir_b,
            board,
            old: Vec::new(),
            client,
            h,
            password: setup.password,
        };
        world.wait_linked().await;
        world
    }

    /// The deck.
    pub fn deck(&self) -> &Deck {
        self.slot.as_ref().expect("a deck")
    }

    /// Waits until the deck has the station with a snapshot and every
    /// feature of an owner recording negotiated.
    async fn wait_linked(&self) {
        let (store, id) = (self.deck().store.clone(), self.b_id.clone());
        eventually("the linked station", || {
            let (store, id) = (store.clone(), id.clone());
            async move {
                store.is_reachable(&id)
                    && store
                        .station(id.clone())
                        .await
                        .unwrap()
                        .is_some_and(|s| s.incarnation.is_some())
                    && FEATURES
                        .iter()
                        .all(|f| store.link_feature(&id, f) == LinkFeature::Negotiated)
            }
        })
        .await;
    }

    /// The headers of an owner request: the login, the CSRF token, an
    /// allowed origin, the capture ID and the video declaration if asked.
    pub fn headers(&self, id: Option<&str>, video: bool) -> Pairs {
        let mut v = self.h.clone();
        v.push((
            "Origin".to_owned(),
            format!("https://deck.tail1234.ts.net:{}", self.board.addr.port()),
        ));
        if let Some(id) = id {
            v.push((CAPTURE_ID_HEADER.to_owned(), id.to_owned()));
        }
        if video {
            v.push((VIDEO.0.to_owned(), VIDEO.1.to_owned()));
        }
        v
    }

    /// Only the login cookie (the raw artifact route needs no more).
    pub fn cookie(&self) -> Pairs {
        self.h
            .iter()
            .filter(|(k, _)| k == "Cookie")
            .cloned()
            .collect()
    }

    /// `GET path` as the owner.
    pub async fn get(&self, path: &str, video: bool) -> Reply {
        self.client.get(path, &self.headers(None, video)).await
    }

    /// `POST path` as the owner with `body`.
    pub async fn post(&self, path: &str, body: Value, video: bool) -> Reply {
        self.client
            .post(path, &self.headers(None, video), body)
            .await
    }

    pub fn act_path() -> &'static str {
        "/api/stations/beta/desktop/act"
    }

    /// The owner start of a recording of `DP-1`, in a task.
    pub fn start_recording(&self, id: &str, seconds: u32) -> tokio::task::JoinHandle<Reply> {
        let (client, headers) = (self.client.clone(), self.headers(Some(id), true));
        let body = json!({"action": "screen_record", "monitor": "DP-1", "seconds": seconds});
        tokio::spawn(async move { client.post(Self::act_path(), &headers, body).await })
    }

    /// The owner stop of the capture `id`.
    pub async fn stop(&self, id: &str) -> Reply {
        let path = format!("/api/stations/beta/desktop/captures/{id}/stop");
        self.post(&path, json!({}), true).await
    }

    /// The status of the capture `id`.
    pub async fn status(&self, id: &str) -> Reply {
        let path = format!("/api/stations/beta/desktop/captures/{id}");
        self.get(&path, true).await
    }

    /// The active recording discovery.
    pub async fn active(&self) -> Reply {
        self.get("/api/stations/beta/desktop/recording", true).await
    }

    /// The artifact list as the board shows it.
    pub async fn artifacts(&self, video: bool) -> Vec<Value> {
        let r = self.get("/api/artifacts", video).await;
        assert_eq!(r.status, 200, "{}", r.text());
        r.json().as_array().cloned().unwrap_or_default()
    }

    /// The path of the raw file of version 1 of an artifact.
    pub fn raw_path(artifact_id: &str) -> String {
        format!("/api/artifacts/beta/{artifact_id}/1/raw/capture.mp4")
    }

    /// Turns the deck setting `desktop_control` on or off.
    pub fn desktop_control(&self, on: bool) {
        set_control(&self.board, on);
    }

    /// Ends the owner login (the board logout).
    pub async fn logout(&self) {
        let r = self.post("/api/auth/logout", json!({}), false).await;
        assert_eq!(r.status, 204, "{}", r.text());
    }

    /// A new owner login (the old one ended).
    pub async fn relogin(&mut self) {
        self.h = owner(&self.board).await;
    }

    /// Waits until the deck has the station linked again, with its features.
    pub async fn wait_reachable(&self) {
        self.wait_linked().await;
    }

    /// An elevation of the owner login (the world has a password).
    pub async fn elevation(&self) -> String {
        assert!(self.password, "the world has no password");
        elevate(&self.board, &self.h).await
    }

    /// The deck fingerprint, the key of the station rows.
    pub fn fp(&self) -> String {
        self.identity.fingerprint().to_string()
    }

    /// The row of the recording on the station.
    pub async fn station_row(&self, id: &str) -> Option<RecordingRow> {
        self.b.store.recording_get(&self.fp(), id).await.unwrap()
    }

    /// The row of the recording on the deck.
    pub async fn deck_row(&self, id: &str) -> Option<DeckRow> {
        self.deck()
            .store
            .recording_get(self.b_id.clone(), id.to_owned())
            .await
            .unwrap()
    }

    /// Waits until the recorder runs and the station says `recording`.
    pub async fn until_recording(&self, id: &str) {
        eventually("the recording", || async {
            self.b.rig.recorder_pid().is_some()
                && self
                    .station_row(id)
                    .await
                    .is_some_and(|r| r.phase == Some(RecordingPhase::Recording))
        })
        .await;
    }

    /// The names in the staging folder and in the artifact folder of B.
    pub fn leftovers(&self) -> (Vec<String>, Vec<String>) {
        let names = |name: &str| -> Vec<String> {
            std::fs::read_dir(self.dir_b.join(name))
                .map(|d| {
                    d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default()
        };
        (names("captures"), names("artifacts"))
    }

    /// The deck restarts: the old runtime ends, a new deck opens the same
    /// database on the same address, and B links again.
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
        let (board, client, h) = attach(&fresh, self.password).await;
        self.old.push(std::mem::replace(&mut self.board, board));
        self.client = client;
        self.h = h;
        self.slot = Some(fresh);
        self.wait_linked().await;
    }
}

/// A board of `deck` with the desktop switch on (the one cell of the board
/// is the cell of the link, as `deck serve` makes it), and an owner login.
async fn attach(deck: &Deck, password: bool) -> (BoardClient, Client, Headers) {
    let board = board_with_node(Arc::clone(&deck.node)).await;
    set_control(&board, true);
    assert!(
        deck.node
            .link()
            .desktop_live
            .set(board.state.live.clone())
            .is_ok()
    );
    if password {
        set_password(&board).await;
    }
    let h = owner(&board).await;
    let client = Client::of(&board);
    (board, client, h)
}

fn set_control(board: &BoardClient, on: bool) {
    let mut live = (*board.state.live.get()).clone();
    live.desktop_control = on;
    board.state.live.publish(live);
}

/// The ID of the station `beta`, once the deck has it.
async fn station_id(deck: &Deck) -> StationId {
    let store = deck.store.clone();
    let found: std::sync::Mutex<Option<StationId>> = std::sync::Mutex::default();
    eventually("a station record", || async {
        let record = store.find_station("beta".to_owned()).await.unwrap();
        let some = record.is_some();
        *found.lock().unwrap() = record.map(|r| r.station_id);
        some
    })
    .await;
    let id = found.lock().unwrap().take();
    id.expect("a station")
}
