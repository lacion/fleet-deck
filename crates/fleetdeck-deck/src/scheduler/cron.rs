//! A small cron parser and the next-run computation (R5, DL-168).
//!
//! Five fields: `minute hour day-of-month month day-of-week`. Each field is a
//! list of items; an item is `*`, a number, a range `a-b`, and may have a
//! step `/n` (`*/15`, `10-50/10`, `5/20` meaning `5-max/20`). Day of week is
//! 0 to 7 (0 and 7 are Sunday). Names (`mon`, `jan`) are not supported.
//! When both day of month and day of week are restricted, a day matches when
//! either one matches (the classic cron rule).
//!
//! Presets: `@hourly` (`0 * * * *`), `@daily` (`0 0 * * *`), `@weekdays`
//! (`0 9 * * 1-5`) and `@weekly` (`0 0 * * 0`).
//!
//! The time zone is `UTC` or a fixed offset `+HH:MM` / `-HH:MM`. A fixed
//! offset has no daylight-saving change: use the offset that you want all
//! year. The deck has no time zone database (no new dependency).

/// The most days that the next-run search walks (nine years, for `29 2 *`).
const SEARCH_DAYS: i64 = 366 * 9;
const MINUTE_MS: i64 = 60_000;
const DAY_MINUTES: i64 = 1440;

/// The presets and their five-field form.
pub const PRESETS: &[(&str, &str)] = &[
    ("@hourly", "0 * * * *"),
    ("@daily", "0 0 * * *"),
    ("@weekdays", "0 9 * * 1-5"),
    ("@weekly", "0 0 * * 0"),
];

/// A parsed cron expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: u64,
    hours: u32,
    /// Bits 1 to 31.
    days: u32,
    /// Bits 1 to 12.
    months: u16,
    /// Bits 0 to 6, Sunday is 0.
    weekdays: u8,
    days_any: bool,
    weekdays_any: bool,
}

impl Cron {
    /// Parses a cron expression or a preset.
    ///
    /// # Errors
    /// A message that names the problem.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let text = PRESETS
            .iter()
            .find(|(name, _)| *name == text)
            .map_or(text, |(_, expr)| *expr);
        let fields: Vec<&str> = text.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields.as_slice() else {
            return Err(
                "a cron expression has 5 fields (minute hour day month weekday) or is one of \
                 @hourly, @daily, @weekdays, @weekly"
                    .to_owned(),
            );
        };
        let weekdays = field(weekday, 0, 7, "weekday")?;
        // 7 is Sunday too.
        let weekdays = u8::try_from((weekdays & 0x7f) | (weekdays >> 7 & 1))
            .map_err(|_| "weekday out of range".to_owned())?;
        let cron = Self {
            minutes: field(minute, 0, 59, "minute")?,
            hours: u32::try_from(field(hour, 0, 23, "hour")?).map_err(|e| e.to_string())?,
            days: u32::try_from(field(day, 1, 31, "day")?).map_err(|e| e.to_string())?,
            months: u16::try_from(field(month, 1, 12, "month")?).map_err(|e| e.to_string())?,
            weekdays,
            days_any: day.starts_with('*'),
            weekdays_any: weekday.starts_with('*'),
        };
        if cron.next_after(0, 0).is_none() {
            return Err("this expression never matches a real date".to_owned());
        }
        Ok(cron)
    }

    fn day_matches(&self, days_since_epoch: i64) -> bool {
        let (_, month, day) = civil_from_days(days_since_epoch);
        if self.months >> month & 1 == 0 {
            return false;
        }
        let by_day = self.days >> day & 1 == 1;
        let weekday = (days_since_epoch + 4).rem_euclid(7);
        let by_weekday = self.weekdays >> weekday & 1 == 1;
        // Classic cron: when either field starts with `*` (also `*/2`), both
        // fields must match; when neither does, either one is enough.
        if self.days_any || self.weekdays_any {
            by_day && by_weekday
        } else {
            by_day || by_weekday
        }
    }

    /// The first matching minute strictly after `after_ms` (Unix ms), in the
    /// zone with `offset_minutes` east of UTC. `None` when no minute matches
    /// in the next nine years.
    #[must_use]
    pub fn next_after(&self, after_ms: i64, offset_minutes: i32) -> Option<i64> {
        let offset = i64::from(offset_minutes);
        let local_ms = after_ms.checked_add(offset * MINUTE_MS)?;
        let start = local_ms.div_euclid(MINUTE_MS) + 1;
        let first_day = start.div_euclid(DAY_MINUTES);
        for day in first_day..first_day + SEARCH_DAYS {
            if !self.day_matches(day) {
                continue;
            }
            let from = if day == first_day {
                start - day * DAY_MINUTES
            } else {
                0
            };
            for minute_of_day in from..DAY_MINUTES {
                let (h, m) = (minute_of_day / 60, minute_of_day % 60);
                if self.hours >> h & 1 == 1 && self.minutes >> m & 1 == 1 {
                    let local_minute = day * DAY_MINUTES + minute_of_day;
                    return (local_minute - offset).checked_mul(MINUTE_MS);
                }
            }
        }
        None
    }
}

