//! The board WebSocket (M2 P3-9b; PLAN §9 "WS for snapshot + deltas",
//! §7.1, L15, DL-056): `GET /api/ws`.
//!
//! - **Login.** A browser opens the socket with its login cookie and must
//!   send an `Origin` of this deck's board (cross-site WebSocket hijacking:
//!   a browser sends cookies on a WebSocket upgrade from any page). A
//!   bearer client asks `POST /api/ws/ticket` for a single-use ticket
//!   (30 s) and opens `GET /api/ws?ticket=…`. The guard checks both.
//! - **Board stream.** `hello`, one `snapshot`, then a `delta` after each
//!   change of the board view ([`crate::store::DeckStore::watch_board`]).
//!   The deltas are computed against what this socket sent last, so a
//!   burst of changes collapses into one delta.
//! - **Timelines.** `subscribe` sends the stored timeline after the
//!   client's cursor, then every folded event of the session (also the
//!   streaming deltas). A changed station incarnation restarts the
//!   subscription with `reset`. A socket that lags behind the live
//!   events catches up from the stored timeline.
//! - **Expiry.** Every [`RECHECK`] (the board's `ws_recheck`) the socket checks that its login
//!   session or token is still valid and closes with code 4401 when not.
//! - **Members** (R11, DL-166). A member's socket sees the cards, the stations
//!   and the timelines of the sessions of its share only. At every change of
//!   the board and at every recheck the deck loads the share again, so a
//!   revoke or a change of a space takes effect at once.
//! - **Displays** (R13 §5, §5D). A display subscribes to the timelines of the
//!   sessions in its effective scope and gets the reduced events of
//!   `ws_display.rs`, never an owner event and never a people frame.
//! - **People.** A `presence` heartbeat of a viewer (every 20 s) and the
//!   `presence`, `chat`, `driver` and `drive_request` messages of the people
//!   features for the sessions that the socket may see.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Json;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::Response;
use fleetdeck_proto::board::{
    BOARD_PROTOCOL, BoardClientMessage, BoardServerMessage, DeckSession, MAX_SUBSCRIPTIONS,
    SessionKey, StationView, WsTicket,
};
use fleetdeck_proto::{EventEnvelope, IncarnationId, StationId};
use tokio::sync::broadcast::error::RecvError;

use super::Board;
use super::guard::ApiError;
use super::read_routes::{member_sessions, member_stations};
use crate::local::station_view;
use crate::store::auth::{random_token, secret_hash};
use crate::store::display_directives::EffectiveScope;
use crate::store::{DeckError, DeckStore, LiveEvents, Principal, SocialEvent};

#[path = "ws_display.rs"]
mod display;

/// How long a WebSocket ticket is valid.
pub const TICKET_TTL_MS: i64 = 30_000;
/// Unused tickets that the deck keeps at most.
pub const MAX_TICKETS: usize = 256;
/// How often an open socket checks its login.
pub const RECHECK: Duration = Duration::from_secs(30);
/// How often the socket of a display checks its login.
pub const DISPLAY_RECHECK: Duration = Duration::from_secs(2);
/// The longest time that one send to a display socket may take.
pub const DISPLAY_SEND: Duration = Duration::from_secs(2);
/// The longest time that the close frame of a display socket may take.
const DISPLAY_CLOSE: Duration = Duration::from_secs(1);
/// The largest client message.
pub const MAX_CLIENT_MESSAGE: usize = 16 * 1024;
/// The close code for an ended login.
pub const CLOSE_LOGIN_ENDED: u16 = 4401;

/// Single-use WebSocket tickets, in memory (a deck restart ends them; the
/// client asks again).
#[derive(Debug, Default)]
pub struct Tickets(Mutex<HashMap<String, (Principal, i64)>>);

impl Tickets {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, (Principal, i64)>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A new ticket for `principal`, or `None` when too many are open.
    ///
    /// # Errors
    /// The random source failed.
    pub fn issue(
        &self,
        principal: Principal,
        now: i64,
    ) -> Result<Option<WsTicket>, crate::store::DeckError> {
        let value = format!("fdws1_{}", random_token()?);
        let mut map = self.map();
        map.retain(|_, (_, expires)| *expires > now);
        if map.len() >= MAX_TICKETS {
            return Ok(None);
        }
        let expires_at = now + TICKET_TTL_MS;
        map.insert(secret_hash(&value), (principal, expires_at));
        Ok(Some(WsTicket {
            ticket: value,
            expires_at,
        }))
    }

