//! Real Codex (ChatGPT) plan usage via the `codex app-server` protocol.
//!
//! Codex's local rollout logs only carry a `rate_limits` snapshot captured at
//! the time of the last request — it goes stale and often reads 0%. The live
//! plan usage the interactive panel shows comes from Codex's own backend, which
//! Codex exposes through its official (experimental) `codex app-server` stdio
//! JSON-RPC interface. We speak that protocol — `initialize` → `initialized` →
//! `account/rateLimits/read` — and read `rateLimits.primary.usedPercent`. This
//! reuses Codex's own auth (we never touch the token) and returns the same
//! numbers as the ChatGPT usage panel.
//!
//! Like the Claude `/usage` shell-out, this spawns a process (~seconds), so the
//! scheduler throttles it; on any failure the caller falls back to the local
//! log snapshot (or shows nothing).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use serde_json::Value;

use crate::model::RateLimitStatus;

/// Hard cap on the whole handshake (cold app-server start can be slow — it
/// refreshes the model list on boot).
const TIMEOUT_SECS: u64 = 20;

/// Locate the `codex` binary. A macOS `.app` launched from Finder gets a
/// minimal PATH that excludes `~/.local/bin`, so we probe known locations
/// (override with `METERLY_CODEX_BIN`).
fn codex_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("METERLY_CODEX_BIN") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(format!("{home}/.local/bin/codex")));
    }
    for p in [
        "/opt/homebrew/bin/codex",
        "/usr/local/bin/codex",
        "/usr/bin/codex",
    ] {
        candidates.push(PathBuf::from(p));
    }
    candidates.into_iter().find(|p| p.exists())
}

/// The three JSON-RPC lines the app-server needs before it will answer a read:
/// `initialize` (request id 1), the `initialized` notification, then
/// `account/rateLimits/read` (request id 2 — the one we wait for).
fn handshake_lines() -> String {
    let init = r#"{"id":1,"method":"initialize","params":{"clientInfo":{"name":"meterly","version":"1"}}}"#;
    let initialized = r#"{"method":"initialized"}"#;
    let read = r#"{"id":2,"method":"account/rateLimits/read","params":{}}"#;
    format!("{init}\n{initialized}\n{read}\n")
}

/// Run the app-server handshake and read the live plan rate limits. Returns
/// [`RateLimitStatus::Unavailable`] on any failure (missing binary, timeout,
/// malformed reply) so the caller can fall back.
pub fn fetch() -> RateLimitStatus {
    let Some(bin) = codex_binary() else {
        crate::logging::warn(
            "codex usage: `codex` binary not found (checked ~/.local/bin, \
             /opt/homebrew/bin, /usr/local/bin, /usr/bin; set METERLY_CODEX_BIN to override)",
        );
        return RateLimitStatus::Unavailable;
    };
    // Hardening for a background, GUI-launched context (see module docs):
    //  - run in a neutral temp cwd so codex never touches the user's project
    //    or protected folders (a Finder-launched app inherits cwd `/`), which
    //    is what triggered the macOS folder/permission prompts;
    //  - `-c notify=[]` disables the Computer Use notify hook;
    //  - new process group so a stray child can't outlive us.
    let mut cmd = Command::new(bin);
    cmd.args(["app-server", "-c", "notify=[]"])
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            crate::logging::warn(&format!("codex usage: failed to spawn app-server: {err}"));
            return RateLimitStatus::Unavailable;
        }
    };

    // Keep stdin open until we have the reply — closing it early makes the
    // server shut down before answering.
    let mut stdin = match child.stdin.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return RateLimitStatus::Unavailable;
        }
    };
    if stdin.write_all(handshake_lines().as_bytes()).is_err() || stdin.flush().is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return RateLimitStatus::Unavailable;
    }

    // Read replies on a worker thread so we can bound the wait. We only care
    // about the response to request id 2.
    let (tx, rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if v.get("id").and_then(Value::as_i64) == Some(2) {
                        let _ = tx.send(v);
                        return;
                    }
                }
            }
        });
    }

    let outcome = match rx.recv_timeout(Duration::from_secs(TIMEOUT_SECS)) {
        Ok(v) => {
            let parsed = parse_rate_limits(&v);
            match &parsed {
                RateLimitStatus::Measured { primary_used_percent, .. } => {
                    crate::logging::info(&format!(
                        "codex app-server: rate limits ok ({primary_used_percent:.0}%)"
                    ));
                }
                _ => {
                    // Got a reply but no usable rateLimits — often "not signed
                    // in to Codex" or an API/version change.
                    let hint = v
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("no rateLimits in reply (signed in to Codex?)");
                    crate::logging::warn(&format!("codex app-server: {hint}"));
                }
            }
            parsed
        }
        Err(_) => {
            crate::logging::warn(&format!(
                "codex app-server: no reply within {TIMEOUT_SECS}s"
            ));
            RateLimitStatus::Unavailable
        }
    };
    // The app-server is long-lived; we're done with it.
    let _ = child.kill();
    let _ = child.wait();
    // Hold stdin until here so it isn't dropped (closed) mid-handshake.
    drop(stdin);
    outcome
}

