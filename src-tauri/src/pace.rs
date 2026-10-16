//! Plan-window pace: how fast each limit window (5-hour session, weekly) is
//! being used, where it will land at reset, and — if it runs out first — when.
//! Also detects window resets so the user can be told the limit is back.
//!
//! Samples are the percentages meterly already reads every refresh (Claude
//! `/usage`, Codex app-server); nothing new is fetched. They live in memory
//! only: after a restart the forecast falls back to the window-average rate
//! until fresh samples accumulate.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::Serialize;

use crate::model::RateLimitStatus;

const SESSION_HOURS: i64 = 5;
const WEEK_DAYS: i64 = 7;
/// Recent-rate lookback for short windows ("at this speed…").
const RECENT_LOOKBACK_MIN: i64 = 60;
/// Minimum sample span before a recent slope is trusted.
const RECENT_MIN_SPAN_MIN: i64 = 15;
/// A finished window must have peaked at least here to announce its reset.
const RESET_ALERT_MIN_PEAK: f64 = 70.0;
/// Don't warn about running out before the window is at least this used.
const EXHAUST_ALERT_MIN_USED: f64 = 30.0;

/// One plan window as read this refresh.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitWindow {
    /// Stable key, e.g. `claude_code:session`, `claude_code:all models`,
    /// `codex:primary`. The UI matches rows by it.
    pub key: String,
    /// Korean label (세션 / 주간 / 주간·Fable / 5시간 …).
    pub label: String,
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
    pub length: Duration,
}

/// Forecast state of a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaceStatus {
    /// Will run out before it resets at the current speed.
    Exhaust,
    /// Will land at 80%+ by reset.
    Tight,
    /// Comfortable.
    Ok,
    /// Already at the limit.
    Full,
    /// Not enough information (no reset time, too early in the window).
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowPace {
    pub key: String,
    pub label: String,
    pub used_percent: f64,
    /// Share of the window's time already elapsed (even-use reference).
    pub expected_percent: Option<f64>,
    /// Usage speed in percentage points per hour.
    pub rate_per_hour: Option<f64>,
    /// When it hits 100% at that speed — only if that is before the reset.
    pub eta_full: Option<DateTime<Utc>>,
    /// Where it lands at reset at that speed (may exceed 100).
    pub projected_at_reset: Option<f64>,
    pub resets_at: Option<DateTime<Utc>>,
    pub status: PaceStatus,
}

/// The plan windows of one source's rate-limit readout.
pub fn windows(source_id: &str, rl: &RateLimitStatus, now: DateTime<Utc>) -> Vec<LimitWindow> {
    match rl {
        RateLimitStatus::Cli {
            session_percent,
            session_resets_at,
            windows,
        } => {
            let mut out = Vec::new();
            if let Some(p) = session_percent {
                out.push(LimitWindow {
                    key: format!("{source_id}:session"),
                    label: "세션".into(),
                    used_percent: *p,
                    resets_at: session_resets_at.as_deref().and_then(|s| parse_reset(s, now)),
                    length: Duration::hours(SESSION_HOURS),
                });
            }
            for w in windows {
                out.push(LimitWindow {
                    key: format!("{source_id}:{}", w.label),
                    label: if w.label == "all models" {
                        "주간".into()
                    } else {
                        format!("주간·{}", w.label)
                    },
                    used_percent: w.used_percent,
                    resets_at: w.resets_label.as_deref().and_then(|s| parse_reset(s, now)),
                    length: Duration::days(WEEK_DAYS),
                });
            }
            out
        }
        RateLimitStatus::Measured {
            primary_used_percent,
            secondary_used_percent,
            window_minutes,
            resets_at,
            secondary_resets_at,
        } => {
            let mut out = vec![LimitWindow {
                key: format!("{source_id}:primary"),
                label: window_label_kr(*window_minutes),
                used_percent: *primary_used_percent,
                resets_at: Some(*resets_at),
                length: Duration::minutes(*window_minutes as i64),
            }];
            if let Some(sp) = secondary_used_percent {
                out.push(LimitWindow {
                    key: format!("{source_id}:secondary"),
                    label: "주간".into(),
                    used_percent: *sp,
                    resets_at: *secondary_resets_at,
                    length: Duration::days(WEEK_DAYS),
                });
            }
            out
        }
        _ => Vec::new(),
    }
}

/// Korean window label from a window length in minutes.
pub fn window_label_kr(minutes: u64) -> String {
    match minutes {
        300 => "세션".into(),
        10080 => "주간".into(),
        m if m < 1440 => format!("{}시간", (m as f64 / 60.0).round() as u64),
        m => format!("{}일", (m as f64 / 1440.0).round() as u64),
    }
}

