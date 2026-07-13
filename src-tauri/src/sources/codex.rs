//! Codex JSONL parser (T5).
//!
//! Parses `~/.codex/sessions/**/*.jsonl` and
//! `~/.codex/archived_sessions/**/*.jsonl` rollout logs into [`UsageEvent`]s.
//! Only `type == "event_msg"` records with `payload.type == "token_count"`
//! carry usage. Aggregation follows the T1 (a) rule (fixtures/README.md):
//! an event's `last_token_usage` counts only when the cumulative
//! `total_token_usage.total_tokens` CHANGED vs. the previous token_count in
//! the same file — unchanged cumulative means a duplicate emission (31.3%
//! measured) and is skipped silently; a decrease is a reset (resume/compact)
//! that starts a new baseline and keeps counting.
//!
//! Identity is the session uuid from the FILE NAME (never file content,
//! V3-A2). The same uuid may exist in both trees (sessions→archived move):
//! sessions/ wins (C1, T1 (g) — 0 divergent pairs observed).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;

use crate::model::{RateLimitStatus, SourceHealth, SourceId, UsageEvent};

use super::{RecentEvents, ScanOutcome, SourceCursors, UsageSource};

/// Registry constructor slot (AC7).
pub fn make(root: PathBuf) -> Box<dyn UsageSource> {
    Box::new(CodexSource::new(root))
}

/// Latest `payload.rate_limits` snapshot seen during scans (T1 (c)).
#[derive(Debug, Clone, PartialEq)]
struct RateLimitSnapshot {
    /// Record timestamp when parseable — newer timestamps win.
    at: Option<DateTime<Utc>>,
    primary_used_percent: f64,
    secondary_used_percent: Option<f64>,
    window_minutes: u64,
    resets_at: DateTime<Utc>,
}

pub struct CodexSource {
    root: PathBuf,
    /// Health of the most recent `scan` (Ok before the first scan).
    last_health: SourceHealth,
    latest_rate_limits: Option<RateLimitSnapshot>,
}

/// Raw record envelope — unknown fields ignored by serde.
#[derive(Deserialize)]
struct RawRecord {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawTokenInfo {
    total_token_usage: Option<RawTotalUsage>,
    last_token_usage: Option<RawLastUsage>,
}

#[derive(Deserialize)]
struct RawTotalUsage {
    total_tokens: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawLastUsage {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
}

#[derive(Deserialize)]
struct RawRateLimits {
    primary: Option<RawRateWindow>,
    secondary: Option<RawRateWindow>,
}

#[derive(Deserialize)]
struct RawRateWindow {
    used_percent: Option<f64>,
    window_minutes: Option<u64>,
    resets_at: Option<i64>,
}

/// Session uuid from a rollout file name: `rollout-<ts>-<uuid>.jsonl`.
/// The uuid is the trailing 36 chars of the stem (8-4-4-4-12 hex). Name-only
/// (V3-A2): no file content is read to resolve identity.
fn session_uuid(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem.len() < 36 {
        return None;
    }
    let candidate = &stem[stem.len() - 36..];
    let bytes = candidate.as_bytes();
    let ok = bytes.iter().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => *b == b'-',
        _ => b.is_ascii_hexdigit(),
    });
    if ok {
        Some(candidate.to_string())
    } else {
        None
    }
}

