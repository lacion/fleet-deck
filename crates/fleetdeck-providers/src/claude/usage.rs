//! The Claude plan limit windows from the `get_usage` control request (R4.5).
//!
//! EXPERIMENTAL: the Agent SDK marks `get_usage` as "Experimental - the
//! response shape may change". The core asks again only while the answers have
//! the expected shape; the first error or odd answer stops the asks of the
//! session (an older CLI answers with an error).

use std::time::{Duration, Instant};

use fleetdeck_proto::Event;
use fleetdeck_proto::event::{RateLimit, Usage};
use serde_json::{Map, Value};

use super::core::{ClaudeCore, HostRequest, Output};
use super::{control, frames::ControlResponse};

/// The least time between two `get_usage` requests of one session.
const MIN_SPACING: Duration = Duration::from_secs(5 * 60);

impl ClaudeCore {
    /// The `get_usage` write, when one is due: not stopped, none pending, and
    /// the last ask is at least [`MIN_SPACING`] ago.
    pub(super) fn usage_request(&mut self) -> Vec<Output> {
        if self.usage_stopped
            || self
                .host_requests
                .values()
                .any(|k| *k == HostRequest::Usage)
            || self
                .usage_asked
                .is_some_and(|at| at.elapsed() < MIN_SPACING)
        {
            return Vec::new();
        }
        self.usage_asked = Some(Instant::now());
        self.usage_seq += 1;
        let id = format!("fd-usage-{}", self.usage_seq);
        self.host_requests.insert(id.clone(), HostRequest::Usage);
        vec![Output::Write(control::get_usage(&id))]
    }
}

/// The answer to a `get_usage` request.
pub(super) fn on_response(core: &mut ClaudeCore, resp: &ControlResponse) -> Vec<Output> {
    let available = resp
        .response
        .get("rate_limits_available")
        .and_then(Value::as_bool);
    let Some(available) = available.filter(|_| resp.ok) else {
        core.usage_stopped = true;
        tracing::debug!(
            error = resp.error.as_deref().unwrap_or_default(),
            "claude get_usage not supported: no more asks in this session"
        );
        return Vec::new();
    };
    let limits = if available {
        resp.response
            .get("rate_limits")
            .and_then(Value::as_object)
            .map(rate_limits)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if limits.is_empty() {
        return Vec::new();
    }
    vec![Output::Emit(Event::UsageUpdated {
        usage: Usage::default(),
        rate_limits: limits,
    })]
}

/// One entry per window with a non-null `utilization`.
fn rate_limits(windows: &Map<String, Value>) -> Vec<RateLimit> {
    windows
        .iter()
        .filter_map(|(scope, window)| {
            let used = window.get("utilization").and_then(Value::as_f64)?;
            Some(RateLimit {
                scope: scope.clone(),
                used_percent: Some(used.clamp(0.0, 100.0)),
                resets_at: window
                    .get("resets_at")
                    .and_then(Value::as_str)
                    .and_then(iso_to_unix_ms),
            })
        })
        .collect()
}

/// An ISO 8601 / RFC 3339 time (`2026-10-13T18:00:00.5Z`, `...+02:00`) as
/// Unix milliseconds; `None` when it does not parse.
fn iso_to_unix_ms(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once(['T', ' '])?;
    let mut d = date.splitn(3, '-');
    let (year, month, day) = (
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
    );
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // The zone starts at `Z`, `+` or `-` (the time itself has no `-`).
    let zone_at = rest.find(['Z', 'z', '+', '-']);
    let (clock, zone) = zone_at.map_or((rest, ""), |i| rest.split_at(i));
    let (hms, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut t = hms.splitn(3, ':');
    let hour = t.next()?.parse::<i64>().ok()?;
    let minute = t.next()?.parse::<i64>().ok()?;
    let second = t.next().map_or(Some(0), |s| s.parse::<i64>().ok())?;
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let millis = if fraction.is_empty() {
        0
    } else {
        if !fraction.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let three: String = fraction.chars().chain("000".chars()).take(3).collect();
        three.parse::<i64>().ok()?
    };
    let offset_minutes = match zone {
        "" | "Z" | "z" => 0,
        z => {
            let sign = if z.starts_with('-') { -1 } else { 1 };
            let (h, m) = z[1..].split_once(':').unwrap_or((&z[1..], "0"));
            let (h, m) = (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?);
            sign * (h * 60 + m)
        }
    };
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
        - offset_minutes * 60;
    Some(seconds * 1000 + millis)
}

/// Days from 1970-01-01 to a civil date (proleptic Gregorian).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
