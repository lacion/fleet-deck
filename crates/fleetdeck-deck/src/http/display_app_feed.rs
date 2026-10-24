//! The wall app feed (B17 §3.4.3): `GET
//! /api/displays/self/app-feed/{station}/{id}/{version}`.
//!
//! The route authenticates with `fd_display` only (the guard). It answers
//! only while the effective directive is this artifact version with `app:
//! true` and the display has `allow_apps` on: it runs the same `authorize` as
//! the artifact route. Every other case is the `404 unknown_artifact` of the
//! artifact route.
//!
//! The feed holds the display cards (the reduced cards that a display
//! already gets) of the author session of the artifact and of the sessions
//! that this session started (the spawn lineage of every grant of the author,
//! one level), inside the effective scope of the directive generation that
//! the answer is bound to: every check must see the generation of the first
//! one. At most [`MAX_FEED_CARDS`] cards
//! and [`MAX_FEED_BYTES`] bytes. A card has no tool, no request text and no
//! mail. The answer is `no-store`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use fleetdeck_proto::display_directive::ArtifactRef;
use fleetdeck_proto::wall_apps::{MAX_FEED_BYTES, MAX_FEED_CARDS, WallAppCard, WallAppFeed};
use fleetdeck_proto::{SessionId, StationId};
use serde::Serialize;

use super::Board;
use super::display_artifact::{authorize, miss};
use super::display_view::display_cards;
use super::guard::ApiError;
use crate::store::Principal;

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new().route(
        "/api/displays/self/app-feed/{station}/{id}/{version}",
        get(feed),
    )
}

/// The serde word of a value (`working`, `needs_you`, `file_change`, ...).
fn word<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Keeps the first [`MAX_FEED_CARDS`] cards, then drops cards from the end
/// until the body of the feed is at most [`MAX_FEED_BYTES`] bytes.
fn fit(mut feed: WallAppFeed) -> Result<Vec<u8>, ApiError> {
    feed.sessions.truncate(MAX_FEED_CARDS);
    loop {
        let body = serde_json::to_vec(&feed).map_err(ApiError::internal)?;
        if body.len() <= MAX_FEED_BYTES || feed.sessions.pop().is_none() {
            return Ok(body);
        }
    }
}

/// `GET /api/displays/self/app-feed/{station}/{id}/{version}`.
///
/// # Errors
/// 401 `login_required`; 403 `display_only`; 404 `unknown_artifact`.
async fn feed(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, id, version)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let Principal::Display(login) = principal else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "display_only",
            "only a wall display login does this",
        ));
    };
    let version = version
        .parse::<u32>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(miss)?;
    let target = ArtifactRef {
        station_id: StationId::new(station),
        artifact_id: id,
        version,
    };
    let first = authorize(&board, &login, &target).await?;
    if !first.app() {
        return Err(miss());
    }
    let row = board
        .store
        .artifact(target.station_id.clone(), target.artifact_id.clone())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(miss)?;
    let mut sessions: Vec<WallAppCard> = Vec::new();
    if let Some(author) = row.session_id {
        let mut set: Vec<(StationId, SessionId)> = board
            .store
            .display_app_lineage(target.station_id.clone(), author.clone())
            .await
            .map_err(ApiError::internal)?;
        let own = (target.station_id.clone(), author);
        if !set.contains(&own) {
            set.push(own);
        }
        let names: BTreeMap<StationId, String> = board
            .store
            .stations()
            .await
            .map_err(ApiError::internal)?
            .into_iter()
            .map(|s| (s.station_id, s.name))
            .collect();
        let all = board.store.sessions().await.map_err(ApiError::internal)?;
        // The scope comes from the last check, after the data: it is the
        // scope of the directive generation that the answer is bound to.
        let last = authorize(&board, &login, &target).await?;
        if !first.holds(&last, true) {
            return Err(miss());
        }
        sessions = display_cards(&all, &last.scope)
            .into_iter()
            .filter(|c| {
                set.iter()
                    .any(|(st, se)| *st == c.station_id && *se == c.summary.session_id)
            })
            .map(|c| WallAppCard {
                station: names
                    .get(&c.station_id)
                    .cloned()
                    .unwrap_or_else(|| c.station_id.as_str().to_owned()),
                session_id: c.summary.session_id.clone(),
                callsign: c.summary.callsign.clone().unwrap_or_default(),
                title: c.summary.title.clone().unwrap_or_default(),
                status: word(&c.status),
                needs_you: c
                    .summary
                    .open_requests
                    .first()
                    .map(|r| word(&r.request_kind)),
            })
            .collect();
    }
    // The authority of this moment, right before the answer leaves: the
    // same directive generation as the first check (a new directive, also of
    // the same artifact with another filter, ends the answer), and the app
    // still allowed.
    if !first.holds(&authorize(&board, &login, &target).await?, true) {
        return Err(miss());
    }
    let body = fit(WallAppFeed {
        at: board.store.now_ms(),
        sessions,
    })?;
    let mut response = Response::new(axum::body::Body::from(body));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(n: usize, title: &str) -> WallAppCard {
        WallAppCard {
            station: "S".to_owned(),
            session_id: SessionId::new(format!("session-{n}")),
            callsign: String::new(),
            title: title.to_owned(),
            status: "idle".to_owned(),
            needs_you: None,
        }
    }

    #[test]
    fn the_feed_keeps_64_cards() {
        let feed = WallAppFeed {
            at: 1,
            sessions: (0..70).map(|n| card(n, "t")).collect(),
        };
        let body = fit(feed).unwrap();
        let back: WallAppFeed = serde_json::from_slice(&body).unwrap();
        assert_eq!(back.sessions.len(), 64);
        assert_eq!(back.sessions[0].session_id.as_str(), "session-0");
    }

    #[test]
    fn the_feed_drops_cards_from_the_end_until_32_kib() {
        let big = "x".repeat(2000);
        let feed = WallAppFeed {
            at: 1,
            sessions: (0..40).map(|n| card(n, &big)).collect(),
        };
        let body = fit(feed).unwrap();
        assert!(body.len() <= MAX_FEED_BYTES);
        let back: WallAppFeed = serde_json::from_slice(&body).unwrap();
        assert!(!back.sessions.is_empty() && back.sessions.len() < 40);
        assert_eq!(back.sessions[0].session_id.as_str(), "session-0");
    }

    #[test]
    fn a_status_and_a_request_kind_are_their_serde_words() {
        use fleetdeck_proto::event::RequestKind;
        use fleetdeck_proto::session::CardStatus;
        assert_eq!(word(&CardStatus::NeedsYou), "needs_you");
        assert_eq!(word(&RequestKind::FileChange), "file_change");
    }
}
