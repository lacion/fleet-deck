//! The pure builder of the fleet summary (`omarchy-p4.md` §3.1 and §3.2).
//!
//! [`build`] reads cards and stations and nothing else. It copies no free-form
//! text: no session title, no tool name, no request title or details, no
//! path. The only texts are the station names, the checked callsign, the
//! closed request kinds, the fixed usage words and ids. Every text loses its
//! control and bidirectional characters, and the limits cut it.

use std::collections::HashMap;

use fleetdeck_proto::board::DeckSession;
use fleetdeck_proto::fleet_summary::{
    DeckState, FleetSummary, MAX_BOARD, MAX_CALLSIGN, MAX_NEEDS_YOU, MAX_STATION_NAME,
    MAX_STATIONS, MAX_SUMMARY_BYTES, NeedsYouItem, SummaryCounts, SummaryStation,
};
use fleetdeck_proto::resources::CpuLoad;
use fleetdeck_proto::{CardStatus, StationId};

use super::usage::summary_usage;
use crate::desktop_relay::is_loopback_origin;

/// One station of the deck's station list, as the builder reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationEntry {
    pub id: StationId,
    /// The name that the owner gave the station.
    pub name: String,
    /// The link is up.
    pub up: bool,
    /// The CPU load of the last `station.info`, when it has one.
    pub cpu: Option<CpuLoad>,
}

/// Builds the summary at `now_ms` (Unix ms).
///
/// `board` is a candidate board origin; it passes only as
/// `scheme://host[:port]` (see [`pick_board`]). `stations` is in the order of
/// the deck's station list.
#[must_use]
pub fn build(
    cards: &[DeckSession],
    stations: &[StationEntry],
    now_ms: i64,
    board: Option<&str>,
) -> FleetSummary {
    let mut summary = FleetSummary {
        v: 1,
        at: rfc3339(now_ms),
        board: board.and_then(origin_of),
        deck: DeckState::Up,
        counts: counts(cards),
        needs_you: needs_you(cards, stations, now_ms),
        usage: summary_usage(cards, now_ms),
        stations: stations
            .iter()
            .take(MAX_STATIONS)
            .map(|s| SummaryStation {
                name: clean(&s.name, MAX_STATION_NAME),
                up: s.up,
                cpu_load_pct: s.cpu.as_ref().and_then(load_pct),
            })
            .collect(),
    };
    fit(&mut summary);
    summary
}

/// The first origin that is not loopback, reduced to `scheme://host[:port]`;
/// `None` when there is none or the first one is not a valid origin.
pub fn pick_board(origins: impl IntoIterator<Item = String>) -> Option<String> {
    origins
        .into_iter()
        .find(|o| !is_loopback_origin(o))
        .and_then(|o| origin_of(&o))
}

fn counts(cards: &[DeckSession]) -> SummaryCounts {
    let mut c = SummaryCounts {
        working: 0,
        idle: 0,
        needs_you: 0,
        blocked: 0,
        lost: 0,
    };
    for card in cards {
        let slot = match card.status {
            CardStatus::Working | CardStatus::Verifying | CardStatus::Queued => &mut c.working,
            CardStatus::Idle => &mut c.idle,
            CardStatus::NeedsYou => &mut c.needs_you,
            CardStatus::Unreachable => &mut c.blocked,
            CardStatus::Lost => &mut c.lost,
            // An ended card is not counted.
            CardStatus::Ended => continue,
        };
        *slot = slot.saturating_add(1);
    }
    c
}

