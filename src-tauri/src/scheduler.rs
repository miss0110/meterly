//! Refresh engine + polling scheduler (T8).
//!
//! Every cycle (default 3 min) each source scans in ISOLATION: one source's
//! Error/panic never blocks the other (AC4). Claude re-parses fully and its
//! buckets are REPLACED; Codex scans incrementally via uuid cursors and its
//! buckets are ADDITIVE (rebuild-on-flag is the only recovery path).

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{Local, Utc};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::aggregate::{self, DailyBucket};
use crate::cache::{self, CacheV1};
use crate::model::{RateLimitStatus, SourceHealth, SourceId};
use crate::sources::{self, SourceCursors, UsageSource};

pub const REFRESH_INTERVAL_SECS: u64 = 180;

/// Minimum gap between `claude -p "/usage"` shell-outs. The call spawns a
/// process (~seconds), so it is throttled independently of the scan cycle.
pub const CLAUDE_USAGE_MIN_INTERVAL_SECS: i64 = 120;

/// Minimum gap between `codex app-server` reads (real Codex plan usage). Same
/// rationale as the Claude one — it spawns a process, so throttle it.
pub const CODEX_USAGE_MIN_INTERVAL_SECS: i64 = 120;

/// Seconds between tray-title rotations (전체 ↔ 이 기기) when 2+ devices sync.
pub const TRAY_ROTATE_SECS: u64 = 5;

/// Latest tray-title states + rotation position (managed Tauri state). The menu
/// bar is narrow, so instead of showing everything at once we cycle through a
/// list of labeled totals (이 기기/전체 × 토큰/비용) every [`TRAY_ROTATE_SECS`].
#[derive(Default, Clone)]
pub struct TrayInfo {
    /// Labeled titles to rotate through; empty = icon mode (no title).
    pub states: Vec<String>,
    pub idx: usize,
}
pub struct TrayRotation(pub Mutex<TrayInfo>);

pub struct AppState(pub Mutex<Engine>);

pub struct Engine {
    cache_path: PathBuf,
    pub cache: CacheV1,
    runtimes: Vec<Runtime>,
    /// Limit-notification dedup: (threshold %, window resets_at) already
    /// notified for. Cleared when the window rolls over or usage drops.
    notified_limit: Option<(u8, chrono::DateTime<Utc>)>,
    /// Logged-in account per source (read once from local auth files).
    claude_account: Option<String>,
    codex_account: Option<String>,
}

struct Runtime {
    id: SourceId,
    display_name: &'static str,
    source: Box<dyn UsageSource>,
}

// ---- IPC payload shapes (plan: Contract surface) ----

