//! Wall apps (B17 §3.4.3): the feed that the wall passes to an app. The
//! feed holds the display cards of the author session of the artifact and of
//! the sessions that this session started, and nothing else.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::SessionId;

/// The most cards in one feed.
pub const MAX_FEED_CARDS: usize = 64;
/// The largest feed body, in bytes.
pub const MAX_FEED_BYTES: usize = 32 * 1024;

/// One session card of the feed. Each field comes from the display card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallAppCard {
    /// The station name, or the station id when the name is not known.
    pub station: String,
    pub session_id: SessionId,
    pub callsign: String,
    pub title: String,
    /// The card status word (`working`, `needs_you`, `idle`, ...).
    pub status: String,
    /// The kind of the first open request (`command`, `file_change`, `tool`,
    /// `question`), or null.
    pub needs_you: Option<String>,
}

/// `GET /api/displays/self/app-feed/{station}/{id}/{version}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallAppFeed {
    /// Unix ms of the answer, from the deck clock.
    pub at: i64,
    pub sessions: Vec<WallAppCard>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_feed_wire_shape_has_no_tool_field_and_a_null_request() {
        let card = WallAppCard {
            station: "Lighting".to_owned(),
            session_id: SessionId::new("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
            callsign: "Echo".to_owned(),
            title: "Fix".to_owned(),
            status: "working".to_owned(),
            needs_you: None,
        };
        let feed = WallAppFeed {
            at: 7,
            sessions: vec![card],
        };
        let json = serde_json::to_value(&feed).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "at": 7,
                "sessions": [{
                    "station": "Lighting",
                    "session_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "callsign": "Echo",
                    "title": "Fix",
                    "status": "working",
                    "needs_you": null
                }]
            })
        );
    }
}