/// One item for each open request of a card that needs the owner, oldest
/// first, at most [`MAX_NEEDS_YOU`]. A card on a station that is not up
/// gives no item.
fn needs_you(cards: &[DeckSession], stations: &[StationEntry], now_ms: i64) -> Vec<NeedsYouItem> {
    let by_id: HashMap<&StationId, &StationEntry> = stations.iter().map(|s| (&s.id, s)).collect();
    let mut items: Vec<(i64, NeedsYouItem)> = Vec::new();
    for card in cards.iter().filter(|c| c.status == CardStatus::NeedsYou) {
        let Some(station) = by_id.get(&card.station_id).filter(|s| s.up) else {
            continue;
        };
        let callsign = card
            .summary
            .callsign
            .as_deref()
            .filter(|c| callsign_ok(c))
            .map(str::to_owned);
        for request in &card.summary.open_requests {
            // The station refuses a whole summary with an id outside the
            // grammar (P4 §3.3): such an item is left out instead.
            if ![
                card.station_id.as_str(),
                card.summary.session_id.as_str(),
                request.request_id.as_str(),
            ]
            .into_iter()
            .all(id_ok)
            {
                continue;
            }
            let age_ms = now_ms.saturating_sub(request.requested_at);
            items.push((
                request.requested_at,
                NeedsYouItem {
                    station: clean(&station.name, MAX_STATION_NAME),
                    station_id: clean(card.station_id.as_str(), usize::MAX),
                    session: clean(card.summary.session_id.as_str(), usize::MAX),
                    request: clean(request.request_id.as_str(), usize::MAX),
                    callsign: callsign.clone(),
                    kind: request.request_kind,
                    age_s: u32::try_from((age_ms / 1000).max(0)).unwrap_or(u32::MAX),
                },
            ));
        }
    }
    items.sort_by(|(a, x), (b, y)| {
        a.cmp(b).then_with(|| {
            (&x.station_id, &x.session, &x.request).cmp(&(&y.station_id, &y.session, &y.request))
        })
    });
    items
        .into_iter()
        .map(|(_, item)| item)
        .take(MAX_NEEDS_YOU)
        .collect()
}

/// `^[a-z0-9-]{1,32}$`.
/// An id of the summary: `^[A-Za-z0-9_-]{1,64}$`.
fn id_ok(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn callsign_ok(callsign: &str) -> bool {
    !callsign.is_empty()
        && callsign.len() <= MAX_CALLSIGN
        && callsign
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `round(load1_x100 / cores)`, not capped; `None` without cores.
fn load_pct(cpu: &CpuLoad) -> Option<u32> {
    if cpu.cores == 0 {
        return None;
    }
    let cores = u64::from(cpu.cores);
    let pct = (u64::from(cpu.load1_x100) * 2 + cores) / (cores * 2);
    Some(u32::try_from(pct).unwrap_or(u32::MAX))
}

/// Drops `needs_you` items from the end, then `stations` from the end, until
/// the JSON fits in [`MAX_SUMMARY_BYTES`].
pub(super) fn fit(summary: &mut FleetSummary) {
    while json_len(summary) > MAX_SUMMARY_BYTES {
        if summary.needs_you.pop().is_none() && summary.stations.pop().is_none() {
            break;
        }
    }
}

/// The size of the serialized summary. These plain types always serialize;
/// the failure arm counts as "too large" so that the caller drops items.
fn json_len(summary: &FleetSummary) -> usize {
    serde_json::to_vec(summary).map_or(usize::MAX, |v| v.len())
}

/// Whether `c` is a bidirectional control character.
fn is_bidi(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// `text` without control and bidirectional characters, cut at `max` bytes
/// on a character boundary.
pub(super) fn clean(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars().filter(|c| !c.is_control() && !is_bidi(*c)) {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

/// `scheme://host[:port]` from an origin or a URL: the scheme is `http` or
/// `https`, the authority holds only letters, digits and `.-_:[]` (no user
/// information, no whitespace), the result is at most [`MAX_BOARD`] bytes.
fn origin_of(raw: &str) -> Option<String> {
    let (scheme, rest) = raw.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let ok = !authority.is_empty()
        && authority.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':' | b'[' | b']')
        });
    let origin = format!("{scheme}://{authority}");
    (ok && origin.len() <= MAX_BOARD).then_some(origin)
}

/// A Unix time in ms as RFC 3339 in UTC with seconds and `Z`.
pub(super) fn rfc3339(at_ms: i64) -> String {
    let secs = at_ms.div_euclid(1000);
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The civil date of a day count since 1970-01-01 (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod id_tests {
    use super::id_ok;

    #[test]
    fn an_id_outside_the_station_grammar_is_left_out() {
        assert!(id_ok("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert!(id_ok("toolu_01-x"));
        for bad in ["", "a:b", "a b", "a/b", &"a".repeat(65)] {
            assert!(!id_ok(bad), "{bad}");
        }
    }
}
