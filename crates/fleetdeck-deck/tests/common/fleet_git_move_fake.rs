//! A fake station for the session move tests of the deck (FG-9, T4.4b): it
//! answers `station.info`, the snapshot and every move method from a script,
//! serves the export stream, receives the import stream, and logs each
//! request in one log that the stations of a test share. Include it with
//! `#[path = "common/fleet_git_move_fake.rs"] mod move_fake;` after `mod
//! common;`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_deck::fleet_git_move::{Deadlines, MoveOptions};
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_link::control::{Control, Inbound, InboundRequest};
use fleetdeck_link::data::{Data, DataEvent};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants};
use fleetdeck_proto::command::{CommandResult, Receipt, ReceiptState, Reject, RejectCode};
use fleetdeck_proto::fleet_git::{GitSnapshot, HomeCheck, HomeCheckState, SnapshotAck};
use fleetdeck_proto::link::{ControlFrame, Outcome, Scope, StreamMode, StreamStart, WireError};
use fleetdeck_proto::{CommandId, IncarnationId, StationId};
use serde_json::{Value, json};

use crate::common::fleet_git_fake::station_scopes;
use crate::common::{Fixture, T};

pub const OID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// What the fake answers to one request.
#[derive(Debug, Clone)]
pub enum R {
    /// The default answer of the method.
    Default,
    /// An answer value.
    Value(Value),
    /// A link error with this code.
    Err(&'static str),
    /// An accepted receipt with this new session id.
    Accept(&'static str),
    /// A rejected receipt with this `RejectCode` name and message.
    Reject(&'static str, &'static str),
    /// No answer at all (the deck times out).
    Silent,
    /// The default answer after this many milliseconds.
    After(u64),
}

/// The shared log of the requests of all stations: `<station>:<method>`.
#[derive(Clone, Default)]
pub struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    pub fn all(&self) -> Vec<String> {
        lock(&self.0).clone()
    }

    pub fn push(&self, entry: String) {
        lock(&self.0).push(entry);
    }

    /// The entries that name a move method, in order.
    pub fn moves(&self) -> Vec<String> {
        self.all()
            .into_iter()
            .filter(|e| e.contains(":session.move_"))
            .collect()
    }

    pub fn count(&self, entry: &str) -> usize {
        self.all().iter().filter(|e| e.as_str() == entry).count()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the export stream carries and what the import stream does.
pub struct Files {
    pub bytes: Vec<u8>,
    /// Answer `session.move_export` with no manifest.
    pub branch_only: bool,
    /// The source resets the export stream with this reason.
    pub export_reset: Option<&'static str>,
    /// The target resets the import stream with this reason at the end.
    pub import_reset: Option<&'static str>,
    /// The `total` of the manifest, when it differs from `bytes`.
    pub total: Option<u64>,
    /// The `provider_instance` of the export answer (a real station always
    /// sends it; FG-9).
    pub provider_instance: Option<&'static str>,
}

pub struct MoveFake {
    pub name: String,
    pub station: StationId,
    pub log: Log,
    seen: Mutex<Vec<(String, Value, Option<CommandId>)>>,
    script: Mutex<HashMap<String, VecDeque<R>>>,
    pub files: Mutex<Files>,
    /// The bytes that the import stream received.
    pub imported: Mutex<Vec<u8>>,
    /// The import streams that ended: `closed` or the reset reason.
    pub import_ends: Mutex<Vec<String>>,
    data: Mutex<Option<Data>>,
}

struct Peer<'a> {
    node: &'a NodeConfig,
    station: &'a StationId,
    inc: &'a IncarnationId,
}

impl MoveFake {
    /// Links the station of the fixture.
    pub async fn connect_first(f: &mut Fixture, name: &str, log: &Log) -> Arc<Self> {
        let node = f.station_node.clone();
        let (station, inc) = (f.station.clone(), f.inc.clone());
        let peer = Peer {
            node: &node,
            station: &station,
            inc: &inc,
        };
        let fake = Self::link(f, &peer, name, log).await;
        f.synced = true;
        fake
    }

    /// Pins a second station and links its fake.
    pub async fn connect_second(f: &mut Fixture, name: &str, log: &Log) -> Arc<Self> {
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
            node: &node,
            station: &rec.station_id,
            inc: &inc,
        };
        Self::link(f, &peer, name, log).await
    }

    async fn link(f: &mut Fixture, peer: &Peer<'_>, name: &str, log: &Log) -> Arc<Self> {
        let (d, a) = tokio::time::timeout(T, async {
            tokio::join!(
                node::dial(peer.node, f.listener.local_addr, f.deck_key),
                f.listener.next_link()
            )
        })
        .await
        .expect("link setup");
        let at_deck = fleetdeck_link::start(&f.deck_node, a.expect("listener"));
        tokio::spawn(fleetdeck_deck::link::serve(Arc::clone(&f.ctx), at_deck));
        let mut station = fleetdeck_link::start(peer.node, d.unwrap());
        let info = info(name, peer.inc);
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
        let data = station.data.take();
        let fake = Arc::new(Self {
            name: name.to_owned(),
            station: peer.station.clone(),
            log: log.clone(),
            seen: Mutex::default(),
            script: Mutex::default(),
            files: Mutex::new(Files {
                bytes: b"the moved conversation".to_vec(),
                branch_only: false,
                export_reset: None,
                import_reset: None,
                total: None,
                provider_instance: Some("claude"),
            }),
            imported: Mutex::default(),
            import_ends: Mutex::default(),
            data: Mutex::new(data.as_ref().map(|d| d.handle.clone())),
        });
        let me = Arc::clone(&fake);
        let control = station.control.clone();
        tokio::spawn(async move {
            while let Some(message) = station.inbound.recv().await {
                if let Inbound::Request(req) = message {
                    me.answer(&control, req).await;
                }
            }
        });
        if let Some(data) = data {
            let me = Arc::clone(&fake);
            tokio::spawn(async move { me.serve_data(data.handle, data.events).await });
        }
        fake
    }

    /// The answers of `method`, in order; the last one repeats.
    pub fn script(&self, method: &str, replies: Vec<R>) {
        lock(&self.script).insert(method.to_owned(), replies.into());
    }

    /// The params of each request of `method`, in order.
    pub fn requests(&self, method: &str) -> Vec<Value> {
        lock(&self.seen)
            .iter()
            .filter(|(m, _, _)| m == method)
            .map(|(_, p, _)| p.clone())
            .collect()
    }

    /// The command ids of each request of `method`.
    pub fn command_ids(&self, method: &str) -> Vec<String> {
        lock(&self.seen)
            .iter()
            .filter(|(m, _, _)| m == method)
            .filter_map(|(_, _, id)| id.as_ref().map(|i| i.as_str().to_owned()))
            .collect()
    }

    pub async fn wait_for(&self, method: &str, n: usize) -> Vec<Value> {
        let deadline = tokio::time::Instant::now() + T;
        loop {
            let found = self.requests(method);
            if found.len() >= n {
                return found;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out: {n} x {method} on {} ({:?})",
                self.name,
                lock(&self.seen)
                    .iter()
                    .map(|s| s.0.clone())
                    .collect::<Vec<_>>()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn next(&self, method: &str) -> R {
        let mut script = lock(&self.script);
        match script.get_mut(method) {
            Some(q) if q.len() > 1 => q.pop_front().unwrap_or(R::Default),
            Some(q) => q.front().cloned().unwrap_or(R::Default),
            None => R::Default,
        }
    }

    async fn answer(&self, control: &Control, req: InboundRequest) {
        lock(&self.seen).push((
            req.method.clone(),
            req.params.clone(),
            req.command_id.clone(),
        ));
        if req.method.starts_with("session.move_") {
            self.log.push(format!("{}:{}", self.name, req.method));
        }
        let outcome = match self.next(&req.method) {
            R::Silent => return,
            R::After(ms) => {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                self.default(&req)
            }
            R::Err(code) => Outcome::Err(WireError::new(code, "the fake says no")),
            R::Value(v) => Outcome::Ok(v),
            R::Accept(session) => receipt(&req, Some(session), None),
            R::Reject(code, message) => receipt(&req, None, Some((code, message))),
            R::Default => self.default(&req),
        };
        let _ = control.respond(req.id, outcome).await;
    }

    fn token(&self, purpose: &str) -> String {
        let data = lock(&self.data);
        data.as_ref()
            .expect("a data channel")
            .issue_token(purpose)
            .expect("a token")
            .as_str()
            .to_owned()
    }

    fn default(&self, req: &InboundRequest) -> Outcome {
        let move_id = req.params["move_id"].as_str().unwrap_or("m").to_owned();
        match req.method.as_str() {
            "station.info" => Outcome::Ok(info(&self.name, &IncarnationId::from("x"))),
            "fleet_git.projects_set" => self.snapshot(&req.params),
            "session.move_out" | "session.move_finish" | "session.move_undo" => {
                receipt(req, None, None)
            }
            "session.move_in" => receipt(req, Some("new-1"), None),
            "session.move_export" => {
                let files = lock(&self.files);
                let publication = json!({
                    "local_branch": "fd/alpha/s1", "dest": "github.com/acme/app",
                    "ref_name": "fd/alpha/s1", "oid": OID
                });
                if files.branch_only {
                    return Outcome::Ok(json!({
                        "publication": publication,
                        "provider_instance": files.provider_instance,
                    }));
                }
                let size = files.bytes.len() as u64;
                Outcome::Ok(json!({
                    "publication": publication,
                    "manifest": {
                        "provider": "claude", "native_session_id": "nat-1",
                        "files": [{ "key": "main", "rel": "x.jsonl", "size": size,
                                    "sha256": "0".repeat(64) }],
                        "total": files.total.unwrap_or(size)
                    },
                    "stream_token": self.token(&format!("move-export:{move_id}")),
                    "provider_instance": files.provider_instance,
                }))
            }
            "session.move_prepare" => Outcome::Ok(json!({
                "stream_token": self.token(&format!("move-import:{move_id}")),
            })),
            "session.move_status" => {
                Outcome::Ok(json!({ "state": "resumed", "session_id": "new-1" }))
            }
            "session.move_discard" => {
                Outcome::Ok(json!({ "state": "discarded", "effect": "none" }))
            }
            "session.move_abort" => Outcome::Ok(json!({})),
            "session.move_release" => Outcome::Ok(json!({ "state": "released" })),
            "session.move_fail" => Outcome::Ok(json!({ "state": "failed" })),
            _ => Outcome::Err(WireError::new("not_implemented", "the fake")),
        }
    }

    fn snapshot(&self, params: &Value) -> Outcome {
        let snapshot: GitSnapshot = serde_json::from_value(params.clone()).expect("a snapshot");
        let checks = snapshot
            .projects
            .iter()
            .map(|p| HomeCheck {
                project_id: p.project_id.clone(),
                generation: p.home_generation,
                state: HomeCheckState::Verified,
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

    async fn serve_data(
        &self,
        handle: Data,
        mut events: tokio::sync::mpsc::UnboundedReceiver<DataEvent>,
    ) {
        let mut imports: Vec<u32> = Vec::new();
        while let Some(event) = events.recv().await {
            match event {
                DataEvent::Opened { stream, purpose } if purpose.starts_with("move-export:") => {
                    let (bytes, reset) = {
                        let files = lock(&self.files);
                        (files.bytes.clone(), files.export_reset)
                    };
                    let handle = handle.clone();
                    tokio::spawn(async move {
                        if let Some(reason) = reset {
                            let _ = handle.reset(stream, reason).await;
                            return;
                        }
                        for chunk in bytes.chunks(4096) {
                            if handle.write(stream, chunk.to_vec()).await.is_err() {
                                return;
                            }
                        }
                        let _ = handle.close(stream).await;
                    });
                }
                DataEvent::Opened { stream, .. } => imports.push(stream),
                DataEvent::Data { stream, bytes } if imports.contains(&stream) => {
                    lock(&self.imported).extend_from_slice(&bytes);
                    let _ = handle.consumed(stream, bytes.len()).await;
                }
                DataEvent::Closed { stream } if imports.contains(&stream) => {
                    let reset = lock(&self.files).import_reset;
                    match reset {
                        Some(reason) => {
                            lock(&self.import_ends).push(reason.to_owned());
                            let _ = handle.reset(stream, reason).await;
                        }
                        None => {
                            lock(&self.import_ends).push("closed".to_owned());
                            let _ = handle.close(stream).await;
                        }
                    }
                }
                DataEvent::Reset { stream, reason } if imports.contains(&stream) => {
                    lock(&self.import_ends).push(format!("reset:{reason}"));
                }
                _ => {}
            }
        }
    }
}

fn receipt(req: &InboundRequest, session: Option<&str>, reject: Option<(&str, &str)>) -> Outcome {
    let id = req.command_id.clone().expect("a command id");
    let receipt = match reject {
        Some((code, message)) => Receipt {
            command_id: id,
            state: ReceiptState::Rejected,
            station_seq: None,
            result: None,
            reject: Some(Reject::new(
                serde_json::from_value::<RejectCode>(json!(code)).expect("a reject code"),
                message,
            )),
            at: 0,
        },
        None => Receipt {
            command_id: id,
            state: ReceiptState::Accepted,
            station_seq: Some(1),
            result: Some(CommandResult {
                session_id: session.map(Into::into),
                input_id: None,
            }),
            reject: None,
            at: 0,
        },
    };
    Outcome::Ok(json!(receipt))
}

fn info(name: &str, inc: &IncarnationId) -> Value {
    json!({
        "name": name,
        "incarnation": inc,
        "head": 0,
        "live_sessions": 0,
        "providers": [{ "instance": "claude", "provider": "claude", "version": null,
                        "auth": "signed_in" }],
        "fleet_git": {
            "clone_root": "/srv/projects",
            "allow_check_cmd": false,
            "allow_local_remotes": true
        }
    })
}

/// The time rules of the tests: the clock of the store is manual, so the
/// waits are short in its time and a pump advances it.
pub fn options() -> MoveOptions {
    MoveOptions {
        retry_unit_ms: 1,
        poll: Duration::from_millis(1),
        polls_max: 5,
        slow_retry: Duration::from_millis(1),
        tick: Duration::from_millis(20),
        starting_limit: Duration::from_secs(60),
        deadlines: Deadlines {
            move_out: Duration::from_millis(600),
            export: Duration::from_millis(600),
            prepare: Duration::from_millis(600),
            pipe_idle: Duration::from_millis(600),
            pipe_total: Duration::from_secs(5),
            upload_end: Duration::from_millis(600),
            move_in: Duration::from_millis(600),
            status: Duration::from_millis(600),
            discard: Duration::from_millis(600),
            abort: Duration::from_millis(600),
            step: Duration::from_millis(600),
            gate_wait: Duration::from_millis(500),
        },
    }
}