impl CodexSource {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            last_health: SourceHealth::Ok,
            latest_rate_limits: None,
        }
    }

    /// List `*.jsonl` under one tree (recursive, readdir/stat only). Missing
    /// tree → empty (not an error). Non-uuid `.jsonl` names are counted into
    /// `skipped_files` (A-2) and excluded.
    fn list_tree(dir: &Path, files: &mut Vec<(String, PathBuf)>, skipped_files: &mut u64) {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return, // missing/unreadable subtree is not fatal here
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::list_tree(&path, files, skipped_files);
            } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                match session_uuid(&path) {
                    Some(uuid) => files.push((uuid, path)),
                    None => *skipped_files += 1,
                }
            }
        }
    }

    /// Parse one rollout file, appending events. Returns lines skipped.
    fn parse_file(
        &mut self,
        uuid: &str,
        path: &Path,
        events: &mut Vec<UsageEvent>,
    ) -> std::io::Result<u64> {
        let content = fs::read_to_string(path)?;
        let mut skipped: u64 = 0;
        // T1 (a) rule state: previous cumulative total in THIS file.
        let mut prev_total: Option<u64> = None;
        // T1 (e): latest preceding turn_context model in THIS file.
        let mut current_model: Option<String> = None;

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let record: RawRecord = match serde_json::from_str(line) {
                Ok(r) => r,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let payload = record.payload.as_ref();
            match record.kind.as_deref() {
                Some("turn_context") => {
                    if let Some(model) = payload
                        .and_then(|p| p.get("model"))
                        .and_then(|m| m.as_str())
                    {
                        current_model = Some(model.to_string());
                    }
                }
                Some("event_msg") => {
                    let is_token_count = payload
                        .and_then(|p| p.get("type"))
                        .and_then(|t| t.as_str())
                        == Some("token_count");
                    if !is_token_count {
                        continue; // other event_msg kinds are not ours
                    }
                    let payload = payload.expect("token_count implies payload");

                    // rate_limits snapshot refresh (info may be null).
                    if let Some(raw) = payload.get("rate_limits") {
                        self.absorb_rate_limits(raw, record.timestamp.as_deref());
                    }

                    // info: null → snapshot-only record (97 measured), no event.
                    let info: RawTokenInfo = match payload.get("info") {
                        Some(v) if !v.is_null() => match serde_json::from_value(v.clone()) {
                            Ok(i) => i,
                            Err(_) => {
                                skipped += 1;
                                continue;
                            }
                        },
                        _ => continue,
                    };
                    let cumulative = info
                        .total_token_usage
                        .and_then(|t| t.total_tokens);
                    let Some(cumulative) = cumulative else {
                        continue; // no cumulative marker — cannot apply (a) rule
                    };

                    // T1 (a): count only when the cumulative total CHANGED.
                    // Unchanged = duplicate emission (skip silently, not an
                    // error); decreased = reset, new baseline, keep counting.
                    let counts = match prev_total {
                        None => true,
                        Some(p) => cumulative != p,
                    };
                    prev_total = Some(cumulative);
                    if !counts {
                        continue;
                    }

                    let last = info.last_token_usage.unwrap_or_default();
                    // C2: subset violation discards the WHOLE record
                    // (checked_sub — no clamp-to-zero, no panic).
                    let Some(net_input) = last.input_tokens.checked_sub(last.cached_input_tokens)
                    else {
                        skipped += 1;
                        continue;
                    };
                    let Some(timestamp) = record
                        .timestamp
                        .as_deref()
                        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                        .map(|t| t.with_timezone(&Utc))
                    else {
                        skipped += 1;
                        continue;
                    };
                    events.push(UsageEvent {
                        source: SourceId::Codex,
                        session_id: uuid.to_string(),
                        dedup_key: None, // Codex identity = uuid cursor, not key
                        timestamp,
                        model: current_model.clone(),
                        input_tokens: net_input,
                        output_tokens: last.output_tokens,
                        cache_read_tokens: last.cached_input_tokens,
                        cache_creation_tokens: 0,
                    });
                }
                _ => {} // session_meta, response_item, legacy flat records, …
            }
        }
        Ok(skipped)
    }

    /// Keep the newest rate_limits snapshot (record timestamps win; a
    /// timestampless snapshot only fills an empty slot).
    fn absorb_rate_limits(&mut self, raw: &serde_json::Value, ts: Option<&str>) {
        let Ok(parsed) = serde_json::from_value::<RawRateLimits>(raw.clone()) else {
            return;
        };
        let Some(primary) = parsed.primary else {
            return; // minimal usable shape requires primary (T1 (c))
        };
        let (Some(used), Some(window), Some(resets)) = (
            primary.used_percent,
            primary.window_minutes,
            primary.resets_at,
        ) else {
            return;
        };
        let Some(resets_at) = Utc.timestamp_opt(resets, 0).single() else {
            return;
        };
        let at = ts
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc));
        let newer = match (&self.latest_rate_limits, at) {
            (None, _) => true,
            (Some(cur), Some(new_at)) => cur.at.map_or(true, |cur_at| new_at >= cur_at),
            (Some(_), None) => false,
        };
        if newer {
            self.latest_rate_limits = Some(RateLimitSnapshot {
                at,
                primary_used_percent: used,
                secondary_used_percent: parsed.secondary.and_then(|s| s.used_percent),
                window_minutes: window,
                resets_at,
            });
        }
    }
}

