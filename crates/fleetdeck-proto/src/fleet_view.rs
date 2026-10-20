//! The fleet view (MCP P3, design `docs/v2/design/mcp-p3.md` §2): the link
//! method `fleet.view` that a station sends to the deck for every session,
//! with or without a coordinator grant. Feature `fleet_view.v1`.
//!
//! The answer is a page of one view: the stations, the projects, the live
//! sessions (titles and states only) or the calling session itself (`me`).
//! Every listed key is always present: a missing value is `null`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::coordinator_spec::GrantScope;
use crate::event::ProviderKind;
use crate::session::CardStatus;
use crate::{SessionId, StationId};

/// The view that a `fleet.view` request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum FleetView {
    Stations,
    Projects,
    Sessions,
    Me,
}

impl FleetView {
    /// The name of the view on the wire.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Stations => "stations",
            Self::Projects => "projects",
            Self::Sessions => "sessions",
            Self::Me => "me",
        }
    }
}

/// The params of `fleet.view` (design §2.1). `station` filters the `projects`
/// view and the other views ignore it; `cursor` is ignored for `me`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetViewRequest {
    /// The session that asks: the claim of the station (ADR-006 §3).
    pub caller: SessionId,
    pub view: FleetView,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub station: Option<StationId>,
}

/// The answer of `fleet.view`: `items` is the JSON of the view (`me`: an
/// object; the others: an array of the item structs below).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetViewPage {
    pub items: serde_json::Value,
    pub next_cursor: Option<String>,
}

// ---------------------------------------------------------------------------
// Limits (design §2.3, "Common").
// ---------------------------------------------------------------------------

/// The most characters of a station name and of a station name in an item.
pub const NAME_MAX_CHARS: usize = 128;
/// The most characters of a session title.
pub const TITLE_MAX_CHARS: usize = 200;
/// The most characters of a callsign.
pub const CALLSIGN_MAX_CHARS: usize = 64;
/// The most characters of a model.
pub const MODEL_MAX_CHARS: usize = 64;
/// The most characters of a mode.
pub const MODE_MAX_CHARS: usize = 32;
/// The most characters of a project name.
pub const PROJECT_NAME_MAX_CHARS: usize = 128;
/// The most characters of a provider `instance`.
pub const PROVIDER_INSTANCE_MAX_CHARS: usize = 64;
/// The most characters of a provider kind name.
pub const PROVIDER_MAX_CHARS: usize = 32;
/// The most characters of a provider `version`.
pub const PROVIDER_VERSION_MAX_CHARS: usize = 64;
/// The most characters of `platform`, `arch` and `desktop`.
pub const PLATFORM_MAX_CHARS: usize = 32;
/// The most bytes of an ID. An item with a longer ID is left out.
pub const ID_MAX_BYTES: usize = 128;

/// The most providers of a station item.
pub const PROVIDERS_MAX: usize = 16;
/// The most classes of a grant item.
pub const GRANT_CLASSES_MAX: usize = 16;
/// The most station names of a grant item.
pub const GRANT_STATIONS_MAX: usize = 64;
/// The most `max_modes` entries of a grant item.
pub const MAX_MODES_MAX: usize = 8;

/// The items of a `stations` page.
pub const STATIONS_PAGE: usize = 32;
/// The items of a `projects` page.
pub const PROJECTS_PAGE: usize = 100;
/// The items of a `sessions` page.
pub const SESSIONS_PAGE: usize = 100;

/// The most bytes of a whole response `{"items":…,"next_cursor":…}`, over
/// the escaped, serialized JSON.
pub const PAGE_MAX_BYTES: usize = 192 * 1024;
/// The most serialized bytes of one item after the cuts; a larger item is
/// left out.
pub const ITEM_MAX_BYTES: usize = 8 * 1024;
/// The most bytes of a cursor.
pub const CURSOR_MAX_BYTES: usize = 4096;

/// The ellipsis that ends a cut text.
pub const ELLIPSIS: char = '…';

/// `text` cut to at most `max` characters; a cut text ends with `…` (the
/// ellipsis is one of the `max` characters).
#[must_use]
pub fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push(ELLIPSIS);
    out
}

// ---------------------------------------------------------------------------
// The items (design §2.3).
// ---------------------------------------------------------------------------

/// One provider instance of a station item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderItem {
    pub instance: String,
    pub provider: ProviderKind,
    pub version: Option<String>,
    pub signed_in: bool,
}

