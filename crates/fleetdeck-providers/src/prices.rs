//! The one dated price table of inferred costs (R4.6, DL-161, DL-444).
//! Codex reports tokens only, so the adapter infers a cost from this table
//! and marks it `cost_inferred`. Claude reports its own cost, so the table
//! has no Claude rows for the live cost; Claude rows serve the analytics
//! estimate only (`estimate_price_of`; the live Claude cost stays the CLI's
//! own). A model that is not in the table has no cost
//! (unknown, never zero). Change a price or add a model together with
//! [`PRICE_TABLE_VERSION`].

use fleetdeck_proto::event::ProviderKind;

/// The date of the rates below (R4.6, DL-161, DL-444).
pub const PRICE_TABLE_VERSION: &str = "2026-10-24";

/// USD per 1M tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

/// Removes a trailing date (`-YYYY-MM-DD`) from a model id.
fn without_date(model: &str) -> &str {
    let bytes = model.as_bytes();
    let Some(start) = bytes.len().checked_sub(11) else {
        return model;
    };
    let tail = &bytes[start..];
    let is_date = tail[0] == b'-'
        && tail[1..5].iter().all(u8::is_ascii_digit)
        && tail[5] == b'-'
        && tail[6..8].iter().all(u8::is_ascii_digit)
        && tail[8] == b'-'
        && tail[9..11].iter().all(u8::is_ascii_digit);
    if is_date { &model[..start] } else { model }
}

/// The price of a model of a provider; `None` for a model that is not
/// listed. A trailing date (`-YYYY-MM-DD`) is removed before the match.
#[must_use]
pub fn price_of(provider: ProviderKind, model: &str) -> Option<Price> {
    if provider != ProviderKind::Codex {
        return None;
    }
    let (input, cache_read, cache_write, output) = match without_date(model) {
        // `gpt-5.6-luna` (the model of the recorded fixtures) has the gpt-5
        // rates as a placeholder: the owner confirms the real price.
        "gpt-5" | "gpt-5-codex" | "gpt-5.1" | "gpt-5.1-codex" | "gpt-5.6-luna" => {
            (1.25, 0.125, 1.25, 10.0)
        }
        "gpt-5-mini" | "gpt-5.1-codex-mini" => (0.25, 0.025, 0.25, 2.0),
        "gpt-5-nano" => (0.05, 0.005, 0.05, 0.4),
        _ => return None,
    };
    Some(Price {
        input,
        cache_read,
        cache_write,
        output,
    })
}

/// The price of a model for an analytics estimate (B29): the Codex rows of
/// `price_of` and the Claude rows fitted from the CLI's own `cost-state`
/// records (design `analytics.md` §1.2). `None`: no price.
#[must_use]
pub fn estimate_price_of(provider: ProviderKind, model: &str) -> Option<Price> {
    if provider != ProviderKind::Claude {
        return price_of(provider, model);
    }
    let model = model.strip_suffix("[1m]").unwrap_or(model);
    let (input, cache_read, cache_write, output) = match without_date(model) {
        "claude-opus-5-5" => (4.0, 0.2, 5.0, 20.0),
        "claude-sonnet-5-5" => (2.0, 0.1, 2.5, 10.0),
        "claude-haiku-5-5" => (0.1, 0.01, 0.125, 0.5),
        _ => return None,
    };
    Some(Price {
        input,
        cache_read,
        cache_write,
        output,
    })
}

/// The estimate of one bucket; `None` when the model has no price.
#[must_use]
pub fn estimate_cost(
    provider: ProviderKind,
    model: &str,
    new_input: u64,
    cache_read: u64,
    cache_write: u64,
    output: u64,
) -> Option<f64> {
    let price = estimate_price_of(provider, model)?;
    Some(cost(price, new_input, cache_read, cache_write, output))
}

/// The cost of new input, cache reads, cache writes and output.
#[must_use]
pub fn cost(price: Price, new_input: u64, cache_read: u64, cache_write: u64, output: u64) -> f64 {
    let per_mtok = |tokens: u64, usd: f64| tokens as f64 / 1_000_000.0 * usd;
    per_mtok(new_input, price.input)
        + per_mtok(cache_read, price.cache_read)
        + per_mtok(cache_write, price.cache_write)
        + per_mtok(output, price.output)
}

#[cfg(test)]
#[path = "prices_tests.rs"]
mod estimate_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listed_model_costs_its_four_parts() {
        let price = price_of(ProviderKind::Codex, "gpt-5-codex").unwrap();
        let m = 1_000_000;
        let got = cost(price, m, m, m, m);
        let expected = 1.25 + 0.125 + 1.25 + 10.0;
        assert!((got - expected).abs() < 1e-9, "{got}");
    }

    #[test]
    fn a_dated_model_id_matches_its_row() {
        assert_eq!(
            price_of(ProviderKind::Codex, "gpt-5-2025-08-07"),
            price_of(ProviderKind::Codex, "gpt-5")
        );
        assert!(price_of(ProviderKind::Codex, "gpt-5-2025-08-07").is_some());
    }

    #[test]
    fn an_unlisted_model_has_no_price() {
        assert!(price_of(ProviderKind::Codex, "gpt-daybreak-blue-latest").is_none());
        assert!(price_of(ProviderKind::Claude, "claude-sonnet-4-5").is_none());
        assert!(price_of(ProviderKind::Claude, "gpt-5").is_none());
        assert!(!PRICE_TABLE_VERSION.is_empty());
    }
}