#[derive(Debug, Clone, Serialize)]
pub struct TokenBreakdown {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceSummary {
    pub id: SourceId,
    pub display_name: String,
    pub health: SourceHealth,
    pub today_tokens: TokenBreakdown,
    pub today_cost_usd: Option<f64>,
    /// USD saved today by cache reads vs full input rate (known models).
    pub today_cache_saved_usd: Option<f64>,
    pub rate_limit: RateLimitStatus,
    /// Daily totals for the last 7 days (oldest → today) — popover/card
    /// sparklines.
    pub last7_totals: Vec<u64>,
    /// Logged-in account this source measures (e.g. "email · Team"), if known.
    pub account: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub generated_at: chrono::DateTime<Utc>,
    pub sources: Vec<SourceSummary>,
}

// ---- Multi-device aggregation payloads ----

#[derive(Debug, Clone, Serialize)]
pub struct DeviceSourceUsage {
    pub id: SourceId,
    pub display_name: String,
    pub today_tokens: TokenBreakdown,
    pub today_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceSummary {
    pub device_id: String,
    pub hostname: String,
    pub updated_at: chrono::DateTime<Utc>,
    /// True for the machine this app instance runs on.
    pub is_current: bool,
    pub sources: Vec<DeviceSourceUsage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DevicesData {
    /// False when no sync folder is configured (only the current device shown).
    pub sync_enabled: bool,
    pub devices: Vec<DeviceSummary>,
}

/// Sum a source's tokens + cost for one day from any bucket iterator (local
/// in-memory buckets or a synced device file). Cost is recomputed via pricing.
fn day_usage<'a>(
    buckets: impl Iterator<Item = &'a DailyBucket>,
    source: SourceId,
    date: chrono::NaiveDate,
) -> (TokenBreakdown, Option<f64>) {
    let mut tk = TokenBreakdown {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_creation: 0,
        total: 0,
    };
    let mut cost: Option<f64> = None;
    for b in buckets.filter(|b| b.source == source && b.date == date) {
        tk.input += b.input;
        tk.output += b.output;
        tk.cache_read += b.cache_read;
        tk.cache_creation += b.cache_creation;
        if let Some(c) = b.cost_usd() {
            *cost.get_or_insert(0.0) += c;
        }
    }
    tk.total = tk.input + tk.output + tk.cache_read + tk.cache_creation;
    (tk, cost)
}

/// Sum one device's daily buckets from `start` onward into a range total.
fn device_range_usage(
    daily: &[DailyBucket],
    hostname: &str,
    is_current: bool,
    updated_at: chrono::DateTime<Utc>,
    start: chrono::NaiveDate,
) -> DeviceRangeUsage {
    let mut tk = TokenBreakdown {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_creation: 0,
        total: 0,
    };
    let mut cost: Option<f64> = None;
    for b in daily.iter().filter(|b| b.date >= start) {
        tk.input += b.input;
        tk.output += b.output;
        tk.cache_read += b.cache_read;
        tk.cache_creation += b.cache_creation;
        if let Some(c) = b.cost_usd() {
            *cost.get_or_insert(0.0) += c;
        }
    }
    tk.total = tk.input + tk.output + tk.cache_read + tk.cache_creation;
    DeviceRangeUsage {
        hostname: hostname.to_string(),
        updated_at,
        is_current,
        tokens: tk,
        cost_usd: cost,
    }
}

/// Machine name — used BOTH as the device identity/file key and the display
/// label. Keying by hostname (not a random id) means relaunching or
/// reinstalling on the same machine reuses its file instead of orphaning the
/// old one and double-counting.
fn hostname() -> String {
    #[cfg(target_os = "windows")]
    let h = std::env::var("COMPUTERNAME").ok();
    #[cfg(not(target_os = "windows"))]
    let h = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok());
    h.map(|s| s.trim().trim_end_matches(".local").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardRow {
    pub period: String,
    pub source: SourceId,
    pub model: Option<String>,
    pub tokens: TokenBreakdown,
    pub cost_usd: Option<f64>,
}

/// Per-host token/cost total over the selected dashboard range (combined view).
#[derive(Debug, Clone, Serialize)]
pub struct DeviceRangeUsage {
    pub hostname: String,
    pub updated_at: chrono::DateTime<Utc>,
    pub is_current: bool,
    pub tokens: TokenBreakdown,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardData {
    pub range: String,
    pub rows: Vec<DashboardRow>,
    pub timezone_note: String,
    /// Per-host totals for the range — only populated in the combined ("all")
    /// scope; empty for the local view.
    pub devices: Vec<DeviceRangeUsage>,
}

impl Engine {
    pub fn new() -> Self {
        let cache_path = cache::cache_path();
        let cache = cache::load(&cache_path).unwrap_or_default();
        let runtimes = sources::registry()
            .into_iter()
            .filter_map(|entry| {
                let make = entry.make_source?;
                Some(Runtime {
                    id: entry.id,
                    display_name: entry.display_name,
                    source: make(entry.root_path),
                })
            })
            .collect();
        Self {
            cache_path,
            cache,
            runtimes,
            notified_limit: None,
            claude_account: crate::accounts::claude_account(),
            codex_account: crate::accounts::codex_account(),
        }
    }

    /// One refresh cycle: scan every source (isolated), fold aggregates,
    /// persist the cache, return the fresh summary.
    pub fn refresh(&mut self) -> Summary {
        let today = Local::now().date_naive();
        let window_start = aggregate::backfill_start(today);
        self.cache.version = cache::CACHE_VERSION;
        self.cache.backfill_start = Some(window_start);

        for rt in &mut self.runtimes {
            // Isolation (AC4): a panicking source must not kill the cycle.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match rt.id {
                SourceId::ClaudeCode => {
                    let outcome = rt.source.scan(&SourceCursors::default());
                    if !matches!(rt.source.health(), SourceHealth::Error { .. }) {
                        // Full re-parse: REPLACE claude aggregates.
                        self.cache.daily_claude.clear();
                        aggregate::ingest(
                            &mut self.cache.daily_claude,
                            &outcome.events,
                            window_start,
                        );
                        self.cache.hourly_claude.clear();
                        aggregate::ingest_hourly(
                            &mut self.cache.hourly_claude,
                            &outcome.events,
                            window_start,
                        );
                        let cutoff =
                            Utc::now() - chrono::Duration::hours(aggregate::RECENT_RETENTION_HOURS);
                        self.cache
                            .recent_events
                            .retain(|e| e.source != SourceId::ClaudeCode);
                        self.cache.recent_events.extend(
                            outcome
                                .events
                                .iter()
                                .filter(|e| e.timestamp >= cutoff)
                                .cloned(),
                        );
                    }
                }
                SourceId::Codex => {
                    let mut outcome = rt.source.scan(&self.cache.codex_cursors.clone());
                    if outcome.needs_rebuild {
                        // Truncation backstop: discard codex state, rescan
                        // once from scratch (always-correct recovery).
                        self.cache.codex_cursors = SourceCursors::default();
                        self.cache.daily_codex.clear();
                        self.cache.hourly_codex.clear();
                        outcome = rt.source.scan(&SourceCursors::default());
                    }
                    aggregate::ingest(&mut self.cache.daily_codex, &outcome.events, window_start);
                    aggregate::ingest_hourly(
                        &mut self.cache.hourly_codex,
                        &outcome.events,
                        window_start,
                    );
                    if let Some(cursors) = outcome.cursors {
                        self.cache.codex_cursors = cursors;
                    }
                    let cutoff =
                        Utc::now() - chrono::Duration::hours(aggregate::RECENT_RETENTION_HOURS);
                    self.cache
                        .recent_events
                        .extend(outcome.events.iter().filter(|e| e.timestamp >= cutoff).cloned());
                }
            }));
            if result.is_err() {
                eprintln!("meterly: source {:?} panicked during scan (isolated)", rt.id);
            }
        }

        // Retention + window pruning.
        let cutoff = Utc::now() - chrono::Duration::hours(aggregate::RECENT_RETENTION_HOURS);
        self.cache.recent_events.retain(|e| e.timestamp >= cutoff);
        aggregate::prune(&mut self.cache.daily_claude, window_start);
        aggregate::prune(&mut self.cache.daily_codex, window_start);
        aggregate::prune_hourly(&mut self.cache.hourly_claude, window_start);
        aggregate::prune_hourly(&mut self.cache.hourly_codex, window_start);

        // Real Claude /usage via the `claude` CLI — throttled (shell-out is
        // ~seconds). Keep the last good reading on failure; only bump the
        // timestamp so we retry no more than once per interval.
        let due = self.cache.claude_cli_usage.as_ref().map_or(true, |(at, _)| {
            (Utc::now() - *at).num_seconds() >= CLAUDE_USAGE_MIN_INTERVAL_SECS
        });
        if due {
            let fetched = crate::sources::claude_usage::fetch();
            if matches!(fetched, RateLimitStatus::Cli { .. }) {
                self.cache.claude_cli_usage = Some((Utc::now(), fetched));
            } else if let Some((at, _)) = self.cache.claude_cli_usage.as_mut() {
                *at = Utc::now();
            }
        }

        // Real Codex plan usage via `codex app-server` — same throttle/keep-last
        // policy as Claude. This is the live number the ChatGPT panel shows, so
        // it supersedes the (often 0%) local log snapshot.
        let codex_due = self
            .cache
            .codex_appserver_usage
            .as_ref()
            .map_or(true, |(at, _)| {
                (Utc::now() - *at).num_seconds() >= CODEX_USAGE_MIN_INTERVAL_SECS
            });
        if codex_due {
            let fetched = crate::sources::codex_usage::fetch();
            if matches!(fetched, RateLimitStatus::Measured { .. }) {
                self.cache.codex_appserver_usage = Some((Utc::now(), fetched));
            } else if let Some((at, _)) = self.cache.codex_appserver_usage.as_mut() {
                *at = Utc::now();
            }
        }

        // Multi-device: publish this device's buckets to the shared folder.
        if let Some(dir) = self.cache.sync_dir.clone() {
            let name = hostname();
            let file = crate::devicesync::DeviceFile {
                device_id: name.clone(),
                hostname: name,
                updated_at: Utc::now(),
                daily: self.all_buckets().into_iter().cloned().collect(),
            };
            if let Err(err) = crate::devicesync::write(std::path::Path::new(&dir), &file) {
                eprintln!("meterly: device usage write failed: {err}");
            }
        }

        if let Err(err) = cache::save(&self.cache_path, &self.cache) {
            eprintln!("meterly: cache save failed: {err}");
        }
        self.summary()
    }

