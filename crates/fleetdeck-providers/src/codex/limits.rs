//! `account/rateLimits/updated` → `usage.updated` rate-limit windows
//! (M3 6-9, DL-303).
//!
//! The notification is a sparse rolling update of one `RateLimitSnapshot`
//! (schema 0.160.0): `limitId`, `primary` and `secondary` windows with a
//! required `usedPercent` and an optional `resetsAt` in Unix seconds. A
//! missing window is not reported, so the summary keeps its last value.

use fleetdeck_proto::event::RateLimit;
use serde_json::Value;

/// The windows of one snapshot, scoped `<limit id>.<primary|secondary>`.
pub(super) fn rate_limits(p: &Value) -> Vec<RateLimit> {
    let Some(snapshot) = p.get("rateLimits") else {
        return Vec::new();
    };
    let limit = snapshot
        .get("limitId")
        .and_then(Value::as_str)
        .unwrap_or("codex");
    ["primary", "secondary"]
        .into_iter()
        .filter_map(|side| {
            let w = snapshot.get(side)?;
            let used = w.get("usedPercent")?.as_f64()?;
            Some(RateLimit {
                scope: format!("{limit}.{side}"),
                used_percent: Some(used),
                resets_at: w
                    .get("resetsAt")
                    .and_then(Value::as_i64)
                    .map(|s| s.saturating_mul(1000)),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codex_rate_limit_windows_keep_their_scope_and_skip_missing_windows() {
        let got = rate_limits(&json!({"rateLimits": {
            "limitId": "codex",
            "primary": {"usedPercent": 42, "resetsAt": 1_767_240_000, "windowDurationMins": 300},
            "secondary": null
        }}));
        assert_eq!(
            got,
            [RateLimit {
                scope: "codex.primary".into(),
                used_percent: Some(42.0),
                resets_at: Some(1_767_240_000_000),
            }]
        );
        assert!(rate_limits(&json!({"rateLimits": {"primary": {}}})).is_empty());
        assert!(rate_limits(&json!({})).is_empty());
    }
}
