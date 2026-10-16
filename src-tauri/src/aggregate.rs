//! Aggregation (T7): daily buckets, dashboard views, backfill window, and
//! the Claude rolling-window estimate.
//!
//! Day boundary = SYSTEM LOCAL timezone midnight (injectable for tests).
//! Note: other tools may bucket by UTC — the dashboard shows a tz tooltip.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{RateLimitStatus, SourceId, UsageEvent};
use crate::pricing;

/// Claude rolling rate-limit window (spec open Q2 — one constant to adjust).
pub const CLAUDE_WINDOW_HOURS: i64 = 5;

/// Recent-events retention for the rolling window (> window, small margin).
pub const RECENT_RETENTION_HOURS: i64 = 6;

/// One (date, source, model, project) daily bucket — the cache's `daily` rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DailyBucket {
    pub date: NaiveDate,
    pub source: SourceId,
    pub model: Option<String>,
    /// Project (basename of the session cwd) this usage belongs to. `None` for
    /// pre-project caches/logs. Defaulted so older cache files deserialize.
    #[serde(default)]
    pub project: Option<String>,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl DailyBucket {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }

    pub fn cost_usd(&self) -> Option<f64> {
        let model = self.model.as_deref()?;
        pricing::cost_usd(
            model,
            self.input,
            self.output,
            self.cache_read,
            self.cache_creation,
        )
    }
}

/// One (date, hour, source) bucket for the weekday×hour usage heatmap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HourlyBucket {
    pub date: NaiveDate,
    pub hour: u8,
    pub source: SourceId,
    pub total: u64,
}

/// Fold events into hourly totals (local tz), dropping pre-window events.
pub fn ingest_hourly(
    buckets: &mut Vec<HourlyBucket>,
    events: &[crate::model::UsageEvent],
    window_start: NaiveDate,
) {
    use chrono::Timelike;
    for ev in events {
        let local = ev.timestamp.with_timezone(&Local);
        let date = local.date_naive();
        if date < window_start {
            continue;
        }
        let hour = local.hour() as u8;
        match buckets
            .iter_mut()
            .find(|b| b.date == date && b.hour == hour && b.source == ev.source)
        {
            Some(b) => b.total += ev.total_tokens(),
            None => buckets.push(HourlyBucket {
                date,
                hour,
                source: ev.source,
                total: ev.total_tokens(),
            }),
        }
    }
}

pub fn prune_hourly(buckets: &mut Vec<HourlyBucket>, window_start: NaiveDate) {
    buckets.retain(|b| b.date >= window_start);
}

/// `backfill_start = min(start of every supported dashboard range)` (V2-A3).
/// Ranges: daily30 (today−29d), weekly12 (this week −11w), monthly6 (first
/// of this month −5mo). monthly6 is always the earliest (~150d > 84d > 29d),
/// so the min is the first day of the month five months back.
pub fn backfill_start(today: NaiveDate) -> NaiveDate {
    let month0 = (today.year() * 12 + today.month() as i32 - 1) - 5;
    NaiveDate::from_ymd_opt(month0.div_euclid(12), (month0.rem_euclid(12) + 1) as u32, 1)
        .expect("valid first-of-month")
}