/// Reset marker → instant. Accepts RFC 3339 (cached utilization, Codex) and
/// the `/usage` text in local time: "Aug 14 at 4:50pm (Asia/Seoul)",
/// "Jul 19 at 9pm", or a bare "4:50pm" (today, or tomorrow if already past).
pub fn parse_reset(label: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let s = label.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    // Drop a trailing "(Zone/Name)".
    let s = s.split('(').next().unwrap_or(s).trim();
    let local_now = now.with_timezone(&Local);
    let (date, time_txt) = match s.split_once(" at ") {
        Some((md, t)) => {
            let mut it = md.split_whitespace();
            let month = month_index(it.next()?)?;
            let day: u32 = it.next()?.trim_end_matches(',').parse().ok()?;
            let mut year = local_now.year_ce().1 as i32;
            let mut d = NaiveDate::from_ymd_opt(year, month, day)?;
            // The panel omits the year; a date long past means next year.
            if d < local_now.date_naive() - Duration::days(31) {
                year += 1;
                d = NaiveDate::from_ymd_opt(year, month, day)?;
            }
            (Some(d), t)
        }
        None => (None, s),
    };
    let time = parse_clock(time_txt)?;
    let date = match date {
        Some(d) => d,
        None => {
            let today = local_now.date_naive();
            // A bare time already more than an hour past means tomorrow.
            if today.and_time(time) < local_now.naive_local() - Duration::hours(1) {
                today + Duration::days(1)
            } else {
                today
            }
        }
    };
    Local
        .from_local_datetime(&date.and_time(time))
        .earliest()
        .map(|t| t.with_timezone(&Utc))
}

fn month_index(m: &str) -> Option<u32> {
    const M: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let m = m.get(..3)?.to_ascii_lowercase();
    M.iter().position(|x| *x == m).map(|i| i as u32 + 1)
}

