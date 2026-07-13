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
use crate::sources::{self, RecentEvents, SourceCursors, UsageSource};

pub const REFRESH_INTERVAL_SECS: u64 = 180;

pub struct AppState(pub Mutex<Engine>);

pub struct Engine {
    cache_path: PathBuf,
    pub cache: CacheV1,
    runtimes: Vec<Runtime>,
    /// Limit-notification dedup: (threshold %, window resets_at) already
    /// notified for. Cleared when the window rolls over or usage drops.
    notified_limit: Option<(u8, chrono::DateTime<Utc>)>,
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
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub generated_at: chrono::DateTime<Utc>,
    pub sources: Vec<SourceSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardRow {
    pub period: String,
    pub source: SourceId,
    pub model: Option<String>,
    pub tokens: TokenBreakdown,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardData {
    pub range: String,
    pub rows: Vec<DashboardRow>,
    pub timezone_note: String,
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

        // Persist the freshest measured Codex snapshot so a restarted app
        // shows limits immediately (until the next token_count refreshes).
        for rt in &self.runtimes {
            if rt.id == SourceId::Codex {
                let rl = rt
                    .source
                    .rate_limit(&RecentEvents(self.cache.recent_events.clone()));
                if matches!(rl, RateLimitStatus::Measured { .. }) {
                    self.cache.codex_rate_limit = Some(rl);
                }
            }
        }

        if let Err(err) = cache::save(&self.cache_path, &self.cache) {
            eprintln!("meterly: cache save failed: {err}");
        }
        self.summary()
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
                    SourceId::ClaudeCode => {
                        aggregate::claude_window_estimate(&self.cache.recent_events, now)
                    }
                    SourceId::Codex => {
                        let live = rt
                            .source
                            .rate_limit(&RecentEvents(self.cache.recent_events.clone()));
                        match live {
                            // Fresh scan hasn't seen a snapshot yet (e.g.
                            // right after restart) → persisted fallback.
                            RateLimitStatus::Unavailable => self
                                .cache
                                .codex_rate_limit
                                .clone()
                                .unwrap_or(RateLimitStatus::Unavailable),
                            other => other,
                        }
                    }
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
                }
            })
            .collect();
        Summary {
            generated_at: now,
            sources,
        }
    }

    pub fn dashboard(&self, range: &str) -> DashboardData {
        let today = Local::now().date_naive();
        let start = aggregate::range_start(range, today).unwrap_or(today);
        let mut rows: Vec<DashboardRow> = Vec::new();
        for b in self.all_buckets() {
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
        let data = self.dashboard(range);
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
    let display = {
        let engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
        engine.cache.tray_display.clone().unwrap_or_default()
    };
    if let Some(tray) = app.tray_by_id("main-tray") {
        let title = match display.as_str() {
            "icon" => None,
            "cost" => {
                let cost: f64 = summary
                    .sources
                    .iter()
                    .filter_map(|s| s.today_cost_usd)
                    .sum();
                Some(format!("${cost:.2}"))
            }
            _ => {
                let total: u64 = summary.sources.iter().map(|s| s.today_tokens.total).sum();
                Some(format_tokens(total))
            }
        };
        // macOS shows text next to the tray icon; Windows has no tray
        // title, so the number goes into the hover tooltip instead.
        #[cfg(target_os = "macos")]
        let _ = tray.set_title(title);
        #[cfg(not(target_os = "macos"))]
        let _ = tray.set_tooltip(Some(match title {
            Some(t) => format!("meterly — 오늘 {t}"),
            None => "meterly".to_string(),
        }));
    }
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

/// Background polling loop (plain thread — scans are blocking file IO).
pub fn start(app: AppHandle) {
    std::thread::spawn(move || loop {
        let _ = refresh_and_publish(&app);
        std::thread::sleep(Duration::from_secs(REFRESH_INTERVAL_SECS));
    });
}
