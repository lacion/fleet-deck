//! MCP P1 cancellation and permit tests (`docs/v2/design/mcp-p1.md` §6, tests
//! 10 and 22): a closed connection stops the wait and frees the binding
//! permit, a durable call stays durable, and a slow reader keeps the permit
//! until its reply is written.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::{CommandId, IncarnationId};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::{Semaphore, watch};

use super::bindings::{BindingState, ToolClass};
use super::forward::{LinkForward, Transport};
use super::ingress::Forward;
use super::mcp_p1_rig_tests::{Fake, MAIL, Rig, http_request, legacy, parse_http};
use crate::store::coordinator_outbox::OutboxState;
use crate::store::{Store, StoreConfig};

/// The deck of the durable call: `request` waits while the gate is closed.
pub(super) struct GatedDeck {
    pub sent: Mutex<Vec<(Option<CommandId>, Value)>>,
    pub gate: AtomicBool,
    pub entered: Semaphore,
    pub open: watch::Sender<bool>,
    pub dropped: AtomicUsize,
}

impl GatedDeck {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            sent: Mutex::default(),
            gate: AtomicBool::new(true),
            entered: Semaphore::new(0),
            open: watch::channel(false).0,
            dropped: AtomicUsize::new(0),
        })
    }
}

struct Dropped<'a>(&'a AtomicUsize, bool);

impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        if self.1 {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[async_trait]
impl Transport for GatedDeck {
    async fn request(
        &self,
        method: &str,
        command_id: Option<&CommandId>,
        params: Value,
    ) -> Result<Outcome, String> {
        assert_eq!(method, "coordinator.call");
        self.sent
            .lock()
            .unwrap()
            .push((command_id.cloned(), params));
        if self.gate.load(Ordering::SeqCst) {
            let mut guard = Dropped(&self.dropped, true);
            let mut rx = self.open.subscribe();
            self.entered.add_permits(1);
            rx.wait_for(|open| *open).await.expect("open");
            guard.1 = false;
        }
        let id = command_id.cloned().unwrap();
        Ok(Outcome::Ok(json!({
            "command_id": id, "state": "accepted", "station_seq": null,
            "result": null, "reject": null, "at": 1
        })))
    }
}

/// The station on a real outbox (`LinkForward`) and a gated deck.
pub(super) struct Durable {
    pub rig: Rig,
    pub store: Store,
    pub deck: Arc<GatedDeck>,
    pub forward: Arc<LinkForward>,
}

impl Durable {
    pub(super) async fn start() -> Self {
        let det = Det::at(1_000_000);
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        store.recover(Vec::new()).await.unwrap();
        let deck = GatedDeck::new();
        let link: Arc<Mutex<Option<Arc<LinkForward>>>> = Arc::default();
        let rig = Rig::start_with(
            {
                let (store, deck, link) = (store.clone(), deck.clone(), link.clone());
                move |bindings| {
                    let forward = Arc::new(LinkForward::with_outbox(store, bindings.clone()));
                    forward.connect_transport(deck, IncarnationId::from("deck-inc".to_owned()));
                    *link.lock().unwrap() = Some(forward.clone());
                    forward as Arc<dyn Forward>
                }
            },
            Fake::new(),
            BindingState::Active,
            MAIL,
        )
        .await;
        let forward = link.lock().unwrap().clone().unwrap();
        Self {
            rig,
            store,
            deck,
            forward,
        }
    }

    /// The `send_mail` call of the tests.
    pub(super) fn mail() -> Value {
        legacy(
            1,
            "tools/call",
            json!({"name": "send_mail", "arguments":
                {"to": {"station": "st", "session_id": "s1"}, "text": "hi"}}),
        )
    }

    /// Waits for the deck request of a held call.
    pub(super) async fn entered(&self) {
        durable_entered(&self.deck).await;
    }
}

async fn durable_entered(deck: &GatedDeck) {
    tokio::time::timeout(Duration::from_secs(5), deck.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
}

pub(super) async fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{what}");
}

/// A request that is sent and not read: the connection of the returned
/// stream stays open.
pub(super) async fn send_only(addr: SocketAddr, token: &str, body: &Value) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = http_request(addr, Some(token), &[], &body.to_string());
    stream.write_all(request.as_bytes()).await.unwrap();
    stream
}

pub(super) fn wait_call(n: usize) -> Value {
    legacy(
        n as i64,
        "tools/call",
        json!({"name": "wait_task", "arguments": {"task_id": format!("t-{n}")}}),
    )
}