    /// Per-device today usage for the combined view. The current device comes
    /// from live in-memory buckets; others from their synced files (its own
    /// file is skipped to avoid double counting). Rate-limit % is intentionally
    /// absent here — it is account-global, not per-device.
    pub fn get_devices(&self) -> DevicesData {
        let today = Local::now().date_naive();
        let current_id = hostname();
        let mut devices = Vec::new();

        let cur_sources = self
            .runtimes
            .iter()
            .map(|rt| {
                let (tk, cost) = day_usage(self.all_buckets().into_iter(), rt.id, today);
                DeviceSourceUsage {
                    id: rt.id,
                    display_name: rt.display_name.to_string(),
                    today_tokens: tk,
                    today_cost_usd: cost,
                }
            })
            .collect();
        devices.push(DeviceSummary {
            device_id: current_id.clone(),
            hostname: hostname(),
            updated_at: Utc::now(),
            is_current: true,
            sources: cur_sources,
        });

        if let Some(dir) = &self.cache.sync_dir {
            for df in crate::devicesync::read_all(std::path::Path::new(dir)) {
                if df.device_id == current_id {
                    continue; // our own file — already covered by live buckets.
                }
                let sources = self
                    .runtimes
                    .iter()
                    .map(|rt| {
                        let (tk, cost) = day_usage(df.daily.iter(), rt.id, today);
                        DeviceSourceUsage {
                            id: rt.id,
                            display_name: rt.display_name.to_string(),
                            today_tokens: tk,
                            today_cost_usd: cost,
                        }
                    })
                    .collect();
                devices.push(DeviceSummary {
                    device_id: df.device_id,
                    hostname: df.hostname,
                    updated_at: df.updated_at,
                    is_current: false,
                    sources,
                });
            }
        }

        DevicesData {
            sync_enabled: self.cache.sync_dir.is_some(),
            devices,
        }
    }

