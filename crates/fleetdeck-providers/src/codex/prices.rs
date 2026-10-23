//! The local price table of Codex models (R4.6, DL-161). Codex reports
//! tokens only, so the adapter infers a cost from this table and marks it
//! `cost_inferred`. A model that is not in the table has no cost (unknown,
//! never zero). Change a price or add a model together with
//! [`PRICE_TABLE_VERSION`].

use fleetdeck_proto::event::Usage;
use serde_json::Value;

/// The version of the table, the date of its last change.
pub const PRICE_TABLE_VERSION: &str = "2026-10-13";

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub cached_input: f64,
    pub output: f64,
}

/// The price of a model id, or `None` for a model that is not in the table.
#[must_use]
pub fn price_of(model: &str) -> Option<Price> {
    let (input, cached_input, output) = match model {
        // `gpt-5.6-luna` (the model of the recorded fixtures) has the gpt-5
        // rates as a placeholder: the owner confirms the real price.
        "gpt-5" | "gpt-5-codex" | "gpt-5.1" | "gpt-5.1-codex" | "gpt-5.6-luna" => {
            (1.25, 0.125, 10.0)
        }
        "gpt-5-mini" | "gpt-5.1-codex-mini" => (0.25, 0.025, 2.0),
        "gpt-5-nano" => (0.05, 0.005, 0.4),
        _ => return None,
    };
    Some(Price {
        input,
        cached_input,
        output,
    })
}

/// The cost of one token count. Cached tokens are part of `input` and
/// cost the cached rate.
#[must_use]
pub fn inferred_cost(model: &str, input: u64, cached: u64, output: u64) -> Option<f64> {
    let price = price_of(model)?;
    let cached = cached.min(input);
    let per_mtok = |tokens: u64, usd: f64| tokens as f64 / 1_000_000.0 * usd;
    Some(
        per_mtok(input - cached, price.input)
            + per_mtok(cached, price.cached_input)
            + per_mtok(output, price.output),
    )
}

/// The usage of one `thread/tokenUsage/updated` `last` block. The cost is
/// inferred only when the model is known and both token counts are known.
#[must_use]
pub(super) fn usage_of(model: Option<&str>, last: &Value) -> Usage {
    let count = |key: &str| last.get(key).and_then(Value::as_u64);
    let input = count("inputTokens");
    let output = count("outputTokens");
    let cost = match (model, input, output) {
        (Some(model), Some(i), Some(o)) => {
            inferred_cost(model, i, count("cachedInputTokens").unwrap_or(0), o)
        }
        _ => None,
    };
    Usage {
        input_tokens: input,
        output_tokens: output,
        cost_usd: cost,
        cost_inferred: cost.is_some(),
        ..Usage::default()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_known_model_costs_its_tokens_and_cached_tokens_are_cheaper() {
        // 1M input of which 600k cached, 100k output on gpt-5-codex.
        let cost = inferred_cost("gpt-5-codex", 1_000_000, 600_000, 100_000).unwrap();
        let expected = 0.4 * 1.25 + 0.6 * 0.125 + 0.1 * 10.0;
        assert!((cost - expected).abs() < 1e-9, "{cost}");
        // A cached count above the input never goes negative.
        assert!(inferred_cost("gpt-5", 10, 99, 0).unwrap() >= 0.0);
    }

    #[test]
    fn an_unknown_model_has_no_price() {
        assert!(price_of("gpt-unknown-9").is_none());
        assert!(inferred_cost("gpt-unknown-9", 1, 0, 1).is_none());
        assert!(!PRICE_TABLE_VERSION.is_empty());
    }

    #[test]
    fn the_usage_of_a_token_update_is_inferred_only_when_everything_is_known() {
        let last = json!({"inputTokens": 12002, "cachedInputTokens": 9984, "outputTokens": 5});
        let u = usage_of(Some("gpt-5-codex"), &last);
        assert_eq!(u.input_tokens, Some(12002));
        assert_eq!(u.output_tokens, Some(5));
        assert!(u.cost_inferred);
        assert!(u.cost_usd.unwrap() > 0.0);
        // Unknown model, no model, or a missing count: cost unknown, not inferred.
        for (model, block) in [
            (Some("gpt-unknown-9"), last.clone()),
            (None, last.clone()),
            (Some("gpt-5-codex"), json!({"inputTokens": 3})),
        ] {
            let u = usage_of(model, &block);
            assert_eq!(u.cost_usd, None);
            assert!(!u.cost_inferred);
        }
    }
}
