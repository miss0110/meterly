//! Static model price table (USD per 1M tokens) and cost computation.
//!
//! Prices are approximate public API list prices, kept as code constants on
//! purpose (plan A3 — no remote fetch). To update: edit `PRICE_TABLE` below
//! and adjust the hand-computed expectations in the tests. Unknown models
//! yield `None`, which the UI renders as "N/A".

/// USD per 1M tokens for each of the four disjoint token slots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_write_per_mtok: f64,
}

/// Prefix-matched price table. Matching picks the longest prefix that
/// matches the model string, so order here does not matter.
const PRICE_TABLE: &[(&str, ModelPrice)] = &[
    (
        "claude-opus-4",
        ModelPrice {
            input_per_mtok: 15.0,
            output_per_mtok: 75.0,
            cache_read_per_mtok: 1.5,
            cache_write_per_mtok: 18.75,
        },
    ),
    (
        "claude-sonnet",
        ModelPrice {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_read_per_mtok: 0.3,
            cache_write_per_mtok: 3.75,
        },
    ),
    (
        "claude-haiku",
        ModelPrice {
            input_per_mtok: 0.8,
            output_per_mtok: 4.0,
            cache_read_per_mtok: 0.08,
            cache_write_per_mtok: 1.0,
        },
    ),
    (
        "claude-fable",
        ModelPrice {
            input_per_mtok: 5.0,
            output_per_mtok: 25.0,
            cache_read_per_mtok: 0.5,
            cache_write_per_mtok: 6.25,
        },
    ),
    (
        "gpt-5.5",
        ModelPrice {
            input_per_mtok: 1.25,
            output_per_mtok: 10.0,
            cache_read_per_mtok: 0.125,
            cache_write_per_mtok: 0.0,
        },
    ),
    (
        "gpt-5",
        ModelPrice {
            input_per_mtok: 1.25,
            output_per_mtok: 10.0,
            cache_read_per_mtok: 0.125,
            cache_write_per_mtok: 0.0,
        },
    ),
];

/// Longest-prefix match against `PRICE_TABLE`. Returns the matched prefix
/// (useful for tests/diagnostics) and its price. Unknown model → `None`.
pub fn price_for(model: &str) -> Option<(&'static str, &'static ModelPrice)> {
    PRICE_TABLE
        .iter()
        .filter(|(prefix, _)| model.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(prefix, price)| (*prefix, price))
}

/// Cost in USD for the four disjoint token slots of one event or bucket.
/// Unknown model → `None` (UI "N/A").
/// USD saved by cache reads vs paying the full input rate for the same
/// tokens: `cache_read * (input rate - cache-read rate)`. `None` for unknown
/// models; 0-savings models (rates equal) return Some(0.0).
pub fn cache_savings_usd(model: &str, cache_read_tokens: u64) -> Option<f64> {
    let (_, price) = price_for(model)?;
    let per_tok = (price.input_per_mtok - price.cache_read_per_mtok).max(0.0) / 1_000_000.0;
    Some(cache_read_tokens as f64 * per_tok)
}

pub fn cost_usd(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
) -> Option<f64> {
    let (_, price) = price_for(model)?;
    Some(
        (input_tokens as f64 * price.input_per_mtok
            + output_tokens as f64 * price.output_per_mtok
            + cache_read_tokens as f64 * price.cache_read_per_mtok
            + cache_creation_tokens as f64 * price.cache_write_per_mtok)
            / 1_000_000.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn known_model_cost_sums_four_rates() {
        // claude-opus-4: 100*15 + 50*75 + 1000*1.5 + 200*18.75 per MTok
        // = 0.0015 + 0.00375 + 0.0015 + 0.00375 = 0.0105 USD.
        let cost = cost_usd("claude-opus-4", 100, 50, 1000, 200).unwrap();
        assert_close(cost, 0.0105);

        // claude-sonnet-5, 1M tokens in each slot: 3 + 15 + 0.3 + 3.75 = 22.05.
        let cost = cost_usd("claude-sonnet-5", 1_000_000, 1_000_000, 1_000_000, 1_000_000)
            .unwrap();
        assert_close(cost, 22.05);
    }

    #[test]
    fn unknown_model_returns_none() {
        assert_eq!(cost_usd("mistral-large-2", 100, 100, 0, 0), None);
        assert_eq!(price_for("unknown-model"), None);
    }

    #[test]
    fn prefix_matching_resolves_model_variants() {
        assert_eq!(price_for("claude-sonnet-5").unwrap().0, "claude-sonnet");
        assert_eq!(price_for("claude-opus-4-8").unwrap().0, "claude-opus-4");
        assert_eq!(price_for("claude-fable-5").unwrap().0, "claude-fable");
    }

    #[test]
    fn longer_prefix_wins() {
        // "gpt-5.5-codex" matches both "gpt-5" and "gpt-5.5" — the longer
        // prefix must win.
        assert_eq!(price_for("gpt-5.5-codex").unwrap().0, "gpt-5.5");
        assert_eq!(price_for("gpt-5.1-codex-max").unwrap().0, "gpt-5");
    }
}
