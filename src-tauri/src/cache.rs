//! Persistent aggregate cache (T6): versioned single JSON file, atomic
//! temp+rename writes. The raw logs stay the source of truth — any anomaly
//! is recovered by a per-source rebuild, never by partial subtraction.
//!
//! Privacy (R9): the schema below holds token counts, dates, model names,
//! session uuids and cursors only — never conversation content.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::aggregate::{DailyBucket, HourlyBucket};
use crate::model::{RateLimitStatus, UsageEvent};
use crate::sources::SourceCursors;

// v2: hourly buckets for the weekday×hour heatmap (+ tray display setting).
// A version bump discards old caches → one full rebuild fills history.
pub const CACHE_VERSION: u32 = 2;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheV1 {
    pub version: u32,
    /// First date covered by aggregates (min over dashboard ranges, V2-A3).
    pub backfill_start: Option<chrono::NaiveDate>,
    /// Codex incremental cursors (uuid-keyed). Claude has none: its scan is
    /// a full re-parse whose events REPLACE the claude buckets each cycle.
    #[serde(default)]
    pub codex_cursors: SourceCursors,
    /// Codex daily buckets — additive across incremental scans.
    #[serde(default)]
    pub daily_codex: Vec<DailyBucket>,
    /// Claude daily buckets — rebuilt from scratch every scan.
    #[serde(default)]
    pub daily_claude: Vec<DailyBucket>,
    /// Recent events (~6h) for the Claude rolling-window estimate.
    #[serde(default)]
    pub recent_events: Vec<UsageEvent>,
    /// Last measured Codex rate-limit snapshot — shown right after app
    /// restart until the next token_count event refreshes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_rate_limit: Option<RateLimitStatus>,
    /// Hourly buckets (v2) — heatmap. Same replace/additive split as daily.
    #[serde(default)]
    pub hourly_claude: Vec<HourlyBucket>,
    #[serde(default)]
    pub hourly_codex: Vec<HourlyBucket>,
    /// Tray title mode: "tokens" (default) | "cost" | "icon".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tray_display: Option<String>,
    /// Last real Claude `/usage` readout (via the `claude` CLI) with the time
    /// it was fetched — throttles the shell-out and survives restarts. Falls
    /// back to the local estimate when absent/stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_cli_usage: Option<(chrono::DateTime<chrono::Utc>, RateLimitStatus)>,
    /// Last real Codex plan usage read via `codex app-server` with the time it
    /// was fetched — throttles the spawn and survives restarts. Falls back to
    /// the local log snapshot (`codex_rate_limit`) when absent/failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_appserver_usage: Option<(chrono::DateTime<chrono::Utc>, RateLimitStatus)>,
    /// Folder (in a synced cloud drive) where this device writes its usage
    /// file and reads the others'. `None` = multi-device sync off (local only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_dir: Option<String>,
    /// Whether plan-usage threshold notifications fire. `None` = default (on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alerts_enabled: Option<bool>,
    /// Custom alert thresholds (percent, ascending). `None` = default 30/50/70/90.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alert_thresholds: Option<Vec<u8>>,
    /// Optional monthly token budget (raw tokens). Drives the dashboard's
    /// this-month progress bar + month-end projection. `None` = no budget set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_budget_tokens: Option<u64>,
    /// Date/time display preference: "auto" (OS locale) | "iso" | "us" | "eu".
    /// `None` = auto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_format: Option<String>,
    /// Last update version the user was notified about — one notification per
    /// version, surviving restarts (the popover banner still shows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_notified_update: Option<String>,
    /// Limit-gauge display: "used" (사용한 양, default) | "remaining" (남은 양).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent_display: Option<String>,
    /// Monday of the week the last weekly report was sent for (dedup).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_weekly_report: Option<chrono::NaiveDate>,
    /// Org reporting (Settings values; a managed file overrides url/token).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_token: Option<String>,
    /// Personal identifier (e.g. 사번) the org told the user to enter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_user_id: Option<String>,
    /// Set after a successful /register — reporting only runs when true.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub org_registered: bool,
    /// Sources included in org reports ("claude_code"/"codex"). `None` = all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_sources: Option<Vec<String>>,
    /// Last successful /usage report (throttles to the report interval).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_org_report: Option<chrono::DateTime<chrono::Utc>>,
}

/// Cache file path: `~/Library/Application Support/com.meterly.app/` on
/// macOS (per plan); overridable for tests via `METERLY_CACHE_DIR`.
pub fn cache_path() -> PathBuf {
    let dir = std::env::var_os("METERLY_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|d| d.join("com.meterly.app")))
        .unwrap_or_else(|| PathBuf::from(".meterly"));
    dir.join("cache-v1.json")
}

/// Load the cache. Version mismatch or parse failure → `None` (caller
/// rebuilds from logs — discard-and-rebuild is the only recovery path).
pub fn load(path: &PathBuf) -> Option<CacheV1> {
    let content = fs::read_to_string(path).ok()?;
    let cache: CacheV1 = serde_json::from_str(&content).ok()?;
    (cache.version == CACHE_VERSION).then_some(cache)
}

/// Atomic save: write to a temp file in the same directory, then rename.
pub fn save(path: &PathBuf, cache: &CacheV1) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "cache path has no parent")
    })?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(".cache-v1.json.tmp");
    let body = serde_json::to_string(cache)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(&tmp, body)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meterly-cache-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir.join("cache-v1.json")
    }

    #[test]
    fn save_load_roundtrip_and_version_gate() {
        let path = temp_cache_path("roundtrip");
        let mut cache = CacheV1 {
            version: CACHE_VERSION,
            ..Default::default()
        };
        cache.backfill_start = chrono::NaiveDate::from_ymd_opt(2026, 2, 1);
        save(&path, &cache).unwrap();
        assert_eq!(load(&path), Some(cache.clone()));

        // Version mismatch → None (discard & rebuild).
        cache.version = 999;
        save(&path, &cache).unwrap();
        assert_eq!(load(&path), None);

        // Corrupt file → None, no panic.
        fs::write(&path, "{not json").unwrap();
        assert_eq!(load(&path), None);
    }
}
