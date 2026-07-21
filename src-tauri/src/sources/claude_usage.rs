//! Real Claude plan usage.
//!
//! Claude's per-plan limits (`/usage`: current session %, weekly windows) are
//! NOT in the local session logs (only token counts are). Primary source:
//! Claude Code's own cache — `~/.claude.json` `cachedUsageUtilization`, the
//! structured data behind the interactive `/usage` panel, refreshed whenever
//! the user runs claude. Reading it needs no subprocess and no OAuth token.
//!
//! Legacy fallback: shell out to `claude -p "/usage"` and parse the printed
//! panel. CLI ≥ 2.1.x no longer prints that panel in print mode (it emits a
//! cost summary instead — seen in the field as the "추정" fallback), but old
//! CLIs still support it and may lack the cache key. On total failure the
//! caller falls back to the local rolling-window estimate.

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

/// Read Claude Code's own cached `/usage` data from `~/.claude.json`.
fn cached_utilization() -> Option<RateLimitStatus> {
    let home = dirs::home_dir()?;
    let content = std::fs::read_to_string(home.join(".claude.json")).ok()?;
    let v: Value = serde_json::from_str(&content).ok()?;
    parse_cached_utilization(v.get("cachedUsageUtilization")?)
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
    let mut windows: Vec<UsageWindow> = Vec::new();
    for l in limits {
        let Some(pct) = l.get("percent").and_then(Value::as_f64) else {
            continue;
        };
        let kind = l.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind == "session" {
            session_percent = Some(pct);
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

/// Real Claude plan usage: the `~/.claude.json` cache first (no subprocess),
/// then the legacy `claude -p "/usage"` shell-out. Returns
/// [`RateLimitStatus::Unavailable`] on total failure so the caller can fall
/// back to the estimate.
pub fn fetch() -> RateLimitStatus {
    if let Some(rl) = cached_utilization() {
        if cli_current(&rl, Utc::now()) {
            return rl;
        }
        // The cache exists but its window already reset — last week's numbers.
        // Don't present them as live; the caller falls back to the local
        // estimate. (Current Claude versions no longer print the `/usage`
        // panel in `-p` mode, so the shell-out below can't refresh it either.)
        crate::logging::info(
            "claude usage: cached /usage utilization is stale (window already reset); \
             falling back to the local estimate",
        );
        return RateLimitStatus::Unavailable;
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
    let mut windows: Vec<UsageWindow> = Vec::new();

    for raw in output.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("Current session:") {
            session_percent = percent_before(rest);
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
        windows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            windows: vec![window(Some("2026-07-19T11:59:59.914462+00:00"))],
        };
        assert!(!cli_current(&stale, now), "reset in the past → stale");

        let fresh = RateLimitStatus::Cli {
            session_percent: Some(0.0),
            windows: vec![window(Some("2026-07-26T11:59:59+00:00"))],
        };
        assert!(cli_current(&fresh, now), "reset in the future → current");

        // No parseable reset → can't prove staleness, keep the reading.
        let no_reset = RateLimitStatus::Cli {
            session_percent: Some(0.0),
            windows: vec![window(None)],
        };
        assert!(cli_current(&no_reset, now));
        // Legacy English reset text isn't RFC3339 → treated as current.
        let legacy = RateLimitStatus::Cli {
            session_percent: Some(0.0),
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
