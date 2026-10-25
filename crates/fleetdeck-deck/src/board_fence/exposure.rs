//! The exposure windows and the one-second cache of the exposed addresses
//! (R13 P7 §7.6, "Exposure" and "Sign-out" rule 2). The store reads the
//! durable rows ([`crate::store::DeckStore::exposed_addresses`]); this module
//! holds the constants, the parse of one snapshot and the cache.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

/// An operation stays exposing this long after its terminal result (§7.6).
pub const FINISHED_TAIL_MS: i64 = 5_000;
/// An operation with no terminal result stays exposing this long after its
/// `attempted` commit: the 40 s deck handler of §10 and 5 s.
pub const UNFINISHED_WINDOW_MS: i64 = 45_000;
/// Rule 2 and the sign-in refusal last this long after the exposure ends.
pub const SIGN_OUT_TAIL_MS: i64 = 60_000;
/// How long the exposed set is kept in memory.
pub const CACHE_MS: i64 = 1_000;

/// The addresses of one `exposure_addrs` value: a JSON array of strings.
/// `None` when the text is not such an array.
#[must_use]
pub fn parse_snapshot(text: &str) -> Option<Vec<String>> {
    serde_json::from_str::<Vec<String>>(text).ok()
}

/// The exposed set at one time, and a counter that `invalidate` raises, so
/// that a read that started before an invalidation cannot store its result.
#[derive(Debug)]
pub struct Cache {
    at_and_set: Mutex<(i64, HashSet<String>)>,
    generation: AtomicU64,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            at_and_set: Mutex::new((i64::MIN, HashSet::new())),
            generation: AtomicU64::new(0),
        }
    }
}

impl Cache {
    /// The kept set when it is younger than [`CACHE_MS`] at `now`.
    #[must_use]
    pub fn get(&self, now: i64) -> Option<HashSet<String>> {
        let guard = self
            .at_and_set
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let age = now.saturating_sub(guard.0);
        (0..CACHE_MS).contains(&age).then(|| guard.1.clone())
    }

    /// The value to give to [`Self::put`] with the result of a read that
    /// starts now.
    #[must_use]
    pub fn ticket(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Keeps `set` as the state at `now`, unless an invalidation came after
    /// `ticket`.
    pub fn put(&self, ticket: u64, now: i64, set: &HashSet<String>) {
        let mut guard = self
            .at_and_set
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.generation.load(Ordering::SeqCst) == ticket {
            *guard = (now, set.clone());
        }
    }

    /// Clears the cache: the next read goes to the database.
    pub fn invalidate(&self) {
        let mut guard = self
            .at_and_set
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.generation.fetch_add(1, Ordering::SeqCst);
        *guard = (i64::MIN, HashSet::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn fence_a_snapshot_is_a_json_array_of_strings() {
        assert_eq!(
            parse_snapshot(r#"["10.0.0.1","::1"]"#),
            Some(vec!["10.0.0.1".to_owned(), "::1".to_owned()])
        );
        assert_eq!(parse_snapshot("[]"), Some(Vec::new()));
        for bad in [
            "",
            "null",
            "{}",
            "[1]",
            r#"["a", 2]"#,
            "not json",
            r#""10.0.0.1""#,
        ] {
            assert_eq!(parse_snapshot(bad), None, "{bad}");
        }
    }

    #[test]
    fn fence_the_cache_keeps_a_set_for_one_second() {
        let cache = Cache::default();
        assert_eq!(cache.get(5_000), None);
        cache.put(cache.ticket(), 5_000, &set(&["a"]));
        assert_eq!(cache.get(5_000), Some(set(&["a"])));
        assert_eq!(cache.get(5_999), Some(set(&["a"])));
        assert_eq!(cache.get(6_000), None);
        // A clock that goes back does not keep the set either.
        assert_eq!(cache.get(4_999), None);
    }

    #[test]
    fn fence_invalidate_clears_the_cache_and_stops_an_older_read() {
        let cache = Cache::default();
        cache.put(cache.ticket(), 5_000, &set(&["a"]));
        cache.invalidate();
        assert_eq!(cache.get(5_000), None);
        // A read that started before the invalidation cannot store.
        let old = cache.ticket();
        cache.invalidate();
        cache.put(old, 5_001, &set(&["stale"]));
        assert_eq!(cache.get(5_001), None);
        // A read that starts after it can.
        cache.put(cache.ticket(), 5_002, &set(&["fresh"]));
        assert_eq!(cache.get(5_002), Some(set(&["fresh"])));
    }
}
