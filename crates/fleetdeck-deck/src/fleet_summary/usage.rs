//! The usage of the summary: a Rust port of `rateMeters` and `tightest` of
//! `web/src/board/usage.ts`, with the closed label mapping of the design
//! (`omarchy-p4.md` §3.2, revision 12b).
//!
//! The latest window of each (station, provider instance, scope) comes from
//! the most recently updated session that reported it. The window with the
//! highest known use wins. No provider string reaches the label: the scope
//! is mapped to a fixed word, and the instance id passes only when it
//! matches `^[a-z0-9_-]{1,32}$`.

use std::cmp::Ordering;
use std::collections::HashMap;

use fleetdeck_proto::board::DeckSession;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_summary::SummaryUsage;

/// The label word of a provider instance that breaks the grammar.
const INSTANCE_FALLBACK: &str = "provider";

/// The identity of a meter: station id, provider instance, scope. The web
/// sorts equal uses by the text of this key.
type Key = (String, String, String);

/// One rate-limit meter: the latest window of one scope of one provider
/// account (station and provider instance).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Meter {
    key: Key,
    used_percent: Option<f64>,
    resets_at: Option<i64>,
}

/// The latest window per station, provider instance and scope. The tightest
/// window (highest use) is first; a window with unknown use is last.
pub(super) fn meters(cards: &[DeckSession]) -> Vec<Meter> {
    let mut latest: HashMap<Key, (i64, Meter)> = HashMap::new();
    for card in cards {
        let sum = &card.summary;
        let instance = sum.provider_instance.as_ref().map_or_else(
            || {
                sum.provider
                    .map_or_else(|| "unknown".to_owned(), provider_word)
            },
            |i| i.as_str().to_owned(),
        );
        for limit in &sum.rate_limits {
            let key: Key = (
                card.station_id.as_str().to_owned(),
                instance.clone(),
                limit.scope.clone(),
            );
            if latest
                .get(&key)
                .is_some_and(|(at, _)| *at >= sum.updated_at)
            {
                continue;
            }
            let meter = Meter {
                key: key.clone(),
                // JSON cannot carry NaN; a non-finite number is unknown.
                used_percent: limit.used_percent.filter(|p| p.is_finite()),
                resets_at: limit.resets_at,
            };
            latest.insert(key, (sum.updated_at, meter));
        }
    }
    let mut out: Vec<Meter> = latest.into_values().map(|(_, m)| m).collect();
    out.sort_by(tighter);
    out
}

/// Highest use first, unknown use last, then by key.
fn tighter(a: &Meter, b: &Meter) -> Ordering {
    match (a.used_percent, b.used_percent) {
        (Some(x), Some(y)) if x != y => y.total_cmp(&x),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        _ => a.key.cmp(&b.key),
    }
}

/// The tightest window with a known use, or `None`.
pub(super) fn tightest(meters: &[Meter]) -> Option<&Meter> {
    meters.iter().find(|m| m.used_percent.is_some())
}

/// The serde word of a provider family. Taken from serde, so the deck never
/// branches on a provider name (the `layering` test).
fn provider_word(kind: ProviderKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// The fixed word of a window scope: `five_hour` is `5 h`, `seven_day` is
/// `7 d`, every other scope (a provider text) is `other`.
pub(super) fn scope_word(scope: &str) -> &'static str {
    match scope {
        "five_hour" => "5 h",
        "seven_day" => "7 d",
        _ => "other",
    }
}

/// The provider instance id when it matches `^[a-z0-9_-]{1,32}$`, else
/// `provider`.
pub(super) fn instance_word(instance: &str) -> &str {
    let ok = !instance.is_empty()
        && instance.len() <= 32
        && instance
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if ok { instance } else { INSTANCE_FALLBACK }
}

/// The usage of the summary at `now_ms`, or `None` without a window with a
/// known use.
pub(super) fn summary_usage(cards: &[DeckSession], now_ms: i64) -> Option<SummaryUsage> {
    let all = meters(cards);
    let m = tightest(&all)?;
    let used = m.used_percent?;
    let (_, instance, scope) = &m.key;
    // A float to integer cast saturates: a negative number gives 0.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pct = used.round() as u32;
    Some(SummaryUsage {
        label: format!("{} · {}", instance_word(instance), scope_word(scope)),
        pct,
        resets_in_s: m
            .resets_at
            .map(|at| u32::try_from((at.saturating_sub(now_ms) / 1000).max(0)).unwrap_or(u32::MAX)),
    })
}
