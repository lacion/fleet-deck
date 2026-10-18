//! The helpers of the P6 deck handler tests (R13 P6 task T1b): a world with a
//! bound `desktop` grant and a fake station that answers every request that the
//! deck sends (a handler per test), the call and query envelopes, and the
//! readers of a reply.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_deck::live::{DeckLive, LiveCell};
use fleetdeck_deck::store::coordinator_desktop::{NewOp, OpTool, request_key};
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_deck::store::{GrantClass, NewGrant};
use fleetdeck_link::control::{Control, Inbound};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LiveLink};
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::desktop::{
    DesktopAllowed, DesktopInfo, DesktopKind, DesktopTools, MonitorInfo,
};
use fleetdeck_proto::link::{ControlFrame, Outcome, Scope, StreamMode, StreamStart, WireError};
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::{IncarnationId, StationId};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use crate::common::fleet::{bound_edit, card, coordinate};
use crate::common::{Fixture, T, fixture_with};

/// The window handle that the standard station answers a preview with.
pub const WINDOW: &str = "0123456789abcdef0123456789abcdef";

/// One request of the deck, as the fake station sees it.
pub struct Req {
    pub method: String,
    pub params: Value,
    pub control: Control,
}

/// The answer of the fake station: `None` is no answer at all.
pub type Handler = Arc<dyn Fn(Req) -> BoxFuture<'static, Option<Outcome>> + Send + Sync>;

/// Makes a [`Handler`] of an async closure.
pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Req) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Option<Outcome>> + Send + 'static,
{
    Arc::new(move |req| Box::pin(f(req)))
}

/// The fake station: it logs and answers each request of the deck on its own
/// task, with the current handler.
pub struct Fake {
    pub control: Control,
    log: Arc<Mutex<Vec<(String, Value)>>>,
    handler: Arc<Mutex<Handler>>,
}

impl Fake {
    pub fn start(station: LiveLink) -> Self {
        let control = station.control.clone();
        let mut inbound = station.inbound;
        let log: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let current: Arc<Mutex<Handler>> = Arc::new(Mutex::new(standard()));
        let (seen, answering) = (Arc::clone(&log), Arc::clone(&current));
        let reply_on = control.clone();
        tokio::spawn(async move {
            while let Some(message) = inbound.recv().await {
                let Inbound::Request(req) = message else {
                    continue;
                };
                if req.method == "station.info" {
                    continue;
                }
                seen.lock()
                    .unwrap()
                    .push((req.method.clone(), req.params.clone()));
                let handle = Arc::clone(&answering.lock().unwrap());
                let control = reply_on.clone();
                tokio::spawn(async move {
                    let (id, method, params) = (req.id, req.method, req.params);
                    let out = handle(Req {
                        method,
                        params,
                        control: control.clone(),
                    })
                    .await;
                    if let Some(out) = out {
                        let _ = control.respond(id, out).await;
                    }
                });
            }
        });
        Self {
            control,
            log,
            handler: current,
        }
    }

    /// Replaces the handler of the next requests.
    pub fn set(&self, h: Handler) {
        *self.handler.lock().unwrap() = h;
    }

    /// Every request that the deck sent, in order.
    pub fn log(&self) -> Vec<(String, Value)> {
        self.log.lock().unwrap().clone()
    }

