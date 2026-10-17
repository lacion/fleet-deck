//! The deck side of a station link (M2 P3-6b; PLAN §8.3, §8.5; G4).
//!
//! For each link the deck runs one sync loop:
//!
//! 1. It looks up the station by its pinned key. A key that is not in the
//!    registry (revoked in a race) closes the link.
//! 2. It asks for `events.replay` with the stored incarnation and cursor of
//!    the read model. The station answers with the events after the cursor,
//!    or with a snapshot batch when the incarnation differs (or is unknown)
//!    or the cursor is ahead of its log; then live events follow.
//! 3. It folds each batch of events into `deck.db` in one transaction and
//!    sends `ack` with the highest received `station_seq` **after** the
//!    commit. A snapshot batch replaces the station's read model at its
//!    watermark; then the deck acks the watermark.
//! 4. Events of another incarnation (a station that restarted with a new
//!    database while the link ran) start a `session.snapshot` resync; the
//!    deck drops events until the batch ends.
//!
//! A station is reachable while at least one of its links is up. When a
//! link comes up, the router resends the station's pending commands
//! ([`router`]) and the router tick delivers its outbox items. Requests of
//! a station are answered by `requests`: `coordinator.call` through the
//! coordinator receipts (G7); the others with `not_implemented` (DL-050,
//! DL-053).

mod coordinator_reads;
pub mod desktop_authority;
mod outbox;
mod requests;
pub mod router;
pub mod runtime;
pub mod streams;

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_link::control::{Control, Inbound};
use fleetdeck_link::methods::code;
use fleetdeck_link::node::NodeConfig;
use fleetdeck_link::revoke::REVOKED;
use fleetdeck_link::{Closing, LiveLink};
use fleetdeck_proto::link::{ControlFrame, MAX_SNAPSHOT_SESSIONS, Outcome, StationInfo};
use fleetdeck_proto::session::SessionSummary;
use fleetdeck_proto::{EventEnvelope, IncarnationId, StationId};
use serde_json::{Value, json};
use tokio::sync::mpsc;

pub use router::CommandKind;
pub use runtime::{DeckNode, NodeError, NodeOptions};

use crate::peers::DeckPeers;
use crate::store::{DeckError, DeckStore};

/// The error code of a method that this build does not serve.
pub const NOT_IMPLEMENTED: &str = "not_implemented";

/// The largest batch of events folded in one transaction.
pub const BATCH: usize = 256;

/// One open link: its token, control channel and data channel.
type LiveEntry = (u64, Control, Option<Arc<streams::DataHub>>);

/// What every link of the deck shares.
#[derive(Debug)]
pub struct DeckLink {
    pub store: DeckStore,
    pub peers: Arc<DeckPeers>,
    pub node: NodeConfig,
    /// The open links of each station.
    live: Mutex<HashMap<StationId, Vec<LiveEntry>>>,
    next_token: AtomicU64,
    inflight: router::Inflight,
}

impl DeckLink {
    #[must_use]
    pub fn new(store: DeckStore, peers: Arc<DeckPeers>, node: NodeConfig) -> Self {
        Self {
            store,
            peers,
            node,
            live: Mutex::default(),
            next_token: AtomicU64::new(0),
            inflight: router::Inflight::default(),
        }
    }

    fn up(
        self: &Arc<Self>,
        id: &StationId,
        control: &Control,
        data: Option<Arc<streams::DataHub>>,
        remote: Option<SocketAddr>,
    ) -> LiveGuard {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        live.entry(id.clone())
            .or_default()
            .push((token, control.clone(), data));
        // The features of this link, for the fleet-control checks (ADR-008
        // section 3).
        self.store
            .set_link_features(id, token, control.params().negotiated.features.clone());
        if let Some(addr) = remote {
            self.store.set_link_peer(id, token, addr.ip());
        }
        self.store.set_reachable(id, true);
        LiveGuard {
            ctx: Arc::clone(self),
            id: id.clone(),
            token,
        }
    }