    /// Build the summary from current state (no rescan).
    pub fn summary(&self) -> Summary {
        let today = Local::now().date_naive();
        let now = Utc::now();
        let sources = self
            .runtimes
            .iter()
            .map(|rt| {
                let buckets: Vec<&DailyBucket> = self
                    .all_buckets()
                    .into_iter()
                    .filter(|b| b.source == rt.id && b.date == today)
                    .collect();
                let mut tk = TokenBreakdown {
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_creation: 0,
                    total: 0,
                };
                let mut cost: Option<f64> = None;
                let mut saved: Option<f64> = None;
                for b in &buckets {
                    tk.input += b.input;
                    tk.output += b.output;
                    tk.cache_read += b.cache_read;
                    tk.cache_creation += b.cache_creation;
                    if let Some(c) = b.cost_usd() {
                        *cost.get_or_insert(0.0) += c;
                    }
                    if let Some(sv) = b
                        .model
                        .as_deref()
                        .and_then(|m| crate::pricing::cache_savings_usd(m, b.cache_read))
                    {
                        *saved.get_or_insert(0.0) += sv;
                    }
                }
                tk.total = tk.input + tk.output + tk.cache_read + tk.cache_creation;
                let last7_totals: Vec<u64> = (0..7)
                    .rev()
                    .map(|days_ago| {
                        let d = today - chrono::Duration::days(days_ago);
                        self.all_buckets()
                            .into_iter()
                            .filter(|b| b.source == rt.id && b.date == d)
                            .map(|b| b.total())
                            .sum()
                    })
                    .collect();
                let rate_limit = match rt.id {
                    SourceId::ClaudeCode => match &self.cache.claude_cli_usage {
                        // Real /usage readout when we have one; else estimate.
                        Some((_, rl @ RateLimitStatus::Cli { .. })) => rl.clone(),
                        _ => aggregate::claude_window_estimate(&self.cache.recent_events, now),
                    },
                    SourceId::Codex => {
                        // Live plan usage from `codex app-server` — matches the
                        // ChatGPT panel. The old local log snapshot is the
                        // source of the stale/0% reading we're replacing, so we
                        // no longer fall back to it: real number or nothing.
                        match &self.cache.codex_appserver_usage {
                            Some((_, rl @ RateLimitStatus::Measured { .. })) => rl.clone(),
                            _ => RateLimitStatus::Unavailable,
                        }
                    }
                };
                let account = match rt.id {
                    SourceId::ClaudeCode => self.claude_account.clone(),
                    SourceId::Codex => self.codex_account.clone(),
                };
                SourceSummary {
                    id: rt.id,
                    display_name: rt.display_name.to_string(),
                    health: rt.source.health(),
                    today_tokens: tk,
                    today_cost_usd: cost,
                    today_cache_saved_usd: saved,
                    rate_limit,
                    last7_totals,
                    account,
                }
            })
            .collect();
        Summary {
            generated_at: now,
            sources,
        }
    }

