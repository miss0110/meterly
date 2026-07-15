//! Aggregation (T7): daily buckets, dashboard views, backfill window, and
//! the Claude rolling-window estimate.
//!
//! Day boundary = SYSTEM LOCAL timezone midnight (injectable for tests).
//! Note: other tools may bucket by UTC — the dashboard shows a tz tooltip.

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
