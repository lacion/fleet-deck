//! `display.directive` (R13 P4 task E, feature `desktop_shell.v1`): a station
//! asks the deck for a wall directive (the hotkeys of the Omarchy desktop
//! shell). Design `omarchy-p4.md` §3.6.
//!
//! The station names an intent, never a body. The deck authorizes by the
//! binding of the display to the calling station, the deck setting
//! `desktop_control`, the ceiling and a rate limit, and it writes an audit
//! row (identifiers only).
//!
//! Order of the checks: rate limit, parse, `desktop_control`, then the store
//! ([`crate::store::display_directives_station`]). Every refusal is a wire
//! error with a fixed text.

use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_link::methods::code;
use fleetdeck_proto::StationId;
use fleetdeck_proto::link::{Outcome, WireError};
use serde::Deserialize;

use super::DeckLink;
use crate::store::display_directives::mode_name;
use crate::store::display_directives_station::StationDirectiveError;
use crate::store::{AuditResult, NewAudit};

/// The audit action (the same as the owner route).
const ACTION: &str = "display.directive";
/// The calls that a station may make in one [`WINDOW`].
const LIMIT: usize = 2;
/// The rate-limit window.
const WINDOW: Duration = Duration::from_millis(1000);

pub(super) const TOO_MANY: &str = "Too many wall requests from this station. Wait one second.";
pub(super) const INVALID: &str = "The wall request is not valid.";
pub(super) const CONTROL_OFF: &str = "Desktop control is off on the deck.";
pub(super) const NOT_BOUND: &str = "This station has no wall display bound to it.";
pub(super) const NOTHING: &str = "No session of this station needs you.";
pub(super) const FAILED: &str = "The wall request failed.";

/// What a station asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DisplayIntent {
    /// The next open request of the caller's own sessions.
    NextNeedsYou,
    /// The next mode of the cycle.
    NextMode,
}

impl DisplayIntent {
    /// The wire name, also the audit text.
    #[must_use]
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::NextNeedsYou => "next_needs_you",
            Self::NextMode => "next_mode",
        }
    }
}

/// The params of the request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    display_id: String,
    intent: DisplayIntent,
}

/// The calls of each station in the last window, in memory.
#[derive(Debug, Default)]
struct Limiter {
    calls: HashMap<StationId, VecDeque<Instant>>,
}

impl Limiter {
    /// Records a call at `now` and returns true, or returns false when the
    /// station already made [`LIMIT`] calls in the window. A refused call is
    /// not recorded.
    fn allow(&mut self, station: &StationId, now: Instant) -> bool {
        let calls = self.calls.entry(station.clone()).or_default();
        while calls
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= WINDOW)
        {
            calls.pop_front();
        }
        if calls.len() >= LIMIT {
            return false;
        }
        calls.push_back(now);
        true
    }
}

static LIMITER: LazyLock<Mutex<Limiter>> = LazyLock::new(Mutex::default);

fn allowed(station: &StationId) -> bool {
    LIMITER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .allow(station, Instant::now())
}

fn refuse(code: &str, text: &str) -> Outcome {
    Outcome::Err(WireError::new(code, text))
}

/// `display.directive`: the answer is the stored `DirectiveView`.
pub(super) async fn display_directive(
    ctx: &DeckLink,
    station: &StationId,
    params: serde_json::Value,
) -> Outcome {
    if !allowed(station) {
        return refuse(code::TOO_MANY_REQUESTS, TOO_MANY);
    }
    let Ok(params) = serde_json::from_value::<Params>(params) else {
        return refuse("invalid_params", INVALID);
    };
    let control = ctx
        .desktop_live
        .get()
        .is_some_and(|live| live.get().desktop_control);
    if !control {
        return refuse("desktop_control_off", CONTROL_OFF);
    }
    let Params { display_id, intent } = params;
    let result = ctx
        .store
        .display_directive_station(station.clone(), display_id.clone(), intent)
        .await;
    match result {
        Ok(view) => {
            let detail = format!(
                "intent={} mode={}",
                intent.name(),
                mode_name(&view.directive)
            );
            ctx.store
                .audit_or_log(
                    NewAudit::new(format!("station:{station}"), ACTION, AuditResult::Ok)
                        .target(display_id)
                        .detail(detail),
                )
                .await;
            match serde_json::to_value(&view) {
                Ok(value) => Outcome::Ok(value),
                Err(e) => {
                    tracing::warn!(error = %e, "display.directive answer");
                    refuse("failed", FAILED)
                }
            }
        }
        Err(StationDirectiveError::NotBound) => refuse("not_bound", NOT_BOUND),
        Err(StationDirectiveError::NothingToShow) => refuse("nothing_to_show", NOTHING),
        Err(e) => {
            tracing::warn!(error = %e, "display.directive");
            refuse("failed", FAILED)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn the_limiter_allows_two_calls_in_a_window_and_refuses_the_third() {
        let base = Instant::now();
        let mut limiter = Limiter::default();
        let s = StationId::from("limit-a");
        assert!(limiter.allow(&s, at(base, 0)));
        assert!(limiter.allow(&s, at(base, 10)));
        assert!(!limiter.allow(&s, at(base, 20)));
        // A refused call is not recorded: the window ends 1000 ms after the
        // first accepted call.
        assert!(limiter.allow(&s, at(base, 1000)));
        assert!(!limiter.allow(&s, at(base, 1005)));
        assert!(limiter.allow(&s, at(base, 2010)));
    }

    #[test]
    fn the_limiter_counts_each_station_alone() {
        let base = Instant::now();
        let mut limiter = Limiter::default();
        let a = StationId::from("limit-a");
        let b = StationId::from("limit-b");
        assert!(limiter.allow(&a, base));
        assert!(limiter.allow(&a, base));
        assert!(!limiter.allow(&a, base));
        assert!(limiter.allow(&b, base));
    }

    #[test]
    fn the_params_take_only_a_display_and_an_intent() {
        let ok = serde_json::json!({"display_id": "d1", "intent": "next_mode"});
        assert!(serde_json::from_value::<Params>(ok).is_ok());
        for bad in [
            serde_json::json!({"display_id": "d1"}),
            serde_json::json!({"display_id": "d1", "intent": "mission"}),
            serde_json::json!({"display_id": "d1", "intent": "next_mode", "body": {}}),
            serde_json::json!({"intent": "next_mode"}),
        ] {
            assert!(serde_json::from_value::<Params>(bad).is_err());
        }
    }
}