    pub fn dashboard(&self, range: &str, scope: &str) -> DashboardData {
        let today = Local::now().date_naive();
        let start = aggregate::range_start(range, today).unwrap_or(today);

        // Buckets to aggregate: this device always; other devices' synced
        // files too when scope == "all". Cloned into an owned list so local
        // (refs) and remote (owned) can be merged uniformly.
        let mut buckets: Vec<DailyBucket> = self.all_buckets().into_iter().cloned().collect();
        let mut devices: Vec<DeviceRangeUsage> = Vec::new();
        if scope == "all" {
            let current_id = hostname();
            devices.push(device_range_usage(&buckets, &current_id, true, Utc::now(), start));
            if let Some(dir) = &self.cache.sync_dir {
                for df in crate::devicesync::read_all(std::path::Path::new(dir)) {
                    if df.device_id == current_id {
                        continue; // our own file — local buckets already cover it.
                    }
                    devices.push(device_range_usage(
                        &df.daily,
                        &df.hostname,
                        false,
                        df.updated_at,
                        start,
                    ));
                    buckets.extend(df.daily.iter().cloned());
                }
            }
            devices.sort_by(|a, b| b.tokens.total.cmp(&a.tokens.total));
        } else if scope != "local" && scope != hostname() {
            // A specific other host: aggregate only that device's synced file.
            buckets = self
                .cache
                .sync_dir
                .as_ref()
                .and_then(|dir| {
                    crate::devicesync::read_all(std::path::Path::new(dir))
                        .into_iter()
                        .find(|df| df.device_id == scope)
                        .map(|df| df.daily)
                })
                .unwrap_or_default();
        }

        let mut rows: Vec<DashboardRow> = Vec::new();
        for b in &buckets {
            if b.date < start {
                continue;
            }
            let Some(period) = aggregate::period_key(range, b.date) else {
                continue;
            };
            let found = rows.iter_mut().find(|r| {
                r.period == period && r.source == b.source && r.model == b.model
            });
            match found {
                Some(row) => {
                    row.tokens.input += b.input;
                    row.tokens.output += b.output;
                    row.tokens.cache_read += b.cache_read;
                    row.tokens.cache_creation += b.cache_creation;
                    row.tokens.total += b.total();
                    if let Some(c) = b.cost_usd() {
                        *row.cost_usd.get_or_insert(0.0) += c;
                    }
                }
                None => rows.push(DashboardRow {
                    period,
                    source: b.source,
                    model: b.model.clone(),
                    tokens: TokenBreakdown {
                        input: b.input,
                        output: b.output,
                        cache_read: b.cache_read,
                        cache_creation: b.cache_creation,
                        total: b.total(),
                    },
                    cost_usd: b.cost_usd(),
                }),
            }
        }
        rows.sort_by(|a, b| a.period.cmp(&b.period));
        DashboardData {
            range: range.to_string(),
            rows,
            timezone_note: format!(
                "'오늘' 경계는 시스템 로컬 타임존({}) 자정 기준 — UTC 기준 도구와 다를 수 있음",
                Local::now().format("%Z")
            ),
            devices,
        }
    }

