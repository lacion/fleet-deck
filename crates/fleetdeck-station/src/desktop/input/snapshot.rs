//! The bounded compositor snapshot of an input operation (R13 P7 §7.3 steps 5
//! and 13): the clients with their geometry, the monitors, the layer
//! surfaces (for a pointer movement) and the lock state. Every parser is
//! strict: a missing or malformed field makes the whole snapshot unreadable,
//! and the caller fails closed (§5.3, §16 item 24).

use serde_json::Value;

use crate::desktop::Fail;
use crate::desktop::info;
use crate::desktop::run::{CALL_TIMEOUT, Runner};

/// A rectangle in logical compositor coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rect {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

impl Rect {
    /// Whether the point is inside; the right and bottom edges are outside.
    pub(crate) fn contains(&self, x: i64, y: i64) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.w)
            && y < self.y.saturating_add(self.h)
    }

    /// Whether the two rectangles share an area.
    pub(crate) fn intersects(&self, other: &Self) -> bool {
        self.w > 0
            && self.h > 0
            && other.w > 0
            && other.h > 0
            && self.x < other.x.saturating_add(other.w)
            && other.x < self.x.saturating_add(self.w)
            && self.y < other.y.saturating_add(other.h)
            && other.y < self.y.saturating_add(self.h)
    }
}

/// A window of `hyprctl -j clients`.
#[derive(Debug, Clone)]
pub(crate) struct Client {
    /// Lower case with the `0x` prefix.
    pub address: String,
    pub class: String,
    pub initial_class: String,
    pub title: String,
    pub initial_title: String,
    pub mapped: bool,
    pub hidden: bool,
    pub rect: Rect,
    pub workspace: i64,
    /// `focusHistoryID` is 0 (R13 P7b §6). A missing field is not focused.
    pub focused: bool,
}

/// A monitor of `hyprctl -j monitors`, with its logical rectangle.
#[derive(Debug, Clone)]
pub(crate) struct Monitor {
    pub name: String,
    pub rect: Rect,
    pub disabled: bool,
    pub workspace: Option<i64>,
    /// The id of the special workspace that the monitor shows now, if any
    /// (R13 P7b §6). A negative number.
    pub special: Option<i64>,
}

/// What one snapshot read.
#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    pub clients: Vec<Client>,
    pub monitors: Vec<Monitor>,
    /// The layer surfaces; empty unless the snapshot asked for them.
    pub layers: Vec<Rect>,
    /// `None` when the lock state is unknown.
    pub locked: Option<bool>,
}

/// Why a snapshot could not be taken.
#[derive(Debug)]
pub(crate) enum TakeError {
    /// A program failed or timed out.
    Run(Fail),
    /// An answer is missing a field or has a wrong type.
    Malformed,
}

fn int(v: &Value, key: &str) -> Option<i64> {
    v.get(key)?.as_i64()
}

fn text(v: &Value, key: &str) -> Option<String> {
    Some(v.get(key)?.as_str()?.to_owned())
}

/// A string that may be absent, but not of another type.
fn text_or_empty(v: &Value, key: &str) -> Option<String> {
    match v.get(key) {
        None => Some(String::new()),
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => None,
    }
}

fn pair(v: &Value, key: &str) -> Option<(i64, i64)> {
    let list = v.get(key)?.as_array()?;
    match list.as_slice() {
        [a, b] => Some((a.as_i64()?, b.as_i64()?)),
        _ => None,
    }
}

fn client_of(v: &Value) -> Option<Client> {
    let (x, y) = pair(v, "at")?;
    let (w, h) = pair(v, "size")?;
    Some(Client {
        address: text(v, "address")?.to_ascii_lowercase(),
        class: text(v, "class")?,
        initial_class: text_or_empty(v, "initialClass")?,
        title: text_or_empty(v, "title")?,
        initial_title: text_or_empty(v, "initialTitle")?,
        mapped: v.get("mapped")?.as_bool()?,
        hidden: v.get("hidden")?.as_bool()?,
        rect: Rect { x, y, w, h },
        workspace: int(v.get("workspace")?, "id")?,
        focused: v.get("focusHistoryID").and_then(Value::as_i64) == Some(0),
    })
}

/// The clients of `hyprctl -j clients`; `None` when any entry is malformed.
pub(crate) fn parse_clients(out: &str) -> Option<Vec<Client>> {
    let list: Value = serde_json::from_str(out).ok()?;
    list.as_array()?.iter().map(client_of).collect()
}

/// The logical size of a monitor: its mode with the sides swapped for an odd
/// transform, divided by the scale.
#[allow(clippy::cast_possible_truncation)]
fn logical_size(width: i64, height: i64, scale: f64, transform: i64) -> Option<(i64, i64)> {
    if !(scale.is_finite() && scale > 0.0) || width <= 0 || height <= 0 {
        return None;
    }
    let (w, h) = if transform % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    };
    // The values are small positive numbers.
    #[allow(clippy::cast_precision_loss)]
    let (fw, fh) = (w as f64, h as f64);
    Some(((fw / scale).round() as i64, (fh / scale).round() as i64))
}

fn monitor_of(v: &Value) -> Option<Monitor> {
    let scale = v.get("scale")?.as_f64()?;
    let transform = match v.get("transform") {
        None => 0,
        Some(t) => t.as_i64()?,
    };
    let disabled = match v.get("disabled") {
        None => false,
        Some(d) => d.as_bool()?,
    };
    let (w, h) = logical_size(int(v, "width")?, int(v, "height")?, scale, transform)?;
    Some(Monitor {
        name: text(v, "name")?,
        rect: Rect {
            x: int(v, "x")?,
            y: int(v, "y")?,
            w,
            h,
        },
        disabled,
        workspace: v.get("activeWorkspace").and_then(|a| int(a, "id")),
        // `specialWorkspace` is `{id: 0, name: ""}` while none is shown.
        special: v
            .get("specialWorkspace")
            .and_then(|a| int(a, "id"))
            .filter(|id| *id < 0),
    })
}