/// Backfill window start as a unix epoch (local midnight) — the stat-only
/// file prune cutoff used by the Codex scanner.
pub fn backfill_window_start_epoch() -> i64 {
    let start = backfill_start(Local::now().date_naive());
    Local
        .from_local_datetime(&start.and_hms_opt(0, 0, 0).expect("midnight"))
        .earliest()
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

/// Local calendar date of an event (system tz unless injected).
pub fn local_date(ts: DateTime<Utc>) -> NaiveDate {
    ts.with_timezone(&Local).date_naive()
}

/// Fold events into daily buckets, dropping events before `window_start`.
pub fn ingest(buckets: &mut Vec<DailyBucket>, events: &[UsageEvent], window_start: NaiveDate) {
    for ev in events {
        let date = local_date(ev.timestamp);
        if date < window_start {
            continue;
        }
        let found = buckets.iter_mut().find(|b| {
            b.date == date
                && b.source == ev.source
                && b.model == ev.model
                && b.project == ev.project
        });
        let bucket = match found {
            Some(b) => b,
            None => {
                buckets.push(DailyBucket {
                    date,
                    source: ev.source,
                    model: ev.model.clone(),
                    project: ev.project.clone(),
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_creation: 0,
                });
                buckets.last_mut().expect("just pushed")
            }
        };
        bucket.input += ev.input_tokens;
        bucket.output += ev.output_tokens;
        bucket.cache_read += ev.cache_read_tokens;
        bucket.cache_creation += ev.cache_creation_tokens;
    }
}

/// Drop buckets that slid out of the backfill window.
pub fn prune(buckets: &mut Vec<DailyBucket>, window_start: NaiveDate) {
    buckets.retain(|b| b.date >= window_start);
}

/// A request re-reading at least this much context is "big" — the insights
/// panel suggests /compact or a fresh session for sessions that live there.
/// (2026-10 data: requests ≥300K were ~half of all requests and most of cost.)
pub const BIG_CONTEXT_TOKENS: u64 = 300_000;
/// Anthropic's default prompt-cache TTL. A request after a longer pause that
/// re-writes most of a large context paid the cache-write price again.
pub const IDLE_REBUILD_MIN_GAP: i64 = 5; // minutes
/// Only contexts at least this big count as a costly rebuild.
pub const IDLE_REBUILD_MIN_CONTEXT: u64 = 100_000;

/// Days of per-session detail kept locally — the "가장 비싼 세션" list. Small
/// (a few hundred rows) and never synced or reported.
pub const SESSION_RETENTION_DAYS: i64 = 7;

/// First date whose per-session buckets are retained.
pub fn session_window_start(today: NaiveDate) -> NaiveDate {
    today - Duration::days(SESSION_RETENTION_DAYS - 1)
}

/// One (date, source, session, model) bucket. Keyed on the model too so a
/// session that mixes models (main + subagent) prices each slice correctly;
/// the dashboard folds slices back into one session row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionBucket {
    pub date: NaiveDate,
    pub source: SourceId,
    /// Session uuid (log file name) — local only.
    pub session_id: String,
    pub model: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    /// Requests (assistant turns with usage) in this bucket.
    #[serde(default)]
    pub requests: u64,
    /// Largest context (input + cache read + cache write) of one request.
    #[serde(default)]
    pub ctx_max: u64,
    /// Requests whose context was ≥ BIG_CONTEXT_TOKENS, and their tokens.
    #[serde(default)]
    pub big_requests: u64,
    #[serde(default)]
    pub big_tokens: u64,
    /// Requests that followed a pause > IDLE_REBUILD_MIN_GAP and re-wrote
    /// most of a ≥ IDLE_REBUILD_MIN_CONTEXT context, and the tokens written.
    #[serde(default)]
    pub idle_rebuilds: u64,
    #[serde(default)]
    pub idle_rebuild_tokens: u64,
}

impl SessionBucket {
    /// Context tokens summed over requests (everything but output).
    pub fn context_tokens(&self) -> u64 {
        self.input + self.cache_read + self.cache_creation
    }

    /// Extra USD paid because idle rebuilds wrote the cache instead of
    /// reading it (write price − read price), at API list prices.
    pub fn idle_rebuild_extra_usd(&self) -> Option<f64> {
        let model = self.model.as_deref()?;
        let t = self.idle_rebuild_tokens;
        Some(pricing::cost_usd(model, 0, 0, 0, t)? - pricing::cost_usd(model, 0, 0, t, 0)?)
    }

    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }

    pub fn cost_usd(&self) -> Option<f64> {
        let model = self.model.as_deref()?;
        pricing::cost_usd(
            model,
            self.input,
            self.output,
            self.cache_read,
            self.cache_creation,
        )
    }
}

