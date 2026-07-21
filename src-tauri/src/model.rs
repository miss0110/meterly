//! Domain types shared by all usage sources (plan: Contract surface).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Identifies a usage source. The serde string form ("claude_code" /
/// "codex") is also the cursor-map namespace key in the cache file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceId {
    ClaudeCode,
    Codex,
}

impl SourceId {
    /// Stable string form, identical to the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            SourceId::ClaudeCode => "claude_code",
            SourceId::Codex => "codex",
        }
    }
}

/// One normalized usage record.
///
/// Invariant: the four token slots are pairwise disjoint — no token is ever
/// counted in two slots (T1 evidence (b)/(f), fixtures/README.md), so
/// `total_tokens()` is exactly the sum of the four slots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageEvent {
    pub source: SourceId,
    /// Session uuid derived from the log file name (never conversation
    /// content).
    pub session_id: String,
    /// B7 (confirmed, T1 (h)): Claude events carry
    /// `Some("<message.id>:<requestId>")` so resume/continue copies dedup
    /// globally. Codex events use `None`. Extraction is the parsers' job
    /// (T4); this type only reserves the slot.
    pub dedup_key: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub model: Option<String>,
    /// Project the work happened in — the basename of the session's `cwd`
    /// (never a full path). `None` when the log carried no cwd. Drives the
    /// dashboard's per-project breakdown.
    pub project: Option<String>,
    /// Non-cached input tokens only.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

impl UsageEvent {
    /// Sum of the four disjoint slots (contract invariant).
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_read_tokens
            + self.cache_creation_tokens
    }
}

/// Per-source parse health surfaced to the UI (AC4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceHealth {
    Ok,
    Partial { skipped_lines: u64, note: String },
    Error { reason: String },
}

/// One usage window as printed by `claude -p "/usage"`, e.g.
/// `Current week (all models): 6% used · resets Jul 19 at 9pm`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    /// Window label inside the parentheses ("all models", "Fable", …).
    pub label: String,
    pub used_percent: f64,
    /// Reset text exactly as the CLI printed it (no fragile date parsing).
    pub resets_label: Option<String>,
}

/// Rate-limit view per source. `Estimated` is Claude's local heuristic (UI
/// label "추정"); `Measured` carries a used-percent/window/reset snapshot — now
/// sourced from the live `codex app-server` plan read (UI label "실시간",
/// `resets_at` from epoch seconds); `Cli` is the real `/usage` readout shelled
/// out via the `claude` binary (also UI "실시간").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitStatus {
    Estimated {
        window_hours: u32,
        window_tokens: u64,
        window_started: DateTime<Utc>,
        resets_at: DateTime<Utc>,
    },
    Measured {
        primary_used_percent: f64,
        secondary_used_percent: Option<f64>,
        window_minutes: u64,
        resets_at: DateTime<Utc>,
        /// Secondary (weekly) window reset — Codex logs carry its own
        /// `resets_at`. Optional so older caches still deserialize.
        #[serde(default)]
        secondary_resets_at: Option<DateTime<Utc>>,
    },
    /// Real usage from `claude -p "/usage"`: an optional session line plus the
    /// weekly windows.
    Cli {
        session_percent: Option<f64>,
        windows: Vec<UsageWindow>,
    },
    Unavailable,
}

/// Whether a source appears signed in — surfaced so the UI can prompt for
/// re-login. `Ok` = signed in (or nothing suggests otherwise); `LoggedOut` =
/// confirmed no credentials (Codex has no `auth.json`); `Stale` = can't
/// confirm and the plan data has expired — usually a lapsed Claude login,
/// detected from a cached `/usage` whose window already reset.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    Ok,
    LoggedOut,
    Stale,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_event() -> UsageEvent {
        UsageEvent {
            source: SourceId::ClaudeCode,
            session_id: "aaaaaaaa-0000-0000-0000-00000000000a".into(),
            dedup_key: Some("msg_fixture_1:req_fixture_1".into()),
            timestamp: Utc.with_ymd_and_hms(2026, 7, 13, 1, 2, 3).unwrap(),
            model: Some("claude-sonnet-5".into()),
            project: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 1000,
            cache_creation_tokens: 200,
        }
    }

    #[test]
    fn total_tokens_is_sum_of_four_disjoint_slots() {
        // 100 + 50 + 1000 + 200 = 1350 (cache_record fixture values).
        assert_eq!(sample_event().total_tokens(), 1350);
    }

    #[test]
    fn source_id_serde_string_forms() {
        assert_eq!(
            serde_json::to_value(SourceId::ClaudeCode).unwrap(),
            serde_json::json!("claude_code")
        );
        assert_eq!(
            serde_json::to_value(SourceId::Codex).unwrap(),
            serde_json::json!("codex")
        );
        let back: SourceId = serde_json::from_str("\"claude_code\"").unwrap();
        assert_eq!(back, SourceId::ClaudeCode);
        assert_eq!(SourceId::ClaudeCode.as_str(), "claude_code");
        assert_eq!(SourceId::Codex.as_str(), "codex");
    }

    #[test]
    fn usage_event_serde_roundtrip() {
        let ev = sample_event();
        let json = serde_json::to_string(&ev).unwrap();
        let back: UsageEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn rate_limit_status_serde_roundtrip() {
        let cases = [
            RateLimitStatus::Estimated {
                window_hours: 5,
                window_tokens: 123_456,
                window_started: Utc.with_ymd_and_hms(2026, 7, 13, 0, 0, 0).unwrap(),
                resets_at: Utc.with_ymd_and_hms(2026, 7, 13, 5, 0, 0).unwrap(),
            },
            RateLimitStatus::Measured {
                primary_used_percent: 25.0,
                secondary_used_percent: Some(40.0),
                window_minutes: 300,
                resets_at: Utc.timestamp_opt(1_782_740_693, 0).unwrap(),
                secondary_resets_at: Utc.timestamp_opt(1_783_000_000, 0).single(),
            },
            RateLimitStatus::Unavailable,
        ];
        for case in cases {
            let json = serde_json::to_string(&case).unwrap();
            let back: RateLimitStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, case);
        }
    }
}