    /// The number of open station links.
    #[must_use]
    pub fn open_links(&self) -> usize {
        let live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        live.values().map(Vec::len).sum()
    }

    /// Closes every station link (deck shutdown).
    pub fn close_all(&self, reason: &str) {
        let live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        for (_, control, _) in live.values().flatten() {
            control.close(reason, "the deck stops");
        }
    }
}

/// Marks the station unreachable when its last link ends.
struct LiveGuard {
    ctx: Arc<DeckLink>,
    id: StationId,
    token: u64,
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        let mut live = self.ctx.live.lock().unwrap_or_else(PoisonError::into_inner);
        let links = live.entry(self.id.clone()).or_default();
        links.retain(|(t, _, _)| *t != self.token);
        self.ctx.store.clear_link_features(&self.id, self.token);
        if links.is_empty() {
            live.remove(&self.id);
            self.ctx.store.set_reachable(&self.id, false);
        }
    }
}

/// Serves `link` until it ends; returns why it ended.
pub async fn serve(ctx: Arc<DeckLink>, link: LiveLink) -> Closing {
    let LiveLink {
        peer,
        control,
        mut inbound,
        data,
        remote,
        ..
    } = link;
    let data = data.map(streams::DataHub::start);
    let station = match ctx.store.station_by_key(peer).await {
        Ok(Some(st)) => st,
        Ok(None) => {
            control.close(REVOKED, "the station is not paired");
            return control.closed().await;
        }
        Err(e) => {
            control.close(code::PROTOCOL_ERROR, &format!("deck store: {e}"));
            return control.closed().await;
        }
    };
    let id = station.station_id.clone();
    let _live = ctx.up(&id, &control, data, remote);
    if let Err(e) = ctx.store.touch_station(id.clone()).await {
        tracing::warn!(error = %e, "station last_seen");
    }
    tracing::info!(station = %station.name, "station link up");
    fetch_info(&ctx, &control, &id, &station.name);
    let mut sync = Sync {
        ctx: Arc::clone(&ctx),
        control: control.clone(),
        station: id,
        snapshot: None,
        resyncing: false,
    };
    match ctx.store.replay_params(sync.station.clone()).await {
        Ok(params) => sync.start("events.replay", json!(params)),
        Err(e) => control.close(code::PROTOCOL_ERROR, &format!("deck store: {e}")),
    }
    // Resend the pending commands of this station on the new link (G3).
    let router = Arc::clone(&ctx);
    let pending_of = sync.station.clone();
    tokio::spawn(async move { router.dispatch_pending(pending_of).await });
    let mut held = VecDeque::new();
    while let Some(message) = next(&mut held, &mut inbound).await {
        // A frame that was queued before the link ended is not handled (NV-12).
        if control.ended().is_some() {
            break;
        }
        match message {
            Inbound::Request(req) => {
                let (ctx, control, station) =
                    (Arc::clone(&ctx), control.clone(), sync.station.clone());
                tokio::spawn(async move {
                    requests::answer(&ctx, &control, station, req).await;
                });
            }
            Inbound::Frame(frame) => match *frame {
                ControlFrame::Event {
                    incarnation,
                    envelope,
                } => {
                    let batch = collect(incarnation, envelope, &mut held, &mut inbound);
                    sync.events(batch.0, batch.1).await;
                }
                other => sync.frame(other).await,
            },
        }
        if control.ended().is_some() {
            break;
        }
    }
    control.closed().await
}

async fn next(
    held: &mut VecDeque<Inbound>,
    inbound: &mut mpsc::Receiver<Inbound>,
) -> Option<Inbound> {
    match held.pop_front() {
        Some(m) => Some(m),
        None => inbound.recv().await,
    }
}