/// The monitors of `hyprctl -j monitors`; `None` when any entry is malformed.
pub(crate) fn parse_monitors(out: &str) -> Option<Vec<Monitor>> {
    let list: Value = serde_json::from_str(out).ok()?;
    list.as_array()?.iter().map(monitor_of).collect()
}

fn layer_of(v: &Value) -> Option<Rect> {
    Some(Rect {
        x: int(v, "x")?,
        y: int(v, "y")?,
        w: int(v, "w")?,
        h: int(v, "h")?,
    })
}

/// The layer levels above normal windows (`top` 2 and `overlay` 3). The
/// `background` 0 and `bottom` 1 levels lie below every window: a wallpaper
/// covers the whole monitor there and never takes the pointer from a preview.
const ABOVE_WINDOWS: [&str; 2] = ["2", "3"];

/// The layer surfaces above windows of `hyprctl -j layers`: `{monitor:
/// {levels: {n: [surface…]}}}`. Every level is checked for its form; only the
/// `top` and `overlay` surfaces are returned. `None` when any part is
/// malformed.
pub(crate) fn parse_layers(out: &str) -> Option<Vec<Rect>> {
    let root: Value = serde_json::from_str(out).ok()?;
    let mut rects = Vec::new();
    for monitor in root.as_object()?.values() {
        for (level, surfaces) in monitor.get("levels")?.as_object()? {
            for surface in surfaces.as_array()? {
                let rect = layer_of(surface)?;
                if ABOVE_WINDOWS.contains(&level.as_str()) {
                    rects.push(rect);
                }
            }
        }
    }
    Some(rects)
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

/// Reads the clients, the monitors, the layers when `layers` is true, and the
/// lock state, one program at a time.
///
/// # Errors
/// [`TakeError::Run`] when a program fails; [`TakeError::Malformed`] when an
/// answer cannot be read.
pub(crate) async fn take(runner: &Runner<'_>, layers: bool) -> Result<Snapshot, TakeError> {
    let read = |what: &'static str| async move {
        runner
            .run_ok("hyprctl", &argv(&["-j", what]), CALL_TIMEOUT)
            .await
            .map(|out| out.stdout)
            .map_err(TakeError::Run)
    };
    let clients = parse_clients(&read("clients").await?).ok_or(TakeError::Malformed)?;
    let monitors = parse_monitors(&read("monitors").await?).ok_or(TakeError::Malformed)?;
    let layers = if layers {
        parse_layers(&read("layers").await?).ok_or(TakeError::Malformed)?
    } else {
        Vec::new()
    };
    let locked = info::locked(runner).await;
    Ok(Snapshot {
        clients,
        monitors,
        layers,
        locked,
    })
}

impl Snapshot {
    /// The client at `address` with `class`.
    pub(crate) fn client(&self, address: &str, class: &str) -> Option<&Client> {
        self.clients
            .iter()
            .find(|c| c.address == address && c.class == class)
    }

    /// Whether a monitor shows the workspace of `client` now: its active
    /// workspace for an ordinary one, its special workspace for a negative id.
    pub(crate) fn shown(&self, client: &Client) -> bool {
        self.monitors.iter().any(|m| {
            !m.disabled
                && if client.workspace >= 0 {
                    m.workspace == Some(client.workspace)
                } else {
                    m.special == Some(client.workspace)
                }
        })
    }

    /// Whether `client` is mapped, not hidden, and on a workspace that a
    /// monitor shows now (R13 P7b §6, §8).
    pub(crate) fn eligible_place(&self, client: &Client) -> bool {
        client.mapped && !client.hidden && self.shown(client)
    }

    /// The enabled monitors that the window of `client` covers. Empty when
    /// the window is on none of them.
    pub(crate) fn covered(&self, client: &Client) -> Vec<&Monitor> {
        self.monitors
            .iter()
            .filter(|m| !m.disabled && m.rect.intersects(&client.rect))
            .collect()
    }

    /// The global point of the window-relative point `(x, y)` of a pointer
    /// movement (§5.3), or `None` when the point is not available: the preview
    /// is not mapped or is hidden, the point is outside it (no clamping), the
    /// arithmetic overflows, no enabled monitor holds the point, a different
    /// mapped client covers it on the shown workspace of its monitor or on a
    /// special workspace, or any layer surface covers it.
    pub(crate) fn destination(&self, client: &Client, x: u32, y: u32) -> Option<(i64, i64)> {
        if !client.mapped || client.hidden {
            return None;
        }
        let (dx, dy) = (i64::from(x), i64::from(y));
        if dx >= client.rect.w || dy >= client.rect.h {
            return None;
        }
        let gx = client.rect.x.checked_add(dx)?;
        let gy = client.rect.y.checked_add(dy)?;
        let monitor = self
            .monitors
            .iter()
            .find(|m| !m.disabled && m.rect.contains(gx, gy))?;
        let covered_by_client = self.clients.iter().any(|other| {
            other.address != client.address
                && other.mapped
                && !other.hidden
                && other.rect.contains(gx, gy)
                && (other.workspace < 0 || Some(other.workspace) == monitor.workspace)
        });
        if covered_by_client || self.layers.iter().any(|l| l.contains(gx, gy)) {
            return None;
        }
        Some((gx, gy))
    }
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
