//! The `[grant_limits]` table of `deck.toml` (B27 §3.2): the ceilings of every
//! grant. The word "none" turns a ceiling off.

use serde::Deserialize;

use crate::config::ConfigError;

/// A ceiling that the owner can turn off: `Some(n)`, or `None` (no limit).
pub type Limit = Option<u32>;

/// The word of "no limit" in `deck.toml` and in the settings values.
pub const NO_LIMIT_WORD: &str = "none";

/// The `[grant_limits]` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantLimits {
    /// The most worker grants in effect, deck-wide (was `WORKER_GRANT_CAP`).
    pub worker_grants: Limit,
    /// The highest spawn cap of an owner grant (was `MAX_SPAWN_CAP`).
    pub spawn_cap: Limit,
    /// The highest spawn cap of a worker grant (was `MAX_WORKER_SPAWN_CAP`).
    pub worker_spawn_cap: Limit,
    /// The longest idle time of a grant, in hours (was `MAX_IDLE_TTL_MS`).
    pub idle_hours: Limit,
    /// The highest answer cap of a grant (was `MAX_ANSWER_CAP`).
    pub answer_cap: Limit,
}

impl Default for GrantLimits {
    fn default() -> Self {
        Self {
            worker_grants: Some(200),
            spawn_cap: Some(50),
            worker_spawn_cap: Some(10),
            idle_hours: Some(24),
            answer_cap: Some(5000),
        }
    }
}

/// The bounds of a number (inclusive), by key.
pub const BOUNDS: &[(&str, u32, u32)] = &[
    ("worker_grants", 1, 100_000),
    ("spawn_cap", 1, 100_000),
    ("worker_spawn_cap", 1, 100_000),
    ("idle_hours", 1, 168),
    ("answer_cap", 1, 100_000),
];

/// A number or a word in the file.
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum RawLimit {
    Count(u32),
    Word(String),
}

impl RawLimit {
    /// `Some(n)` inside `min..=max`, `None` for `"none"`.
    ///
    /// # Errors
    /// `field`: "use a whole number from {min} to {max}, or \"none\"".
    pub(crate) fn validate(
        self,
        field: &str,
        (min, max): (u32, u32),
    ) -> Result<Limit, ConfigError> {
        let bad = || ConfigError::Invalid {
            field: field.to_owned(),
            message: format!("use a whole number from {min} to {max}, or \"{NO_LIMIT_WORD}\""),
        };
        match self {
            Self::Count(n) if (min..=max).contains(&n) => Ok(Some(n)),
            Self::Word(w) if w == NO_LIMIT_WORD => Ok(None),
            Self::Count(_) | Self::Word(_) => Err(bad()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawGrantLimits {
    worker_grants: Option<RawLimit>,
    spawn_cap: Option<RawLimit>,
    worker_spawn_cap: Option<RawLimit>,
    idle_hours: Option<RawLimit>,
    answer_cap: Option<RawLimit>,
}

fn bounds_of(key: &str) -> (u32, u32) {
    BOUNDS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map_or((1, 100_000), |(_, min, max)| (*min, *max))
}

impl RawGrantLimits {
    pub(crate) fn validate(self) -> Result<GrantLimits, ConfigError> {
        let d = GrantLimits::default();
        let one = |key: &str, raw: Option<RawLimit>, default: Limit| match raw {
            None => Ok(default),
            Some(raw) => raw.validate(&format!("grant_limits.{key}"), bounds_of(key)),
        };
        Ok(GrantLimits {
            worker_grants: one("worker_grants", self.worker_grants, d.worker_grants)?,
            spawn_cap: one("spawn_cap", self.spawn_cap, d.spawn_cap)?,
            worker_spawn_cap: one(
                "worker_spawn_cap",
                self.worker_spawn_cap,
                d.worker_spawn_cap,
            )?,
            idle_hours: one("idle_hours", self.idle_hours, d.idle_hours)?,
            answer_cap: one("answer_cap", self.answer_cap, d.answer_cap)?,
        })
    }
}

/// True when `new` is above `old`; `None` is above every number.
#[must_use]
pub fn raised(old: Limit, new: Limit) -> bool {
    match (old, new) {
        (Some(_), None) => true,
        (Some(o), Some(n)) => n > o,
        (None, _) => false,
    }
}

/// The keys (`grant_limits.<key>`) that `new` raises against `old`.
#[must_use]
pub fn raises(old: &GrantLimits, new: &GrantLimits) -> Vec<&'static str> {
    let pairs = [
        (
            "grant_limits.worker_grants",
            old.worker_grants,
            new.worker_grants,
        ),
        ("grant_limits.spawn_cap", old.spawn_cap, new.spawn_cap),
        (
            "grant_limits.worker_spawn_cap",
            old.worker_spawn_cap,
            new.worker_spawn_cap,
        ),
        ("grant_limits.idle_hours", old.idle_hours, new.idle_hours),
        ("grant_limits.answer_cap", old.answer_cap, new.answer_cap),
    ];
    pairs
        .into_iter()
        .filter(|(_, o, n)| raised(*o, *n))
        .map(|(key, _, _)| key)
        .collect()
}

/// `n <= limit`; every `n` is within `None`.
#[must_use]
pub fn within(n: u32, limit: Limit) -> bool {
    limit.is_none_or(|l| n <= l)
}

/// The value of an omitted cap (revision 2): the built-in default, lowered
/// to the limit (`min(default, limit)`; `None` keeps the default). The
/// resolved value is then checked like a given value.
#[must_use]
pub fn default_within(default: u32, limit: Limit) -> u32 {
    limit.map_or(default, |l| default.min(l))
}

/// The keys of `[grant_limits]` (a test compares them with the field table).
#[cfg(test)]
pub(crate) const RAW_GRANT_LIMITS_KEYS: &[&str] = &[
    "worker_grants",
    "spawn_cap",
    "worker_spawn_cap",
    "idle_hours",
    "answer_cap",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_within_lowers_the_default_to_the_limit() {
        assert_eq!(default_within(500, Some(1)), 1);
        assert_eq!(default_within(500, Some(5000)), 500);
        assert_eq!(default_within(500, None), 500);
    }

    #[test]
    fn raised_treats_none_as_above_every_number() {
        assert!(raised(Some(5), None));
        assert!(raised(Some(5), Some(6)));
        assert!(!raised(Some(5), Some(5)));
        assert!(!raised(Some(5), Some(4)));
        assert!(!raised(None, Some(4)));
        assert!(!raised(None, None));
    }

    #[test]
    fn within_accepts_every_number_under_none() {
        assert!(within(u32::MAX, None));
        assert!(within(3, Some(3)));
        assert!(!within(4, Some(3)));
    }
}
