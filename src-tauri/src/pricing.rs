//! Static model price table (USD per 1M tokens) and cost computation.
//!
//! Prices are public API list prices, kept as code constants on purpose
//! (plan A3 — no remote fetch). Last verified 2026-09-18 against
//! <https://platform.claude.com/docs/en/about-claude/pricing> and
//! <https://developers.openai.com/api/docs/pricing>. To update: edit
//! `PRICE_TABLE` below and adjust the hand-computed expectations in the tests.
//! Unknown models yield `None`, which the UI renders as "N/A" — so a model
//! missing here silently drops that usage out of the cost figures.
//!
//! Cache columns: Anthropic bills cache writes (we use the 5-minute rate, what
//! Claude Code writes by default) and cache reads separately. OpenAI bills
//! cached input only, with no separate write charge, so those rows use 0.

/// USD per 1M tokens for each of the four disjoint token slots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_write_per_mtok: f64,
}

/// Shorthand so the table below stays readable.
const fn p(
    input_per_mtok: f64,
    output_per_mtok: f64,
    cache_read_per_mtok: f64,
    cache_write_per_mtok: f64,
) -> ModelPrice {
    ModelPrice {
        input_per_mtok,
        output_per_mtok,
        cache_read_per_mtok,
        cache_write_per_mtok,
    }
}