impl UsageSource for CodexSource {
    fn id(&self) -> SourceId {
        SourceId::Codex
    }

    fn display_name(&self) -> &str {
        "Codex"
    }

    fn scan(&mut self, _cursors: &SourceCursors) -> ScanOutcome {
        // TODO(T6): honor cursors for incremental offsets. Full parse for now;
        // uuid resolution and sessions-priority dedup are already final (C1).
        let mut skipped_files: u64 = 0;
        let mut notes: Vec<String> = Vec::new();

        if !self.root.is_dir() {
            self.last_health = SourceHealth::Error {
                reason: format!("codex root not readable: {}", self.root.display()),
            };
            return ScanOutcome::default();
        }

        // uuid → current path, sessions/ first so it wins on duplicates (C1).
        let mut sessions_files: Vec<(String, PathBuf)> = Vec::new();
        let mut archived_files: Vec<(String, PathBuf)> = Vec::new();
        Self::list_tree(
            &self.root.join("sessions"),
            &mut sessions_files,
            &mut skipped_files,
        );
        Self::list_tree(
            &self.root.join("archived_sessions"),
            &mut archived_files,
            &mut skipped_files,
        );
        let mut by_uuid: BTreeMap<String, PathBuf> = BTreeMap::new();
        for (uuid, path) in sessions_files {
            by_uuid.insert(uuid, path);
        }
        for (uuid, path) in archived_files {
            by_uuid.entry(uuid).or_insert(path);
        }

        let mut events: Vec<UsageEvent> = Vec::new();
        let mut skipped_lines: u64 = 0;
        for (uuid, path) in &by_uuid {
            match self.parse_file(uuid, path, &mut events) {
                Ok(skipped) => skipped_lines += skipped,
                // A-3 (TOCTOU): listed-then-moved file is transient — no
                // event, no cursor change, no error; retry next scan.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => notes.push(format!("{}: {}", path.display(), e)),
            }
        }

        let total_skipped = skipped_lines + skipped_files;
        self.last_health = if total_skipped > 0 || !notes.is_empty() {
            SourceHealth::Partial {
                skipped_lines: total_skipped,
                note: if notes.is_empty() {
                    format!("{skipped_files} non-uuid files, {skipped_lines} bad lines")
                } else {
                    notes.join("; ")
                },
            }
        } else {
            SourceHealth::Ok
        };
        ScanOutcome {
            events,
            needs_rebuild: false,
        }
    }

    fn health(&self) -> SourceHealth {
        self.last_health.clone()
    }