/// The first event plus every queued event of the same incarnation (up to
/// [`BATCH`]); another message is held for the next turn.
fn collect(
    incarnation: IncarnationId,
    first: EventEnvelope,
    held: &mut VecDeque<Inbound>,
    inbound: &mut mpsc::Receiver<Inbound>,
) -> (IncarnationId, Vec<EventEnvelope>) {
    let mut batch = vec![first];
    while batch.len() < BATCH && held.is_empty() {
        let Ok(message) = inbound.try_recv() else {
            break;
        };
        match message {
            Inbound::Frame(frame) => match *frame {
                ControlFrame::Event {
                    incarnation: inc,
                    envelope,
                } if inc == incarnation => batch.push(envelope),
                other => held.push_back(Inbound::Frame(Box::new(other))),
            },
            request @ Inbound::Request(_) => held.push_back(request),
        }
    }
    (incarnation, batch)
}

/// A snapshot batch that is not complete yet.
struct OpenSnapshot {
    incarnation: IncarnationId,
    watermark: u64,
    count: u64,
    sessions: Vec<SessionSummary>,
}

struct Sync {
    ctx: Arc<DeckLink>,
    control: Control,
    station: StationId,
    snapshot: Option<OpenSnapshot>,
    /// A `session.snapshot` resync is on its way: events are dropped.
    resyncing: bool,
}

impl Sync {
    /// Requests a stream start; a refusal closes the link.
    fn start(&self, method: &'static str, params: Value) {
        let control = self.control.clone();
        tokio::spawn(async move {
            match control.request(method, None, params).await {
                Ok(Outcome::Ok(_)) => {}
                Ok(Outcome::Err(e)) => {
                    control.close(code::PROTOCOL_ERROR, &format!("{method}: {e}"));
                }
                Err(e) => tracing::debug!(error = %e, method, "stream request"),
            }
        });
    }

    async fn events(&mut self, incarnation: IncarnationId, batch: Vec<EventEnvelope>) {
        if self.snapshot.take().is_some() {
            // The stream restarted inside a batch; the new stream decides.
            tracing::debug!("a snapshot batch was cut off");
        }
        if self.resyncing {
            return;
        }
        let top = batch.iter().map(|e| e.station_seq).max().unwrap_or(0);
        let result = self
            .ctx
            .store
            .apply_events(self.station.clone(), incarnation.clone(), batch)
            .await;
        match result {
            Ok(_) => {
                // Conflict mails that wait for a generated sentence (DL-167).
                crate::http::assist_routes::spawn_conflict_sentences(&self.ctx);
                self.ack(incarnation, top).await;
            }
            Err(DeckError::IncarnationMismatch { expected, got, .. }) => {
                tracing::info!(?expected, %got, "station incarnation changed; resync");
                self.resyncing = true;
                self.start("session.snapshot", json!({}));
            }
            Err(e) => self.fail(&e),
        }
    }

    async fn frame(&mut self, frame: ControlFrame) {
        match frame {
            ControlFrame::SnapshotBegin {
                incarnation,
                watermark,
                session_count,
            } => {
                if session_count > MAX_SNAPSHOT_SESSIONS {
                    self.snapshot = None;
                    return self.protocol(&format!(
                        "a snapshot of {session_count} sessions is above the limit of {MAX_SNAPSHOT_SESSIONS}"
                    ));
                }
                // A restarted stream replaces an unfinished batch.
                self.snapshot = Some(OpenSnapshot {
                    incarnation,
                    watermark,
                    count: session_count,
                    sessions: Vec::new(),
                });
            }
            ControlFrame::SnapshotSession {
                incarnation,
                snapshot,
            } => match &mut self.snapshot {
                Some(open)
                    if open.incarnation == incarnation
                        && (open.sessions.len() as u64) < open.count =>
                {
                    open.sessions.push(snapshot.summary);
                }
                Some(open) if open.incarnation == incarnation => {
                    self.snapshot = None;
                    self.protocol("a snapshot session beyond the announced count");
                }
                _ => self.protocol("a snapshot session outside its batch"),
            },
            ControlFrame::SnapshotEnd {
                incarnation,
                watermark,
                coordinator,
            } => self.end_snapshot(incarnation, watermark, coordinator).await,
            other => self.protocol(&format!(
                "a deck does not take `{}` frames",
                other.type_name()
            )),
        }
    }