#[tokio::test]
async fn a_cancelled_wait_frees_the_binding_permit_and_a_durable_call_stays_pending() {
    let Durable {
        rig: r,
        store,
        deck,
        forward,
    } = Durable::start().await;

    // The durable call: the client closes its connection while the deck
    // request is held.
    let mail = Durable::mail();
    let client = send_only(r.addr(), &r.token, &mail).await;
    durable_entered(&deck).await;
    drop(client);
    until("the deck request was not dropped", || {
        deck.dropped.load(Ordering::SeqCst) == 1
    })
    .await;
    let id = deck.sent.lock().unwrap()[0].0.clone().unwrap();
    let row = store.coordinator_row(id.clone()).await.unwrap().unwrap();
    assert_eq!(
        row.state,
        OutboxState::Pending,
        "a drop never deletes a row"
    );
    assert_eq!(row.sends, 1);
    assert_eq!(store.coordinator_pending().await.unwrap().len(), 1);

    // The next flush sends the same row again; the drop made no new command.
    deck.gate.store(false, Ordering::SeqCst);
    forward.flush().await.unwrap();
    let sent = deck.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1], "the same command id and envelope");
    let row = store.coordinator_row(id).await.unwrap().unwrap();
    assert_eq!(row.state, OutboxState::Done);

    // Eight waits that never answer: the client closes all eight
    // connections, the permits are free, the next request is not 429.
    let w = Rig::start(BindingState::Active, MAIL).await;
    w.fake.gate.store(true, Ordering::SeqCst);
    let mut clients = Vec::new();
    for n in 0..8 {
        clients.push(send_only(w.addr(), &w.token, &wait_call(n)).await);
    }
    w.fake.entered(8).await;
    // All eight permits are held: a ninth request is refused.
    let ninth = w.http(Some(&w.token), &[], &wait_call(9)).await;
    assert_eq!(ninth.status, 429, "{}", ninth.body);
    drop(clients);
    w.fake.dropped(8).await;
    w.fake.gate.store(false, Ordering::SeqCst);
    let mut status = 0;
    for _ in 0..200 {
        status = w.http(Some(&w.token), &[], &wait_call(10)).await.status;
        if status != 429 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        status, 200,
        "the permits of the closed connections are free"
    );
}

/// The text that `set_focus` answers: about 16 MiB, so the reply cannot fit
/// the socket buffers.
const BIG: usize = 16 * 1024 * 1024;

fn focus_call() -> Value {
    legacy(
        1,
        "tools/call",
        json!({"name": "set_focus", "arguments": {"text": "x"}}),
    )
}

async fn slow_reader_rig() -> Rig {
    let r = Rig::start(BindingState::Active, &[ToolClass::Read, ToolClass::Own]).await;
    r.fleet.focus_bytes.store(BIG, Ordering::SeqCst);
    r
}

#[tokio::test]
async fn a_slow_reader_keeps_the_binding_permit_until_the_reply_is_written() {
    on_http().await;
    on_the_socket().await;
}

async fn on_http() {
    let r = slow_reader_rig().await;
    // Eight clients on eight connections send a request and do not read.
    let mut clients = Vec::new();
    for _ in 0..8 {
        clients.push(send_only(r.addr(), &r.token, &focus_call()).await);
    }
    until("the eight calls did not arrive", || {
        r.fleet.calls.lock().unwrap().len() == 8
    })
    .await;
    // Give the replies time to reach hyper's writer (or the kernel buffer).
    tokio::time::sleep(Duration::from_millis(500)).await;
    // A ninth request on a new connection is limited.
    let ninth = r
        .http(Some(&r.token), &[], &legacy(2, "tools/list", json!({})))
        .await;
    assert_eq!(ninth.status, 429, "{}", ninth.body);
    // One client reads its reply to the end.
    let mut first = clients.remove(0);
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(120), first.read_to_end(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let reply = parse_http(&buf[..buf.len().min(4096)]);
    assert_eq!(reply.status, 200);
    assert!(buf.len() > BIG, "the whole reply was read");
    // The next request succeeds (the permit was released after the flush).
    let mut status = 0;
    for _ in 0..500 {
        status = r
            .http(Some(&r.token), &[], &legacy(3, "tools/list", json!({})))
            .await
            .status;
        if status != 429 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(status, 200);
    drop(clients);
}

async fn on_the_socket() {
    let r = slow_reader_rig().await;
    let message = focus_call().to_string();
    let mut clients = Vec::new();
    for _ in 0..8 {
        let mut stream = UnixStream::connect(&r.path).await.unwrap();
        stream
            .write_all(format!("AUTH {}\n{message}\n", r.token).as_bytes())
            .await
            .unwrap();
        clients.push(stream);
    }
    until("the eight calls did not arrive", || {
        r.fleet.calls.lock().unwrap().len() == 8
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    // A ninth request is limited: the correlated line with code -32000.
    let ninth = r
        .sock(&r.token, &legacy(2, "tools/list", json!({})).to_string())
        .await;
    let ninth: Value = serde_json::from_str(&ninth).unwrap();
    assert_eq!(ninth["id"], 2);
    assert_eq!(ninth["error"]["code"], -32000, "{ninth}");
    // One client reads its line to the end.
    let mut first = clients.remove(0);
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(120), first.read_to_end(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert!(buf.len() > BIG, "the whole line was read");
    let mut limited = true;
    for _ in 0..500 {
        let line = r
            .sock(&r.token, &legacy(3, "tools/list", json!({})).to_string())
            .await;
        let v: Value = serde_json::from_str(&line).unwrap();
        limited = v["error"]["code"] == -32000;
        if !limited {
            assert!(v["result"]["tools"].is_array(), "{v}");
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !limited,
        "the permit was released after the line was written"
    );
    drop(clients);
}
