//! Quiet hours of the wake rule (R13 §6D item 3, §5E item 5): `HH:MM-HH:MM`
//! in the local time of the station. The deck uses no time zone library: the
//! station reports its UTC offset with `desktop.info`.

use fleetdeck_proto::desktop::DesktopInfo;

use crate::store::displays::check_quiet_hours;

/// An offset sample is used when it is at most this old.
pub const OFFSET_MAX_AGE_MS: i64 = 15_000;
/// An offset sample is used when it is at most this far in the future.
pub const OFFSET_MAX_FUTURE_MS: i64 = 5_000;

fn minutes(part: &str) -> Option<i64> {
    let (h, m) = part.split_once(':')?;
    Some(i64::from(h.parse::<u8>().ok()?) * 60 + i64::from(m.parse::<u8>().ok()?))
}

/// True when the local time is inside `quiet`. The interval is half-open
/// (the start is inside, the end is outside), a start later than the end
/// wraps over midnight, and equal endpoints mean no quiet hours. `None` for
/// a text that is not valid.
#[must_use]
pub fn inside(quiet: &str, now_ms: i64, offset_minutes: i32) -> Option<bool> {
    check_quiet_hours(quiet).ok()?;
    let (start, end) = quiet.split_once('-')?;
    let (start, end) = (minutes(start)?, minutes(end)?);
    let local = (now_ms.div_euclid(60_000) + i64::from(offset_minutes)).rem_euclid(1440);
    Some(match start.cmp(&end) {
        std::cmp::Ordering::Equal => false,
        std::cmp::Ordering::Less => start <= local && local < end,
        std::cmp::Ordering::Greater => local >= start || local < end,
    })
}

/// The offset of `info`, when its sample is in [now - 15 s, now + 5 s].
#[must_use]
pub fn fresh_offset(info: &DesktopInfo, now_ms: i64) -> Option<i32> {
    let offset = info.utc_offset_minutes?;
    let at = info.offset_sampled_at?;
    (now_ms - OFFSET_MAX_AGE_MS <= at && at <= now_ms + OFFSET_MAX_FUTURE_MS).then_some(offset)
}

/// Whether a wake may go out for a display with `quiet`. `false` for quiet
/// hours that are active, an unknown or stale offset, or a text that is not
/// valid (fail closed). Without quiet hours the offset does not matter.
#[must_use]
pub fn allows_wake(quiet: Option<&str>, info: &DesktopInfo, now_ms: i64) -> bool {
    let Some(quiet) = quiet else {
        return true;
    };
    let Some(offset) = fresh_offset(info, now_ms) else {
        return false;
    };
    inside(quiet, now_ms, offset) == Some(false)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const MIN: i64 = 60_000;
    /// 2026-01-01T00:00:00Z.
    const DAY: i64 = 1_767_225_600_000;

    fn at(h: i64, m: i64) -> i64 {
        DAY + (h * 60 + m) * MIN
    }

    #[test]
    fn the_start_is_inside_and_the_end_is_outside() {
        assert_eq!(inside("09:00-17:00", at(9, 0), 0), Some(true));
        assert_eq!(inside("09:00-17:00", at(8, 59), 0), Some(false));
        assert_eq!(inside("09:00-17:00", at(16, 59), 0), Some(true));
        assert_eq!(inside("09:00-17:00", at(17, 0), 0), Some(false));
    }

    #[test]
    fn a_start_later_than_the_end_wraps_over_midnight() {
        for (h, m, want) in [
            (22, 0, true),
            (23, 59, true),
            (0, 0, true),
            (6, 59, true),
            (7, 0, false),
            (21, 59, false),
            (12, 0, false),
        ] {
            assert_eq!(inside("22:00-07:00", at(h, m), 0), Some(want), "{h}:{m}");
        }
    }

    #[test]
    fn equal_endpoints_mean_no_quiet_hours() {
        for (h, m) in [(0, 0), (10, 0), (10, 1), (23, 59)] {
            assert_eq!(inside("10:00-10:00", at(h, m), 0), Some(false));
        }
    }

    #[test]
    fn the_offset_moves_the_local_time_in_both_directions() {
        // 23:30 UTC is 01:30 at +120 and 20:30 at -180.
        assert_eq!(inside("01:00-02:00", at(23, 30), 120), Some(true));
        assert_eq!(inside("01:00-02:00", at(23, 30), -180), Some(false));
        assert_eq!(inside("20:00-21:00", at(23, 30), -180), Some(true));
        // 00:10 UTC at -60 is 23:10 of the day before.
        assert_eq!(inside("23:00-23:30", at(0, 10), -60), Some(true));
    }

    #[test]
    fn a_text_that_is_not_valid_is_none() {
        for text in [
            "",
            "9:00-17:00",
            "24:00-01:00",
            "09:00",
            "09:00-17:60",
            "ab",
        ] {
            assert_eq!(inside(text, at(10, 0), 0), None, "{text}");
        }
    }

    fn info(offset: Option<i32>, sampled: Option<i64>) -> DesktopInfo {
        let mut i = serde_json::from_value::<DesktopInfo>(serde_json::json!({
            "kind": "omarchy", "omarchy_version": null, "monitors": [], "locked": null,
            "dnd": null, "idle_inhibited": null, "walls": [],
            "allowed": {"desktop": true, "capture": false, "input": false},
            "tools": {"wtype": false, "pointer": false, "recorder": false,
                      "grim": false, "chromium": true, "notify_send": true},
        }))
        .unwrap();
        i.utc_offset_minutes = offset;
        i.offset_sampled_at = sampled;
        i
    }

    #[test]
    fn an_offset_sample_is_used_only_inside_its_window() {
        let now = at(12, 0);
        for (sampled, ok) in [
            (now, true),
            (now - 15_000, true),
            (now - 15_001, false),
            (now + 5_000, true),
            (now + 5_001, false),
        ] {
            assert_eq!(
                fresh_offset(&info(Some(60), Some(sampled)), now).is_some(),
                ok,
                "{sampled}"
            );
        }
        assert_eq!(fresh_offset(&info(None, Some(now)), now), None);
        assert_eq!(fresh_offset(&info(Some(60), None), now), None);
    }

    #[test]
    fn a_wake_needs_a_known_offset_and_a_time_outside_the_quiet_hours() {
        let now = at(12, 0);
        let good = info(Some(0), Some(now));
        assert!(allows_wake(None, &info(None, None), now));
        assert!(allows_wake(Some("22:00-07:00"), &good, now));
        assert!(!allows_wake(Some("11:00-13:00"), &good, now));
        assert!(!allows_wake(Some("22:00-07:00"), &info(None, None), now));
        assert!(!allows_wake(
            Some("22:00-07:00"),
            &info(Some(0), Some(now - 60_000)),
            now
        ));
        assert!(!allows_wake(Some("nope"), &good, now));
    }
}
