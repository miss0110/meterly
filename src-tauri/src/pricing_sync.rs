//! Periodic refresh of the model price table from a maintained public feed.
//!
//! The built-in [`crate::pricing`] table is a snapshot: every new model ships
//! unpriced, and its usage silently drops out of the cost figures until the
//! next meterly release. This module keeps that table current by fetching
//! LiteLLM's `model_prices_and_context_window.json` — a machine-readable feed
//! that tracks Anthropic's and OpenAI's list prices — once a day, slimming it
//! to the bare model ids meterly actually sees in logs (~190 entries, ~12 KB)
//! and caching that to disk.
//!
//! No usage data leaves the machine: this is a plain GET of a public file. The
//! built-in table stays the fallback, so meterly prices correctly offline, on
//! first run, and whenever a fetch fails.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::pricing::ModelPrice;

const SOURCE_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
/// Prices move rarely; once a day is plenty and keeps this invisible.
pub const REFRESH_INTERVAL_SECS: i64 = 24 * 60 * 60;
const HTTP_TIMEOUT_SECS: u64 = 30;

/// One entry of the upstream feed (only the fields we price with).
#[derive(Deserialize)]
struct FeedEntry {
    litellm_provider: Option<String>,
    input_cost_per_token: Option<f64>,
    output_cost_per_token: Option<f64>,
    cache_read_input_token_cost: Option<f64>,
    cache_creation_input_token_cost: Option<f64>,
}

/// Our slimmed on-disk form: `[input, output, cache_read, cache_write]` USD per
/// 1M tokens, keyed by the bare model id.
#[derive(Serialize, Deserialize)]
pub struct CachedPrices {
    pub fetched_at: DateTime<Utc>,
    pub source: String,
    pub models: BTreeMap<String, [f64; 4]>,
}

fn cache_file() -> Option<PathBuf> {
    Some(crate::cache::cache_path().parent()?.join("model-prices.json"))
}

fn to_entries(models: &BTreeMap<String, [f64; 4]>) -> Vec<(String, ModelPrice)> {
    models
        .iter()
        .map(|(name, v)| {
            (
                name.clone(),
                ModelPrice {
                    input_per_mtok: v[0],
                    output_per_mtok: v[1],
                    cache_read_per_mtok: v[2],
                    cache_write_per_mtok: v[3],
                },
            )
        })
        .collect()
}

/// Read the on-disk cache (if any) and install it. Runs at startup so a fetched
/// table survives restarts and is available before the first refresh.
pub fn load_cached() -> Option<CachedPrices> {
    let content = std::fs::read_to_string(cache_file()?).ok()?;
    let cached: CachedPrices = serde_json::from_str(&content).ok()?;
    crate::pricing::install_overlay(to_entries(&cached.models));
    crate::logging::info(&format!(
        "pricing: loaded {} fetched model prices (fetched {})",
        cached.models.len(),
        cached.fetched_at.to_rfc3339()
    ));
    Some(cached)
}

/// When the cached table was last fetched, for the Settings panel.
pub fn last_fetched() -> Option<DateTime<Utc>> {
    let content = std::fs::read_to_string(cache_file()?).ok()?;
    serde_json::from_str::<CachedPrices>(&content)
        .ok()
        .map(|c| c.fetched_at)
}

/// Keep only the bare Anthropic/OpenAI ids — the shapes that appear as `model`
/// in Claude Code and Codex logs. Provider-prefixed variants (`azure/…`,
/// `bedrock/…`) would never match and would bloat the cache 20x.
fn slim(feed: BTreeMap<String, FeedEntry>) -> BTreeMap<String, [f64; 4]> {
    feed.into_iter()
        .filter(|(name, e)| {
            !name.contains('/')
                && matches!(e.litellm_provider.as_deref(), Some("anthropic") | Some("openai"))
                && e.input_cost_per_token.is_some()
        })
        .map(|(name, e)| {
            let per_mtok = |v: Option<f64>| v.unwrap_or(0.0) * 1_000_000.0;
            (
                name,
                [
                    per_mtok(e.input_cost_per_token),
                    per_mtok(e.output_cost_per_token),
                    per_mtok(e.cache_read_input_token_cost),
                    per_mtok(e.cache_creation_input_token_cost),
                ],
            )
        })
        .collect()
}

/// Fetch the feed on a dedicated plain thread. reqwest's blocking client
/// panics when entered from an async-runtime thread, so isolate it the same
/// way the org reporter does; `join` turns a panic into an error.
fn fetch_feed() -> Result<BTreeMap<String, FeedEntry>, String> {
    let work = || -> Result<BTreeMap<String, FeedEntry>, String> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(SOURCE_URL)
            .header("User-Agent", format!("meterly/{}", env!("CARGO_PKG_VERSION")))
            .send()
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status().as_u16()));
        }
        resp.json::<BTreeMap<String, FeedEntry>>()
            .map_err(|e| e.to_string())
    };
    std::thread::spawn(work)
        .join()
        .map_err(|_| "내부 오류 — 가격 갱신 스레드가 중단됐습니다".to_string())?
}

