//! The fleet summary that the deck sends to a station for the desktop shell
//! (R13 P4, `omarchy-p4.md` §3.1; feature `desktop_shell.v1`).
//!
//! The summary has no free-form text: no session title, no tool name, no
//! request title and no label that a person or an agent wrote. The only
//! texts are the station names, the checked callsign, the closed request
//! kinds, the fixed usage words and ids. The types carry no validation; the
//! builder (deck) and the intake (station) enforce the limits below.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::event::RequestKind;

/// The most items in `needs_you`.
pub const MAX_NEEDS_YOU: usize = 10;
/// The most entries in `stations`.
pub const MAX_STATIONS: usize = 32;
/// The longest `callsign`, in bytes.
pub const MAX_CALLSIGN: usize = 32;
/// The longest station `name`, in bytes.
pub const MAX_STATION_NAME: usize = 64;
/// The longest `board` origin, in bytes.
pub const MAX_BOARD: usize = 2048;
/// The largest serialized summary, in bytes (16 KiB).
pub const MAX_SUMMARY_BYTES: usize = 16 * 1024;

/// Whether the deck is reachable, as the summary tells it. The deck sends
/// `up`; the station and the feed correct it to `down` or `unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_summary/")]
pub enum DeckState {
    Up,
    Down,
    Unknown,
}

/// The card counts of the fleet. `working` includes verifying and queued;
/// `blocked` is an unreachable station.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_summary/")]
pub struct SummaryCounts {
    pub working: u32,
    pub idle: u32,
    pub needs_you: u32,
    pub blocked: u32,
    pub lost: u32,
}

/// One open request that waits for the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_summary/")]
pub struct NeedsYouItem {
    /// The station name.
    pub station: String,
    pub station_id: String,
    pub session: String,
    /// The request id, for deduplication.
    pub request: String,
    /// `None` when the session has no callsign or it breaks the grammar
    /// `^[a-z0-9-]{1,32}$`.
    pub callsign: Option<String>,
    pub kind: RequestKind,
    pub age_s: u32,
}

/// The tightest usage window of the fleet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_summary/")]
pub struct SummaryUsage {
    /// `<instance> · <window word>` with a closed set of window words.
    pub label: String,
    pub pct: u32,
    pub resets_in_s: Option<u32>,
}

/// One station of the fleet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_summary/")]
pub struct SummaryStation {
    pub name: String,
    pub up: bool,
    pub cpu_load_pct: Option<u32>,
}

/// The fleet summary (params of the link method `fleet.summary`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_summary/")]
pub struct FleetSummary {
    /// The version of the shape: 1.
    pub v: u32,
    /// The time of the summary, RFC 3339 in UTC.
    pub at: String,
    /// The origin of the board (`scheme://host[:port]`, no path), or `None`.
    pub board: Option<String>,
    pub deck: DeckState,
    pub counts: SummaryCounts,
    pub needs_you: Vec<NeedsYouItem>,
    pub usage: Option<SummaryUsage>,
    pub stations: Vec<SummaryStation>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"{"v":1,"at":"2026-10-18T12:04:00Z","board":"https://deck.example.ts.net:7420","deck":"up",
 "counts":{"working":7,"idle":3,"needs_you":2,"blocked":1,"lost":0},
 "needs_you":[{"station":"srv","station_id":"st1","session":"s1","request":"r1","callsign":"lynx-12",
               "kind":"tool","age_s":95}],
 "usage":{"label":"claude · 5 h","pct":63,"resets_in_s":4320},
 "stations":[{"name":"srv","up":true,"cpu_load_pct":41},{"name":"laptop","up":true,"cpu_load_pct":null}]}"#;

    #[test]
    fn the_annex_example_round_trips() {
        let summary: FleetSummary = serde_json::from_str(EXAMPLE).unwrap();
        assert_eq!(summary.v, 1);
        assert_eq!(summary.deck, DeckState::Up);
        assert_eq!(summary.needs_you[0].kind, RequestKind::Tool);
        assert_eq!(summary.stations[1].cpu_load_pct, None);
        let again: FleetSummary =
            serde_json::from_str(&serde_json::to_string(&summary).unwrap()).unwrap();
        assert_eq!(again, summary);
    }

    #[test]
    fn absent_values_are_null_not_omitted() {
        let mut summary: FleetSummary = serde_json::from_str(EXAMPLE).unwrap();
        summary.board = None;
        summary.usage = None;
        summary.needs_you[0].callsign = None;
        let value = serde_json::to_value(&summary).unwrap();
        assert!(value["board"].is_null());
        assert!(value["usage"].is_null());
        assert!(value["needs_you"][0]["callsign"].is_null());
    }

    #[test]
    fn deck_states_use_snake_case_words() {
        for (state, word) in [
            (DeckState::Up, "\"up\""),
            (DeckState::Down, "\"down\""),
            (DeckState::Unknown, "\"unknown\""),
        ] {
            assert_eq!(serde_json::to_string(&state).unwrap(), word);
        }
    }
}