    async fn end_snapshot(
        &mut self,
        incarnation: IncarnationId,
        watermark: u64,
        coordinator: Option<Vec<fleetdeck_proto::CoordinatorBindingState>>,
    ) {
        let Some(open) = self.snapshot.take() else {
            return self.protocol("a snapshot end without its begin");
        };
        let complete = open.incarnation == incarnation
            && open.watermark == watermark
            && open.count == open.sessions.len() as u64;
        if !complete {
            return self.protocol("a snapshot batch does not match its begin");
        }
        let result = self
            .ctx
            .store
            .apply_snapshot_with(
                self.station.clone(),
                incarnation.clone(),
                watermark,
                open.sessions,
                // Only a link with `coordinator.v1` carries a projection
                // (ADR-006 §6.1).
                coordinator.filter(|_| {
                    self.control
                        .params()
                        .negotiated
                        .has(fleetdeck_proto::version::feature::COORDINATOR)
                }),
                Some(Arc::new({
                    let control = self.control.clone();
                    move || control.is_current()
                })),
            )
            .await;
        match result {
            Ok(()) => {
                self.resyncing = false;
                self.ack(incarnation, watermark).await;
            }
            Err(e) => {
                if matches!(e, DeckError::StaleSnapshot | DeckError::Superseded) {
                    tracing::warn!(
                        error = %e,
                        station = %self.station,
                        "a snapshot was not applied"
                    );
                }
                self.fail(&e);
            }
        }
    }

    async fn ack(&self, incarnation: IncarnationId, station_seq: u64) {
        let frame = ControlFrame::Ack {
            incarnation,
            station_seq,
        };
        if let Err(e) = self.control.send(frame).await {
            tracing::debug!(error = %e, "ack");
        }
    }

    fn protocol(&self, detail: &str) {
        self.control.close(code::PROTOCOL_ERROR, detail);
    }

    /// A store error: the link closes; the next link resumes at the
    /// committed cursor.
    fn fail(&self, e: &DeckError) {
        tracing::warn!(error = %e, station = %self.station, "read model");
        match e {
            DeckError::UnknownStation(_) => {
                self.control.close(REVOKED, "the station is not paired")
            }
            _ => self
                .control
                .close(code::PROTOCOL_ERROR, &format!("deck store: {e}")),
        }
    }
}

/// How often the deck asks a live link for `station.info` again (DL-193).
const INFO_REFRESH: Duration = Duration::from_secs(60);

/// Asks the station for `station.info` when the link comes up and then every
/// `INFO_REFRESH` while it is live, and keeps the answer for the board
/// (version, capacity, providers, resources). The board name follows the
/// station's own `name` setting: the deck adopts the name of the first
/// answer only.
fn fetch_info(ctx: &Arc<DeckLink>, control: &Control, id: &StationId, name: &str) {
    let (ctx, control, id, stored) = (
        Arc::clone(ctx),
        control.clone(),
        id.clone(),
        name.to_owned(),
    );
    tokio::spawn(async move {
        let ask = || ask_info(&control);
        info_loop(
            &ctx.store,
            &id,
            &stored,
            INFO_REFRESH,
            ask,
            control.closed(),
        )
        .await;
    });
}