/// Whether the cache is old enough (or missing) to warrant a fetch.
pub fn is_due() -> bool {
    last_fetched().map_or(true, |at| {
        (Utc::now() - at).num_seconds() >= REFRESH_INTERVAL_SECS
    })
}

/// Fetch, slim, cache and install. `force` skips the interval check (the
/// Settings "지금 갱신" button). Returns how many models were installed.
pub fn refresh(force: bool) -> Result<usize, String> {
    if !force && !is_due() {
        return Ok(crate::pricing::overlay_len());
    }
    let models = slim(fetch_feed()?);
    if models.is_empty() {
        return Err("가격 목록이 비어 있습니다 (형식이 바뀌었을 수 있음)".into());
    }
    let count = models.len();
    let cached = CachedPrices {
        fetched_at: Utc::now(),
        source: SOURCE_URL.to_string(),
        models,
    };
    if let Some(path) = cache_file() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string(&cached) {
            Ok(body) => {
                if let Err(err) = std::fs::write(&path, body) {
                    crate::logging::warn(&format!("pricing: cache write failed: {err}"));
                }
            }
            Err(err) => crate::logging::warn(&format!("pricing: serialize failed: {err}")),
        }
    }
    crate::pricing::install_overlay(to_entries(&cached.models));
    crate::logging::info(&format!("pricing: refreshed {count} model prices"));
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(json: &str) -> BTreeMap<String, FeedEntry> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn slim_keeps_bare_anthropic_and_openai_ids() {
        let models = slim(feed(
            r#"{
              "claude-opus-5": {"litellm_provider":"anthropic","input_cost_per_token":0.000005,
                "output_cost_per_token":0.000025,"cache_read_input_token_cost":0.0000005,
                "cache_creation_input_token_cost":0.00000625},
              "gpt-6-astra": {"litellm_provider":"openai","input_cost_per_token":0.00001,
                "output_cost_per_token":0.00005,"cache_read_input_token_cost":0.000001},
              "azure/gpt-6-astra": {"litellm_provider":"azure","input_cost_per_token":0.00001},
              "bedrock/claude-opus-5": {"litellm_provider":"bedrock","input_cost_per_token":0.000005},
              "gemini-3-pro": {"litellm_provider":"vertex_ai","input_cost_per_token":0.000002},
              "no-price": {"litellm_provider":"openai"}
            }"#,
        ));
        assert_eq!(
            models.keys().collect::<Vec<_>>(),
            vec!["claude-opus-5", "gpt-6-astra"]
        );
        // Per-token costs are scaled to per-1M-token, the unit meterly prices in.
        assert_eq!(models["claude-opus-5"], [5.0, 25.0, 0.5, 6.25]);
        // A missing cache-write cost (OpenAI) becomes 0, not a dropped entry.
        assert_eq!(models["gpt-6-astra"], [10.0, 50.0, 1.0, 0.0]);
    }

    /// Live end-to-end check of the real feed (network). Ignored by default:
    /// `cargo test -- --ignored live_refresh`.
    #[test]
    #[ignore]
    fn live_refresh_prices_todays_models() {
        let dir = std::env::temp_dir().join(format!("meterly-price-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("METERLY_CACHE_DIR", &dir);
        let count = refresh(true).expect("refresh should succeed");
        assert!(count > 50, "expected a substantial table, got {count}");
        for model in ["claude-opus-5", "gpt-6-astra", "claude-fable-5-1"] {
            assert!(
                crate::pricing::price_for(model).is_some(),
                "{model} should be priced after a refresh"
            );
        }
        // The cache round-trips: a fresh load re-installs the same table.
        crate::pricing::install_overlay(Vec::new());
        let cached = load_cached().expect("cache should be readable");
        assert_eq!(cached.models.len(), count);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installed_overlay_prices_models_the_builtin_table_lacks() {
        crate::pricing::install_overlay(vec![(
            "totally-new-model-9".to_string(),
            ModelPrice {
                input_per_mtok: 1.0,
                output_per_mtok: 2.0,
                cache_read_per_mtok: 0.1,
                cache_write_per_mtok: 0.0,
            },
        )]);
        assert_eq!(
            crate::pricing::matched_prefix("totally-new-model-9-preview").as_deref(),
            Some("totally-new-model-9")
        );
        // The built-in table still answers for everything else.
        assert_eq!(
            crate::pricing::matched_prefix("claude-opus-5").as_deref(),
            Some("claude-opus-5")
        );
        crate::pricing::install_overlay(Vec::new()); // don't leak into other tests
    }
}
