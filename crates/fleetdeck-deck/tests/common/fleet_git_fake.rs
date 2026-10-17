//! A fake station for the Fleet Git tests of the deck (T1.5): it answers
//! `station.info`, `fleet_git.projects_set` (with a configurable home check),
//! `fleet_git.push_now` and every session command, and it records each
//! request. The deck sends the snapshot after the first `station.info`, so
//! this fake answers it (the plain `Fixture::connect` leaves it unanswered).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_link::control::{Control, Inbound};
use fleetdeck_link::node;
use fleetdeck_proto::CommandId;
use fleetdeck_proto::fleet_git::{
    GitSnapshot, HomeCheck, HomeCheckState, PushNowAnswer, PushState, SnapshotAck,
};
use fleetdeck_proto::link::{ControlFrame, Outcome, StreamMode, StreamStart, WireError};
use serde_json::{Value, json};

use std::collections::BTreeSet;

use fleetdeck_proto::integrations::{ForgeBinding, ForgeKind, Project, ProjectInput};
use fleetdeck_proto::link::Scope;

use super::board::{BoardClient, Http, board};
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

pub struct FakeGit {
    seen: Mutex<Vec<(String, Value)>>,
    pub check: Mutex<Check>,
    /// Answer the next snapshot with `stored: false` and this revision.
    pub stale_once: Mutex<Option<u64>>,
    pub push: Mutex<PushAnswer>,
    pub control: Control,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakeGit {
    /// Links a fake station named `name` to the deck of `f`, answers its
    /// first `station.info` and starts the responder.
    pub async fn connect(f: &mut Fixture, name: &str) -> Arc<Self> {
        let (d, a) = tokio::time::timeout(T, async {
            tokio::join!(
                node::dial(&f.station_node, f.listener.local_addr, f.deck_key),
                f.listener.next_link()
            )
        })
        .await
        .expect("link setup");
        let at_deck = fleetdeck_link::start(&f.deck_node, a.expect("listener"));
        tokio::spawn(fleetdeck_deck::link::serve(Arc::clone(&f.ctx), at_deck));
        let mut station = fleetdeck_link::start(&f.station_node, d.unwrap());
        let info = info(name, &f.inc);
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
                    incarnation: f.inc.clone(),
                    from: 0,
                };
                station
                    .control
                    .respond(req.id, Outcome::Ok(json!(start)))
                    .await
                    .unwrap();
                for frame in [
                    ControlFrame::SnapshotBegin {
                        incarnation: f.inc.clone(),
                        watermark: 0,
                        session_count: 0,
                    },
                    ControlFrame::SnapshotEnd {
                        incarnation: f.inc.clone(),
                        watermark: 0,
                        coordinator: None,
                    },
                ] {
                    station.control.send(frame).await.unwrap();
                }
                break;
            }
        }
        f.wait("the snapshot", |f| {
            let (store, id) = (f.store.clone(), f.station.clone());
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
        f.synced = true;
        let fake = Arc::new(Self {
            seen: Mutex::default(),
            check: Mutex::new(Check::Verified),
            stale_once: Mutex::new(None),
            push: Mutex::new(PushAnswer::State(PushState::Due)),
            control: station.control.clone(),
        });
        let me = Arc::clone(&fake);
        tokio::spawn(async move {
            while let Some(message) = station.inbound.recv().await {
                if let Inbound::Request(req) = message {
                    me.answer(&station.control, req, &info).await;
                }
            }
        });
        fake
    }

    async fn answer(
        &self,
        control: &Control,
        req: fleetdeck_link::control::InboundRequest,
        info: &Value,
    ) {
        lock(&self.seen).push((req.method.clone(), req.params.clone()));
        let outcome = match req.method.as_str() {
            "station.info" => Outcome::Ok(info.clone()),
            "fleet_git.projects_set" => self.snapshot(&req.params),
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

fn info(name: &str, inc: &fleetdeck_proto::IncarnationId) -> Value {
    json!({
        "name": name,
        "incarnation": inc,
        "head": 0,
        "live_sessions": 0,
    })
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
    let f = fixture().await;
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