/// "4:50pm" / "9pm" / "12am" / "16:30" → time.
fn parse_clock(t: &str) -> Option<NaiveTime> {
    let t = t.trim().to_ascii_lowercase();
    let (body, pm) = if let Some(b) = t.strip_suffix("pm") {
        (b.trim(), Some(true))
    } else if let Some(b) = t.strip_suffix("am") {
        (b.trim(), Some(false))
    } else {
        (t.as_str(), None)
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (body.parse::<u32>().ok()?, 0),
    };
    let h = match pm {
        Some(true) => h % 12 + 12,
        Some(false) => h % 12,
        None => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// Stable id of a window period: its reset instant rounded to 30 minutes, so
/// the same reset written as ISO or as panel text compares equal.
pub fn period_id(resets_at: Option<DateTime<Utc>>) -> Option<i64> {
    resets_at.map(|r| (r.timestamp() + 900).div_euclid(1800))
}

#[derive(Debug, Default)]
struct Series {
    period: Option<i64>,
    samples: Vec<(DateTime<Utc>, f64)>,
    /// Highest usage seen in the current period.
    peak: f64,
    /// Exhaust warning already sent for this period.
    warned: bool,
}

/// In-memory sample history per window + pending notifications.
#[derive(Debug, Default)]
pub struct PaceTracker {
    series: HashMap<String, Series>,
    pending: Vec<(String, String)>,
}

impl PaceTracker {
    /// Record this refresh's readings. Detects resets (queues a "limit is
    /// back" notification when the finished period peaked ≥ 70%) and, after
    /// the forecast, an "at this speed you run out at …" warning once per
    /// period. `source_name` is the display name used in titles.
    pub fn record(&mut self, source_name: &str, wins: &[LimitWindow], now: DateTime<Utc>) {
        for w in wins {
            let period = period_id(w.resets_at);
            let s = self.series.entry(w.key.clone()).or_default();
            let rolled = match (s.period, period) {
                (Some(old), Some(new)) => new > old,
                _ => false,
            };
            // Usage can't fall inside one window; a big drop is a reset even
            // when the reset time is unknown.
            let dropped = s
                .samples
                .last()
                .is_some_and(|(_, p)| w.used_percent + 10.0 < *p);
            if rolled || dropped {
                if s.peak >= RESET_ALERT_MIN_PEAK && w.used_percent < s.peak - 20.0 {
                    self.pending.push((
                        format!("{source_name} · {} 한도 리셋", w.label),
                        format!(
                            "다시 사용할 수 있어요 · 직전 최고 {:.0}% → 지금 {:.0}%",
                            s.peak, w.used_percent
                        ),
                    ));
                }
                *s = Series::default();
            }
            s.period = period.or(s.period);
            s.peak = s.peak.max(w.used_percent);
            let changed = s.samples.last().map_or(true, |(t, p)| {
                (*p - w.used_percent).abs() > f64::EPSILON || now - *t >= Duration::minutes(5)
            });
            if changed {
                s.samples.push((now, w.used_percent));
            }
            let horizon = now - Duration::hours(8);
            s.samples.retain(|(t, _)| *t >= horizon);
            if s.samples.len() > 400 {
                let cut = s.samples.len() - 400;
                s.samples.drain(..cut);
            }

            let pace = self.compute(w, now);
            let s = self.series.get_mut(&w.key).expect("inserted above");
            if pace.status == PaceStatus::Exhaust
                && !s.warned
                && w.used_percent >= EXHAUST_ALERT_MIN_USED
            {
                if let Some(eta) = pace.eta_full {
                    s.warned = true;
                    let reset = pace
                        .resets_at
                        .map(|r| format!(" (리셋 {})", clock(r, now)))
                        .unwrap_or_default();
                    self.pending.push((
                        format!("{source_name} · {} {:.0}%", w.label, w.used_percent),
                        format!("이 속도면 {}에 한도에 도달해요{reset}", clock(eta, now)),
                    ));
                }
            }
        }
    }

    /// Notifications queued by [`record`] since the last call.
    pub fn take_notifications(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.pending)
    }

    pub fn compute(&self, w: &LimitWindow, now: DateTime<Utc>) -> WindowPace {
        let mut out = WindowPace {
            key: w.key.clone(),
            label: w.label.clone(),
            used_percent: w.used_percent,
            expected_percent: None,
            rate_per_hour: None,
            eta_full: None,
            projected_at_reset: None,
            resets_at: w.resets_at,
            status: PaceStatus::Unknown,
        };
        if w.used_percent >= 100.0 {
            out.status = PaceStatus::Full;
            return out;
        }
        let Some(reset) = w.resets_at.filter(|r| *r > now) else {
            return out;
        };
        let len_h = w.length.num_seconds() as f64 / 3600.0;
        let remaining_h = ((reset - now).num_seconds() as f64 / 3600.0).min(len_h);
        let elapsed_h = (len_h - remaining_h).max(0.0);
        out.expected_percent = Some((elapsed_h / len_h * 100.0).clamp(0.0, 100.0));

        // Average since the window opened — the weekly forecast, and the
        // fallback for short windows before enough samples exist.
        let min_elapsed_h = (len_h * 0.05).max(10.0 / 60.0);
        let average = (elapsed_h >= min_elapsed_h).then(|| w.used_percent / elapsed_h);
        // Recent slope — "at this speed" for the 5-hour window.
        let recent = self.series.get(&w.key).and_then(|s| {
            let from = now - Duration::minutes(RECENT_LOOKBACK_MIN);
            let pts: Vec<_> = s.samples.iter().filter(|(t, _)| *t >= from).collect();
            let (t0, p0) = pts.first()?;
            let (t1, p1) = pts.last()?;
            let span_h = (*t1 - *t0).num_seconds() as f64 / 3600.0;
            (span_h * 60.0 >= RECENT_MIN_SPAN_MIN as f64).then(|| ((p1 - p0) / span_h).max(0.0))
        });
        let short = w.length <= Duration::days(1);
        let rate = if short { recent.or(average) } else { average };
        let Some(rate) = rate else {
            return out;
        };
        out.rate_per_hour = Some(rate);
        let projected = w.used_percent + rate * remaining_h;
        out.projected_at_reset = Some(projected.min(999.0));
        if rate > 0.0 && projected >= 100.0 {
            let eta = now + Duration::seconds(((100.0 - w.used_percent) / rate * 3600.0) as i64);
            if eta < reset {
                out.eta_full = Some(eta);
            }
        }
        out.status = if out.eta_full.is_some() {
            PaceStatus::Exhaust
        } else if projected >= 80.0 {
            PaceStatus::Tight
        } else {
            PaceStatus::Ok
        };
        out
    }
}

/// "15:40" today, "내일 09:00", or "10/18(토) 09:00" further out (local).
fn clock(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let lt = t.with_timezone(&Local);
    let days = (lt.date_naive() - now.with_timezone(&Local).date_naive()).num_days();
    let hm = lt.format("%H:%M");
    match days {
        0 => hm.to_string(),
        1 => format!("내일 {hm}"),
        _ => {
            const WD: [&str; 7] = ["월", "화", "수", "목", "금", "토", "일"];
            let wd = WD[lt.format("%u").to_string().parse::<usize>().unwrap_or(1) - 1];
            format!("{}({wd}) {hm}", lt.format("%m/%d"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(key: &str, used: f64, reset_in_h: f64, len_h: i64, now: DateTime<Utc>) -> LimitWindow {
        LimitWindow {
            key: key.into(),
            label: "세션".into(),
            used_percent: used,
            resets_at: Some(now + Duration::seconds((reset_in_h * 3600.0) as i64)),
            length: Duration::hours(len_h),
        }
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 16, 3, 0, 0).unwrap()
    }

    #[test]
    fn recent_slope_drives_short_window_eta() {
        let mut p = PaceTracker::default();
        let now = t0();
        // 5h window, resets in 3h. 30% → 50% over the last 40 minutes = 30%/h.
        for (min, used) in [(0, 30.0), (20, 40.0), (40, 50.0)] {
            let at = now + Duration::minutes(min);
            p.record("Claude Code", &[win("c:session", used, 3.0 - min as f64 / 60.0, 5, at)], at);
        }
        let at = now + Duration::minutes(40);
        let pace = p.compute(&win("c:session", 50.0, 3.0 - 40.0 / 60.0, 5, at), at);
        assert_eq!(pace.status, PaceStatus::Exhaust);
        let rate = pace.rate_per_hour.unwrap();
        assert!((rate - 30.0).abs() < 0.5, "rate {rate}");
        // 50 more points at 30/h = 100 minutes.
        assert_eq!(pace.eta_full.unwrap(), at + Duration::minutes(100));
        // Warned exactly once.
        let n = p.take_notifications();
        assert_eq!(n.len(), 1, "{n:?}");
        assert!(n[0].1.contains("이 속도면"));
    }

    #[test]
    fn weekly_uses_window_average_and_lands_ok() {
        let p = PaceTracker::default();
        let now = t0();
        // 7-day window, 3.5 days elapsed, 20% used → 40% at reset.
        let w = win("c:all models", 20.0, 84.0, 168, now);
        let pace = p.compute(&w, now);
        assert_eq!(pace.status, PaceStatus::Ok);
        assert!((pace.expected_percent.unwrap() - 50.0).abs() < 0.01);
        assert!((pace.projected_at_reset.unwrap() - 40.0).abs() < 0.01);
        assert!(pace.eta_full.is_none());
    }

    #[test]
    fn too_early_in_the_window_is_unknown() {
        let p = PaceTracker::default();
        let now = t0();
        // 5h window, 5 minutes in, no samples.
        let pace = p.compute(&win("c:session", 10.0, 5.0 - 5.0 / 60.0, 5, now), now);
        assert_eq!(pace.status, PaceStatus::Unknown);
        assert!(pace.eta_full.is_none());
    }

    #[test]
    fn reset_after_a_high_peak_is_announced_once() {
        let mut p = PaceTracker::default();
        let now = t0();
        p.record("Codex", &[win("x:primary", 92.0, 0.2, 5, now)], now);
        // Next period: reset moved 5h later, usage back near zero.
        let later = now + Duration::minutes(15);
        p.record("Codex", &[win("x:primary", 3.0, 4.95, 5, later)], later);
        let n = p.take_notifications();
        assert_eq!(n.len(), 1, "{n:?}");
        assert!(n[0].0.contains("리셋") && n[0].1.contains("92%"));
        // Same period again → nothing new.
        p.record("Codex", &[win("x:primary", 4.0, 4.9, 5, later + Duration::minutes(3))], later + Duration::minutes(3));
        assert!(p.take_notifications().is_empty());
    }

    #[test]
    fn low_peak_reset_and_label_format_changes_stay_quiet() {
        let mut p = PaceTracker::default();
        let now = t0();
        p.record("Claude Code", &[win("c:session", 40.0, 0.1, 5, now)], now);
        let later = now + Duration::minutes(10);
        p.record("Claude Code", &[win("c:session", 2.0, 4.95, 5, later)], later);
        assert!(p.take_notifications().is_empty(), "peak 40% → no reset alert");
        // Same instant written differently (ISO vs panel) → same period id.
        let a = parse_reset("2026-10-16T08:00:00.123456+00:00", now);
        let b = Some(Utc.with_ymd_and_hms(2026, 10, 16, 8, 0, 0).unwrap());
        assert_eq!(period_id(a), period_id(b));
    }

    #[test]
    fn parses_usage_panel_reset_text_in_local_time() {
        let now = t0();
        let got = parse_reset("Oct 18 at 4:50pm (Asia/Seoul)", now).unwrap();
        let want = Local.with_ymd_and_hms(2026, 10, 18, 16, 50, 0).unwrap().with_timezone(&Utc);
        assert_eq!(got, want);
        let got = parse_reset("Jan 2 at 9am", now).unwrap(); // past → next year
        assert_eq!(got.with_timezone(&Local).year_ce().1, 2027);
        let bare = parse_reset("11:59pm", now).unwrap();
        assert_eq!(bare.with_timezone(&Local).format("%H:%M").to_string(), "23:59");
        assert!(parse_reset("garbage", now).is_none());
    }
}
