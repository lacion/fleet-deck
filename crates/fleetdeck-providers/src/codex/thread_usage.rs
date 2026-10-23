//! Codex token usage as per-thread running totals (B14, DL-445).
//!
//! The app-server reports, for each thread, a `total` that grows over the
//! life of the thread (and starts from the parent's total after a resume or
//! a fork). The core counts the growth of that total, so a replay is never
//! counted twice and a sub-agent thread is counted once.

use std::collections::BTreeMap;

use fleetdeck_proto::event::{ProviderKind, USAGE_SCHEME, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::prices::{PRICE_TABLE_VERSION, cost, price_of};

/// One `TokenUsageBreakdown` (app-server camelCase, rollout snake_case).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenTotals {
    /// `inputTokens` / `input_tokens` (contains the cached input).
    pub input: u64,
    /// `cachedInputTokens` / `cached_input_tokens`.
    pub cached: u64,
    /// `cacheWriteInputTokens` / `cache_write_input_tokens` (0 when absent).
    pub cache_write: u64,
    /// `outputTokens` / `output_tokens`.
    pub output: u64,
}

impl TokenTotals {
    fn from_keys(v: &Value, keys: [&str; 4]) -> Option<Self> {
        let count = |key: &str| v.get(key).and_then(Value::as_u64);
        Some(Self {
            input: count(keys[0])?,
            cached: count(keys[1]).unwrap_or(0),
            cache_write: count(keys[2]).unwrap_or(0),
            output: count(keys[3])?,
        })
    }

    /// The breakdown of an app-server notification; needs `inputTokens`
    /// and `outputTokens`.
    #[must_use]
    pub fn from_app_server(v: &Value) -> Option<Self> {
        Self::from_keys(
            v,
            [
                "inputTokens",
                "cachedInputTokens",
                "cacheWriteInputTokens",
                "outputTokens",
            ],
        )
    }

    /// The breakdown of a rollout record; needs `input_tokens` and
    /// `output_tokens`.
    #[must_use]
    pub fn from_rollout(v: &Value) -> Option<Self> {
        Self::from_keys(
            v,
            [
                "input_tokens",
                "cached_input_tokens",
                "cache_write_input_tokens",
                "output_tokens",
            ],
        )
    }

    /// `self - prev` per field; `self` itself when any field of `self` is
    /// lower (a new count).
    #[must_use]
    pub fn since(&self, prev: &TokenTotals) -> TokenTotals {
        if self.input < prev.input
            || self.cached < prev.cached
            || self.cache_write < prev.cache_write
            || self.output < prev.output
        {
            return *self;
        }
        TokenTotals {
            input: self.input - prev.input,
            cached: self.cached - prev.cached,
            cache_write: self.cache_write - prev.cache_write,
            output: self.output - prev.output,
        }
    }

    /// All four counts are 0.
    fn is_zero(&self) -> bool {
        *self == TokenTotals::default()
    }

    /// The normalized usage of these tokens, with the cost of `model`
    /// (DL-441, DL-444).
    #[must_use]
    pub fn usage(&self, model: Option<&str>) -> Usage {
        let new_input = self
            .input
            .saturating_sub(self.cached)
            .saturating_sub(self.cache_write);
        let price = model.and_then(|m| price_of(ProviderKind::Codex, m));
        let cost_usd =
            price.map(|p| cost(p, new_input, self.cached, self.cache_write, self.output));
        Usage {
            input_tokens: Some(new_input),
            output_tokens: Some(self.output),
            cost_usd,
            cost_inferred: cost_usd.is_some(),
            cache_read_tokens: Some(self.cached),
            cache_write_tokens: Some(self.cache_write),
            prices: cost_usd.map(|_| PRICE_TABLE_VERSION.to_owned()),
            scheme: Some(USAGE_SCHEME),
            tokens_partial: false,
            cost_partial: cost_usd.is_none() && !self.is_zero(),
        }
    }
}

/// The usage state of a core; part of the checkpoint.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThreadUsage {
    /// The last counted `total` of each thread of this session (own and
    /// sub-agents).
    pub totals: BTreeMap<String, TokenTotals>,
    /// A `turn/started` of the own thread came in this process.
    pub turn_seen: bool,
    /// Restored from a checkpoint of a build before this design: the
    /// restored app-server did not reopen its thread, so no resume replay
    /// comes, and the old core counted the `last` blocks. The first update
    /// of a thread without an entry counts its `last` block and seeds the
    /// entry.
    #[serde(default)]
    pub legacy: bool,
}

impl ThreadUsage {
    /// The state of a checkpoint written before this design.
    #[must_use]
    pub fn legacy() -> Self {
        Self {
            totals: BTreeMap::new(),
            turn_seen: true,
            legacy: true,
        }
    }

    /// The delta of one `thread/tokenUsage/updated` (`p` is its params);
    /// `None`: nothing to emit.
    pub fn delta(&mut self, thread: &str, own: bool, p: &Value) -> Option<TokenTotals> {
        let last = || TokenTotals::from_app_server(&p["tokenUsage"]["last"]);
        // 1. An older app-server sends no `total`: the `last` block counts.
        let Some(total) = TokenTotals::from_app_server(&p["tokenUsage"]["total"]) else {
            return last();
        };
        let Some(prev) = self.totals.insert(thread.to_owned(), total) else {
            // No entry for this thread.
            return if self.legacy {
                // 2. The old core counted the earlier calls: count this call.
                last()
            } else if own && !self.turn_seen {
                // 3. The replay of a resume or a fork: a base, no usage.
                None
            } else {
                // 4. A new thread (a sub-agent).
                Some(total)
            };
        };
        // 5. The growth of the total.
        let growth = total.since(&prev);
        (!growth.is_zero()).then_some(growth)
    }
}