    fn all_buckets(&self) -> Vec<&DailyBucket> {
        self.cache
            .daily_claude
            .iter()
            .chain(self.cache.daily_codex.iter())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HeatmapCell {
    /// 0 = 월요일 … 6 = 일요일 (local tz).
    pub weekday: u8,
    pub hour: u8,
    pub total: u64,
}

impl Engine {
    /// Weekday×hour token totals over the whole backfill window.
    pub fn heatmap(&self) -> Vec<HeatmapCell> {
        use chrono::Datelike;
        let mut grid = [[0u64; 24]; 7];
        for b in self.cache.hourly_claude.iter().chain(self.cache.hourly_codex.iter()) {
            let wd = b.date.weekday().num_days_from_monday() as usize;
            grid[wd][b.hour as usize] += b.total;
        }
        let mut cells = Vec::with_capacity(7 * 24);
        for (wd, row) in grid.iter().enumerate() {
            for (h, total) in row.iter().enumerate() {
                cells.push(HeatmapCell {
                    weekday: wd as u8,
                    hour: h as u8,
                    total: *total,
                });
            }
        }
        cells
    }

    /// Export the current range's aggregate rows to ~/Downloads.
    /// Returns the written file path.
    pub fn export(&self, range: &str, format: &str) -> Result<String, String> {
        let data = self.dashboard(range, "local");
        let today = Local::now().format("%Y%m%d-%H%M%S");
        let dir = dirs::download_dir().ok_or("다운로드 폴더를 찾을 수 없음")?;
        let ext = if format == "csv" { "csv" } else { "json" };
        let path = dir.join(format!("meterly-{range}-{today}.{ext}"));
        let body = if format == "csv" {
            let mut out = String::from(
                "period,source,model,input,output,cache_read,cache_creation,total,cost_usd\n",
            );
            for r in &data.rows {
                out.push_str(&format!(
                    "{},{},{},{},{},{},{},{},{}\n",
                    r.period,
                    r.source.as_str(),
                    r.model.as_deref().unwrap_or("unknown"),
                    r.tokens.input,
                    r.tokens.output,
                    r.tokens.cache_read,
                    r.tokens.cache_creation,
                    r.tokens.total,
                    r.cost_usd.map_or(String::from(""), |c| format!("{c:.4}")),
                ));
            }
            out
        } else {
            serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?
        };
        std::fs::write(&path, body).map_err(|e| e.to_string())?;
        Ok(path.display().to_string())
    }
}

/// Limit thresholds that trigger a macOS notification (measured sources).
const LIMIT_THRESHOLDS: [u8; 2] = [95, 80];

impl Engine {
    /// Returns (title, body) when a measured limit crossed a threshold not
    /// yet notified for this window. Windows roll over via resets_at.
    pub fn limit_notification(&mut self, summary: &Summary) -> Option<(String, String)> {
        for s in &summary.sources {
            let RateLimitStatus::Measured {
                primary_used_percent,
                secondary_used_percent,
                resets_at,
                ..
            } = &s.rate_limit
            else {
                continue;
            };
            let pct = primary_used_percent.max(secondary_used_percent.unwrap_or(0.0));
            // New window → forget the old notification.
            if self
                .notified_limit
                .is_some_and(|(_, r)| r != *resets_at)
            {
                self.notified_limit = None;
            }
            let crossed = LIMIT_THRESHOLDS.iter().copied().find(|t| pct >= *t as f64)?;
            let already = self
                .notified_limit
                .is_some_and(|(t, _)| t >= crossed);
            if already {
                continue;
            }
            self.notified_limit = Some((crossed, *resets_at));
            let local_reset = resets_at.with_timezone(&Local).format("%H:%M");
            return Some((
                format!("{} 한도 {:.0}% 사용", s.display_name, pct),
                format!("사용량이 {crossed}% 임계값을 넘었습니다. 리셋: {local_reset} (로그 기준)"),
            ));
        }
        None
    }
}

/// Tray title token formatter ("12.3M" style — mirrors format.ts).
pub fn format_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}K", n as f64 / 1_000.0),
        1_000_000..=999_999_999 => format!("{:.1}M", n as f64 / 1_000_000.0),
        _ => format!("{:.1}B", n as f64 / 1_000_000_000.0),
    }
}