    /// Takes a ticket: valid once, before its expiry.
    pub fn redeem(&self, value: &str, now: i64) -> Option<Principal> {
        let (principal, expires) = self.map().remove(&secret_hash(value))?;
        (expires > now).then_some(principal)
    }
}

/// `POST /api/ws/ticket`.
pub async fn ticket(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<WsTicket>, ApiError> {
    match board
        .tickets
        .issue(principal, board.store.now_ms())
        .map_err(ApiError::internal)?
    {
        Some(t) => Ok(Json(t)),
        None => Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_tickets",
            "too many unused WebSocket tickets; try again in 30 s",
        )),
    }
}

/// `GET /api/ws`.
pub async fn upgrade(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.max_message_size(MAX_CLIENT_MESSAGE)
        .max_frame_size(MAX_CLIENT_MESSAGE)
        .on_upgrade(move |socket| async move {
            let socket_id = board.store.new_socket_id();
            let store = board.store.clone();
            if let Err(e) = Session::new(board, principal, socket, socket_id)
                .run()
                .await
            {
                tracing::debug!(error = %e, "board socket");
            }
            store.presence_leave(socket_id);
        })
}

/// Why a socket ended.
#[derive(Debug, thiserror::Error)]
enum WsError {
    #[error("send: {0}")]
    Send(#[from] axum::Error),
    #[error(transparent)]
    Store(#[from] DeckError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// A display did not take a message in time (R13 §5A): the socket ends.
    #[error("a display socket send timed out")]
    SendTimeout,
}

/// One timeline subscription.
#[derive(Debug)]
struct Sub {
    incarnation: Option<IncarnationId>,
    /// The last `station_seq` sent for this session.
    cursor: u64,
}

struct Session {
    board: Arc<Board>,
    principal: Principal,
    /// The id of this socket for presence.
    socket_id: u64,
    socket: WebSocket,
    cards: BTreeMap<SessionKey, DeckSession>,
    stations: Vec<StationView>,
    subs: BTreeMap<SessionKey, Sub>,
    /// The effective scope of a display at its last view (R13 §5D item 17).
    scope: Option<EffectiveScope>,
}

/// The board view: the paired stations and every card. A free function
/// (not a method of [`Session`]): the socket inside a session is not
/// `Sync`, so no `&Session` may live across an await.
async fn view(
    store: &DeckStore,
    principal: &Principal,
) -> Result<(Vec<StationView>, Vec<DeckSession>, Option<EffectiveScope>), WsError> {
    if let Principal::Display(d) = principal {
        return display::view(store, d).await;
    }
    let stations: Vec<StationView> = store
        .stations()
        .await?
        .iter()
        .map(|st| station_view(st, store))
        .collect();
    let sessions = store.sessions_for(principal).await?;
    Ok(match principal {
        Principal::Member(_, grant) => {
            let sessions = member_sessions(grant, sessions);
            (member_stations(stations, &sessions), sessions, None)
        }
        _ => (stations, sessions, None),
    })
}

async fn incarnation_of(
    store: &DeckStore,
    station: &StationId,
) -> Result<Option<IncarnationId>, WsError> {
    Ok(store
        .station(station.clone())
        .await?
        .and_then(|r| r.incarnation))
}

fn key(c: &DeckSession) -> SessionKey {
    SessionKey {
        station_id: c.station_id.clone(),
        session_id: c.summary.session_id.clone(),
    }
}

impl Session {
    fn new(board: Arc<Board>, principal: Principal, socket: WebSocket, socket_id: u64) -> Self {
        Self {
            board,
            principal,
            socket_id,
            socket,
            cards: BTreeMap::new(),
            stations: Vec::new(),
            subs: BTreeMap::new(),
            scope: None,
        }
    }

    async fn send(&mut self, m: &BoardServerMessage) -> Result<(), WsError> {
        let text = serde_json::to_string(m)?;
        if matches!(self.principal, Principal::Display(_)) {
            // A display that does not read must not hold the socket (R13 §5A).
            tokio::time::timeout(
                self.board.ws_display_send,
                self.socket.send(Message::Text(text.into())),
            )
            .await
            .map_err(|_| WsError::SendTimeout)??;
        } else {
            self.socket.send(Message::Text(text.into())).await?;
        }
        Ok(())
    }

    /// Sends a close frame and ignores a failure. A display gets 1 s.
    async fn close(&mut self, code: u16, reason: &str) {
        let frame = Message::Close(Some(CloseFrame {
            code,
            reason: reason.to_owned().into(),
        }));
        if matches!(self.principal, Principal::Display(_)) {
            let _ = tokio::time::timeout(DISPLAY_CLOSE, self.socket.send(frame)).await;
        } else {
            let _ = self.socket.send(frame).await;
        }
    }

    async fn run(mut self) -> Result<(), WsError> {
        let store = self.board.store.clone();
        let mut board_rx = store.watch_board();
        let mut live_rx = store.subscribe_live();
        let mut social_rx = store.subscribe_social();
        self.send(&BoardServerMessage::Hello {
            protocol: BOARD_PROTOCOL,
            deck_incarnation: store.incarnation().clone(),
        })
        .await?;
        let version = *board_rx.borrow_and_update();
        if !self.refresh_member().await? && matches!(self.principal, Principal::Display(_)) {
            // The display was revoked after its ticket was made.
            self.close(CLOSE_LOGIN_ENDED, "login ended").await;
            return Ok(());
        }
        let (stations, sessions, scope) = view(&self.board.store, &self.principal).await?;
        self.scope = scope;
        self.cards = sessions.iter().map(|c| (key(c), c.clone())).collect();
        self.stations.clone_from(&stations);
        self.send(&BoardServerMessage::Snapshot {
            version,
            stations,
            sessions,
        })
        .await?;
        // A display checks its login every 2 s and a send takes at most 2 s: a
        // revoke ends the transport in about 4 s (R13 §5).
        let every = match self.principal {
            Principal::Display(_) => self.board.ws_recheck.min(DISPLAY_RECHECK),
            _ => self.board.ws_recheck,
        };
        let mut recheck = tokio::time::interval(every);
        recheck.tick().await;
        let mut sweep = tokio::time::interval(Duration::from_secs(
            fleetdeck_proto::share::PRESENCE_HEARTBEAT_S,
        ));
        sweep.tick().await;
        loop {
            tokio::select! {
                changed = board_rx.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    let version = *board_rx.borrow_and_update();
                    // A display takes its new ceiling first (R13 §5).
                    if matches!(self.principal, Principal::Display(_))
                        && !self.refresh_member().await?
                    {
                        self.close(CLOSE_LOGIN_ENDED, "login ended").await;
                        return Ok(());
                    }
                    self.delta(version).await?;
                }
                live = live_rx.recv() => match live {
                    Ok(batch) => self.live(&batch).await?,
                    Err(RecvError::Lagged(_)) => self.catch_up().await?,
                    Err(RecvError::Closed) => return Ok(()),
                },
                social = social_rx.recv() => match social {
                    Ok(event) => self.social(&event).await?,
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return Ok(()),
                },
                _ = sweep.tick() => store.presence_sweep(),
                msg = self.socket.recv() => match msg {
                    Some(Ok(Message::Text(text))) => self.client(text.as_str()).await?,
                    Some(Ok(Message::Binary(_))) => {
                        self.error("invalid_message", "the board protocol uses text messages").await?;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return Ok(()),
                },
                _ = recheck.tick() => {
                    if !store.principal_alive(&self.principal).await? || !self.refresh_member().await? {
                        self.close(CLOSE_LOGIN_ENDED, "login ended").await;
                        return Ok(());
                    }
                    // A directive changes no board version: the display looks
                    // at its scope on each recheck (R9 Q6).
                    if matches!(self.principal, Principal::Display(_)) {
                        let version = *board_rx.borrow();
                        self.display_rescope(version).await?;
                    }
                }
            }
        }
    }

    async fn error(&mut self, code: &str, message: &str) -> Result<(), WsError> {
        self.send(&BoardServerMessage::Error {
            code: code.to_owned(),
            message: message.to_owned(),
        })
        .await
    }

    /// Sends what changed since the last snapshot or delta, and restarts
    /// the subscriptions of a station with a new incarnation.
    async fn delta(&mut self, version: u64) -> Result<(), WsError> {
        if !self.refresh_member().await? {
            self.close(CLOSE_LOGIN_ENDED, "login ended").await;
            return Err(WsError::Send(axum::Error::new("the member login ended")));
        }
        let (stations, sessions, scope) = view(&self.board.store, &self.principal).await?;
        let mut next: BTreeMap<SessionKey, DeckSession> =
            sessions.into_iter().map(|c| (key(&c), c)).collect();
        let changed: Vec<DeckSession> = next
            .iter()
            .filter(|(k, c)| self.cards.get(*k) != Some(*c))
            .map(|(_, c)| c.clone())
            .collect();
        let removed: Vec<SessionKey> = self
            .cards
            .keys()
            .filter(|k| !next.contains_key(*k))
            .cloned()
            .collect();
        let stations_changed = stations != self.stations;
        std::mem::swap(&mut self.cards, &mut next);
        if matches!(self.principal, Principal::Display(_)) {
            // A subscription whose session left the scope ends at once
            // (R13 §5A item 4, §5D item 16).
            let cards = &self.cards;
            self.subs.retain(|k, _| cards.contains_key(k));
            self.scope = scope;
        }
        if !changed.is_empty() || !removed.is_empty() || stations_changed {
            self.send(&BoardServerMessage::Delta {
                version,
                stations: stations_changed.then(|| stations.clone()),
                sessions: changed,
                removed,
            })
            .await?;
        }
        self.stations = stations;
        self.resync_incarnations().await
    }

    async fn resync_incarnations(&mut self) -> Result<(), WsError> {
        let store = self.board.store.clone();
        let subs: Vec<(SessionKey, Option<IncarnationId>)> = self
            .subs
            .iter()
            .map(|(k, s)| (k.clone(), s.incarnation.clone()))
            .collect();
        let mut restart = Vec::new();
        for (k, had) in subs {
            if incarnation_of(&store, &k.station_id).await? != had {
                restart.push(k);
            }
        }
        for k in restart {
            self.start(k, None, None).await?;
        }
        Ok(())
    }

    async fn live(&mut self, batch: &LiveEvents) -> Result<(), WsError> {
        if matches!(self.principal, Principal::Display(_)) {
            return self.display_live(batch).await;
        }
        let mut out: BTreeMap<SessionKey, Vec<EventEnvelope>> = BTreeMap::new();
        for e in &batch.events {
            let k = SessionKey {
                station_id: batch.station_id.clone(),
                session_id: e.session_id.clone(),
            };
            if let Some(sub) = self.subs.get_mut(&k)
                && sub.incarnation.as_ref() == Some(&batch.incarnation)
                && e.station_seq > sub.cursor
            {
                sub.cursor = e.station_seq;
                out.entry(k).or_default().push(e.clone());
            }
        }
        for (k, events) in out {
            self.send(&BoardServerMessage::TimelineEvents {
                station_id: k.station_id,
                session_id: k.session_id,
                events,
            })
            .await?;
        }
        Ok(())
    }

    /// A member's share is loaded again: the covered sessions can change.
    /// False when the login or its share ended. A subscription to a session
    /// that the share does not cover any more ends.
    async fn refresh_member(&mut self) -> Result<bool, WsError> {
        if let Principal::Display(d) = &self.principal {
            // The ceiling can change and the display can be revoked.
            return Ok(match self.board.store.display_refresh(d).await? {
                Some(fresh) => {
                    self.principal = Principal::Display(fresh);
                    true
                }
                None => false,
            });
        }
        let Principal::Member(id, grant) = &self.principal else {
            return Ok(true);
        };
        let Some(fresh) = self.board.store.member_refresh(grant).await? else {
            return Ok(false);
        };
        let id = id.clone();
        self.subs.retain(|k, _| {
            fresh
                .scope
                .covers(k.station_id.as_str(), k.session_id.as_str())
        });
        self.principal = Principal::Member(id, Box::new(fresh));
        Ok(true)
    }

    /// True when this socket may see the session.
    fn may_see(&self, k: &SessionKey) -> bool {
        match &self.principal {
            Principal::Member(_, g) => g.scope.covers(k.station_id.as_str(), k.session_id.as_str()),
            // A display gets no people features. Its timelines use
            // `display::authorized`.
            Principal::Display(_) => false,
            _ => true,
        }
    }

    /// An update of the people features (presence, chat, driver).
    async fn social(&mut self, event: &SocialEvent) -> Result<(), WsError> {
        if self.may_see(&event.key) {
            self.send(&event.message).await?;
        }
        Ok(())
    }

    /// After a lag: every subscription resends the stored timeline after
    /// its cursor (the streaming deltas in between are lost).
    async fn catch_up(&mut self) -> Result<(), WsError> {
        let keys: Vec<SessionKey> = self.subs.keys().cloned().collect();
        for k in keys {
            self.stored_after(&k).await?;
        }
        Ok(())
    }

    async fn stored_after(&mut self, k: &SessionKey) -> Result<(), WsError> {
        if matches!(self.principal, Principal::Display(_)) {
            return self.display_stored_after(k).await;
        }
        let Some(cursor) = self.subs.get(k).map(|s| s.cursor) else {
            return Ok(());
        };
        let events = self
            .board
            .store
            .timeline_after(k.station_id.clone(), k.session_id.clone(), cursor)
            .await?;
        if let (Some(last), Some(sub)) = (events.last(), self.subs.get_mut(k)) {
            sub.cursor = last.station_seq;
        }
        if !events.is_empty() {
            self.send(&BoardServerMessage::TimelineEvents {
                station_id: k.station_id.clone(),
                session_id: k.session_id.clone(),
                events,
            })
            .await?;
        }
        Ok(())
    }

    async fn client(&mut self, text: &str) -> Result<(), WsError> {
        let msg = match serde_json::from_str::<BoardClientMessage>(text) {
            Ok(m) => m,
            Err(e) => {
                let detail = format!(
                    "not a board message (line {}, column {})",
                    e.line(),
                    e.column()
                );
                return self.error("invalid_message", &detail).await;
            }
        };
        if matches!(self.principal, Principal::Display(_))
            && !matches!(
                msg,
                BoardClientMessage::Unsubscribe { .. } | BoardClientMessage::Subscribe { .. }
            )
        {
            return self
                .error("display_read_only", "A wall display can only read.")
                .await;
        }
        match msg {
            BoardClientMessage::Subscribe {
                station_id,
                session_id,
                incarnation,
                after,
            } => {
                let k = SessionKey {
                    station_id,
                    session_id,
                };
                if !self.subs.contains_key(&k) && self.subs.len() >= MAX_SUBSCRIPTIONS {
                    let detail = format!("at most {MAX_SUBSCRIPTIONS} timelines per socket");
                    return self.error("too_many_subscriptions", &detail).await;
                }
                let visible = if let Principal::Display(d) = &self.principal {
                    let (store, id) = (self.board.store.clone(), d.display_id.clone());
                    display::authorized(&store, &id, &k).await?
                } else {
                    self.may_see(&k)
                };
                if !visible {
                    return self.error("unknown_session", "no such session").await;
                }
                if self
                    .board
                    .store
                    .station(k.station_id.clone())
                    .await?
                    .is_none()
                {
                    return self.error("unknown_station", "no such station").await;
                }
                self.start(k, incarnation, after).await
            }
            BoardClientMessage::Presence {
                station_id,
                session_id,
            } => {
                let k = SessionKey {
                    station_id,
                    session_id,
                };
                if !self.may_see(&k) {
                    return self.error("unknown_session", "no such session").await;
                }
                let actor = self.principal.actor();
                self.board
                    .store
                    .presence_heartbeat(&k, &actor, self.socket_id);
                Ok(())
            }
            BoardClientMessage::Unsubscribe {
                station_id,
                session_id,
            } => {
                self.subs.remove(&SessionKey {
                    station_id,
                    session_id,
                });
                Ok(())
            }
        }
    }

    /// (Re)starts a subscription: the client's cursor applies only for the
    /// station's current incarnation.
    async fn start(
        &mut self,
        k: SessionKey,
        incarnation: Option<IncarnationId>,
        after: Option<u64>,
    ) -> Result<(), WsError> {
        let store = self.board.store.clone();
        let current = incarnation_of(&store, &k.station_id).await?;
        let resume = match (&current, &incarnation, after) {
            (Some(now), Some(theirs), Some(after)) if now == theirs => Some(after),
            _ => None,
        };
        self.subs.insert(
            k.clone(),
            Sub {
                incarnation: current.clone(),
                cursor: resume.unwrap_or(0),
            },
        );
        self.send(&BoardServerMessage::TimelineStart {
            station_id: k.station_id.clone(),
            session_id: k.session_id.clone(),
            incarnation: current,
            reset: resume.is_none(),
        })
        .await?;
        self.stored_after(&k).await?;
        if matches!(self.principal, Principal::Display(_)) {
            // A display gets no presence, driver or drive request (R13 §5D
            // item 16).
            return Ok(());
        }
        // The current state of the people features of the session.
        let who = store.presence_of(&k);
        if !who.is_empty() {
            self.send(&BoardServerMessage::Presence {
                station_id: k.station_id.clone(),
                session_id: k.session_id.clone(),
                who,
            })
            .await?;
        }
        let state = store
            .driver_state(k.station_id.0.clone(), k.session_id.0.clone())
            .await?;
        if state.driver.is_some() || !state.requests.is_empty() {
            self.send(&BoardServerMessage::Driver {
                station_id: k.station_id.clone(),
                session_id: k.session_id.clone(),
                driver: state.driver,
            })
            .await?;
            self.send(&BoardServerMessage::DriveRequest {
                station_id: k.station_id.clone(),
                session_id: k.session_id.clone(),
                requests: state.requests,
            })
            .await?;
        }
        Ok(())
    }
}