    fn rate_limit(&self, _recent: &RecentEvents) -> RateLimitStatus {
        match &self.latest_rate_limits {
            Some(s) => RateLimitStatus::Measured {
                primary_used_percent: s.primary_used_percent,
                secondary_used_percent: s.secondary_used_percent,
                window_minutes: s.window_minutes,
                resets_at: s.resets_at,
            },
            None => RateLimitStatus::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    const UUID_A: &str = "0195aaaa-1111-7000-8000-000000000001";
    const UUID_B: &str = "0195bbbb-2222-7000-8000-000000000002";
    const UUID_X: &str = "0195cccc-3333-7000-8000-000000000003";

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/codex")
    }

    /// Fresh per-test scan root under the OS temp dir (no tempfile dep).
    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meterly-codex-parser-test-{}-{}",
            std::process::id(),
            name
        ));
        if dir.exists() {
            fs::remove_dir_all(&dir).unwrap();
        }
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Install a fixture file at `root/<rel>` (creating parents). Fixture
    /// files are read-only inputs; the destination name carries the uuid.
    fn install_fixture(root: &Path, rel: &str, fixture: &str) {
        let dest = root.join(rel);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(fixture_dir().join(fixture), dest).unwrap();
    }

    fn install_inline(root: &Path, rel: &str, content: &str) {
        let dest = root.join(rel);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(dest, content).unwrap();
    }

    fn scan_root(root: &Path) -> (CodexSource, ScanOutcome) {
        let mut source = CodexSource::new(root.to_path_buf());
        let outcome = source.scan(&SourceCursors::default());
        (source, outcome)
    }

    fn slots(ev: &UsageEvent) -> (u64, u64, u64, u64) {
        (
            ev.input_tokens,
            ev.output_tokens,
            ev.cache_read_tokens,
            ev.cache_creation_tokens,
        )
    }

    fn total_sum(events: &[UsageEvent]) -> u64 {
        events.iter().map(|e| e.total_tokens()).sum()
    }

    /// Build a token_count line: cumulative total + last usage fields.
    fn token_count_line(
        ts: &str,
        cumulative_total: u64,
        input: u64,
        cached: u64,
        output: u64,
    ) -> String {
        format!(
            concat!(
                r#"{{"timestamp": "{ts}", "type": "event_msg", "payload": {{"type": "token_count", "#,
                r#""info": {{"total_token_usage": {{"total_tokens": {total}}}, "#,
                r#""last_token_usage": {{"input_tokens": {input}, "cached_input_tokens": {cached}, "#,
                r#""output_tokens": {output}, "reasoning_output_tokens": 0, "total_tokens": {last_total}}}}}}}}}"#,
            ),
            ts = ts,
            total = cumulative_total,
            input = input,
            cached = cached,
            output = output,
            last_total = input + output,
        )
    }

    // Test case (1): subset semantics — input slot = input - cached, cache
    // slot = cached, output as-is (reasoning ⊆ output, never added), so the
    // event total equals raw input+output exactly. Adversarial: disjoint-sum
    // misread would give 26209, reasoning-added misread 21677 — both fail
    // the exact equality below.
    #[test]
    fn subset_semantics_normalizes_to_disjoint_slots() {
        let root = temp_root("subset");
        install_fixture(
            &root,
            &format!("sessions/2026/07/09/rollout-2026-07-09T03-10-00-{UUID_X}.jsonl"),
            "subset_semantics.jsonl",
        );
        let (source, outcome) = scan_root(&root);

        assert_eq!(outcome.events.len(), 1);
        let ev = &outcome.events[0];
        assert_eq!(slots(ev), (15323, 902, 4992, 0));
        assert_eq!(ev.total_tokens(), 21217); // == raw input + output
        assert_eq!(ev.source, SourceId::Codex);
        assert_eq!(ev.session_id, UUID_X);
        assert_eq!(ev.dedup_key, None); // Codex dedups by uuid cursor, not key
        assert_eq!(ev.model.as_deref(), Some("gpt-5.5")); // T1 (e) turn_context
        assert_eq!(
            ev.timestamp,
            Utc.with_ymd_and_hms(2026, 7, 9, 3, 11, 0).unwrap()
        );
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // Test case (2): cumulative-vs-delta double-count trap. Parser
    // precondition (V3-Q3): each event total equals its record's own
    // last total field (100/150/150). Adversarial: summing the cumulative
    // total_token_usage per event would give 750 and fail the ==400.
    #[test]
    fn basic_session_aggregates_last_deltas_not_cumulative_totals() {
        let root = temp_root("basic");
        install_fixture(
            &root,
            &format!("sessions/2026/07/09/rollout-2026-07-09T03-00-00-{UUID_X}.jsonl"),
            "basic_session.jsonl",
        );
        let (source, outcome) = scan_root(&root);

        assert_eq!(outcome.events.len(), 3);
        // Each last's own total field (raw input+output) survives
        // normalization exactly.
        let totals: Vec<u64> = outcome.events.iter().map(|e| e.total_tokens()).collect();
        assert_eq!(totals, vec![100, 150, 150]);
        let input: u64 = outcome.events.iter().map(|e| e.input_tokens).sum();
        let cache_read: u64 = outcome.events.iter().map(|e| e.cache_read_tokens).sum();
        let output: u64 = outcome.events.iter().map(|e| e.output_tokens).sum();
        let cache_creation: u64 = outcome
            .events
            .iter()
            .map(|e| e.cache_creation_tokens)
            .sum();
        assert_eq!(
            (input, cache_read, output, cache_creation),
            (110, 180, 110, 0)
        );
        assert_eq!(total_sum(&outcome.events), 400);
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // Test case (2'): duplicate emission (cumulative total unchanged, 31.3%
    // measured) is skipped — NOT counted as a skipped line (normal
    // behavior). naive Σlast would give 350 and fail the ==250.
    #[test]
    fn repeated_total_duplicate_emissions_are_skipped_silently() {
        let root = temp_root("repeated");
        install_fixture(
            &root,
            &format!("sessions/2026/07/09/rollout-2026-07-09T03-25-00-{UUID_X}.jsonl"),
            "repeated_total.jsonl",
        );
        let (source, outcome) = scan_root(&root);

        assert_eq!(outcome.events.len(), 2);
        let totals: Vec<u64> = outcome.events.iter().map(|e| e.total_tokens()).collect();
        assert_eq!(totals, vec![100, 150]);
        assert_eq!(total_sum(&outcome.events), 250);
        assert_eq!(source.health(), SourceHealth::Ok); // skip is not Partial
    }

    // Test case (2''): cumulative decrease = reset (resume/compact). The
    // resetting record's last counts as a new baseline and counting
    // continues: 100 + 150 + 50 = 300 (using the final cumulative 50 is the
    // forbidden wrong answer).
    #[test]
    fn total_reset_starts_new_baseline_and_keeps_counting() {
        let root = temp_root("reset");
        install_fixture(
            &root,
            &format!("sessions/2026/07/09/rollout-2026-07-09T03-28-00-{UUID_X}.jsonl"),
            "total_reset.jsonl",
        );
        let (source, outcome) = scan_root(&root);

        assert_eq!(outcome.events.len(), 3);
        let totals: Vec<u64> = outcome.events.iter().map(|e| e.total_tokens()).collect();
        assert_eq!(totals, vec![100, 150, 50]);
        assert_eq!(total_sum(&outcome.events), 300);
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // Test case (3): same uuid in both trees counts ONCE (sessions wins).
    // Adversarial: unconditional summation of both trees would give
    // 130*2 + 45 = 305 and fail. Negative: archived-only uuid still counts.
    #[test]
    fn dup_trees_count_same_uuid_once_sessions_preferred() {
        // Real layout: sessions nested YYYY/MM/DD, archived flat (T1 (g)).
        let root = temp_root("dup");
        install_fixture(
            &root,
            &format!("sessions/2026/01/01/rollout-2026-01-01T00-00-00-{UUID_A}.jsonl"),
            "dup/sessions/2026/01/01/rollout-2026-01-01T00-00-00-0195aaaa-1111-7000-8000-000000000001.jsonl",
        );
        install_fixture(
            &root,
            &format!("archived_sessions/rollout-2026-01-01T00-00-00-{UUID_A}.jsonl"),
            "dup/archived_sessions/rollout-2026-01-01T00-00-00-0195aaaa-1111-7000-8000-000000000001.jsonl",
        );
        install_fixture(
            &root,
            &format!("archived_sessions/rollout-2026-01-02T00-00-00-{UUID_B}.jsonl"),
            "dup/archived_sessions/rollout-2026-01-02T00-00-00-0195bbbb-2222-7000-8000-000000000002.jsonl",
        );
        let (source, outcome) = scan_root(&root);

        let sum_a: u64 = outcome
            .events
            .iter()
            .filter(|e| e.session_id == UUID_A)
            .map(|e| e.total_tokens())
            .sum();
        let count_a = outcome
            .events
            .iter()
            .filter(|e| e.session_id == UUID_A)
            .count();
        let sum_b: u64 = outcome
            .events
            .iter()
            .filter(|e| e.session_id == UUID_B)
            .map(|e| e.total_tokens())
            .sum();
        assert_eq!(count_a, 2); // 4 would mean both trees were summed
        assert_eq!(sum_a, 130);
        assert_eq!(sum_b, 45); // archived-only uuid aggregates normally
        assert_eq!(total_sum(&outcome.events), 175);
        assert_eq!(source.health(), SourceHealth::Ok);

        // Exact equality with a sessions-only scan of the same uuid.
        let solo = temp_root("dup-solo");
        install_fixture(
            &solo,
            &format!("sessions/2026/01/01/rollout-2026-01-01T00-00-00-{UUID_A}.jsonl"),
            "dup/sessions/2026/01/01/rollout-2026-01-01T00-00-00-0195aaaa-1111-7000-8000-000000000001.jsonl",
        );
        let (_, solo_outcome) = scan_root(&solo);
        assert_eq!(sum_a, total_sum(&solo_outcome.events));
    }

    // Test case (3b), scan aspect: moving a file sessions/ → archived_sessions/
    // leaves the full-parse total unchanged — uuid resolution is
    // location-independent. (Cursor-offset continuation is T6's half.)
    #[test]
    fn moved_file_rescan_total_unchanged() {
        let root = temp_root("move");
        let sessions_rel =
            format!("sessions/2026/07/09/rollout-2026-07-09T03-00-00-{UUID_X}.jsonl");
        install_fixture(&root, &sessions_rel, "basic_session.jsonl");
        let (_, before) = scan_root(&root);
        assert_eq!(before.events.len(), 3);
        assert_eq!(total_sum(&before.events), 400);

        let archived_rel =
            format!("archived_sessions/rollout-2026-07-09T03-00-00-{UUID_X}.jsonl");
        fs::create_dir_all(root.join("archived_sessions")).unwrap();
        fs::rename(root.join(&sessions_rel), root.join(&archived_rel)).unwrap();

        let (source, after) = scan_root(&root);
        assert_eq!(after.events.len(), 3);
        assert_eq!(total_sum(&after.events), 400); // exact: move is a no-op
        assert!(after.events.iter().all(|e| e.session_id == UUID_X));
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // Test case (6): rate_limits snapshot → Measured with the fixture's
    // exact values; resets_at is epoch seconds → DateTime (T1 (c):
    // 1782740693 == 2026-06-29T13:44:53Z).
    #[test]
    fn rate_limits_snapshot_yields_measured_exact_values() {
        let root = temp_root("ratelimits");
        install_fixture(
            &root,
            &format!("sessions/2026/07/09/rollout-2026-07-09T03-30-00-{UUID_X}.jsonl"),
            "rate_limits.jsonl",
        );
        let (source, outcome) = scan_root(&root);

        assert_eq!(outcome.events.len(), 1);
        assert_eq!(total_sum(&outcome.events), 15);
        let expected_resets = Utc.timestamp_opt(1_782_740_693, 0).unwrap();
        assert_eq!(
            expected_resets,
            Utc.with_ymd_and_hms(2026, 6, 29, 13, 44, 53).unwrap()
        );
        assert_eq!(
            source.rate_limit(&RecentEvents::default()),
            RateLimitStatus::Measured {
                primary_used_percent: 25.0,
                secondary_used_percent: Some(40.0),
                window_minutes: 300,
                resets_at: expected_resets,
            }
        );
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // Test case (6) negative: `info: null` token_count (97 measured) makes
    // NO event but still refreshes the rate_limits snapshot; no error, no
    // Partial. The fixture also carries extra rate_limits fields (limit_id,
    // plan_type, ...) that must be ignored (optional, T1 (c)).
    #[test]
    fn info_null_updates_snapshot_without_events() {
        let root = temp_root("infonull");
        install_fixture(
            &root,
            &format!("sessions/2026/07/09/rollout-2026-07-09T03-40-00-{UUID_X}.jsonl"),
            "info_null.jsonl",
        );
        let (source, outcome) = scan_root(&root);

        assert_eq!(outcome.events.len(), 0);
        assert_eq!(source.health(), SourceHealth::Ok);
        assert_eq!(
            source.rate_limit(&RecentEvents::default()),
            RateLimitStatus::Measured {
                primary_used_percent: 25.0,
                secondary_used_percent: Some(40.0),
                window_minutes: 300,
                resets_at: Utc.timestamp_opt(1_782_740_693, 0).unwrap(),
            }
        );
    }

    // No snapshot ever seen → Unavailable.
    #[test]
    fn rate_limit_without_snapshot_is_unavailable() {
        let root = temp_root("nosnapshot");
        fs::create_dir_all(root.join("sessions")).unwrap();
        let (source, _) = scan_root(&root);
        assert_eq!(
            source.rate_limit(&RecentEvents::default()),
            RateLimitStatus::Unavailable
        );
    }

    // Test case (6b), forbidden: cached > input (subset violation) — the
    // WHOLE record is discarded (checked_sub, Q-1: no clamp-to-zero), the
    // scan continues, the valid record aggregates, health is
    // Partial{skipped_lines: 1}, no panic. Adversarial: a wrapping
    // subtraction would inject a huge u64 and fail the ==60.
    #[test]
    fn subset_violation_skips_record_and_reports_partial() {
        let root = temp_root("violation");
        install_fixture(
            &root,
            &format!("sessions/2026/07/09/rollout-2026-07-09T03-20-00-{UUID_X}.jsonl"),
            "subset_violation.jsonl",
        );
        let (source, outcome) = scan_root(&root);

        assert_eq!(outcome.events.len(), 1);
        assert_eq!(slots(&outcome.events[0]), (30, 10, 20, 0));
        assert_eq!(total_sum(&outcome.events), 60);
        match source.health() {
            SourceHealth::Partial { skipped_lines, .. } => assert_eq!(skipped_lines, 1),
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    // Extra: malformed JSON line skips+counts; a valid-JSON record with an
    // unknown payload.type is silently ignored (NOT counted); the valid
    // token_count still aggregates.
    #[test]
    fn malformed_lines_skip_and_report_partial() {
        let root = temp_root("malformed");
        install_fixture(
            &root,
            &format!("sessions/2026/07/09/rollout-2026-07-09T03-50-00-{UUID_X}.jsonl"),
            "malformed.jsonl",
        );
        let (source, outcome) = scan_root(&root);

        assert_eq!(outcome.events.len(), 1);
        assert_eq!(total_sum(&outcome.events), 15);
        match source.health() {
            SourceHealth::Partial { skipped_lines, .. } => assert_eq!(skipped_lines, 1),
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    // Extra (A-2): a .jsonl whose file name carries no session uuid is
    // skipped (counted, Partial) without disturbing valid files. Path-keyed
    // fallback parsing is deliberately absent.
    #[test]
    fn non_uuid_jsonl_file_is_skipped_with_partial() {
        let root = temp_root("nouuid");
        install_fixture(
            &root,
            &format!("sessions/2026/07/09/rollout-2026-07-09T03-30-00-{UUID_X}.jsonl"),
            "rate_limits.jsonl",
        );
        install_fixture(&root, "sessions/notes.jsonl", "rate_limits.jsonl");
        let (source, outcome) = scan_root(&root);

        assert_eq!(outcome.events.len(), 1); // only the uuid-named file
        assert_eq!(outcome.events[0].session_id, UUID_X);
        match source.health() {
            SourceHealth::Partial { skipped_lines, .. } => assert_eq!(skipped_lines, 1),
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    // T1 (e): model comes from the latest preceding turn_context in the same
    // file; before any turn_context (or without one) it is None ("unknown"
    // rendering is the UI's job).
    #[test]
    fn turn_context_model_attribution_follows_latest_preceding() {
        let content = [
            token_count_line("2026-07-09T06:00:00.000Z", 10, 8, 0, 2),
            r#"{"timestamp": "2026-07-09T06:01:00.000Z", "type": "turn_context", "payload": {"model": "gpt-5.5"}}"#.to_string(),
            token_count_line("2026-07-09T06:02:00.000Z", 30, 15, 0, 5),
            r#"{"timestamp": "2026-07-09T06:03:00.000Z", "type": "turn_context", "payload": {"model": "gpt-5.1-codex-max"}}"#.to_string(),
            token_count_line("2026-07-09T06:04:00.000Z", 60, 20, 0, 10),
        ]
        .join("\n");
        let root = temp_root("model");
        install_inline(
            &root,
            &format!("sessions/2026/07/09/rollout-2026-07-09T06-00-00-{UUID_X}.jsonl"),
            &content,
        );
        let (source, outcome) = scan_root(&root);

        assert_eq!(outcome.events.len(), 3);
        let models: Vec<Option<&str>> =
            outcome.events.iter().map(|e| e.model.as_deref()).collect();
        assert_eq!(
            models,
            vec![None, Some("gpt-5.5"), Some("gpt-5.1-codex-max")]
        );
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // Boundary cases: an empty file and a 2025-09 legacy flat-format file
    // (`record_type` key, no event_msg envelope, zero token_count) produce
    // zero events and stay health Ok.
    #[test]
    fn empty_and_legacy_flat_files_yield_no_events_health_ok() {
        let root = temp_root("legacy");
        install_inline(
            &root,
            &format!("sessions/2025/09/01/rollout-2025-09-01T00-00-00-{UUID_A}.jsonl"),
            "",
        );
        install_inline(
            &root,
            &format!("sessions/2025/09/02/rollout-2025-09-02T00-00-00-{UUID_B}.jsonl"),
            concat!(
                r#"{"record_type": "state"}"#,
                "\n",
                r#"{"record_type": "message", "content": "legacy"}"#,
                "\n",
            ),
        );
        let (source, outcome) = scan_root(&root);
        assert_eq!(outcome.events.len(), 0);
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // Filesystem access failure (root unreadable) → Error health, no panic.
    // A missing tree (e.g. no archived_sessions/) is NOT an error though.
    #[test]
    fn missing_root_reports_error_health_but_missing_tree_is_fine() {
        let root = temp_root("missing-root").join("does-not-exist");
        let mut source = CodexSource::new(root);
        let outcome = source.scan(&SourceCursors::default());
        assert_eq!(outcome.events.len(), 0);
        assert!(matches!(source.health(), SourceHealth::Error { .. }));

        // sessions/ only, no archived_sessions/: Ok.
        let root = temp_root("no-archived");
        install_fixture(
            &root,
            &format!("sessions/2026/07/09/rollout-2026-07-09T03-30-00-{UUID_X}.jsonl"),
            "rate_limits.jsonl",
        );
        let (source, outcome) = scan_root(&root);
        assert_eq!(outcome.events.len(), 1);
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // AC7: the registry entry carries this parser's constructor.
    #[test]
    fn registry_wires_codex_constructor() {
        let entries = crate::sources::registry();
        let entry = entries
            .iter()
            .find(|e| e.id == SourceId::Codex)
            .expect("codex entry");
        let make_source = entry.make_source.expect("codex parser registered");
        let source = make_source(PathBuf::from("/nonexistent"));
        assert_eq!(source.id(), SourceId::Codex);
        assert_eq!(source.display_name(), "Codex");
    }
}