/// Map an `account/rateLimits/read` response into [`RateLimitStatus::Measured`].
/// Uses the top-level `rateLimits` bucket (the main plan limit); `primary` is
/// the active window, `secondary` (when present) the longer one.
pub fn parse_rate_limits(v: &Value) -> RateLimitStatus {
    let Some(rl) = v.get("result").and_then(|r| r.get("rateLimits")) else {
        return RateLimitStatus::Unavailable;
    };
    let primary = rl.get("primary").filter(|p| !p.is_null());
    let Some(primary) = primary else {
        return RateLimitStatus::Unavailable;
    };
    let Some(used) = primary.get("usedPercent").and_then(Value::as_f64) else {
        return RateLimitStatus::Unavailable;
    };
    let window_minutes = primary
        .get("windowDurationMins")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let Some(resets_at) = primary
        .get("resetsAt")
        .and_then(Value::as_i64)
        .and_then(|s| Utc.timestamp_opt(s, 0).single())
    else {
        return RateLimitStatus::Unavailable;
    };

    let secondary = rl.get("secondary").filter(|s| !s.is_null());
    let secondary_used_percent = secondary
        .and_then(|s| s.get("usedPercent"))
        .and_then(Value::as_f64);
    let secondary_resets_at = secondary
        .and_then(|s| s.get("resetsAt"))
        .and_then(Value::as_i64)
        .and_then(|s| Utc.timestamp_opt(s, 0).single());

    RateLimitStatus::Measured {
        primary_used_percent: used,
        secondary_used_percent,
        window_minutes,
        resets_at,
        secondary_resets_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_live_rate_limits() {
        let v: Value = serde_json::from_str(
            r#"{"id":2,"result":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":3,"windowDurationMins":10080,"resetsAt":1784681127},"secondary":null,"planType":"pro"}}}"#,
        )
        .unwrap();
        let RateLimitStatus::Measured {
            primary_used_percent,
            secondary_used_percent,
            window_minutes,
            resets_at,
            ..
        } = parse_rate_limits(&v)
        else {
            panic!("expected Measured");
        };
        assert_eq!(primary_used_percent, 3.0);
        assert_eq!(secondary_used_percent, None);
        assert_eq!(window_minutes, 10080);
        assert_eq!(resets_at.timestamp(), 1784681127);
    }

    #[test]
    fn maps_secondary_window_when_present() {
        let v: Value = serde_json::from_str(
            r#"{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":12.5,"windowDurationMins":300,"resetsAt":1784681127},"secondary":{"usedPercent":40,"windowDurationMins":10080,"resetsAt":1785000000}}}}"#,
        )
        .unwrap();
        let RateLimitStatus::Measured {
            primary_used_percent,
            secondary_used_percent,
            window_minutes,
            secondary_resets_at,
            ..
        } = parse_rate_limits(&v)
        else {
            panic!("expected Measured");
        };
        assert_eq!(primary_used_percent, 12.5);
        assert_eq!(secondary_used_percent, Some(40.0));
        assert_eq!(window_minutes, 300);
        assert_eq!(secondary_resets_at.unwrap().timestamp(), 1785000000);
    }

    #[test]
    fn missing_rate_limits_is_unavailable() {
        let v: Value = serde_json::from_str(r#"{"id":2,"result":{}}"#).unwrap();
        assert_eq!(parse_rate_limits(&v), RateLimitStatus::Unavailable);
        let err: Value =
            serde_json::from_str(r#"{"id":2,"error":{"code":-1,"message":"nope"}}"#).unwrap();
        assert_eq!(parse_rate_limits(&err), RateLimitStatus::Unavailable);
    }
}
