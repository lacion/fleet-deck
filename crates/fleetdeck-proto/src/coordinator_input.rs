//! The arguments of the P7 coordinator input tools `key_press`, `type_text`
//! and `pointer_move` (feature `desktop_input_mcp.v1`;
//! `docs/v2/design/omarchy-p7.md` §4, §5.1 to §5.3), the arguments of the
//! P7b tools `pointer_click` and `window_list` (features
//! `desktop_click_mcp.v1` and `desktop_input_any.v1`;
//! `docs/v2/design/omarchy-p7b.md` §5), and the `window_list` answer (§6).
//!
//! Every struct refuses unknown fields, and an explicit `null` is not an
//! omission (§4): `station` is either absent or a string. The `validate`
//! methods check the schema bundle of §4 and the grammar of §5; they name the
//! field that failed and never show its value. [`crate::coordinator_desktop`]
//! re-exports the structs, and its `DesktopCall::validate` calls these.

use serde::{Deserialize, Deserializer, Serialize};
use ts_rs::TS;

use crate::coordinator_desktop::{ArgError, bad, check_request, check_station};

/// §4 and §5.2: the most Unicode scalars of `text`.
pub const TEXT_MAX_SCALARS: usize = 2000;
/// §4: the most UTF-8 bytes of `text`.
pub const TEXT_MAX_BYTES: usize = 8000;
/// §4 and §5.3: the highest pointer coordinate.
pub const POINTER_MAX: u32 = 32_767;
/// §4: the length of a `window_id` (32 lowercase hexadecimal digits).
pub const WINDOW_ID_LEN: usize = 32;

/// `station` is either absent or a string: a `null` is refused (§4).
fn station_not_null<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

/// `key_press` arguments (§4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct KeyPressArguments {
    pub request_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "station_not_null"
    )]
    #[ts(optional)]
    pub station: Option<String>,
    pub window_id: String,
    pub chord: String,
}

/// `type_text` arguments (§4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct TypeTextArguments {
    pub request_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "station_not_null"
    )]
    #[ts(optional)]
    pub station: Option<String>,
    pub window_id: String,
    pub text: String,
}

/// `pointer_move` arguments (§4). `x` and `y` are logical coordinates
/// relative to the top-left of the preview (§5.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct PointerMoveArguments {
    pub request_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "station_not_null"
    )]
    #[ts(optional)]
    pub station: Option<String>,
    pub window_id: String,
    pub x: u32,
    pub y: u32,
}

/// R13 P7b §5.1: `pointer_click` arguments. The same fields as
/// [`PointerMoveArguments`]; there is no button argument (one left click).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct PointerClickArguments {
    pub request_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "station_not_null"
    )]
    #[ts(optional)]
    pub station: Option<String>,
    pub window_id: String,
    pub x: u32,
    pub y: u32,
}

/// R13 P7b §5.2: `window_list` arguments. There is no title filter, address
/// selector, scope override or pagination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct WindowListArguments {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "station_not_null"
    )]
    #[ts(optional)]
    pub station: Option<String>,
}

/// `^[0-9a-f]{32}$`.
fn window_id_ok(text: &str) -> bool {
    text.len() == WINDOW_ID_LEN
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn check_window(text: &str) -> Result<(), ArgError> {
    if window_id_ok(text) {
        Ok(())
    } else {
        Err(bad("window_id"))
    }
}

/// The named keys of §5.1 (the schema's key set is exhaustive).
const NAMED_KEYS: [&str; 15] = [
    "Return",
    "Tab",
    "Escape",
    "BackSpace",
    "Delete",
    "Insert",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Left",
    "Right",
    "Up",
    "Down",
    "Space",
];

/// The chord grammar of §4 and §5.1:
/// `^(Ctrl\+)?(Shift\+)?([a-z0-9]|<named key>)$`, 1 to 32 scalars. The
/// modifiers are `Ctrl` and `Shift`, each at most once and in that order;
/// they cannot appear alone.
fn chord_ok(text: &str) -> bool {
    if !(1..=32).contains(&text.chars().count()) {
        return false;
    }
    let rest = text.strip_prefix("Ctrl+").unwrap_or(text);
    let key = rest.strip_prefix("Shift+").unwrap_or(rest);
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => c.is_ascii_lowercase() || c.is_ascii_digit(),
        _ => NAMED_KEYS.contains(&key),
    }
}

/// `text` of §4 and §5.2: 1 to 2,000 Unicode scalars, at most 8,000 UTF-8
/// bytes, and none of the C0 controls U+0000 to U+001F, DEL and the C1
/// controls U+007F to U+009F, U+2028 and U+2029.
fn text_ok(text: &str) -> bool {
    (1..=TEXT_MAX_SCALARS).contains(&text.chars().count())
        && text.len() <= TEXT_MAX_BYTES
        && !text.chars().any(|c| {
            matches!(c, '\u{0000}'..='\u{001F}' | '\u{007F}'..='\u{009F}' | '\u{2028}' | '\u{2029}')
        })
}

impl KeyPressArguments {
    /// Checks the patterns and lengths of §4 and the chord grammar of §5.1.
    ///
    /// # Errors
    /// [`ArgError`] with the name of the first field that fails.
    pub fn validate(&self) -> Result<(), ArgError> {
        check_request(&self.request_id)?;
        check_station(self.station.as_ref())?;
        check_window(&self.window_id)?;
        if chord_ok(&self.chord) {
            Ok(())
        } else {
            Err(bad("chord"))
        }
    }
}

