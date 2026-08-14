//! Real Claude plan usage.
//!
//! Claude's per-plan limits (`/usage`: current session %, weekly windows) are
//! NOT in the local session logs (only token counts are). Primary source:
//! Claude Code's own cache — `~/.claude.json` `cachedUsageUtilization`, the
//! structured data behind the interactive `/usage` panel, refreshed whenever
//! the user runs claude. Reading it needs no subprocess and no OAuth token.
//!
//! Live fallback: shell out to `claude -p "/usage"` and parse the printed
//! panel (verified against 2.1.208 — it prints the full panel with resets when
//! signed in; a signed-out CLI prints only a cost summary, which parses as
//! Unavailable). We shell out whenever the cache can't be trusted: either
//! window rolled over, or Claude hasn't rewritten it within
//! [`CACHE_MAX_AGE_SECS`]. On total failure the caller falls back to the local
//! rolling-window estimate.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::{RateLimitStatus, UsageWindow};

/// Hard cap on the `claude -p "/usage"` call (cold CLI start + network).
const TIMEOUT_SECS: u64 = 30;

/// Locate the `claude` binary. A macOS `.app` launched from Finder gets a
/// minimal PATH that excludes `~/.local/bin`, so we probe known locations
/// (override with `METERLY_CLAUDE_BIN`).
fn claude_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("METERLY_CLAUDE_BIN") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(format!("{home}/.local/bin/claude")));
    }
    for p in [
        "/opt/homebrew/bin/claude",
        "/usr/local/bin/claude",
        "/usr/bin/claude",
    ] {
        candidates.push(PathBuf::from(p));
    }
    candidates.into_iter().find(|p| p.exists())
}

/// How long a cached reading may be trusted before we re-read live. Claude
/// rewrites the cache whenever it runs, so during active use it stays fresh on
/// its own; this only bounds how stale the panel can get while idle.
const CACHE_MAX_AGE_SECS: i64 = 600;

/// Read Claude Code's own cached `/usage` data from `~/.claude.json`, plus when
/// Claude last refreshed it (`fetchedAtMs`) so callers can age it out.
fn cached_utilization() -> Option<(RateLimitStatus, Option<DateTime<Utc>>)> {
    let home = dirs::home_dir()?;
    let content = std::fs::read_to_string(home.join(".claude.json")).ok()?;
    let v: Value = serde_json::from_str(&content).ok()?;
    let c = v.get("cachedUsageUtilization")?;
    let fetched_at = c
        .get("fetchedAtMs")
        .and_then(Value::as_i64)
        .and_then(|ms| DateTime::from_timestamp_millis(ms));
    Some((parse_cached_utilization(c)?, fetched_at))
}