/// Fold events into per-session buckets (local date), dropping events before
/// `window_start`. Also tallies context size and idle cache rebuilds; the gap
/// before a request is measured against the previous request of the same
/// session in `events` (Claude passes a full re-parse, so it's complete). Keyed on the project too: one session can move between
/// repos, and the dashboard names a session after the repo it spent most in.
pub fn ingest_sessions(
    buckets: &mut Vec<SessionBucket>,
    events: &[UsageEvent],
    window_start: NaiveDate,
) {
    // Time order within each session, for the idle-gap check.
    let mut order: Vec<usize> = (0..events.len()).collect();
    order.sort_by(|&a, &b| {
        (&events[a].session_id, events[a].timestamp).cmp(&(&events[b].session_id, events[b].timestamp))
    });
    let mut prev_at: HashMap<&str, DateTime<Utc>> = HashMap::new();
    for ev in order.into_iter().map(|i| &events[i]) {
        let gap = prev_at
            .insert(ev.session_id.as_str(), ev.timestamp)
            .map(|p| ev.timestamp - p);
        let date = local_date(ev.timestamp);
        if date < window_start {
            continue;
        }
        let found = buckets.iter_mut().find(|b| {
            b.date == date
                && b.source == ev.source
                && b.session_id == ev.session_id
                && b.model == ev.model
                && b.project == ev.project
        });
        let b = match found {
            Some(b) => b,
            None => {
                buckets.push(SessionBucket {
                    date,
                    source: ev.source,
                    session_id: ev.session_id.clone(),
                    model: ev.model.clone(),
                    project: ev.project.clone(),
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_creation: 0,
                    first_at: ev.timestamp,
                    last_at: ev.timestamp,
                    requests: 0,
                    ctx_max: 0,
                    big_requests: 0,
                    big_tokens: 0,
                    idle_rebuilds: 0,
                    idle_rebuild_tokens: 0,
                });
                buckets.last_mut().expect("just pushed")
            }
        };
        b.input += ev.input_tokens;
        b.output += ev.output_tokens;
        b.cache_read += ev.cache_read_tokens;
        b.cache_creation += ev.cache_creation_tokens;
        let ctx = ev.input_tokens + ev.cache_read_tokens + ev.cache_creation_tokens;
        b.requests += 1;
        b.ctx_max = b.ctx_max.max(ctx);
        if ctx >= BIG_CONTEXT_TOKENS {
            b.big_requests += 1;
            b.big_tokens += ev.total_tokens();
        }
        let idle = gap.is_some_and(|g| g > Duration::minutes(IDLE_REBUILD_MIN_GAP));
        if idle && ctx >= IDLE_REBUILD_MIN_CONTEXT && ev.cache_creation_tokens * 2 >= ctx {
            b.idle_rebuilds += 1;
            b.idle_rebuild_tokens += ev.cache_creation_tokens;
        }
        if ev.timestamp < b.first_at {
            b.first_at = ev.timestamp;
        }
        if ev.timestamp > b.last_at {
            b.last_at = ev.timestamp;
        }
    }
}

pub fn prune_sessions(buckets: &mut Vec<SessionBucket>, window_start: NaiveDate) {
    buckets.retain(|b| b.date >= window_start);
}

/// This week vs last week, per weekday (월..일), for the overlay chart.
/// `*_to_date` compare like-for-like: Monday through today's weekday in both
/// weeks, so a Wednesday view doesn't pit 3 days against 7.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WeekCompare {
    /// Monday of this week (local).
    pub week_start: NaiveDate,
    /// 0 = 월 … 6 = 일.
    pub today_weekday: u8,
    pub this_week: Vec<u64>,
    pub last_week: Vec<u64>,
    pub this_cost: Option<f64>,
    pub last_cost: Option<f64>,
    pub this_to_date: u64,
    pub last_to_date: u64,
}

pub fn week_compare<'a>(
    buckets: impl Iterator<Item = &'a DailyBucket>,
    today: NaiveDate,
) -> WeekCompare {
    let wd = today.weekday().num_days_from_monday() as usize;
    let week_start = today - Duration::days(wd as i64);
    let last_start = week_start - Duration::weeks(1);
    let mut this_week = vec![0u64; 7];
    let mut last_week = vec![0u64; 7];
    let mut this_cost: Option<f64> = None;
    let mut last_cost: Option<f64> = None;
    for b in buckets {
        if b.date >= week_start && b.date <= today {
            let i = (b.date - week_start).num_days() as usize;
            this_week[i] += b.total();
            if let Some(c) = b.cost_usd() {
                *this_cost.get_or_insert(0.0) += c;
            }
        } else if b.date >= last_start && b.date < week_start {
            let i = (b.date - last_start).num_days() as usize;
            last_week[i] += b.total();
            if let Some(c) = b.cost_usd() {
                *last_cost.get_or_insert(0.0) += c;
            }
        }
    }
    let this_to_date = this_week[..=wd].iter().sum();
    let last_to_date = last_week[..=wd].iter().sum();
    WeekCompare {
        week_start,
        today_weekday: wd as u8,
        this_week,
        last_week,
        this_cost,
        last_cost,
        this_to_date,
        last_to_date,
    }
}