/// Refresh once and push results to the UI + tray. Runs on a worker thread.
/// Set the tray title to the current rotation state (empty states = icon mode,
/// no title). Windows has no tray title, so it goes to the hover tooltip.
fn apply_tray_title(app: &AppHandle, info: &TrayInfo) {
    let title: Option<String> = if info.states.is_empty() {
        None
    } else {
        Some(info.states[info.idx % info.states.len()].clone())
    };
    let Some(tray) = app.tray_by_id("main-tray") else {
        return;
    };
    #[cfg(target_os = "macos")]
    let _ = tray.set_title(title);
    #[cfg(not(target_os = "macos"))]
    let _ = tray.set_tooltip(Some(match title {
        Some(t) => format!("meterly — 오늘 {t}"),
        None => "meterly".to_string(),
    }));
}

pub fn refresh_and_publish(app: &AppHandle) -> Option<Summary> {
    use tauri_plugin_notification::NotificationExt;
    let state = app.state::<AppState>();
    let (summary, notification) = {
        let mut engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
        let summary = engine.refresh();
        let notification = engine.limit_notification(&summary);
        (summary, notification)
    };
    if let Some((title, body)) = notification {
        let _ = app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show();
    }
    let (display, devices) = {
        let engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
        (
            engine.cache.tray_display.clone().unwrap_or_default(),
            engine.get_devices(),
        )
    };
    // Build the tray rotation states. Non-icon modes cycle tokens & cost; with
    // 2+ synced devices each also splits into 이 기기 / 전체.
    let this_tokens: u64 = summary.sources.iter().map(|s| s.today_tokens.total).sum();
    let this_cost: f64 = summary.sources.iter().filter_map(|s| s.today_cost_usd).sum();
    let all_srcs = || devices.devices.iter().flat_map(|d| d.sources.iter());
    let all_tokens: u64 = all_srcs().map(|s| s.today_tokens.total).sum();
    let all_cost: f64 = all_srcs().filter_map(|s| s.today_cost_usd).sum();

    let states: Vec<String> = if display == "icon" {
        Vec::new()
    } else if devices.sync_enabled && devices.devices.len() >= 2 {
        vec![
            format!("이 기기 {}", format_tokens(this_tokens)),
            format!("전체 {}", format_tokens(all_tokens)),
            format!("이 기기 ${:.2}", this_cost),
            format!("전체 ${:.2}", all_cost),
        ]
    } else {
        vec![format_tokens(this_tokens), format!("${:.2}", this_cost)]
    };

    let snapshot = {
        let tr = app.state::<TrayRotation>();
        let mut info = tr.0.lock().unwrap_or_else(|e| e.into_inner());
        if info.idx >= states.len() {
            info.idx = 0;
        }
        info.states = states;
        info.clone()
    };
    apply_tray_title(app, &snapshot);

    let _ = app.emit("usage-updated", &summary);
    Some(summary)
}

