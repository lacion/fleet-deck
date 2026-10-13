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
use crate::local::station_view;
use crate::store::auth::{random_token, secret_hash};
use crate::store::{DeckError, DeckStore, LiveEvents, Principal};

/// How long a WebSocket ticket is valid.
pub const TICKET_TTL_MS: i64 = 30_000;
/// Unused tickets that the deck keeps at most.
pub const MAX_TICKETS: usize = 256;
/// How often an open socket checks its login.
pub const RECHECK: Duration = Duration::from_secs(30);
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
            if let Err(e) = Session::new(board, principal, socket).run().await {
                tracing::debug!(error = %e, "board socket");
            }
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
    socket: WebSocket,
    cards: BTreeMap<SessionKey, DeckSession>,
    stations: Vec<StationView>,
    subs: BTreeMap<SessionKey, Sub>,
}

/// The board view: the paired stations and every card. A free function
/// (not a method of [`Session`]): the socket inside a session is not
/// `Sync`, so no `&Session` may live across an await.
async fn view(
    store: &DeckStore,
    principal: &Principal,
) -> Result<(Vec<StationView>, Vec<DeckSession>), WsError> {
    let stations = store
        .stations()
        .await?
        .iter()
        .map(|st| station_view(st, store))
        .collect();
    Ok((stations, store.sessions_for(principal).await?))
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
    fn new(board: Arc<Board>, principal: Principal, socket: WebSocket) -> Self {
        Self {
            board,
            principal,
            socket,
            cards: BTreeMap::new(),
            stations: Vec::new(),
            subs: BTreeMap::new(),
        }
    }

    async fn send(&mut self, m: &BoardServerMessage) -> Result<(), WsError> {
        let text = serde_json::to_string(m)?;
        self.socket.send(Message::Text(text.into())).await?;
        Ok(())
    }

    async fn run(mut self) -> Result<(), WsError> {
        let store = self.board.store.clone();
        let mut board_rx = store.watch_board();
        let mut live_rx = store.subscribe_live();
        self.send(&BoardServerMessage::Hello {
            protocol: BOARD_PROTOCOL,
            deck_incarnation: store.incarnation().clone(),
        })
        .await?;
        let version = *board_rx.borrow_and_update();
        let (stations, sessions) = view(&self.board.store, &self.principal).await?;
        self.cards = sessions.iter().map(|c| (key(c), c.clone())).collect();
        self.stations.clone_from(&stations);
        self.send(&BoardServerMessage::Snapshot {
            version,
            stations,
            sessions,
        })
        .await?;
        let mut recheck = tokio::time::interval(self.board.ws_recheck);
        recheck.tick().await;
        loop {
            tokio::select! {
                changed = board_rx.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    let version = *board_rx.borrow_and_update();
                    self.delta(version).await?;
                }
                live = live_rx.recv() => match live {
                    Ok(batch) => self.live(&batch).await?,
                    Err(RecvError::Lagged(_)) => self.catch_up().await?,
                    Err(RecvError::Closed) => return Ok(()),
                },
                msg = self.socket.recv() => match msg {
                    Some(Ok(Message::Text(text))) => self.client(text.as_str()).await?,
                    Some(Ok(Message::Binary(_))) => {
                        self.error("invalid_message", "the board protocol uses text messages").await?;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return Ok(()),
                },
                _ = recheck.tick() => {
                    if !store.principal_alive(&self.principal).await? {
                        let _ = self.socket.send(Message::Close(Some(CloseFrame {
                            code: CLOSE_LOGIN_ENDED,
                            reason: "login ended".into(),
                        }))).await;
                        return Ok(());
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
        let (stations, sessions) = view(&self.board.store, &self.principal).await?;
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
        self.stored_after(&k).await
    }
}
