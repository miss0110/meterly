//! Real Claude plan usage via the `claude` CLI.
//!
//! Claude's per-plan limits (`/usage`: current session %, weekly windows) are
//! NOT in the local session logs (only token counts are), so meterly shells
//! out to `claude -p "/usage"` and parses the printed panel. This reuses
//! Claude Code's own auth — meterly never touches the OAuth token. The call
//! spawns a process (~seconds) so the scheduler throttles it (see
//! `CLAUDE_USAGE_MIN_INTERVAL`); on any failure the caller falls back to the
//! local rolling-window estimate.
//!
//! Parsing targets these lines (labels/order tolerant, extra lines ignored):
//! ```text
//! Current session: 0% used
//! Current week (all models): 6% used · resets Jul 19 at 9pm (Asia/Seoul)
//! Current week (Fable): 10% used · resets Jul 19 at 9pm (Asia/Seoul)
//! ```

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

/// Run `claude -p "/usage"` and parse it. Returns [`RateLimitStatus::Unavailable`]
/// on any failure (missing binary, timeout, non-zero exit, unparseable output)
/// so the caller can fall back to the estimate.
pub fn fetch() -> RateLimitStatus {
    let Some(bin) = claude_binary() else {
        return RateLimitStatus::Unavailable;
    };
    // Run in a neutral temp cwd: a Finder-launched app inherits cwd `/`, and
    // we don't want `claude` treating the user's folders as a project (which
    // can trigger macOS folder-permission prompts). `/usage` needs no project.
    let mut child = match Command::new(bin)
        .args(["-p", "/usage"])
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return RateLimitStatus::Unavailable,
    };

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > Duration::from_secs(TIMEOUT_SECS) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return RateLimitStatus::Unavailable;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return RateLimitStatus::Unavailable,
        }
    }

    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        use std::io::Read;
        let _ = stdout.read_to_string(&mut out);
    }
    parse_usage(&out)
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