/// Claude rolling-window ESTIMATE ("추정" label in the UI): tokens and reset
/// time derived from recent event timestamps — no official quota data exists
/// on disk for Claude (T1). Window = CLAUDE_WINDOW_HOURS from the earliest
/// in-window event.
pub fn claude_window_estimate(recent: &[UsageEvent], now: DateTime<Utc>) -> RateLimitStatus {
    let window = Duration::hours(CLAUDE_WINDOW_HOURS);
    let mut in_window: Vec<&UsageEvent> = recent
        .iter()
        .filter(|e| e.source == SourceId::ClaudeCode && now - e.timestamp <= window)
        .collect();
    if in_window.is_empty() {
        return RateLimitStatus::Unavailable;
    }
    in_window.sort_by_key(|e| e.timestamp);
    let started = in_window[0].timestamp;
    let tokens: u64 = in_window.iter().map(|e| e.total_tokens()).sum();
    RateLimitStatus::Estimated {
        window_hours: CLAUDE_WINDOW_HOURS as u32,
        window_tokens: tokens,
        window_started: started,
        resets_at: started + window,
    }
}

/// Dashboard period key for a date under a given range.
pub fn period_key(range: &str, date: NaiveDate) -> Option<String> {
    match range {
        "daily30" => Some(date.format("%Y-%m-%d").to_string()),
        "weekly12" => {
            let week = date.iso_week();
            Some(format!("{}-W{:02}", week.year(), week.week()))
        }
        "monthly6" => Some(date.format("%Y-%m").to_string()),
        _ => None,
    }
}

