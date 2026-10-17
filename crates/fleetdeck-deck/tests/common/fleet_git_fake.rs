//! A fake station for the Fleet Git tests of the deck (T1.5): it answers
//! `station.info`, `fleet_git.projects_set` (with a configurable home check),
//! `fleet_git.push_now` and every session command, and it records each
//! request. The deck sends the snapshot after the first `station.info`, so
//! this fake answers it (the plain `Fixture::connect` leaves it unanswered).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_deck::link::runtime::STATION_SCOPES;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_link::control::{Control, Inbound};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants};
use fleetdeck_proto::fleet_git::{
    GitSnapshot, HomeCheck, HomeCheckState, PushNowAnswer, PushState, SnapshotAck,
};
use fleetdeck_proto::link::{ControlFrame, Outcome, StreamMode, StreamStart, WireError};
use fleetdeck_proto::{CommandId, IncarnationId, StationId};
use serde_json::{Value, json};

use std::collections::BTreeSet;

use fleetdeck_proto::integrations::{ForgeBinding, ForgeKind, Project, ProjectInput};
use fleetdeck_proto::link::Scope;

use super::board::{BoardClient, Http, board};
use super::fleet_git_land::{GitReply, LandReply, RadarReply};
use super::{Fixture, T, accepted, fixture};

/// What the fake answers to a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Verified,
    Mismatch,
}

/// What the fake answers to `fleet_git.push_now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushAnswer {
    State(PushState),
    Error(&'static str),
}

/// What the fake answers to `project.ensure_home` and `project.home_status`.
#[derive(Debug, Clone)]
pub enum Reply {
    /// A `HomeStatus` with this state and these fields.
    State {
        state: &'static str,
        path: Option<String>,
        reserved: Option<String>,
        code: Option<&'static str>,
        message: Option<String>,
    },
    /// A link error with this code.
    Error(&'static str),
}

impl Reply {
    pub fn cloning(reserved: &str) -> Self {
        Self::State {
            state: "cloning",
            path: None,
            reserved: Some(reserved.to_owned()),
            code: None,
            message: None,
        }
    }

    pub fn ready(path: &str) -> Self {
        Self::State {
            state: "ready",
            path: Some(path.to_owned()),
            reserved: Some(path.to_owned()),
            code: None,
            message: None,
        }
    }