/// One field as a bit set (bit `n` is the value `n`).
fn field(text: &str, min: u32, max: u32, name: &str) -> Result<u64, String> {
    let mut bits = 0_u64;
    for item in text.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s
                    .parse()
                    .map_err(|_| format!("the {name} step `{s}` is not a number"))?;
                if step == 0 || step > max {
                    return Err(format!("the {name} step must be 1 to {max}"));
                }
                (r, step)
            }
            None => (item, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (number(a, min, max, name)?, number(b, min, max, name)?)
        } else {
            let n = number(range, min, max, name)?;
            (n, if item.contains('/') { max } else { n })
        };
        if lo > hi {
            return Err(format!("the {name} range `{range}` runs backwards"));
        }
        let mut v = lo;
        while v <= hi {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok(bits)
}

fn number(text: &str, min: u32, max: u32, name: &str) -> Result<u32, String> {
    let n: u32 = text
        .parse()
        .map_err(|_| format!("the {name} value `{text}` is not a number"))?;
    if n < min || n > max {
        return Err(format!("the {name} value {n} is not in {min} to {max}"));
    }
    Ok(n)
}

/// The offset of a zone text in minutes east of UTC: `UTC`, `+HH:MM` or
/// `-HH:MM` (at most 14:00).
///
/// # Errors
/// A message that names the problem.
pub fn parse_offset(text: &str) -> Result<i32, String> {
    let text = text.trim();
    if text == "UTC" {
        return Ok(0);
    }
    let bad = || "a time zone is `UTC` or an offset like `+02:00` or `-05:30`".to_owned();
    let bytes = text.as_bytes();
    if bytes.len() != 6 || !matches!(bytes[0], b'+' | b'-') || bytes[3] != b':' {
        return Err(bad());
    }
    let digits = |s: &str| -> Result<i32, String> {
        if s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse().map_err(|_| bad())
        } else {
            Err(bad())
        }
    };
    let (h, m) = (digits(&text[1..3])?, digits(&text[4..6])?);
    if m > 59 || h > 14 || (h == 14 && m > 0) {
        return Err("an offset is at most 14:00 and its minutes are 00 to 59".to_owned());
    }
    let minutes = h * 60 + m;
    Ok(if bytes[0] == b'-' { -minutes } else { minutes })
}