/// Prefix-matched price table. Matching picks the longest prefix that matches
/// the model string, so order here does not matter and a family-wide entry
/// (e.g. "gpt-5.1") also prices its variants ("gpt-5.1-codex-max").
const PRICE_TABLE: &[(&str, ModelPrice)] = &[
    // ---- Anthropic (input, output, cache read, 5m cache write) ----
    ("claude-opus-5", p(5.0, 25.0, 0.50, 6.25)),
    ("claude-opus-4-8", p(5.0, 25.0, 0.50, 6.25)),
    ("claude-opus-4-7", p(5.0, 25.0, 0.50, 6.25)),
    ("claude-opus-4-6", p(5.0, 25.0, 0.50, 6.25)),
    ("claude-opus-4-5", p(5.0, 25.0, 0.50, 6.25)),
    // Opus 4 / 4.1 (retired) kept the old, much higher rates.
    ("claude-opus-4", p(15.0, 75.0, 1.50, 18.75)),
    // Fable/Mythos 5.1 cache hits are 0.025x input, not the usual 0.1x.
    ("claude-fable-5-1", p(10.0, 50.0, 0.25, 12.50)),
    ("claude-fable", p(10.0, 50.0, 1.00, 12.50)),
    ("claude-mythos-5-1", p(10.0, 50.0, 0.25, 12.50)),
    ("claude-mythos", p(10.0, 50.0, 1.00, 12.50)),
    ("claude-sonnet-5", p(2.0, 10.0, 0.20, 2.50)),
    ("claude-sonnet", p(3.0, 15.0, 0.30, 3.75)),
    ("claude-haiku-3", p(0.80, 4.0, 0.08, 1.00)),
    ("claude-haiku", p(1.0, 5.0, 0.10, 1.25)),
    // ---- OpenAI (no separate cache-write charge) ----
    ("gpt-6-astra", p(10.0, 50.0, 1.00, 0.0)),
    ("gpt-6", p(10.0, 50.0, 1.00, 0.0)),
    ("gpt-5.6-terra", p(2.0, 12.0, 0.20, 0.0)),
    // Bare "gpt-5.6" and "-sol" bill at the flagship 5.6 rate.
    ("gpt-5.6", p(4.0, 20.0, 0.40, 0.0)),
    ("gpt-5.5", p(5.0, 30.0, 0.50, 0.0)),
    ("gpt-5.4", p(2.50, 15.0, 0.25, 0.0)),
    ("gpt-5.3", p(1.75, 14.0, 0.175, 0.0)),
    ("gpt-5.2", p(1.75, 14.0, 0.175, 0.0)),
    ("gpt-5.1", p(1.25, 10.0, 0.125, 0.0)),
    ("gpt-5", p(1.25, 10.0, 0.125, 0.0)),
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

/// USD saved by cache reads vs paying the full input rate for the same
/// tokens: `cache_read * (input rate - cache-read rate)`. `None` for unknown
/// models; 0-savings models (rates equal) return Some(0.0).
pub fn cache_savings_usd(model: &str, cache_read_tokens: u64) -> Option<f64> {
    let (_, price) = price_for(model)?;
    let per_tok = (price.input_per_mtok - price.cache_read_per_mtok).max(0.0) / 1_000_000.0;
    Some(cache_read_tokens as f64 * per_tok)
}

/// Cost in USD for the four disjoint token slots of one event or bucket.
/// Unknown model → `None` (UI "N/A").
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

    /// 1M tokens in each of the four slots, so the expected value is just the
    /// four rates added up.
    fn cost_1m_each(model: &str) -> f64 {
        cost_usd(model, 1_000_000, 1_000_000, 1_000_000, 1_000_000)
            .unwrap_or_else(|| panic!("{model} should be priced"))
    }

    #[test]
    fn known_model_cost_sums_four_rates() {
        // claude-opus-5: 100*5 + 50*25 + 1000*0.5 + 200*6.25 per MTok
        // = 0.0005 + 0.00125 + 0.0005 + 0.00125 = 0.0035 USD.
        assert_close(cost_usd("claude-opus-5", 100, 50, 1000, 200).unwrap(), 0.0035);
        assert_close(cost_1m_each("claude-sonnet-5"), 2.0 + 10.0 + 0.20 + 2.50);
    }

    #[test]
    fn unknown_model_returns_none() {
        assert_eq!(cost_usd("mistral-large-2", 100, 100, 0, 0), None);
        assert_eq!(price_for("unknown-model"), None);
    }

    /// Regression: these are the models actually in the logs as of 2026-09.
    /// When any of them is missing from the table the whole card's cost
    /// collapses to "API 환산 N/A", which is how this was first noticed.
    #[test]
    fn current_models_all_priced() {
        for model in [
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-fable-5",
            "claude-fable-5-1",
            "claude-sonnet-5",
            "claude-haiku-4-5-20251001",
            "gpt-6-astra",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.3-codex",
            "gpt-5.2-codex",
            "gpt-5.1-codex-max",
            "gpt-5-codex",
        ] {
            assert!(
                cost_usd(model, 1, 1, 1, 1).is_some(),
                "{model} must be priced"
            );
        }
        assert_close(cost_1m_each("claude-opus-5"), 36.75);
        assert_close(cost_1m_each("claude-fable-5-1"), 72.75);
        assert_close(cost_1m_each("gpt-6-astra"), 61.0);
        assert_close(cost_1m_each("gpt-5.6-sol"), 24.40);
    }

    /// Opus 4.5+ dropped to a third of the retired Opus 4/4.1 rates; a single
    /// "claude-opus-4" entry used to bill 4.8 at the old price.
    #[test]
    fn opus_generations_are_priced_apart() {
        assert_close(cost_1m_each("claude-opus-4-8"), 36.75);
        assert_close(cost_1m_each("claude-opus-4-1"), 110.25);
    }

    #[test]
    fn prefix_matching_resolves_model_variants() {
        assert_eq!(price_for("claude-sonnet-5").unwrap().0, "claude-sonnet-5");
        assert_eq!(price_for("claude-sonnet-4-6").unwrap().0, "claude-sonnet");
        assert_eq!(price_for("claude-opus-4-8").unwrap().0, "claude-opus-4-8");
        assert_eq!(price_for("claude-fable-5-1").unwrap().0, "claude-fable-5-1");
        assert_eq!(price_for("claude-fable-5").unwrap().0, "claude-fable");
    }

    #[test]
    fn longer_prefix_wins() {
        // "gpt-5.5-codex" matches both "gpt-5" and "gpt-5.5" — the longer
        // prefix must win. Same for the 5.6 variants over bare "gpt-5".
        assert_eq!(price_for("gpt-5.5-codex").unwrap().0, "gpt-5.5");
        assert_eq!(price_for("gpt-5.1-codex-max").unwrap().0, "gpt-5.1");
        assert_eq!(price_for("gpt-5.6-sol").unwrap().0, "gpt-5.6");
        assert_eq!(price_for("gpt-5.6-terra").unwrap().0, "gpt-5.6-terra");
        assert_eq!(price_for("gpt-6-astra").unwrap().0, "gpt-6-astra");
    }
}