    pub fn failed(code: &'static str, message: &str) -> Self {
        Self::State {
            state: "failed",
            path: None,
            reserved: Some("/srv/projects/App".to_owned()),
            code: Some(code),
            message: Some(message.to_owned()),
        }
    }
}

/// What the fake answers to `fleet_git.share_verdict` (FG-12).
#[derive(Debug, Clone)]
pub enum ShareReply {
    /// A `ShareVerdictAnswer` as JSON.
    Answer(Value),
    /// A link error with this code.
    Error(&'static str),
    /// No answer at all (the deck times out).
    Silent,
}

impl ShareReply {
    /// A clear share: the path is `ok`, nothing is exposed.
    pub fn clear() -> Self {
        Self::Answer(json!({
            "inventory_revision": 7, "checked_at": 1234,
            "path_state": "ok", "exposure_state": "clear", "exposed": []
        }))
    }
}

/// What a fake station reports in `station.info`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FakeOptions {
    /// `fleet_git.allow_local_remotes`; `None`: no `fleet_git` object.
    pub fleet_git: Option<bool>,
}

pub struct FakeGit {
    seen: Mutex<Vec<(String, Value)>>,
    pub check: Mutex<Check>,
    /// Answer the next snapshot with `stored: false` and this revision.
    pub stale_once: Mutex<Option<u64>>,
    pub push: Mutex<PushAnswer>,
    /// The answer of `project.ensure_home`.
    pub ensure: Mutex<Reply>,
    /// The answer of `project.home_status`.
    pub status: Mutex<Reply>,
    /// The attempts that this station knows, by id, with their generation.
    pub attempts: Mutex<std::collections::BTreeMap<String, u64>>,
    /// The answer of `fleet_git.share_verdict`.
    pub share_verdict: Mutex<ShareReply>,
    /// The answers of `session.land` (FG-8).
    pub land: Mutex<std::collections::VecDeque<LandReply>>,
    /// The answer of `fleet_git.land_status`.
    pub land_status: Mutex<Value>,
    /// The answer of `fleet_git.land_abandon`.
    pub land_abandon: Mutex<Value>,
    /// The answer of `project.radar` (FG-7).
    pub radar: Mutex<RadarReply>,
    /// The answer of `project.branches` (FG-10).
    pub branches: Mutex<GitReply>,
    /// The answer of `project.fetch` (FG-10).
    pub fetch: Mutex<GitReply>,
    /// The answer of `files.list`: `Err((code, message))` is a link error.
    pub files: Mutex<Result<Value, (&'static str, &'static str)>>,
    /// The `station.info` answer (the setters change it).
    info: Mutex<Value>,
    pub control: Control,
}

/// The node of a fake station and what the deck knows of it.
struct Peer {
    node: NodeConfig,
    station: StationId,
    inc: IncarnationId,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakeGit {
    /// Links a fake station named `name` to the deck of `f`, answers its
    /// first `station.info` and starts the responder.
    pub async fn connect(f: &mut Fixture, name: &str) -> Arc<Self> {
        Self::connect_with(f, name, FakeOptions::default()).await
    }

    /// [`Self::connect`] with what the station reports in `station.info`.
    pub async fn connect_with(f: &mut Fixture, name: &str, options: FakeOptions) -> Arc<Self> {
        let peer = Peer {
            node: f.station_node.clone(),
            station: f.station.clone(),
            inc: f.inc.clone(),
        };
        let fake = Self::link(f, &peer, name, options).await;
        f.synced = true;
        fake
    }

    /// Pins a second station named `name` and links its fake to the deck.
    pub async fn connect_second(
        f: &mut Fixture,
        name: &str,
        options: FakeOptions,
    ) -> (StationId, Arc<Self>) {
        let identity = Arc::new(Identity::generate().unwrap());
        let rec = f
            .store
            .pin_station(NewStation {
                key: identity.public_key(),
                name: name.to_owned(),
                scopes: station_scopes(),
                dial_url: None,
            })
            .await
            .unwrap();
        f.ctx.peers.refresh(rec.station_id.clone()).await.unwrap();
        f.deck_node.pins.pin(identity.public_key(), Role::Station);
        let inc = IncarnationId::from(format!("{name}-db"));
        let grants = Grants::new();
        grants.grant(
            f.deck_key.fingerprint(),
            [Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin],
        );
        let node = NodeConfig::new(
            identity,
            LocalHello::new(Role::Station, inc.clone()),
            Arc::new(Epochs::in_memory()),
        )
        .with_store(grants);
        node.pins.pin(f.deck_key, Role::Deck);
        let peer = Peer {
            node,
            station: rec.station_id.clone(),
            inc,
        };
        let fake = Self::link(f, &peer, name, options).await;
        (rec.station_id, fake)
    }

    async fn link(f: &mut Fixture, peer: &Peer, name: &str, options: FakeOptions) -> Arc<Self> {
        let (d, a) = tokio::time::timeout(T, async {
            tokio::join!(
                node::dial(&peer.node, f.listener.local_addr, f.deck_key),
                f.listener.next_link()
            )
        })
        .await
        .expect("link setup");
        let at_deck = fleetdeck_link::start(&f.deck_node, a.expect("listener"));
        tokio::spawn(fleetdeck_deck::link::serve(Arc::clone(&f.ctx), at_deck));
        let mut station = fleetdeck_link::start(&peer.node, d.unwrap());
        let info = info(name, &peer.inc, options);
        loop {
            let message = tokio::time::timeout(T, station.inbound.recv())
                .await
                .expect("a request")
                .expect("the link ended");
            let Inbound::Request(req) = message else {
                continue;
            };
            if req.method == "station.info" {
                station
                    .control
                    .respond(req.id, Outcome::Ok(info.clone()))
                    .await
                    .unwrap();
            } else if req.method == "events.replay" {
                let start = StreamStart {
                    mode: StreamMode::Snapshot,
                    incarnation: peer.inc.clone(),
                    from: 0,
                };
                station
                    .control
                    .respond(req.id, Outcome::Ok(json!(start)))
                    .await
                    .unwrap();
                for frame in [
                    ControlFrame::SnapshotBegin {
                        incarnation: peer.inc.clone(),
                        watermark: 0,
                        session_count: 0,
                    },
                    ControlFrame::SnapshotEnd {
                        incarnation: peer.inc.clone(),
                        watermark: 0,
                        coordinator: None,
                    },
                ] {
                    station.control.send(frame).await.unwrap();
                }
                break;
            }
        }
        let id = peer.station.clone();
        f.wait("the snapshot", |f| {
            let (store, id) = (f.store.clone(), id.clone());
            async move {
                store
                    .station(id)
                    .await
                    .unwrap()
                    .unwrap()
                    .incarnation
                    .is_some()
            }
        })
        .await;
        let fake = Arc::new(Self {
            seen: Mutex::default(),
            check: Mutex::new(Check::Verified),
            stale_once: Mutex::new(None),
            push: Mutex::new(PushAnswer::State(PushState::Due)),
            ensure: Mutex::new(Reply::cloning("/srv/projects/App")),
            status: Mutex::new(Reply::cloning("/srv/projects/App")),
            attempts: Mutex::default(),
            share_verdict: Mutex::new(ShareReply::clear()),
            land: Mutex::new(std::collections::VecDeque::from([LandReply::Accept])),
            land_status: Mutex::new(json!({ "state": "unknown" })),
            land_abandon: Mutex::new(json!({ "state": "busy" })),
            radar: Mutex::new(RadarReply::clear()),
            branches: Mutex::new(GitReply::Answer(json!({
                "remote": "origin", "default_branch": "main",
                "branches": [], "truncated": false
            }))),
            fetch: Mutex::new(GitReply::Answer(json!({ "fetched_at": 7 }))),
            files: Mutex::new(Ok(json!({
                "root": "/artifacts", "path": "", "entries": [], "truncated": false
            }))),
            info: Mutex::new(info.clone()),
            control: station.control.clone(),
        });
        let me = Arc::clone(&fake);
        tokio::spawn(async move {
            while let Some(message) = station.inbound.recv().await {
                if let Inbound::Request(req) = message {
                    me.answer(&station.control, req).await;
                }
            }
        });
        fake
    }

    async fn answer(&self, control: &Control, req: fleetdeck_link::control::InboundRequest) {
        lock(&self.seen).push((req.method.clone(), req.params.clone()));
        let outcome = match req.method.as_str() {
            "station.info" => Outcome::Ok(lock(&self.info).clone()),
            "fleet_git.share_verdict" => match lock(&self.share_verdict).clone() {
                ShareReply::Answer(v) => Outcome::Ok(v),
                ShareReply::Error(code) => Outcome::Err(WireError::new(code, "the fake says no")),
                ShareReply::Silent => return,
            },
            "files.list" => match lock(&self.files).clone() {
                Ok(v) => Outcome::Ok(v),
                Err((code, message)) => Outcome::Err(WireError::new(code, message)),
            },
            "fleet_git.projects_set" => self.snapshot(&req.params),
            "session.land" => self.land_answer(req.command_id.as_ref()),
            "fleet_git.land_status" => Outcome::Ok(lock(&self.land_status).clone()),
            "fleet_git.land_abandon" => Outcome::Ok(lock(&self.land_abandon).clone()),
            "project.radar" => lock(&self.radar).outcome(),
            "project.branches" => lock(&self.branches).outcome(),
            "project.fetch" => lock(&self.fetch).outcome(),
            // One open GitHub issue `#7`, for the batch start.
            "issues.get" => Outcome::Ok(json!({
                "summary": {
                    "ref": { "system": "github", "key": "#7", "url": "u", "title": "Fix it" },
                    "status": "open", "assignee": null, "priority": null,
                    "labels": [], "updated_at": 0
                },
                "body_markdown": "body", "estimate": null, "comments": [],
                "linked_prs": [], "images": []
            })),
            "project.ensure_home" => self.ensure_home(&req.params),
            "project.home_status" => self.home_status(&req.params),
            "fleet_git.push_now" => match *lock(&self.push) {
                PushAnswer::State(state) => Outcome::Ok(json!(PushNowAnswer { state })),
                PushAnswer::Error(code) => Outcome::Err(WireError::new(code, "the fake says no")),
            },
            _ => match &req.command_id {
                Some(id) => accepted(id),
                None => Outcome::Err(WireError::new("not_implemented", "the fake")),
            },
        };
        let _ = control.respond(req.id, outcome).await;
    }

    /// The next reply to `session.land` (the last one repeats).
    fn land_answer(&self, id: Option<&CommandId>) -> Outcome {
        let Some(id) = id else {
            return Outcome::Err(WireError::new("not_implemented", "no command id"));
        };
        let reply = {
            let mut replies = lock(&self.land);
            if replies.len() > 1 {
                replies.pop_front()
            } else {
                replies.front().cloned()
            }
        };
        match reply.unwrap_or(LandReply::Accept) {
            LandReply::Accept => accepted(id),
            LandReply::Reject(code, message) => {
                let code = serde_json::from_value(json!(code)).expect("a reject code");
                let receipt = fleetdeck_proto::Receipt::rejected(
                    id.clone(),
                    fleetdeck_proto::Reject::new(code, message),
                    0,
                );
                Outcome::Ok(json!(receipt))
            }
            LandReply::Error(code) => Outcome::Err(WireError::new(code, "the fake says no")),
        }
    }

    /// A reply as the answer for `attempt` of `generation`.
    fn reply(reply: &Reply, attempt: &str, generation: u64) -> Outcome {
        match reply {
            Reply::Error(code) => Outcome::Err(WireError::new(*code, "the fake says no")),
            Reply::State {
                state,
                path,
                reserved,
                code,
                message,
            } => {
                let mut status = json!({
                    "attempt_id": attempt, "generation": generation, "state": state
                });
                for (key, value) in [
                    ("path", path.clone()),
                    ("reserved_path", reserved.clone()),
                    ("code", code.map(str::to_owned)),
                    ("message", message.clone()),
                ] {
                    if let Some(value) = value {
                        status[key] = json!(value);
                    }
                }
                Outcome::Ok(status)
            }
        }
    }

    fn ensure_home(&self, params: &Value) -> Outcome {
        let attempt = params["attempt_id"].as_str().unwrap_or_default().to_owned();
        let generation = params["generation"].as_u64().unwrap_or_default();
        lock(&self.attempts).insert(attempt.clone(), generation);
        Self::reply(&lock(&self.ensure), &attempt, generation)
    }

    fn home_status(&self, params: &Value) -> Outcome {
        let attempt = params["attempt_id"].as_str().unwrap_or_default().to_owned();
        let Some(generation) = lock(&self.attempts).get(&attempt).copied() else {
            return Outcome::Err(WireError::new("unknown_attempt", "no such attempt"));
        };
        Self::reply(&lock(&self.status), &attempt, generation)
    }

    fn snapshot(&self, params: &Value) -> Outcome {
        let snapshot: GitSnapshot = serde_json::from_value(params.clone()).expect("a snapshot");
        if let Some(revision) = lock(&self.stale_once).take() {
            return Outcome::Ok(json!(SnapshotAck {
                stored: false,
                revision,
                checks: Vec::new(),
            }));
        }
        let state = match *lock(&self.check) {
            Check::Verified => HomeCheckState::Verified,
            Check::Mismatch => HomeCheckState::Mismatch,
        };
        let checks = snapshot
            .projects
            .iter()
            .map(|p| HomeCheck {
                project_id: p.project_id.clone(),
                generation: p.home_generation,
                state,
                canonical_path: Some(p.home_path.clone()),
                seen: None,
                code: None,
            })
            .collect();
        Outcome::Ok(json!(SnapshotAck {
            stored: true,
            revision: snapshot.revision,
            checks,
        }))
    }

    /// Sets one field of the `station.info` answer.
    pub fn set_info(&self, key: &str, value: Value) {
        lock(&self.info)[key] = value;
    }

    /// The traits that the station reports (FG-6).
    pub fn set_traits(&self, os: &str, arch: &str, battery: Option<bool>, labels: &[&str]) {
        let mut traits = json!({ "os": os, "arch": arch, "labels": labels });
        if let Some(b) = battery {
            traits["on_battery"] = json!(b);
        }
        self.set_info("traits", traits);
    }

    /// The Taildrive report that the station reports (FG-12).
    pub fn set_taildrive(&self, report: Value) {
        self.set_info("taildrive", report);
    }

    /// A signed-in `claude` instance, so the station can take a new session.
    pub fn set_signed_in(&self) {
        self.set_info(
            "providers",
            json!([{
                "instance": "claude", "provider": "claude", "version": "2.1.0",
                "auth": "signed_in"
            }]),
        );
    }

    /// Hands the current `station.info` answer to the deck store at once
    /// (the deck asks again only every minute).
    pub fn publish(&self, store: &fleetdeck_deck::DeckStore, station: &StationId) {
        let info = lock(&self.info).clone();
        store.set_station_info(
            station,
            serde_json::from_value(info).expect("a station.info"),
        );
    }

    /// The params of every request of `method` so far.
    pub fn requests(&self, method: &str) -> Vec<Value> {
        lock(&self.seen)
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// Waits until the fake got at least `n` requests of `method`.
    pub async fn wait_for(&self, method: &str, n: usize) -> Vec<Value> {
        let deadline = tokio::time::Instant::now() + T;
        loop {
            let got = self.requests(method);
            if got.len() >= n {
                return got;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out: {n} x {method}, got {got:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The snapshots received so far.
    pub fn snapshots(&self) -> Vec<GitSnapshot> {
        self.requests("fleet_git.projects_set")
            .into_iter()
            .map(|p| serde_json::from_value(p).expect("a snapshot"))
            .collect()
    }

    /// The `session.start` commands received so far, as `CommandEnvelope`s.
    pub fn starts(&self) -> Vec<fleetdeck_proto::command::CommandEnvelope> {
        self.requests("session.start")
            .into_iter()
            .map(|p| serde_json::from_value(p).expect("a command envelope"))
            .collect()
    }
}

fn info(name: &str, inc: &fleetdeck_proto::IncarnationId, options: FakeOptions) -> Value {
    let mut info = json!({
        "name": name,
        "incarnation": inc,
        "head": 0,
        "live_sessions": 0,
    });
    if let Some(local) = options.fleet_git {
        info["fleet_git"] = json!({
            "clone_root": "/srv/projects",
            "allow_check_cmd": false,
            "allow_local_remotes": local,
        });
    }
    info
}

/// A command id of a test.
pub fn command_id(n: u32) -> CommandId {
    CommandId::from(format!("cmd-{n}"))
}

// ----- the shared setup of the Fleet Git deck tests -----

pub const DAY: i64 = 24 * 60 * 60 * 1000;
pub const REMOTE: &str = "git@github.com:acme/app.git";

pub fn input(name: &str, path: &str) -> ProjectInput {
    ProjectInput {
        name: name.into(),
        station: "fake".into(),
        path: path.into(),
        forge: Some(ForgeBinding {
            kind: ForgeKind::Github,
            host: "github.com".into(),
            repo: "acme/app".into(),
        }),
        trackers: vec![],
        prompt_template: None,
        review_prompt: None,
    }
}

pub struct Setup {
    pub f: Fixture,
    pub c: BoardClient,
    pub app: Project,
    pub cookie: String,
    pub csrf: String,
}

impl Setup {
    pub fn h(&self) -> [(&str, &str); 2] {
        [
            ("Cookie", self.cookie.as_str()),
            ("X-CSRF-Token", self.csrf.as_str()),
        ]
    }

    pub async fn put(&self, id: &str, body: Value) -> Http {
        self.c
            .req(
                "PUT",
                &format!("/api/projects/{id}/git"),
                &self.h(),
                Some(body),
            )
            .await
    }

    /// Saves `settings` (and `remote`) of the app project at its stored
    /// revision and returns the answer.
    pub async fn save(&self, settings: Value, remote: Option<&str>) -> Http {
        let revision = self.app().await.git_revision;
        self.put(&self.app.project_id, body(settings, revision, remote))
            .await
    }

    pub async fn app(&self) -> Project {
        self.f
            .store
            .project(self.app.project_id.clone())
            .await
            .unwrap()
            .unwrap()
    }

    pub async fn start(&self, id: &str, cwd: &str, workspace: Value) -> Http {
        let mut command = json!({
            "op": "start",
            "spec": { "provider_instance": "claude", "cwd": cwd },
            "prompt": null
        });
        if !workspace.is_null() {
            command["workspace"] = workspace;
        }
        self.c
            .post(
                "/api/commands/session.start",
                &self.h(),
                json!({ "station": "fake", "command_id": id, "command": command }),
            )
            .await
    }

    /// Waits until the home of the app project reads `state`.
    pub async fn home(&self, state: &str) {
        let id = self.app.project_id.clone();
        self.f
            .wait(&format!("home {state}"), move |f| {
                let (store, id, state) = (f.store.clone(), id.clone(), state.to_owned());
                async move {
                    let p = store.project(id).await.unwrap().unwrap();
                    serde_json::to_value(p.homes[0].state).unwrap() == json!(state)
                }
            })
            .await;
    }
}

pub fn body(settings: Value, revision: u64, remote: Option<&str>) -> Value {
    let mut b = json!({ "settings": settings, "revision": revision });
    if let Some(remote) = remote {
        b["remote_url"] = json!(remote);
    }
    b
}

pub async fn setup() -> Setup {
    setup_with(fixture().await).await
}

/// The scopes of a test station: exactly the pairing grant `STATION_SCOPES`,
/// which `fleet_git.artifact_authorize` (scope `admin`) needs.
pub fn station_scopes() -> BTreeSet<Scope> {
    STATION_SCOPES.into_iter().collect()
}

/// [`setup`] with a first station that holds `scopes`.
pub async fn setup_scopes(scopes: BTreeSet<Scope>) -> Setup {
    setup_with(super::fixture_with(scopes).await).await
}

/// [`setup`] with a first station that holds [`station_scopes`].
pub async fn setup_reading() -> Setup {
    setup_with(super::fixture_with(station_scopes()).await).await
}

/// [`setup`] on a deck store that is a file of `dir`.
pub async fn setup_in(dir: &std::path::Path) -> Setup {
    setup_with(super::fixture_file(dir).await).await
}

async fn setup_with(f: Fixture) -> Setup {
    let app = f
        .store
        .create_project(f.station.0.clone(), input("App", "/work/app"))
        .await
        .unwrap();
    let c = board(Arc::clone(&f.ctx)).await;
    let (cookie, csrf) = c.login().await;
    Setup {
        f,
        c,
        app,
        cookie,
        csrf,
    }
}

/// A setup with a fake station that answers the link, and a sync project.
pub async fn synced() -> (Setup, Arc<FakeGit>) {
    let mut s = setup().await;
    let r = s.save(json!({ "sync": true }), Some(REMOTE)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let fake = FakeGit::connect(&mut s.f, "fake").await;
    s.home("ready").await;
    (s, fake)
}

pub fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
}

pub fn invalid(r: &Http, field: &str, message: &str) {
    expect(r, 400, "invalid_git_settings");
    assert_eq!(r.body["field"], field, "{}", r.body);
    assert_eq!(r.body["message"], message, "{}", r.body);
}

pub async fn bearer(c: &BoardClient, name: &str, scopes: &[Scope]) -> (String, String) {
    let (_, t) = c
        .store
        .create_token(
            name.into(),
            scopes.iter().copied().collect::<BTreeSet<_>>(),
            DAY,
        )
        .await
        .unwrap();
    ("Authorization".to_owned(), format!("Bearer {}", t.expose()))
}

/// Runs the home attempt sender of the deck of `f` (the fixture does not
/// start it). `stop.send(true)` ends it.
pub fn spawn_sender(
    f: &Fixture,
    send: bool,
) -> (
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
) {
    let (stop, rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(fleetdeck_deck::fleet_git_homes::run(
        f.store.clone(),
        Arc::clone(&f.ctx),
        Arc::clone(&f.ctx.homes_kick),
        rx,
        fleetdeck_deck::fleet_git_homes::HomesRunOptions { send },
    ));
    (stop, handle)
}