/// The civil date of a day count since 1970-01-01 (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `yyyy-mm-dd` of `at_ms` in the zone with `offset_minutes`.
#[must_use]
pub fn date_text(at_ms: i64, offset_minutes: i32) -> String {
    let local = at_ms.saturating_add(i64::from(offset_minutes) * MINUTE_MS);
    let (y, m, d) = civil_from_days(local.div_euclid(MINUTE_MS).div_euclid(DAY_MINUTES));
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unix ms of a UTC date and time.
    fn at(y: i64, mo: i64, d: i64, h: i64, mi: i64) -> i64 {
        // Days from civil (Hinnant).
        let y = if mo <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let mp = if mo > 2 { mo - 3 } else { mo + 9 };
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        ((days * 24 + h) * 60 + mi) * MINUTE_MS
    }

    fn next(expr: &str, after: i64, offset: i32) -> Option<i64> {
        Cron::parse(expr).unwrap().next_after(after, offset)
    }

    #[test]
    fn the_epoch_date_and_weekday_are_right() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(date_text(at(2026, 10, 13, 23, 59), 0), "2026-10-13");
        assert_eq!(date_text(at(2026, 10, 13, 23, 59), 120), "2026-10-14");
        assert_eq!(date_text(at(2026, 10, 13, 0, 30), -60), "2026-10-12");
        // 2026-10-13 is a Tuesday (2), 2024-01-01 a Monday (1).
        assert_eq!((19_723_i64 + 4).rem_euclid(7), 1);
    }

    #[test]
    fn every_minute_and_steps() {
        let t = at(2026, 10, 13, 8, 59);
        assert_eq!(next("* * * * *", t, 0), Some(t + MINUTE_MS));
        assert_eq!(next("*/15 * * * *", t, 0), Some(at(2026, 10, 13, 9, 0)));
        assert_eq!(
            next("*/15 * * * *", at(2026, 10, 13, 9, 0), 0),
            Some(at(2026, 10, 13, 9, 15))
        );
        assert_eq!(next("5/20 * * * *", t, 0), Some(at(2026, 10, 13, 9, 5)));
        assert_eq!(
            next("10-50/20 * * * *", at(2026, 10, 13, 9, 11), 0),
            Some(at(2026, 10, 13, 9, 30))
        );
    }

    #[test]
    fn the_next_run_is_strictly_after() {
        let t = at(2026, 10, 13, 9, 0);
        assert_eq!(next("0 9 * * *", t, 0), Some(at(2026, 10, 14, 9, 0)));
        assert_eq!(
            next("0 9 * * *", t - 1, 0),
            Some(t),
            "a time inside the minute before"
        );
    }

    #[test]
    fn lists_ranges_and_the_year_wraps() {
        assert_eq!(
            next("30 8,17 * * *", at(2026, 10, 13, 9, 0), 0),
            Some(at(2026, 10, 13, 17, 30))
        );
        assert_eq!(
            next("0 0 1 1 *", at(2026, 10, 13, 9, 0), 0),
            Some(at(2027, 1, 1, 0, 0))
        );
        assert_eq!(
            next("0 0 * 12 *", at(2026, 12, 31, 0, 0), 0),
            Some(at(2027, 12, 1, 0, 0))
        );
    }

    #[test]
    fn presets_expand() {
        let tue = at(2026, 10, 13, 8, 0);
        assert_eq!(next("@hourly", tue, 0), Some(at(2026, 10, 13, 9, 0)));
        assert_eq!(next("@daily", tue, 0), Some(at(2026, 10, 14, 0, 0)));
        // @weekdays: 09:00 Monday to Friday. 2026-10-16 is a Friday.
        assert_eq!(next("@weekdays", tue, 0), Some(at(2026, 10, 13, 9, 0)));
        assert_eq!(
            next("@weekdays", at(2026, 10, 16, 9, 0), 0),
            Some(at(2026, 10, 19, 9, 0))
        );
        // @weekly: Sunday 00:00. 2026-10-18 is a Sunday.
        assert_eq!(next("@weekly", tue, 0), Some(at(2026, 10, 18, 0, 0)));
    }

    #[test]
    fn sunday_is_zero_or_seven() {
        let t = at(2026, 10, 13, 8, 0);
        assert_eq!(next("0 12 * * 0", t, 0), Some(at(2026, 10, 18, 12, 0)));
        assert_eq!(next("0 12 * * 7", t, 0), Some(at(2026, 10, 18, 12, 0)));
        assert_eq!(next("0 12 * * 5-7", t, 0), Some(at(2026, 10, 16, 12, 0)));
    }

    #[test]
    fn day_and_weekday_both_set_match_either() {
        // The 15th, or any Monday: 2026-10-13 is a Tuesday; Monday 19th comes
        // after the 15th (a Thursday).
        assert_eq!(
            next("0 0 15 * 1", at(2026, 10, 13, 8, 0), 0),
            Some(at(2026, 10, 15, 0, 0))
        );
        assert_eq!(
            next("0 0 15 * 1", at(2026, 10, 15, 8, 0), 0),
            Some(at(2026, 10, 19, 0, 0))
        );
    }

    #[test]
    fn a_leap_day_is_found() {
        assert_eq!(
            next("0 0 29 2 *", at(2026, 10, 13, 8, 0), 0),
            Some(at(2028, 2, 29, 0, 0))
        );
    }

    #[test]
    fn a_fixed_offset_moves_the_due_time() {
        // 09:00 at +02:00 is 07:00 UTC.
        assert_eq!(
            next("0 9 * * *", at(2026, 10, 13, 0, 0), 120),
            Some(at(2026, 10, 13, 7, 0))
        );
        // 09:00 at -05:30 is 14:30 UTC.
        assert_eq!(
            next("0 9 * * *", at(2026, 10, 13, 0, 0), -330),
            Some(at(2026, 10, 13, 14, 30))
        );
        // At +02:00, after 07:00 UTC the next 09:00 local is tomorrow.
        assert_eq!(
            next("0 9 * * *", at(2026, 10, 13, 7, 0), 120),
            Some(at(2026, 10, 14, 7, 0))
        );
    }

    #[test]
    fn bad_expressions_are_refused() {
        for bad in [
            "",
            "* * * *",
            "* * * * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * 32 * *",
            "* * * 13 *",
            "* * * * 8",
            "*/0 * * * *",
            "5-1 * * * *",
            "a * * * *",
            "@yearly",
            "0 0 31 2 *",
            "1,,2 * * * *",
            "*/ * * * *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn offsets_parse() {
        assert_eq!(parse_offset("UTC"), Ok(0));
        assert_eq!(parse_offset("+02:00"), Ok(120));
        assert_eq!(parse_offset("-05:30"), Ok(-330));
        assert_eq!(parse_offset("+14:00"), Ok(840));
        for bad in [
            "",
            "Z",
            "+2:00",
            "+02",
            "02:00",
            "+14:30",
            "+15:00",
            "+02:60",
            "+0a:00",
            "Europe/Paris",
        ] {
            assert!(parse_offset(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_stepped_day_field_still_limits_the_days() {
        // `*/2` in the day of month: odd dates only (1, 3, …), every weekday.
        let t = at(2026, 10, 13, 10, 0);
        assert_eq!(next("0 9 */2 * *", t, 0), Some(at(2026, 10, 15, 9, 0)));
        // `*/2` in the weekday: Sunday, Tuesday, Thursday, Saturday. After
        // Tuesday 2026-10-13 10:00 the next is Thursday 2026-10-15.
        assert_eq!(next("0 9 * * */2", t, 0), Some(at(2026, 10, 15, 9, 0)));
        // Neither field starts with `*`: either one is enough (the 14th is a
        // Wednesday, the first Monday after the 13th is the 19th).
        assert_eq!(next("0 9 14 * 1", t, 0), Some(at(2026, 10, 14, 9, 0)));
    }
}