/// One item of the `stations` view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StationItem {
    pub station_id: StationId,
    pub name: String,
    pub online: bool,
    /// `"omarchy"`, `"macos"`, the `traits.os`, or `null` without traits.
    pub platform: Option<String>,
    pub arch: Option<String>,
    /// `"omarchy"`, `"hyprland"` or `null`.
    pub desktop: Option<String>,
    /// At most [`PROVIDERS_MAX`], in instance order.
    pub providers: Vec<ProviderItem>,
    pub providers_more: bool,
    pub live_sessions: u64,
    pub max_sessions: Option<u32>,
    pub project_count: u32,
}

/// One item of the `projects` view: no path, forge, tracker or prompt field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectItem {
    pub project_id: String,
    pub name: String,
    pub station_id: StationId,
    /// The name of the project's own station.
    pub station: String,
}

/// One item of the `sessions` view: nothing else than these keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionItem {
    pub station_id: StationId,
    pub station: String,
    pub session_id: SessionId,
    pub callsign: String,
    pub title: Option<String>,
    pub provider: ProviderKind,
    pub status: CardStatus,
}

/// The in-effect grant of the calling session, in the `me` view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeGrant {
    pub grant_id: String,
    /// The class names in `snake_case`, at most [`GRANT_CLASSES_MAX`].
    pub classes: Vec<String>,
    /// The names of the stations of the grant, at most [`GRANT_STATIONS_MAX`].
    pub stations: Vec<String>,
    pub scope: GrantScope,
    pub spawn_cap: u32,
    pub spawn_used: u32,
    /// At most [`MAX_MODES_MAX`] entries.
    pub max_modes: BTreeMap<String, String>,
    /// Unix ms.
    pub expires_at: i64,
}

/// The `me` view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeItem {
    pub station_id: StationId,
    pub station: String,
    pub session_id: SessionId,
    pub callsign: String,
    pub title: Option<String>,
    pub provider: ProviderKind,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub grant: Option<MeGrant>,
}

// ---------------------------------------------------------------------------
// The cursor (design §2.3, "Common").
// ---------------------------------------------------------------------------

/// The parts of the cursor key of a view, in order, with the most characters
/// of each: stations `(name, station_id)`, projects `(name, project_id)`,
/// sessions `(station name, callsign, session_id)`. `me` has no cursor.
#[must_use]
pub const fn cursor_parts(view: FleetView) -> &'static [usize] {
    match view {
        FleetView::Stations => &[NAME_MAX_CHARS, ID_MAX_BYTES],
        FleetView::Projects => &[PROJECT_NAME_MAX_CHARS, ID_MAX_BYTES],
        FleetView::Sessions => &[NAME_MAX_CHARS, CALLSIGN_MAX_CHARS, ID_MAX_BYTES],
        FleetView::Me => &[],
    }
}

/// A cursor that is not valid for its view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the cursor is not valid")]
pub struct CursorInvalid;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url without padding.
fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            let index = (n >> (18 - 6 * i)) & 0x3f;
            out.push(char::from(ALPHABET[index as usize]));
        }
    }
    out
}

/// The inverse of [`b64_encode`]; `None` for any other text (also for a
/// text with padding, a wrong alphabet or non-canonical trailing bits).
fn b64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 2);
    for chunk in bytes.chunks(4) {
        let mut n = 0_u32;
        for (i, c) in chunk.iter().enumerate() {
            let v = ALPHABET.iter().position(|a| a == c)?;
            n |= u32::try_from(v).ok()? << (18 - 6 * i);
        }
        let count = chunk.len() - 1;
        for i in 0..count {
            out.push(u8::try_from((n >> (16 - 8 * i)) & 0xff).ok()?);
        }
    }
    (b64_encode(&out) == text).then_some(out)
}

/// The cursor of a key: base64url (no padding) of a JSON array of the key
/// parts as strings, in the sort order of the view.
#[must_use]
pub fn encode_cursor(parts: &[String]) -> String {
    let json = serde_json::to_string(parts).unwrap_or_else(|_| "[]".to_owned());
    b64_encode(json.as_bytes())
}