impl TypeTextArguments {
    /// Checks the patterns and lengths of §4 and the text rules of §5.2.
    ///
    /// # Errors
    /// [`ArgError`] with the name of the first field that fails.
    pub fn validate(&self) -> Result<(), ArgError> {
        check_request(&self.request_id)?;
        check_station(self.station.as_ref())?;
        check_window(&self.window_id)?;
        if text_ok(&self.text) {
            Ok(())
        } else {
            Err(bad("text"))
        }
    }
}

impl PointerMoveArguments {
    /// Checks the patterns and the coordinate range of §4 (0 to 32,767).
    ///
    /// # Errors
    /// [`ArgError`] with the name of the first field that fails.
    pub fn validate(&self) -> Result<(), ArgError> {
        check_request(&self.request_id)?;
        check_station(self.station.as_ref())?;
        check_window(&self.window_id)?;
        if self.x > POINTER_MAX {
            return Err(bad("x"));
        }
        if self.y > POINTER_MAX {
            return Err(bad("y"));
        }
        Ok(())
    }
}

impl PointerClickArguments {
    /// Checks the patterns and the coordinate range of §5.1 (0 to 32,767).
    ///
    /// # Errors
    /// [`ArgError`] with the name of the first field that fails.
    pub fn validate(&self) -> Result<(), ArgError> {
        check_request(&self.request_id)?;
        check_station(self.station.as_ref())?;
        check_window(&self.window_id)?;
        if self.x > POINTER_MAX {
            return Err(bad("x"));
        }
        if self.y > POINTER_MAX {
            return Err(bad("y"));
        }
        Ok(())
    }
}

impl WindowListArguments {
    /// Checks the station of §5.2.
    ///
    /// # Errors
    /// [`ArgError`] with the name of the field that fails.
    pub fn validate(&self) -> Result<(), ArgError> {
        check_station(self.station.as_ref())
    }
}

/// One monitor of an [`InputWindowList`] (R13 P7b §6): the global logical
/// geometry that relates a window to a monitor screenshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct InputMonitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The global logical rectangle of a listed window (R13 P7b §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct InputRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// One listed window (R13 P7b §6): an opaque handle and geometry. It has no
/// title, class, URL, PID or native address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct InputWindow {
    pub window_id: String,
    pub rect: InputRect,
    /// The names of the monitors that the window overlaps.
    pub monitors: Vec<String>,
    pub focused: bool,
    #[ts(type = "number")]
    pub expires_in_ms: u64,
}

/// The closed `window_list` projection (R13 P7b §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct InputWindowList {
    pub station: String,
    pub monitors: Vec<InputMonitor>,
    pub windows: Vec<InputWindow>,
}

/// An [`InputWindowList`] that does not meet a limit of R13 P7b §6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowListError {
    /// More than 64 windows or monitors, or more than 16 KiB serialized: the
    /// code `desktop_result_too_large`.
    TooLarge,
    /// A zero width or height, a `window_id` that is not of the P7 form, or a
    /// monitor name of a window that is not in the list of monitors.
    Invalid,
}

impl WindowListError {
    /// The error code of the failure: `desktop_result_too_large` for
    /// [`Self::TooLarge`], `desktop_failed` for [`Self::Invalid`].
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::TooLarge => crate::coordinator_desktop_text::code::DESKTOP_RESULT_TOO_LARGE,
            Self::Invalid => crate::coordinator_desktop_text::code::DESKTOP_FAILED,
        }
    }
}

impl InputWindowList {
    /// Checks the limits of R13 P7b §6: at most 64 windows and 64 monitors,
    /// positive width and height, a `window_id` of the P7 form, and at most
    /// 16 KiB serialized.
    ///
    /// # Errors
    /// [`WindowListError::TooLarge`] for a count or size over the limit;
    /// [`WindowListError::Invalid`] for a zero dimension, a bad handle or an
    /// unknown monitor name of a window.
    pub fn validate(&self) -> Result<(), WindowListError> {
        use crate::coordinator_desktop_text::limits;
        if self.windows.len() > limits::WINDOW_LIST_MAX_WINDOWS as usize
            || self.monitors.len() > limits::WINDOW_LIST_MAX_MONITORS as usize
        {
            return Err(WindowListError::TooLarge);
        }
        let size = serde_json::to_vec(self).map_or(u64::MAX, |b| b.len() as u64);
        if size > limits::WINDOW_LIST_MAX_BYTES {
            return Err(WindowListError::TooLarge);
        }
        if self.monitors.iter().any(|m| m.width == 0 || m.height == 0)
            || self
                .windows
                .iter()
                .any(|w| w.rect.width == 0 || w.rect.height == 0 || !window_id_ok(&w.window_id))
            || self.windows.iter().any(|w| {
                w.monitors
                    .iter()
                    .any(|name| !self.monitors.iter().any(|m| m.name == *name))
            })
        {
            return Err(WindowListError::Invalid);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "coordinator_input_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "coordinator_input_p7b_tests.rs"]
mod p7b_tests;
