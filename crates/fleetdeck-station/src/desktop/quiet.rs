//! The local time of the station (R13 §6D item 3, §5E item 5): the UTC
//! offset that the station reads with one fixed call (`date +%z`, in the
//! system time zone), and the quiet hours `HH:MM-HH:MM` of a display. The
//! deck uses no time zone library; the station re-checks the quiet hours
//! right before the first side effect of a wake.

use std::time::{SystemTime, UNIX_EPOCH};

use fleetdeck_core::ChildEnv;

use super::run::{CALL_TIMEOUT, Runner};

const MINUTES_PER_DAY: i64 = 24 * 60;

/// The station wall clock in Unix ms (0 when the clock is before 1970).
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Parses the output of `date +%z`: exactly `[+-]HHMM` (a final line feed is
/// allowed), HH at most 23 and MM at most 59. The result is minutes east of
/// UTC. Anything else is `None`.
#[must_use]
pub fn parse_offset(text: &str) -> Option<i32> {
    let text = text.strip_suffix('\n').unwrap_or(text);
    let bytes = text.as_bytes();
    if bytes.len() != 5 || !bytes[1..].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digit = |i: usize| i32::from(bytes[i] - b'0');
    let hours = digit(1) * 10 + digit(2);
    let minutes = digit(3) * 10 + digit(4);
    (hours <= 23 && minutes <= 59).then_some(sign * (hours * 60 + minutes))
}

/// Reads the UTC offset of the station with `date +%z`. `None` when the
/// program fails or prints anything else than `[+-]HHMM`.
pub async fn read_offset(env: &ChildEnv) -> Option<i32> {
    let runner = Runner {
        env,
        gate: None,
        start: None,
    };
    let out = runner
        .run("date", &["+%z".to_owned()], CALL_TIMEOUT)
        .await
        .ok()?;
    if out.code == 0 {
        parse_offset(&out.stdout)
    } else {
        None
    }
}

/// `HH:MM-HH:MM` in the local time of the station: half-open (the start is
/// inside, the end is outside), a start later than the end wraps over
/// midnight, and equal endpoints mean no quiet hours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuietHours {
    start: u16,
    end: u16,
}

impl QuietHours {
    /// Parses exactly `HH:MM-HH:MM` (HH at most 23, MM at most 59).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (start, end) = text.split_once('-')?;
        Some(Self {
            start: clock(start)?,
            end: clock(end)?,
        })
    }

    /// Whether `minute` (minutes since local midnight) is inside.
    #[must_use]
    pub fn contains(self, minute: u16) -> bool {
        use std::cmp::Ordering;
        match self.start.cmp(&self.end) {
            Ordering::Equal => false,
            Ordering::Less => self.start <= minute && minute < self.end,
            Ordering::Greater => minute >= self.start || minute < self.end,
        }
    }

    /// Whether the Unix time `now_ms` is inside, at `offset_minutes` east of
    /// UTC.
    #[must_use]
    pub fn contains_at(self, now_ms: i64, offset_minutes: i32) -> bool {
        let local = now_ms.div_euclid(60_000) + i64::from(offset_minutes);
        let minute = u16::try_from(local.rem_euclid(MINUTES_PER_DAY)).unwrap_or(0);
        self.contains(minute)
    }
}

/// `HH:MM` as minutes since midnight.
fn clock(text: &str) -> Option<u16> {
    let bytes = text.as_bytes();
    if bytes.len() != 5 || bytes[2] != b':' {
        return None;
    }
    if ![0, 1, 3, 4].iter().all(|&i| bytes[i].is_ascii_digit()) {
        return None;
    }
    let digit = |i: usize| u16::from(bytes[i] - b'0');
    let hours = digit(0) * 10 + digit(1);
    let minutes = digit(3) * 10 + digit(4);
    (hours <= 23 && minutes <= 59).then_some(hours * 60 + minutes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offset_parses_only_the_date_format() {
        assert_eq!(parse_offset("+0200\n"), Some(120));
        assert_eq!(parse_offset("-0530"), Some(-330));
        assert_eq!(parse_offset("+0000\n"), Some(0));
        assert_eq!(parse_offset("+1400"), Some(840));
        for bad in [
            "",
            "0200",
            "+200",
            "+02:00",
            "+2400",
            "+0260",
            " +0200",
            "+0200 ",
            "+02000",
            "UTC",
            "+0200\n\n",
        ] {
            assert_eq!(parse_offset(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn quiet_hours_parse_strictly() {
        assert!(QuietHours::parse("22:00-07:00").is_some());
        assert!(QuietHours::parse("00:00-23:59").is_some());
        for bad in [
            "",
            "22:00",
            "22:00-07",
            "24:00-07:00",
            "22:60-07:00",
            "2:00-07:00",
            "22:00 - 07:00",
            "22:00-07:00-08:00",
            "aa:bb-cc:dd",
        ] {
            assert_eq!(QuietHours::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_interval_is_half_open_and_wraps_and_equal_endpoints_are_empty() {
        let day = QuietHours::parse("09:00-17:00").unwrap();
        assert!(day.contains(9 * 60));
        assert!(day.contains(17 * 60 - 1));
        assert!(!day.contains(17 * 60));
        assert!(!day.contains(9 * 60 - 1));
        let night = QuietHours::parse("22:00-07:00").unwrap();
        assert!(night.contains(22 * 60));
        assert!(night.contains(23 * 60 + 59));
        assert!(night.contains(0));
        assert!(night.contains(7 * 60 - 1));
        assert!(!night.contains(7 * 60));
        assert!(!night.contains(21 * 60 + 59));
        let same = QuietHours::parse("10:00-10:00").unwrap();
        assert!(!same.contains(10 * 60));
        assert!(!same.contains(0));
    }

    #[test]
    fn the_local_minute_follows_the_offset() {
        let night = QuietHours::parse("22:00-07:00").unwrap();
        // 2024-01-01T21:30:00Z is 1_704_144_600 s.
        let utc = 1_704_144_600_000;
        assert!(!night.contains_at(utc, 0));
        assert!(night.contains_at(utc, 60));
        // West of UTC: 21:30Z at -0600 is 15:30.
        assert!(!night.contains_at(utc, -360));
        // A time before the epoch still maps into one day.
        assert!(night.contains_at(-60_000, 0));
    }
}