    /// The params of the requests of `method`.
    pub fn params_of(&self, method: &str) -> Vec<Value> {
        self.log()
            .into_iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p)
            .collect()
    }

    pub fn count(&self, method: &str) -> usize {
        self.params_of(method).len()
    }

    /// Waits until the deck has sent `n` requests of `method`.
    pub async fn wait_for(&self, method: &str, n: usize) {
        let deadline = tokio::time::Instant::now() + T;
        while self.count(method) < n {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out: {n} x {method}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// No request of the deck, but the ones of `except`, in `wait`.
    pub async fn quiet(&self, wait: Duration, except: &[&str]) {
        tokio::time::sleep(wait).await;
        let extra: Vec<String> = self
            .log()
            .into_iter()
            .map(|(m, _)| m)
            .filter(|m| !except.contains(&m.as_str()))
            .collect();
        assert!(extra.is_empty(), "unexpected requests {extra:?}");
    }
}

pub fn ok(v: Value) -> Outcome {
    Outcome::Ok(v)
}

pub fn fail(code: &str, message: &str) -> Outcome {
    Outcome::Err(WireError::new(code, message))
}

/// The desktop information of the fake station.
pub fn info() -> DesktopInfo {
    DesktopInfo {
        kind: DesktopKind::Omarchy,
        omarchy_version: Some("3.0".to_owned()),
        monitors: vec![MonitorInfo {
            name: "DP-1".to_owned(),
            description: "wall".to_owned(),
            width: 3440,
            height: 1440,
            scale_pct: 100,
            refresh_hz: 60,
            focused: true,
            dpms_on: Some(true),
        }],
        locked: Some(false),
        dnd: None,
        idle_inhibited: None,
        walls: vec![],
        allowed: DesktopAllowed {
            desktop: true,
            capture: false,
            capture_monitors: Vec::new(),
            input: false,
        },
        tools: DesktopTools {
            wtype: false,
            pointer: false,
            recorder: false,
            grim: false,
            chromium: true,
            notify_send: true,
        },
        utc_offset_minutes: Some(120),
        offset_sampled_at: Some(1),
        features: Vec::new(),
    }
}

/// The answer of a station that allows everything.
pub fn standard_answer(req: &Req) -> Outcome {
    match req.method.as_str() {
        "coordinator.policy_now" => ok(json!({
            "fleet_control": true, "bound": true, "binding_matches": true
        })),
        "desktop.policy_now" => ok(json!({
            "allow_fleet_control": true, "allow_desktop": true,
            "allow_desktop_capture": true, "monitor_allowed": true
        })),
        "desktop.act" => ok(json!({"address": null, "window_id": WINDOW, "info": info()})),
        _ => fail("not_found", "no such method in the fake"),
    }
}

/// The standard station, except that `method` is answered by `f`.
pub fn except(
    method: &'static str,
    f: impl Fn(&Req) -> Option<Outcome> + Send + Sync + 'static,
) -> Handler {
    handler(move |req| {
        let out = if req.method == method {
            f(&req)
        } else {
            Some(standard_answer(&req))
        };
        async move { out }
    })
}

pub fn standard() -> Handler {
    handler(|req| async move { Some(standard_answer(&req)) })
}

/// A world: a bound `desktop` grant of `coord-1` on the fixture station, which
/// is also the target, with fleet control and desktop control on.
pub struct World {
    pub f: Fixture,
    pub fake: Fake,
    pub live: LiveCell,
    pub b: CoordinatorBinding,
    /// A second station of the grant; it links only through
    /// [`World::link_beta`].
    pub beta: StationId,
    beta_identity: Arc<Identity>,
}

pub fn deck_live(desktop_control: bool) -> DeckLive {
    DeckLive {
        name: "deck".into(),
        conflict_mail: false,
        fleet_control: true,
        desktop_control,
        tailscale_cert: false,
        tailscale_bin: None,
    }
}

pub async fn world() -> World {
    world_with(&[GrantClass::Desktop], |_| {}).await
}

pub async fn world_with(classes: &[GrantClass], edit: impl FnOnce(&mut NewGrant)) -> World {
    let mut f = fixture_with(coordinate()).await;
    let station = f.connect().await;
    let mut coord = card("coord-1");
    coord.callsign = Some("swift-fox".to_owned());
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![coord, card("w1")])
        .await
        .unwrap();
    f.store.set_fleet_control(true);
    let live = LiveCell::new(deck_live(true));
    f.ctx.desktop_live.set(live.clone()).ok();
    let beta_identity = Arc::new(Identity::generate().unwrap());
    let beta = f
        .store
        .pin_station(NewStation {
            key: beta_identity.public_key(),
            name: "beta".to_owned(),
            scopes: coordinate(),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id;
    let in_grant = beta.clone();
    let b = bound_edit(&f, &f.station, "coord-1", classes, |g| {
        g.stations.insert(in_grant);
        edit(g);
    })
    .await;
    // Another grant keeps the `coordinate` scope of the station when the
    // grant of the caller ends, so a refusal is the handler's and not the
    // link layer's.
    bound_edit(&f, &f.station, "keeper", &[GrantClass::Read], |_| {}).await;
    World {
        f,
        fake: Fake::start(station),
        live,
        b,
        beta,
        beta_identity,
    }
}

impl World {
    /// Links the second station `beta` to the deck and returns its fake.
    pub async fn link_beta(&mut self) -> Fake {
        let f = &mut self.f;
        f.ctx.peers.refresh(self.beta.clone()).await.unwrap();
        f.deck_node
            .pins
            .pin(self.beta_identity.public_key(), Role::Station);
        let inc = IncarnationId::from("beta-db");
        let grants = Grants::new();
        grants.grant(
            f.deck_key.fingerprint(),
            [Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin],
        );
        let node = NodeConfig::new(
            Arc::clone(&self.beta_identity),
            LocalHello::new(Role::Station, inc.clone()),
            Arc::new(Epochs::in_memory()),
        )
        .with_store(grants);
        node.pins.pin(f.deck_key, Role::Deck);
        let (d, a) = tokio::time::timeout(T, async {
            tokio::join!(
                node::dial(&node, f.listener.local_addr, f.deck_key),
                f.listener.next_link()
            )
        })
        .await
        .expect("link setup");
        let at_deck = fleetdeck_link::start(&f.deck_node, a.expect("listener"));
        tokio::spawn(fleetdeck_deck::link::serve(Arc::clone(&f.ctx), at_deck));
        let mut station = fleetdeck_link::start(&node, d.unwrap());
        loop {
            let message = tokio::time::timeout(T, station.inbound.recv())
                .await
                .expect("a request")
                .expect("the link ended");
            let Inbound::Request(req) = message else {
                continue;
            };
            if req.method != "events.replay" {
                continue;
            }
            let start = StreamStart {
                mode: StreamMode::Snapshot,
                incarnation: inc.clone(),
                from: 0,
            };
            station
                .control
                .respond(req.id, Outcome::Ok(json!(start)))
                .await
                .unwrap();
            for frame in [
                ControlFrame::SnapshotBegin {
                    incarnation: inc.clone(),
                    watermark: 0,
                    session_count: 0,
                },
                ControlFrame::SnapshotEnd {
                    incarnation: inc.clone(),
                    watermark: 0,
                    coordinator: None,
                },
            ] {
                station.control.send(frame).await.unwrap();
            }
            break;
        }
        let id = self.beta.clone();
        f.wait("the snapshot of beta", |f| {
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
        Fake::start(station)
    }

    pub fn station(&self) -> StationId {
        self.f.station.clone()
    }

    pub fn now(&self) -> i64 {
        self.f.store.now_ms()
    }

    /// The node fingerprint of the station: what the hash binds.
    pub fn node(&self) -> String {
        self.f.station_node.identity.fingerprint().to_string()
    }

    pub fn context(&self) -> CoordinatorContext {
        CoordinatorContext {
            grant_id: self.b.grant_id.clone(),
            binding_id: self.b.binding_id.clone(),
            generation: self.b.generation,
            caller_station: self.station(),
            caller_session: "coord-1".into(),
            caller_label: "swift-fox".to_owned(),
        }
    }

    /// The call envelope of `call` with the command ID `id`.
    pub fn call_env(&self, id: &str, call: Value) -> Value {
        json!({
            "command_id": id,
            "target_incarnation": self.f.store.incarnation(),
            "retry_until": self.now() + 10_000,
            "caller": "coord-1",
            "binding": self.b,
            "call": call,
        })
    }

    pub fn query_env(&self, query: Value) -> Value {
        json!({
            "query_id": "q1",
            "target_incarnation": self.f.store.incarnation(),
            "deadline": self.now() + 10_000,
            "caller": "coord-1",
            "binding": self.b,
            "query": query,
        })
    }

    /// The deck binding of the grant moves to generation 2 (a renewal that the
    /// caller did not follow): the binding of `self.b` is stale.
    pub async fn rebind(&self) {
        let gid = self.b.grant_id.clone();
        let n = self
            .f
            .store
            .db()
            .call(move |conn| {
                Ok(conn.execute(
                    "UPDATE tokens SET bound_generation = 2 WHERE grant_id = ?1",
                    [gid],
                )?)
            })
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    /// `coordinator.desktop_call` with the frame command ID `id`.
    pub async fn call_raw(&self, id: Option<&str>, params: Value) -> Outcome {
        let id = id.map(fleetdeck_proto::CommandId::from);
        tokio::time::timeout(
            T,
            self.fake
                .control
                .request("coordinator.desktop_call", id.as_ref(), params),
        )
        .await
        .expect("an answer")
        .expect("the link")
    }

    /// The reply of a call (the answer must be a `DesktopReply`).
    pub async fn call(&self, id: &str, call: Value) -> Value {
        reply(self.call_raw(Some(id), self.call_env(id, call)).await)
    }

    pub async fn query_raw(&self, params: Value) -> Outcome {
        tokio::time::timeout(
            T,
            self.fake
                .control
                .request("coordinator.desktop_query", None, params),
        )
        .await
        .expect("an answer")
        .expect("the link")
    }

    /// The reply of a query.
    pub async fn query(&self, query: Value) -> Value {
        reply(self.query_raw(self.query_env(query)).await)
    }

    /// `desktop.authority_check` from the fake station.
    pub async fn check_raw(&self, params: Value) -> Outcome {
        tokio::time::timeout(
            Duration::from_secs(15),
            self.fake
                .control
                .request("desktop.authority_check", None, params),
        )
        .await
        .expect("an answer")
        .expect("the link")
    }

    /// `desktop.authority_check` from the fake station; the answer value.
    pub async fn check(&self, params: Value) -> Value {
        match self.check_raw(params).await {
            Outcome::Ok(v) => v,
            Outcome::Err(e) => panic!("a wire error: {e}"),
        }
    }

    /// A new operation of the `desktop` class for the check tests.
    pub fn new_op(&self, id: &str, tool: OpTool, hash: &str, target: &StationId) -> NewOp {
        NewOp {
            operation_id: id.to_owned(),
            grant_id: self.b.grant_id.clone(),
            request_key: request_key(id),
            payload_hash: format!("payload-{id}"),
            tool,
            class: if tool == OpTool::Screenshot {
                fleetdeck_deck::store::coordinator_desktop::OpClass::Capture
            } else {
                fleetdeck_deck::store::coordinator_desktop::OpClass::Desktop
            },
            ctx: self.context(),
            target_station: target.clone(),
            action_hash: Some(hash.to_owned()),
            resource: json!({}),
        }
    }

    /// Admits an operation and makes it `attempted`.
    pub async fn attempted(&self, op: NewOp) {
        let id = op.operation_id.clone();
        let got = self.f.store.desktop_admit(op, self.now()).await.unwrap();
        assert!(
            matches!(
                got,
                fleetdeck_deck::store::coordinator_desktop::Admission::Accepted(_)
            ),
            "{got:?}"
        );
        assert!(
            self.f
                .store
                .desktop_op_attempted(&id, self.now())
                .await
                .unwrap()
        );
    }

    /// Every audit row of an action `desktop.*` as one text.
    pub async fn audit_text(&self) -> String {
        self.audit_rows(None)
            .await
            .iter()
            .map(|(a, t, d)| format!("{a}|{t}|{d}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The rows `(action, target, detail)` of the coordinator actor, oldest
    /// first, optionally of one action.
    pub async fn audit_rows(&self, action: Option<&str>) -> Vec<(String, String, String)> {
        let mut rows: Vec<_> = self
            .f
            .store
            .audit_log(500)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.actor == "coordinator")
            .filter(|e| action.is_none_or(|a| e.action == a))
            .map(|e| {
                (
                    e.action,
                    e.target.unwrap_or_default(),
                    e.detail.unwrap_or_default(),
                )
            })
            .collect();
        rows.reverse();
        rows
    }

    /// The station gets a second live link that negotiated no feature: from
    /// now on `link_feature` is `Without` for every feature.
    pub fn strip_features(&self) {
        self.f
            .store
            .set_link_features(&self.f.station, 97, Vec::new());
    }
}

/// The `DesktopReply` of an answer.
pub fn reply(out: Outcome) -> Value {
    match out {
        Outcome::Ok(v) => v,
        Outcome::Err(e) => panic!("a wire error: {e}"),
    }
}

/// `status` and `error` of a reply: `("err", code)`, or `("ok", "")`.
pub fn verdict(reply: &Value) -> (String, String) {
    let status = reply["status"].as_str().unwrap_or_default().to_owned();
    let code = reply["error"].as_str().unwrap_or_default().to_owned();
    (status, code)
}

/// The error code of a reply, which must be an error.
pub fn code(reply: &Value) -> String {
    let (status, code) = verdict(reply);
    assert_eq!(status, "err", "{reply}");
    code
}

pub fn notify(request_id: &str, title: &str) -> Value {
    json!({"tool": "notify", "arguments": {
        "request_id": request_id, "title": title, "body": "body text"}})
}

pub fn preview(request_id: &str, url: &str) -> Value {
    json!({"tool": "preview_open", "arguments": {"request_id": request_id, "url": url}})
}

pub fn close(request_id: &str, window_id: &str) -> Value {
    json!({"tool": "window_close", "arguments": {
        "request_id": request_id, "window_id": window_id}})
}

pub fn status_query(request_id: &str) -> Value {
    json!({"tool": "desktop_action_status", "arguments": {"request_id": request_id}})
}

pub fn info_query(station: Option<&str>) -> Value {
    match station {
        Some(s) => json!({"tool": "desktop_info", "arguments": {"station": s}}),
        None => json!({"tool": "desktop_info", "arguments": {}}),
    }
}

/// A `CoordinatorDesktopInfo` of the station, as `desktop.coordinator_read`
/// answers it (`station` is empty: the deck fills it).
pub fn read_info(monitors: usize) -> Value {
    let list: Vec<Value> = (0..monitors)
        .map(|i| {
            json!({"name": format!("DP-{i}"), "width": 1920, "height": 1080, "scale_pct": 100,
                "focused": i == 0, "dpms_on": true, "capture_allowed": i % 2 == 0})
        })
        .collect();
    json!({"station": "", "kind": "omarchy", "monitors": list, "locked": false,
        "capabilities": {"notify": true, "preview": true, "screenshot": true},
        "walls": [{"display_id": "wall-1", "monitor": "DP-0"}]})
}

/// The grant classes of a world that holds both.
pub const BOTH: [GrantClass; 2] = [GrantClass::Desktop, GrantClass::Capture];

/// A set of stations with the fixture station.
pub fn stations(more: &[&str]) -> BTreeSet<StationId> {
    more.iter().map(|s| StationId::from(*s)).collect()
}