/// The key parts of `cursor` for `view`.
///
/// # Errors
/// [`CursorInvalid`] when the cursor is longer than [`CURSOR_MAX_BYTES`],
/// is not base64url, is not JSON, is not an array of strings, does not have
/// exactly the parts of the view, or has a part above its limit in characters.
pub fn decode_cursor(view: FleetView, cursor: &str) -> Result<Vec<String>, CursorInvalid> {
    if cursor.len() > CURSOR_MAX_BYTES {
        return Err(CursorInvalid);
    }
    let limits = cursor_parts(view);
    if limits.is_empty() {
        return Err(CursorInvalid);
    }
    let bytes = b64_decode(cursor).ok_or(CursorInvalid)?;
    let parts: Vec<String> = serde_json::from_slice(&bytes).map_err(|_| CursorInvalid)?;
    let within = |p: &String, max: &usize| p.chars().count() <= *max;
    if parts.len() != limits.len() || !parts.iter().zip(limits).all(|(p, l)| within(p, l)) {
        return Err(CursorInvalid);
    }
    Ok(parts)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_round_trips_every_length_of_text() {
        for n in 0..12 {
            let text = "x".repeat(n);
            let encoded = b64_encode(text.as_bytes());
            assert_eq!(b64_decode(&encoded).unwrap(), text.as_bytes(), "{n}");
        }
    }

    #[test]
    fn a_cursor_of_the_longest_valid_key_fits_the_limit() {
        let name = "\u{1F600}".repeat(NAME_MAX_CHARS);
        let callsign = "\u{1F600}".repeat(CALLSIGN_MAX_CHARS);
        let id = "i".repeat(ID_MAX_BYTES);
        let key = vec![name, callsign, id];
        let cursor = encode_cursor(&key);
        assert!(cursor.len() <= CURSOR_MAX_BYTES, "{}", cursor.len());
        assert_eq!(decode_cursor(FleetView::Sessions, &cursor).unwrap(), key);
    }

    #[test]
    fn a_cursor_with_the_wrong_shape_is_refused() {
        let good = encode_cursor(&["a".to_owned(), "b".to_owned()]);
        assert!(decode_cursor(FleetView::Stations, &good).is_ok());
        // Wrong view (three parts), padding, bad alphabet, bad length.
        assert!(decode_cursor(FleetView::Sessions, &good).is_err());
        assert!(decode_cursor(FleetView::Me, &good).is_err());
        assert!(decode_cursor(FleetView::Stations, &format!("{good}=")).is_err());
        assert!(decode_cursor(FleetView::Stations, "a+b/").is_err());
        assert!(decode_cursor(FleetView::Stations, "A").is_err());
        assert!(decode_cursor(FleetView::Stations, "").is_err());
        // Not JSON, not an array of strings.
        for bad in [
            &b"\xff\xfe"[..],
            b"a\nb",
            b"[1,2]",
            b"{\"a\":\"b\"}",
            b"[\"a\",\"b\"] x",
        ] {
            assert!(decode_cursor(FleetView::Stations, &b64_encode(bad)).is_err());
        }
        // A part above its limit.
        let long = encode_cursor(&["n".repeat(NAME_MAX_CHARS + 1), "i".to_owned()]);
        assert!(decode_cursor(FleetView::Stations, &long).is_err());
        let long_id = encode_cursor(&["n".to_owned(), "i".repeat(ID_MAX_BYTES + 1)]);
        assert!(decode_cursor(FleetView::Stations, &long_id).is_err());
        // A cursor above the byte limit.
        let huge = "A".repeat(CURSOR_MAX_BYTES + 4);
        assert!(decode_cursor(FleetView::Stations, &huge).is_err());
    }

    #[test]
    fn a_text_is_cut_to_the_limit_and_ends_with_an_ellipsis() {
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("abcd", 3), "ab…");
        assert_eq!(cut(&"\u{1F600}".repeat(5), 4).chars().count(), 4);
    }

    #[test]
    fn a_request_refuses_unknown_keys_and_unknown_views() {
        let ok = serde_json::json!({"caller": "s1", "view": "projects", "station": "st"});
        let req: FleetViewRequest = serde_json::from_value(ok).unwrap();
        assert_eq!(req.view, FleetView::Projects);
        for bad in [
            serde_json::json!({"caller": "s1", "view": "stations", "x": 1}),
            serde_json::json!({"caller": "s1", "view": "everything"}),
            serde_json::json!({"view": "me"}),
        ] {
            assert!(serde_json::from_value::<FleetViewRequest>(bad).is_err());
        }
    }
}