/// First date included in a range (relative to `today`).
pub fn range_start(range: &str, today: NaiveDate) -> Option<NaiveDate> {
    match range {
        "daily30" => Some(today - Duration::days(29)),
        "weekly12" => {
            let this_week_start =
                today - Duration::days(today.weekday().num_days_from_monday() as i64);
            Some(this_week_start - Duration::weeks(11))
        }
        "monthly6" => Some(backfill_start(today)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backfill_start_is_first_of_month_five_back() {
        let d = NaiveDate::from_ymd_opt(2026, 7, 14).unwrap();
        assert_eq!(backfill_start(d), NaiveDate::from_ymd_opt(2026, 2, 1).unwrap());
        // Year boundary.
        let d = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
        assert_eq!(backfill_start(d), NaiveDate::from_ymd_opt(2025, 10, 1).unwrap());
        // Window covers every supported range start.
        let today = NaiveDate::from_ymd_opt(2026, 7, 14).unwrap();
        for range in ["daily30", "weekly12", "monthly6"] {
            assert!(range_start(range, today).unwrap() >= backfill_start(today));
        }
    }

    fn ev(day: u32, hour: u32, session: &str, model: &str, tokens: u64) -> UsageEvent {
        UsageEvent {
            source: SourceId::ClaudeCode,
            session_id: session.into(),
            dedup_key: None,
            // Local tz may shift the date; use midday so the local date is stable.
            timestamp: Local
                .with_ymd_and_hms(2026, 9, day, hour, 0, 0)
                .unwrap()
                .with_timezone(&Utc),
            model: Some(model.into()),
            project: Some("proj".into()),
            input_tokens: tokens,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        }
    }

    #[test]
    fn sessions_fold_per_session_and_model_and_track_span() {
        let mut b = Vec::new();
        let events = vec![
            ev(18, 10, "s1", "claude-opus-5", 100),
            ev(18, 12, "s1", "claude-opus-5", 50),
            ev(18, 11, "s1", "claude-haiku-4-5", 7), // subagent slice
            ev(18, 9, "s2", "claude-opus-5", 1),
            ev(10, 9, "old", "claude-opus-5", 999), // before window
        ];
        let start = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
        ingest_sessions(&mut b, &events, start);
        assert_eq!(b.len(), 3);
        let s1 = b
            .iter()
            .find(|x| x.session_id == "s1" && x.model.as_deref() == Some("claude-opus-5"))
            .unwrap();
        assert_eq!(s1.total(), 150);
        assert_eq!(s1.first_at, events[0].timestamp);
        assert_eq!(s1.last_at, events[1].timestamp);
        assert_eq!(s1.project.as_deref(), Some("proj"));
        prune_sessions(&mut b, NaiveDate::from_ymd_opt(2026, 9, 19).unwrap());
        assert!(b.is_empty());
    }

    #[test]
    fn sessions_tally_big_context_and_idle_cache_rebuilds() {
        let base = Local.with_ymd_and_hms(2026, 9, 18, 10, 0, 0).unwrap().with_timezone(&Utc);
        let mk = |min: i64, cr: u64, cw: u64| UsageEvent {
            source: SourceId::ClaudeCode,
            session_id: "s".into(),
            dedup_key: None,
            timestamp: base + Duration::minutes(min),
            model: Some("claude-opus-5".into()),
            project: Some("p".into()),
            input_tokens: 1_000,
            output_tokens: 500,
            cache_read_tokens: cr,
            cache_creation_tokens: cw,
        };
        // Out of order on purpose: gaps are measured in time order.
        let events = vec![
            mk(2, 390_000, 9_000),  // 2 min later, warm cache: big, not idle
            mk(0, 0, 150_000),      // session start (no previous) → not a rebuild
            mk(70, 5_000, 395_000), // 68 min idle, re-wrote the context → rebuild
            mk(72, 2_000, 40_000),  // small context
        ];
        let mut b = Vec::new();
        ingest_sessions(&mut b, &events, NaiveDate::from_ymd_opt(2026, 9, 1).unwrap());
        assert_eq!(b.len(), 1);
        let s = &b[0];
        assert_eq!(s.requests, 4);
        assert_eq!(s.ctx_max, 401_000);
        assert_eq!(s.big_requests, 2);
        assert_eq!(s.idle_rebuilds, 1);
        assert_eq!(s.idle_rebuild_tokens, 395_000);
        // Opus 5: write 6.25 − read 0.50 per Mtok on 395K tokens ≈ $2.27.
        let extra = s.idle_rebuild_extra_usd().unwrap();
        assert!((extra - 0.395 * 5.75).abs() < 0.01, "{extra}");
    }

    #[test]
    fn week_compare_aligns_weekdays_and_compares_to_date() {
        // 2026-09-16 is a Wednesday (wd=2); this week starts Mon 09-14.
        let today = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
        let mk = |y: i32, m: u32, d: u32, t: u64| DailyBucket {
            date: NaiveDate::from_ymd_opt(y, m, d).unwrap(),
            source: SourceId::Codex,
            model: None,
            project: None,
            input: t,
            output: 0,
            cache_read: 0,
            cache_creation: 0,
        };
        let buckets = vec![
            mk(2026, 9, 14, 10), // this Mon
            mk(2026, 9, 16, 20), // this Wed (today)
            mk(2026, 9, 7, 5),   // last Mon
            mk(2026, 9, 9, 6),   // last Wed
            mk(2026, 9, 13, 100), // last Sun — outside to-date compare
            mk(2026, 9, 1, 999),  // two weeks ago — ignored
        ];
        let w = week_compare(buckets.iter(), today);
        assert_eq!(w.week_start, NaiveDate::from_ymd_opt(2026, 9, 14).unwrap());
        assert_eq!(w.today_weekday, 2);
        assert_eq!(w.this_week, vec![10, 0, 20, 0, 0, 0, 0]);
        assert_eq!(w.last_week, vec![5, 0, 6, 0, 0, 0, 100]);
        assert_eq!(w.this_to_date, 30);
        assert_eq!(w.last_to_date, 11);
    }

    #[test]
    fn claude_estimate_windows_and_resets() {
        let now = Utc.with_ymd_and_hms(2026, 7, 14, 12, 0, 0).unwrap();
        let mk = |hours_ago: i64, tokens: u64| UsageEvent {
            source: SourceId::ClaudeCode,
            session_id: "s".into(),
            dedup_key: None,
            timestamp: now - Duration::hours(hours_ago),
            model: None,
            project: None,
            input_tokens: tokens,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        };
        // t-6h is outside the 5h window; t-4h and t-1h are inside.
        let events = vec![mk(6, 100), mk(4, 40), mk(1, 2)];
        match claude_window_estimate(&events, now) {
            RateLimitStatus::Estimated {
                window_hours,
                window_tokens,
                window_started,
                resets_at,
            } => {
                assert_eq!(window_hours, 5);
                assert_eq!(window_tokens, 42);
                assert_eq!(window_started, now - Duration::hours(4));
                assert_eq!(resets_at, now + Duration::hours(1));
            }
            other => panic!("expected Estimated, got {other:?}"),
        }
    }
}