/// Change the tray display mode ("tokens"|"cost"|"icon"), persist, refresh.
pub fn set_tray_display(app: &AppHandle, mode: &str) {
    {
        let state = app.state::<AppState>();
        let mut engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
        engine.cache.tray_display = Some(mode.to_string());
        let path = engine.cache_path.clone();
        let _ = cache::save(&path, &engine.cache);
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let _ = refresh_and_publish(&app);
    });
}

/// Set (or clear with `None`) the multi-device sync folder, persist, then
/// refresh so this device's file is written and the combined view updates.
pub fn set_sync_dir(app: &AppHandle, dir: Option<String>) {
    {
        let state = app.state::<AppState>();
        let mut engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
        engine.cache.sync_dir = dir;
        let path = engine.cache_path.clone();
        let _ = cache::save(&path, &engine.cache);
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let _ = refresh_and_publish(&app);
    });
}

/// Background polling loop (plain thread — scans are blocking file IO) plus a
/// filesystem watcher for near-instant refresh when logs change.
pub fn start(app: AppHandle) {
    let watch_app = app.clone();
    std::thread::spawn(move || watch_loop(watch_app));

    // Tray-title rotation: cycle 이 기기/전체 × 토큰/비용 every few seconds
    // (only when the last refresh produced more than one state to show).
    let rot_app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(TRAY_ROTATE_SECS));
        let snapshot = {
            let tr = rot_app.state::<TrayRotation>();
            let mut info = tr.0.lock().unwrap_or_else(|e| e.into_inner());
            if info.states.len() > 1 {
                info.idx = (info.idx + 1) % info.states.len();
            }
            info.clone()
        };
        if snapshot.states.len() > 1 {
            apply_tray_title(&rot_app, &snapshot);
        }
    });

    std::thread::spawn(move || loop {
        let _ = refresh_and_publish(&app);
        std::thread::sleep(Duration::from_secs(REFRESH_INTERVAL_SECS));
    });
}

/// Watch the log roots (`~/.claude/projects`, `~/.codex`) and refresh once a
/// burst of writes settles — debounced by a quiet gap, but bounded so a long
/// streaming response still refreshes every few seconds. The 180s poll loop
/// remains as a backstop (and covers dirs that don't exist yet).
fn watch_loop(app: AppHandle) {
    use notify::{RecursiveMode, Watcher};
    use std::sync::mpsc::{channel, RecvTimeoutError};

    let (tx, rx) = channel();
    let mut watcher = match notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    }) {
        Ok(w) => w,
        Err(err) => {
            eprintln!("meterly: file watcher unavailable ({err}); polling only");
            return;
        }
    };

    let mut watched = 0u32;
    for entry in sources::registry() {
        if watcher
            .watch(&entry.root_path, RecursiveMode::Recursive)
            .is_ok()
        {
            watched += 1;
        }
    }
    if watched == 0 {
        return; // nothing to watch yet — the poll loop still refreshes.
    }

    let quiet = Duration::from_millis(800);
    let max = Duration::from_secs(5);
    loop {
        // Block until the first change of a burst.
        if rx.recv().is_err() {
            return; // watcher dropped.
        }
        // Coalesce: refresh after `quiet` idle, or `max` since the burst began.
        let start = std::time::Instant::now();
        loop {
            let remaining = max.checked_sub(start.elapsed()).unwrap_or_default();
            match rx.recv_timeout(quiet.min(remaining)) {
                Ok(_) => {
                    if start.elapsed() >= max {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let _ = refresh_and_publish(&app);
    }
}
