//! The active-window observation of an input operation (R13 P7 §7.3 steps 15
//! and 16, §7.5): `hyprctl -j activewindow` within its own time limit, a
//! strict parser, and the comparison with the recorded preview. A failure to
//! read or to parse is never a pass.

use std::time::Duration;

use serde_json::Value;

use super::barrier::{self, Names};
use crate::desktop::events::Record;
use crate::desktop::run::{CALL_TIMEOUT, Runner};

/// The active window. An empty address means that no window is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Active {
    pub address: String,
    pub class: String,
    pub initial_class: String,
    pub title: String,
    pub initial_title: String,
}

/// What an observation of the active window means for a recorded preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The active window is the recorded preview.
    Same,
    /// Another window (or none) is active. Nothing says what it is.
    Foreign,
    /// The recorded preview is active and now looks like a board window.
    Board,
}

/// Why the active window could not be observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unreadable {
    /// The program failed or passed its limit.
    Run,
    /// The answer is not what `activewindow -j` prints.
    Malformed,
}

fn string(v: &Value, key: &str) -> Option<String> {
    match v.get(key) {
        None => Some(String::new()),
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => None,
    }
}

/// Parses the answer of `hyprctl -j activewindow`. `{}` and `null` mean that
/// no window is active.
#[must_use]
pub(crate) fn parse_active(out: &str) -> Option<Active> {
    let v: Value = serde_json::from_str(out).ok()?;
    if v.is_null() || v.as_object().is_some_and(serde_json::Map::is_empty) {
        return Some(Active {
            address: String::new(),
            class: String::new(),
            initial_class: String::new(),
            title: String::new(),
            initial_title: String::new(),
        });
    }
    v.as_object()?;
    let address = v.get("address")?.as_str()?.to_ascii_lowercase();
    if address.is_empty() {
        return None;
    }
    Some(Active {
        address,
        class: v.get("class")?.as_str()?.to_owned(),
        initial_class: string(&v, "initialClass")?,
        title: string(&v, "title")?,
        initial_title: string(&v, "initialTitle")?,
    })
}

/// Reads the active window with a total limit of `limit` (the spawn counts).
///
/// # Errors
/// [`Unreadable`].
pub(crate) async fn read_active(
    runner: &Runner<'_>,
    limit: Duration,
) -> Result<Active, Unreadable> {
    let args = vec!["-j".to_owned(), "activewindow".to_owned()];
    let run = tokio::time::timeout(limit, runner.run_ok("hyprctl", &args, CALL_TIMEOUT)).await;
    let out = match run {
        Ok(Ok(out)) => out,
        Ok(Err(_)) | Err(_) => return Err(Unreadable::Run),
    };
    parse_active(&out.stdout).ok_or(Unreadable::Malformed)
}

/// Compares an observation with the recorded preview (§7.3 step 16): the same
/// address and class, then the board barrier on the names of the active
/// window. An observation of another window never says whether it was a
/// board (§7.2).
#[must_use]
pub(crate) fn verdict(active: &Active, record: &Record, deck_hosts: &[String]) -> Verdict {
    if active.address != record.address || active.class != record.class {
        return Verdict::Foreign;
    }
    let names = Names {
        class: &active.class,
        initial_class: &active.initial_class,
        title: &active.title,
        initial_title: &active.initial_title,
    };
    if barrier::is_board(record.kind, &names, deck_hosts) {
        Verdict::Board
    } else {
        Verdict::Same
    }
}

#[cfg(test)]
#[path = "focus_tests.rs"]
mod tests;