/// The loop of [`fetch_info`]. It ends when `closed` resolves or `ask`
/// fails. An answer that is `Ok(None)` is skipped and tried again later.
async fn info_loop<F, Fut>(
    store: &DeckStore,
    id: &StationId,
    stored: &str,
    every: Duration,
    mut ask: F,
    closed: impl Future,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<StationInfo>, ()>>,
{
    tokio::pin!(closed);
    match ask().await {
        Ok(Some(info)) => {
            let reported = info.name.clone();
            store.set_station_info(id, info);
            adopt_name(store, id.clone(), stored, &reported).await;
        }
        Ok(None) => {}
        Err(()) => return,
    }
    loop {
        tokio::select! {
            _ = &mut closed => return,
            () = tokio::time::sleep(every) => {}
        }
        match ask().await {
            Ok(Some(info)) => store.set_station_info(id, info),
            Ok(None) => {}
            Err(()) => return,
        }
    }
}

/// One `station.info` request. `Err` when the link is gone; `Ok(None)` when
/// the answer is an error or does not parse.
async fn ask_info(control: &Control) -> Result<Option<StationInfo>, ()> {
    match control.request("station.info", None, json!({})).await {
        Ok(Outcome::Ok(v)) => match serde_json::from_value::<StationInfo>(v) {
            Ok(info) => Ok(Some(info)),
            Err(e) => {
                tracing::debug!(error = %e, "station.info does not parse");
                Ok(None)
            }
        },
        Ok(other) => {
            tracing::debug!(outcome = ?other, "station.info refused");
            Ok(None)
        }
        Err(e) => {
            tracing::debug!(error = %e, "station.info on a link that ended");
            Err(())
        }
    }
}

/// Gives the station the name that it reports, when the name is new and
/// free. A name that another station has leaves the old name.
async fn adopt_name(store: &DeckStore, id: StationId, stored: &str, reported: &str) {
    let reported = reported.trim();
    if reported.is_empty() || reported == stored {
        return;
    }
    match store.rename_station(id.clone(), reported.to_owned()).await {
        Ok(()) => tracing::info!(station = %id, name = %reported, "station named"),
        Err(DeckError::NameTaken(_)) => tracing::warn!(
            station = %id,
            name = %reported,
            "another station has this name; the board keeps the old name"
        ),
        Err(e) => tracing::warn!(error = %e, station = %id, "station keeps its name"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use fleetdeck_core::identity::Identity;
    use fleetdeck_proto::link::Scope;
    use fleetdeck_testkit::Det;

    use super::*;
    use crate::store::stations::NewStation;

    async fn paired(store: &DeckStore, name: &str) -> StationId {
        let rec = store
            .pin_station(NewStation {
                key: Identity::generate().unwrap().public_key(),
                name: name.to_owned(),
                scopes: BTreeSet::from([Scope::Read]),
                dial_url: None,
            })
            .await
            .unwrap();
        rec.station_id
    }

    async fn name_of(store: &DeckStore, id: &StationId) -> String {
        store.station(id.clone()).await.unwrap().unwrap().name
    }

    #[tokio::test]
    async fn a_station_that_reports_a_new_name_is_renamed() {
        let det = Det::new();
        let store = DeckStore::open_in_memory(det.clock(), det.ids())
            .await
            .unwrap();
        let id = paired(&store, "old").await;
        adopt_name(&store, id.clone(), "old", " new ").await;
        assert_eq!(name_of(&store, &id).await, "new");
        adopt_name(&store, id.clone(), "new", "").await;
        assert_eq!(name_of(&store, &id).await, "new");
    }

    #[tokio::test]
    async fn a_reported_name_that_another_station_has_leaves_the_old_name() {
        let det = Det::new();
        let store = DeckStore::open_in_memory(det.clock(), det.ids())
            .await
            .unwrap();
        paired(&store, "taken").await;
        let id = paired(&store, "old").await;
        adopt_name(&store, id.clone(), "old", "taken").await;
        assert_eq!(name_of(&store, &id).await, "old");
    }

    /// The interval of the refresh tests (the real one is `INFO_REFRESH`).
    const TICK: Duration = Duration::from_millis(20);

    fn info(name: &str, head: u64) -> StationInfo {
        StationInfo {
            name: name.to_owned(),
            incarnation: "inc".to_owned().into(),
            head,
            live_sessions: 0,
            version: None,
            max_sessions: None,
            allow_shell: None,
            allow_setup_cmd: None,
            providers: Vec::new(),
            assist_tokens_today: None,
            assist_cap: None,
            resources: None,
            desktop: None,
        }
    }

    async fn store_with(name: &str) -> (DeckStore, StationId) {
        let det = Det::new();
        let store = DeckStore::open_in_memory(det.clock(), det.ids())
            .await
            .unwrap();
        let id = paired(&store, name).await;
        (store, id)
    }

    fn head_of(store: &DeckStore, id: &StationId) -> Option<u64> {
        store.station_info(id).map(|(info, _)| info.head)
    }

    async fn until_head(store: &DeckStore, id: &StationId, head: u64) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while head_of(store, id) < Some(head) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out: head {head}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    type Answer = Result<Option<StationInfo>, ()>;

    /// Answers `head` 1, 2, 3, ... for each request; `Ok(None)` for a call
    /// in `skip` (an answer that does not parse); `Err` after `last` calls.
    fn asker(
        calls: &Arc<AtomicU64>,
        skip: &'static [u64],
        last: u64,
    ) -> impl FnMut() -> std::future::Ready<Answer> + use<> {
        let calls = Arc::clone(calls);
        move || {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            std::future::ready(if n > last {
                Err(())
            } else if skip.contains(&n) {
                Ok(None)
            } else {
                Ok(Some(info("same", n)))
            })
        }
    }

    #[tokio::test]
    async fn the_refresh_loop_stores_a_new_info_every_interval() {
        let (store, id) = store_with("same").await;
        let calls = Arc::new(AtomicU64::new(0));
        let ask = asker(&calls, &[2], u64::MAX);
        let (close, closed) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn({
            let (store, id) = (store.clone(), id.clone());
            async move { info_loop(&store, &id, "same", TICK, ask, closed).await }
        });
        // The second answer is skipped: the loop goes on to the third.
        until_head(&store, &id, 3).await;
        // The loop ends when the link closes.
        drop(close);
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn the_refresh_loop_ends_when_a_request_fails() {
        let (store, id) = store_with("same").await;
        let calls = Arc::new(AtomicU64::new(0));
        let ask = asker(&calls, &[], 2);
        let (_keep, closed) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn({
            let (store, id) = (store.clone(), id.clone());
            async move { info_loop(&store, &id, "same", TICK, ask, closed).await }
        });
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(head_of(&store, &id), Some(2));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn the_refresh_loop_starts_when_the_first_answer_is_skipped() {
        let (store, id) = store_with("same").await;
        let calls = Arc::new(AtomicU64::new(0));
        let ask = asker(&calls, &[1], u64::MAX);
        let (_keep, closed) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn({
            let (store, id) = (store.clone(), id.clone());
            async move { info_loop(&store, &id, "same", TICK, ask, closed).await }
        });
        until_head(&store, &id, 2).await;
        task.abort();
    }

    #[test]
    fn the_refresh_interval_is_one_minute() {
        assert_eq!(INFO_REFRESH, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn only_the_first_answer_names_the_station() {
        let (store, id) = store_with("old").await;
        let calls = Arc::new(AtomicU64::new(0));
        let ask = {
            let calls = Arc::clone(&calls);
            move || {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                let name = if n == 1 { "first" } else { "second" };
                std::future::ready::<Answer>(Ok(Some(info(name, n))))
            }
        };
        let (_keep, closed) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn({
            let (store, id) = (store.clone(), id.clone());
            async move { info_loop(&store, &id, "old", TICK, ask, closed).await }
        });
        until_head(&store, &id, 3).await;
        task.abort();
        assert_eq!(name_of(&store, &id).await, "first");
    }
}
