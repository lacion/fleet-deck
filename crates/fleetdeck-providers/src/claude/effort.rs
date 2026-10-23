//! The reasoning effort of a Claude session from the `get_settings` control
//! request (B18, docs/v2/design/effort.md §4.3).
//!
//! The CLI answers `get_settings` with `{effective, sources, applied}`. The
//! `effective` and `sources` parts hold the whole settings of the user, with
//! the `env` table (credentials included). The core reads `applied.effort`
//! only, passes it through [`effort_of`], and keeps nothing else; the raw-frame
//! log gets the answer through [`raw_view`], with `applied` only.
//!
//! The ask follows the shape of `get_usage` (`usage.rs`): one request open at
//! a time, and the first error or odd answer stops the asks of the process.
//! The open request is the field `settings_open`, not a `host_requests`
//! entry: an older build must decode the checkpoint (§4.3.1).

use std::borrow::Cow;
use std::time::{Duration, Instant};

use fleetdeck_proto::Event;
use fleetdeck_proto::effort::effort_of;
use serde_json::{Value, json};

use super::core::{ClaudeCore, Output};
use super::{control, frames::ControlResponse};

/// The id prefix of the host's `get_settings` requests.
pub const SETTINGS_ID_PREFIX: &str = "fd-settings-";
/// After this time an unanswered request no longer blocks a new one.
const SETTINGS_STALE: Duration = Duration::from_secs(60);

impl ClaudeCore {
    /// The `get_settings` write, when one is due: not stopped, the session
    /// started, and no live request open. A live request open makes the next
    /// ask wait for its answer.
    pub(super) fn settings_request(&mut self) -> Vec<Output> {
        if self.settings_stopped || !self.started {
            return Vec::new();
        }
        if self.settings_open.is_some()
            && self
                .settings_asked
                .is_some_and(|at| at.elapsed() < SETTINGS_STALE)
        {
            self.settings_again = true;
            return Vec::new();
        }
        // None open, or a stale one (its late answer has another `N`).
        self.settings_seq += 1;
        self.settings_open = Some(self.settings_seq);
        self.settings_asked = Some(Instant::now());
        self.settings_again = false;
        let id = format!("{SETTINGS_ID_PREFIX}{}", self.settings_seq);
        vec![Output::Write(control::get_settings(&id))]
    }
}

/// The `N` of an `fd-settings-N` request id.
fn settings_seq_of(request_id: &str) -> Option<u64> {
    request_id.strip_prefix(SETTINGS_ID_PREFIX)?.parse().ok()
}

/// The answer to a `get_settings` request (its id starts with
/// [`SETTINGS_ID_PREFIX`]).
pub(super) fn on_response(core: &mut ClaudeCore, resp: &ControlResponse) -> Vec<Output> {
    if settings_seq_of(&resp.request_id) != core.settings_open {
        // The late answer of a replaced request.
        return Vec::new();
    }
    core.settings_open = None;
    core.settings_asked = None;
    let applied = resp
        .response
        .get("applied")
        .filter(|a| resp.ok && a.is_object());
    let Some(applied) = applied else {
        core.settings_stopped = true;
        // The error text only: never `resp.response` (it holds the settings).
        tracing::debug!(
            error = resp.error.as_deref().unwrap_or_default(),
            "claude get_settings not supported: no more asks in this process"
        );
        return Vec::new();
    };
    let effort = effort_of(applied.get("effort").and_then(Value::as_str));
    let mut out = Vec::new();
    if effort != core.effort {
        core.effort.clone_from(&effort);
        out.push(Output::Emit(Event::EffortChanged { effort }));
    }
    if std::mem::take(&mut core.settings_again) {
        out.extend(core.settings_request());
    }
    out
}

/// The frame as the raw-frame log gets it. The answer of a `get_settings`
/// request keeps `applied` only (`effective` and `sources` hold the settings
/// `env` values, credentials included); every other frame is unchanged.
#[must_use]
pub fn raw_view(frame: &Value) -> Cow<'_, Value> {
    let is_settings_answer = frame["type"] == "control_response"
        && frame["response"]["request_id"]
            .as_str()
            .is_some_and(|id| id.starts_with(SETTINGS_ID_PREFIX));
    if !is_settings_answer {
        return Cow::Borrowed(frame);
    }
    let mut view = frame.clone();
    let applied = frame["response"]["response"]["applied"].clone();
    view["response"]["response"] = json!({ "applied": applied });
    Cow::Owned(view)
}