/// Map `cachedUsageUtilization.utilization.limits` into [`RateLimitStatus::Cli`].
/// Shape (one row per gauge):
/// `{kind: "session"|"weekly_all"|"weekly_scoped", percent, resets_at, scope}`.
/// `weekly_all` keeps the legacy "all models" label (UI renders it as 주간);
/// scoped rows use their model display name (UI: 주간·<name>). `resets_at` is
/// an ISO timestamp the UI formats to the user's locale.
pub fn parse_cached_utilization(c: &Value) -> Option<RateLimitStatus> {
    let limits = c.get("utilization")?.get("limits")?.as_array()?;
    let mut session_percent: Option<f64> = None;
    let mut session_resets_at: Option<String> = None;
    let mut windows: Vec<UsageWindow> = Vec::new();
    for l in limits {
        let Some(pct) = l.get("percent").and_then(Value::as_f64) else {
            continue;
        };
        let kind = l.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind == "session" {
            session_percent = Some(pct);
            // Keep the 5-hour window's reset so the UI can count down to it.
            session_resets_at = l
                .get("resets_at")
                .and_then(Value::as_str)
                .map(str::to_string);
            continue;
        }
        let scope_name = l
            .get("scope")
            .and_then(|s| s.get("model"))
            .and_then(|m| m.get("display_name"))
            .and_then(Value::as_str);
        let label = match (kind, scope_name) {
            (_, Some(name)) => name.to_string(),
            ("weekly_all", None) => "all models".to_string(),
            (other, None) => other.to_string(),
        };
        windows.push(UsageWindow {
            label,
            used_percent: pct,
            resets_label: l
                .get("resets_at")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    (session_percent.is_some() || !windows.is_empty()).then_some(RateLimitStatus::Cli {
        session_percent,
        session_resets_at,
        windows,
    })
}

/// Whether a `/usage` reading still describes the CURRENT window.
///
/// Claude Code refreshes `cachedUsageUtilization` only occasionally, so a
/// cached reading whose weekly window has already reset holds the *previous*
/// week's percentages — showing it as live is misleading (it reads far lower
/// than reality once a new week starts). We treat a reading as stale when the
/// latest parseable weekly `resets_at` is in the past. When no reset is
/// parseable (session-only, or the legacy English reset text) we can't prove
/// staleness, so we keep the reading rather than break the existing behavior.
pub fn cli_current(rl: &RateLimitStatus, now: DateTime<Utc>) -> bool {
    let RateLimitStatus::Cli { windows, .. } = rl else {
        return false;
    };
    let latest = windows
        .iter()
        .filter_map(|w| w.resets_label.as_deref())
        .filter_map(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .max();
    latest.map_or(true, |reset| reset > now)
}

/// Whether the 5-hour session window in a reading is still the current one.
/// That window rolls over several times a day, so a cached reading goes stale
/// far sooner than its weekly windows do — without this check we keep showing
/// the previous window's percentage (and a reset time already in the past).
/// Unparseable/absent resets can't prove staleness, so they count as current.
pub fn session_current(rl: &RateLimitStatus, now: DateTime<Utc>) -> bool {
    let RateLimitStatus::Cli {
        session_resets_at, ..
    } = rl
    else {
        return false;
    };
    session_resets_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map_or(true, |reset| reset.with_timezone(&Utc) > now)
}

/// Whether a reading is usable as-is: both its weekly windows AND its 5-hour
/// session window still describe the current periods.
pub fn reading_current(rl: &RateLimitStatus, now: DateTime<Utc>) -> bool {
    cli_current(rl, now) && session_current(rl, now)
}

/// Real Claude plan usage: the `~/.claude.json` cache first (no subprocess),
/// then a `claude -p "/usage"` shell-out. Returns
/// [`RateLimitStatus::Unavailable`] on total failure so the caller can fall
/// back to the estimate.
pub fn fetch() -> RateLimitStatus {
    let now = Utc::now();
    if let Some((rl, fetched_at)) = cached_utilization() {
        let age_secs = fetched_at.map(|t| (now - t).num_seconds());
        let fresh_enough = age_secs.map_or(true, |a| a < CACHE_MAX_AGE_SECS);
        if reading_current(&rl, now) && fresh_enough {
            return rl; // fast path: current cache, no subprocess needed
        }
        // Either a window (weekly or the 5-hour session) already rolled over,
        // or Claude hasn't rewritten the cache in a while. Both mean the
        // percentages are behind reality — shell out for the live panel.
        crate::logging::info(&format!(
            "claude usage: cached /usage utilization not usable \
             (weekly_current={}, session_current={}, age={}); shelling out for a fresh panel",
            cli_current(&rl, now),
            session_current(&rl, now),
            age_secs.map_or("unknown".into(), |a| format!("{}s", a)),
        ));
    }
    let Some(bin) = claude_binary() else {
        crate::logging::warn(
            "claude usage: `claude` binary not found (checked ~/.local/bin, \
             /opt/homebrew/bin, /usr/local/bin, /usr/bin; set METERLY_CLAUDE_BIN to override)",
        );
        return RateLimitStatus::Unavailable;
    };
    // Run in a neutral temp cwd: a Finder-launched app inherits cwd `/`, and
    // we don't want `claude` treating the user's folders as a project (which
    // can trigger macOS folder-permission prompts). `/usage` needs no project.
    let mut child = match Command::new(bin)
        .args(["-p", "/usage"])
        .current_dir(std::env::temp_dir())
        // Same PATH augmentation as codex — npm-installed claude is a node
        // launcher script and a GUI app's minimal PATH lacks node.
        .env("PATH", crate::sources::spawn_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()) // captured for diagnostics on failure
        .spawn()
    {
        Ok(c) => c,
        Err(err) => {
            crate::logging::warn(&format!("claude usage: failed to spawn claude: {err}"));
            return RateLimitStatus::Unavailable;
        }
    };

    let status;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) => {
                if start.elapsed() > Duration::from_secs(TIMEOUT_SECS) {
                    let _ = child.kill();
                    let _ = child.wait();
                    crate::logging::warn(&format!(
                        "claude usage: `claude -p /usage` timed out after {TIMEOUT_SECS}s"
                    ));
                    return RateLimitStatus::Unavailable;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return RateLimitStatus::Unavailable,
        }
    }

    // Outputs are tiny (a short panel), so reading after exit is safe.
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        use std::io::Read;
        let _ = stdout.read_to_string(&mut out);
    }
    let parsed = parse_usage(&out);
    if matches!(parsed, RateLimitStatus::Unavailable) {
        // Ran but produced nothing parseable — log what it actually said so a
        // field log explains the "추정" fallback (wrong version, not signed in,
        // changed panel wording, error text, …).
        let mut err = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            use std::io::Read;
            let _ = stderr.read_to_string(&mut err);
        }
        let head = |s: &str| -> String {
            let one = s.trim().replace('\n', " | ");
            one.chars().take(300).collect()
        };
        crate::logging::warn(&format!(
            "claude usage: unparseable /usage output (exit: {}); stdout: {}; stderr: {}",
            status.map_or("unknown".into(), |s| s.to_string()),
            if out.trim().is_empty() { "<empty>".into() } else { head(&out) },
            if err.trim().is_empty() { "<empty>".into() } else { head(&err) },
        ));
    }
    parsed
}

/// Pull the `N` out of a `… 42% used …` fragment (accepts decimals).
fn percent_before(marker_slice: &str) -> Option<f64> {
    let head = marker_slice.split('%').next()?;
    head.trim()
        .rsplit(|c: char| c.is_whitespace() || c == ':')
        .find(|t| !t.is_empty())?
        .parse::<f64>()
        .ok()
}

/// Reset text after `resets ` (kept verbatim; may be `None`).
fn resets_after(s: &str) -> Option<String> {
    let idx = s.find("resets ")?;
    let text = s[idx + "resets ".len()..].trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Parse the `/usage` panel text into a [`RateLimitStatus::Cli`]. Returns
/// [`RateLimitStatus::Unavailable`] if neither a session nor any window is found.
pub fn parse_usage(output: &str) -> RateLimitStatus {
    let mut session_percent: Option<f64> = None;
    let mut session_resets_at: Option<String> = None;
    let mut windows: Vec<UsageWindow> = Vec::new();

    for raw in output.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("Current session:") {
            // "25% used · resets Aug 14 at 4:50pm (Asia/Seoul)"
            session_percent = percent_before(rest);
            session_resets_at = resets_after(rest);
        } else if let Some(rest) = line.strip_prefix("Current week ") {
            // rest = "(all models): 6% used · resets Jul 19 at 9pm (Asia/Seoul)"
            let rest = rest.trim_start();
            let Some(rest) = rest.strip_prefix('(') else {
                continue;
            };
            let Some(close) = rest.find("):") else {
                continue;
            };
            let label = rest[..close].trim().to_string();
            let tail = &rest[close + 2..];
            let Some(pct) = percent_before(tail) else {
                continue;
            };
            windows.push(UsageWindow {
                label,
                used_percent: pct,
                resets_label: resets_after(tail),
            });
        }
    }

    if session_percent.is_none() && windows.is_empty() {
        return RateLimitStatus::Unavailable;
    }
    RateLimitStatus::Cli {
        session_percent,
        session_resets_at,
        windows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_rollover_makes_a_reading_unusable() {
        // The real 2026-08-14 case: the weekly window is still current (Aug 16)
        // but the 5-hour session already reset at 16:50, so the cached 25% is
        // the previous window's. Weekly-only staleness missed this.
        let now = DateTime::parse_from_rfc3339("2026-08-14T08:20:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let rolled = RateLimitStatus::Cli {
            session_percent: Some(25.0),
            session_resets_at: Some("2026-08-14T07:50:00+00:00".into()), // past
            windows: vec![UsageWindow {
                label: "all models".into(),
                used_percent: 31.0,
                resets_label: Some("2026-08-16T08:00:00+00:00".into()), // future
            }],
        };
        assert!(cli_current(&rolled, now), "weekly window is still current");
        assert!(!session_current(&rolled, now), "session window rolled over");
        assert!(!reading_current(&rolled, now), "so the reading must not be used");

        // Same reading before the session reset is fully usable.
        let before = DateTime::parse_from_rfc3339("2026-08-14T07:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(reading_current(&rolled, before));

        // No session reset recorded (older cache) → can't prove staleness.
        let unknown = RateLimitStatus::Cli {
            session_percent: Some(25.0),
            session_resets_at: None,
            windows: vec![],
        };
        assert!(session_current(&unknown, now));
    }

    #[test]
    fn captures_session_reset_from_both_sources() {
        // Shell-out panel: session carries its own "resets" clause.
        let out = "Current session: 25% used · resets Aug 14 at 4:50pm (Asia/Seoul)\n\
                   Current week (all models): 31% used · resets Aug 16 at 5pm (Asia/Seoul)\n";
        let RateLimitStatus::Cli { session_percent, session_resets_at, .. } = parse_usage(out) else {
            panic!("expected Cli");
        };
        assert_eq!(session_percent, Some(25.0));
        assert_eq!(
            session_resets_at.as_deref(),
            Some("Aug 14 at 4:50pm (Asia/Seoul)")
        );

        // Cached utilization: the session limit's resets_at is kept, not dropped.
        let v: Value = serde_json::from_str(
            r#"{"utilization":{"limits":[
                {"kind":"session","percent":25,"resets_at":"2026-08-14T07:50:00.423398+00:00","scope":null},
                {"kind":"weekly_all","percent":31,"resets_at":"2026-08-16T08:00:00.423424+00:00","scope":null}
            ]}}"#,
        )
        .unwrap();
        let RateLimitStatus::Cli { session_resets_at, .. } =
            parse_cached_utilization(&v).expect("parses")
        else {
            panic!("expected Cli");
        };
        assert_eq!(
            session_resets_at.as_deref(),
            Some("2026-08-14T07:50:00.423398+00:00")
        );
    }

    #[test]
    fn parses_reset_less_shellout_panel() {
        // Current `claude -p "/usage"` output: session + weekly %, no inline
        // "resets" clause and no scoped windows. Must still parse (reset = None).
        let out = "You are currently using your subscription to power your Claude Code usage\n\
                   \n\
                   Current session: 3% used\n\
                   Current week (all models): 42% used\n\
                   \n\
                   What's contributing to your limits usage?\n";
        let RateLimitStatus::Cli { session_percent, windows, .. } = parse_usage(out) else {
            panic!("expected Cli");
        };
        assert_eq!(session_percent, Some(3.0));
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "all models");
        assert_eq!(windows[0].used_percent, 42.0);
        assert_eq!(windows[0].resets_label, None); // no reset in the new format
        // No reset parseable → we can't prove staleness, so it's treated as current.
        assert!(cli_current(&RateLimitStatus::Cli {
            session_percent: Some(3.0),
            session_resets_at: None,
            windows,
        }, Utc::now()));
    }

    #[test]
    fn parses_cached_utilization_limits() {
        // Real-world shape from ~/.claude.json (claude 2.1.208).
        let v: Value = serde_json::from_str(
            r#"{"fetchedAtMs":1784113351068,"utilization":{"limits":[
                {"kind":"session","group":"session","percent":0,"severity":"normal","resets_at":null,"scope":null},
                {"kind":"weekly_all","group":"weekly","percent":8,"severity":"normal","resets_at":"2026-07-19T11:59:59.914462+00:00","scope":null},
                {"kind":"weekly_scoped","group":"weekly","percent":10,"severity":"normal","resets_at":"2026-07-19T11:59:59.914891+00:00","scope":{"model":{"id":null,"display_name":"Fable"},"surface":null}}
            ]}}"#,
        )
        .unwrap();
        let RateLimitStatus::Cli {
            session_percent,
            windows,
            ..
        } = parse_cached_utilization(&v).expect("should parse")
        else {
            panic!("expected Cli variant");
        };
        assert_eq!(session_percent, Some(0.0));
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "all models");
        assert_eq!(windows[0].used_percent, 8.0);
        assert_eq!(
            windows[0].resets_label.as_deref(),
            Some("2026-07-19T11:59:59.914462+00:00")
        );
        assert_eq!(windows[1].label, "Fable");
        assert_eq!(windows[1].used_percent, 10.0);
    }

    fn window(reset: Option<&str>) -> UsageWindow {
        UsageWindow {
            label: "all models".into(),
            used_percent: 8.0,
            resets_label: reset.map(str::to_string),
        }
    }

    #[test]
    fn cli_current_flags_expired_windows() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-21T00:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let stale = RateLimitStatus::Cli {
            session_percent: Some(0.0),
            session_resets_at: None,
            windows: vec![window(Some("2026-07-19T11:59:59.914462+00:00"))],
        };
        assert!(!cli_current(&stale, now), "reset in the past → stale");

        let fresh = RateLimitStatus::Cli {
            session_percent: Some(0.0),
            session_resets_at: None,
            windows: vec![window(Some("2026-07-26T11:59:59+00:00"))],
        };
        assert!(cli_current(&fresh, now), "reset in the future → current");

        // No parseable reset → can't prove staleness, keep the reading.
        let no_reset = RateLimitStatus::Cli {
            session_percent: Some(0.0),
            session_resets_at: None,
            windows: vec![window(None)],
        };
        assert!(cli_current(&no_reset, now));
        // Legacy English reset text isn't RFC3339 → treated as current.
        let legacy = RateLimitStatus::Cli {
            session_percent: Some(0.0),
            session_resets_at: None,
            windows: vec![window(Some("Jul 19 at 9pm"))],
        };
        assert!(cli_current(&legacy, now));

        assert!(!cli_current(&RateLimitStatus::Unavailable, now));
    }

    #[test]
    fn cached_utilization_missing_or_empty_is_none() {
        let empty: Value = serde_json::from_str(r#"{"utilization":{"limits":[]}}"#).unwrap();
        assert!(parse_cached_utilization(&empty).is_none());
        let no_key: Value = serde_json::from_str(r#"{}"#).unwrap();
        assert!(parse_cached_utilization(&no_key).is_none());
    }

    const SAMPLE: &str = "You are currently using your subscription to power your Claude Code usage\n\
\n\
Current session: 0% used\n\
Current week (all models): 6% used · resets Jul 19 at 9pm (Asia/Seoul)\n\
Current week (Fable): 10% used · resets Jul 19 at 9pm (Asia/Seoul)\n\
\n\
What's contributing to your limits usage?\n\
Last 24h · 1126 requests · 22 sessions\n";

    #[test]
    fn parses_session_and_weekly_windows() {
        let RateLimitStatus::Cli {
            session_percent,
            windows,
            ..
        } = parse_usage(SAMPLE)
        else {
            panic!("expected Cli variant");
        };
        assert_eq!(session_percent, Some(0.0));
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "all models");
        assert_eq!(windows[0].used_percent, 6.0);
        assert_eq!(
            windows[0].resets_label.as_deref(),
            Some("Jul 19 at 9pm (Asia/Seoul)")
        );
        assert_eq!(windows[1].label, "Fable");
        assert_eq!(windows[1].used_percent, 10.0);
    }

    #[test]
    fn handles_decimals_and_missing_reset() {
        let RateLimitStatus::Cli {
            session_percent,
            windows,
            ..
        } = parse_usage("Current session: 42.5% used\nCurrent week (all models): 3% used\n")
        else {
            panic!("expected Cli variant");
        };
        assert_eq!(session_percent, Some(42.5));
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, 3.0);
        assert_eq!(windows[0].resets_label, None);
    }

    #[test]
    fn empty_or_noise_is_unavailable() {
        assert_eq!(parse_usage(""), RateLimitStatus::Unavailable);
        assert_eq!(
            parse_usage("some unrelated output\n"),
            RateLimitStatus::Unavailable
        );
    }
}
