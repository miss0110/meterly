//! Claude Code JSONL parser (T4).
//!
//! Parses `~/.claude/projects/**/*.jsonl` session logs into [`UsageEvent`]s.
//! Only `type == "assistant"` records carry usage; the four token slots map
//! 1:1 because Claude's fields are pairwise disjoint (T1 (b),
//! fixtures/README.md). Resume/continue copies are deduplicated globally by
//! `message.id + requestId` within one scan (B7, T1 (h)).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::DateTime;
use serde::Deserialize;

use crate::model::{RateLimitStatus, SourceHealth, SourceId, UsageEvent};

use super::{RecentEvents, ScanOutcome, SourceCursors, UsageSource};

/// Claude's rolling rate-limit window. The actual estimate over
/// `RecentEvents` is the aggregator's job (T7); only the constant lives here.
pub const RATE_LIMIT_WINDOW_HOURS: u32 = 5;

/// Registry constructor slot (AC7).
pub fn make(root: PathBuf) -> Box<dyn UsageSource> {
    Box::new(ClaudeCodeSource::new(root))
}

pub struct ClaudeCodeSource {
    root: PathBuf,
    /// Health of the most recent `scan` (Ok before the first scan).
    last_health: SourceHealth,
}

impl ClaudeCodeSource {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            last_health: SourceHealth::Ok,
        }
    }
}

impl UsageSource for ClaudeCodeSource {
    fn id(&self) -> SourceId {
        SourceId::ClaudeCode
    }

    fn display_name(&self) -> &str {
        "Claude Code"
    }

    fn scan(&mut self, _cursors: &SourceCursors) -> ScanOutcome {
        // TODO(T6): honor cursors (incremental offsets). Until T6 lands
        // every scan re-parses the whole directory tree.
        let mut files = Vec::new();
        let mut notes = Vec::new();
        if let Err(err) = collect_jsonl_files(&self.root, &mut files, &mut notes) {
            self.last_health = SourceHealth::Error {
                reason: format!("cannot read {}: {err}", self.root.display()),
            };
            return ScanOutcome::default();
        }
        // Deterministic order: file paths, so duplicate-key resolution does
        // not depend on directory iteration order.
        files.sort();

        let mut events: Vec<UsageEvent> = Vec::new();
        // dedup_key -> index into `events` (B7: global across all files of
        // this scan; persistent seen_keys across scans is T6's job).
        let mut by_key: HashMap<String, usize> = HashMap::new();
        let mut skipped_lines: u64 = 0;

        for path in &files {
            let content = match fs::read_to_string(path) {
                Ok(content) => content,
                Err(err) => {
                    // Per-file failure: skip the file, note it, keep going.
                    notes.push(format!("skipped {}: {err}", path.display()));
                    continue;
                }
            };
            let session_id = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                match parse_line(line, &session_id) {
                    LineOutcome::Event(event) => match &event.dedup_key {
                        Some(key) => {
                            if let Some(&idx) = by_key.get(key) {
                                // T1 (h): duplicate keys are streaming
                                // snapshots / resume copies — adopt the
                                // record with the largest output_tokens
                                // (the final snapshot). Not a skip.
                                if event.output_tokens > events[idx].output_tokens {
                                    events[idx] = event;
                                }
                            } else {
                                by_key.insert(key.clone(), events.len());
                                events.push(event);
                            }
                        }
                        // No dedup key (missing id/requestId): aggregate
                        // as-is.
                        None => events.push(event),
                    },
                    LineOutcome::Ignored => {}
                    LineOutcome::Skipped => skipped_lines += 1,
                }
            }
        }

        self.last_health = if skipped_lines > 0 || !notes.is_empty() {
            let note = if notes.is_empty() {
                format!("{skipped_lines} malformed line(s) skipped")
            } else {
                notes.join("; ")
            };
            SourceHealth::Partial {
                skipped_lines,
                note,
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
        // TODO(T7): estimate usage over RATE_LIMIT_WINDOW_HOURS from
        // `recent` (rolling-window heuristic, UI label "추정").
        RateLimitStatus::Unavailable
    }
}

/// Raw log line shape — allowlist projection of what the parser needs.
/// Unknown keys are ignored by serde, so schema growth is harmless.
#[derive(Deserialize)]
struct RawRecord {
    #[serde(rename = "type")]
    record_type: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    message: Option<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<RawUsage>,
}

/// The four token slots are pairwise disjoint in Claude logs (T1 (b)) and
/// map 1:1 onto `UsageEvent`. Missing fields default to 0 (legacy variants).
#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

enum LineOutcome {
    Event(UsageEvent),
    /// Valid JSON that carries no usage (non-assistant, or assistant
    /// without usage) — a normal variant, never counted as skipped.
    Ignored,
    /// Malformed line — counted in `skipped_lines`.
    Skipped,
}

fn parse_line(line: &str, session_id: &str) -> LineOutcome {
    let raw: RawRecord = match serde_json::from_str(line) {
        Ok(raw) => raw,
        Err(_) => return LineOutcome::Skipped,
    };
    if raw.record_type.as_deref() != Some("assistant") {
        return LineOutcome::Ignored;
    }
    let Some(message) = raw.message else {
        return LineOutcome::Ignored;
    };
    // Assistant records without usage exist as a normal variant (interim
    // message chunks): no event, no skip count.
    let Some(usage) = message.usage else {
        return LineOutcome::Ignored;
    };
    // A usage-bearing record without a parseable timestamp cannot be
    // bucketed — treat it as malformed (counted).
    let Some(timestamp) = raw
        .timestamp
        .as_deref()
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
    else {
        return LineOutcome::Skipped;
    };
    // B7 (T1 (h)): dedup key is message.id + requestId; either one missing
    // means the record cannot dedup and aggregates as-is.
    let dedup_key = match (&message.id, &raw.request_id) {
        (Some(id), Some(request_id)) => Some(format!("{id}:{request_id}")),
        _ => None,
    };
    LineOutcome::Event(UsageEvent {
        source: SourceId::ClaudeCode,
        session_id: session_id.to_owned(),
        dedup_key,
        timestamp: timestamp.with_timezone(&chrono::Utc),
        model: message.model,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_input_tokens,
        cache_creation_tokens: usage.cache_creation_input_tokens,
    })
}

/// Recursively collect `*.jsonl` under `dir`. Only the root-level failure
/// propagates (→ Error health); nested failures become notes so the scan
/// keeps going.
fn collect_jsonl_files(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    notes: &mut Vec<String>,
) -> std::io::Result<()> {
    let entries = fs::read_dir(dir)?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                notes.push(format!("skipped entry under {}: {err}", dir.display()));
                continue;
            }
        };
        let path = entry.path();
        if path.is_dir() {
            if let Err(err) = collect_jsonl_files(&path, out, notes) {
                notes.push(format!("skipped dir {}: {err}", path.display()));
            }
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/claude")
    }

    /// Fresh per-test scan root under the OS temp dir (no tempfile dep).
    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meterly-claude-parser-test-{}-{}",
            std::process::id(),
            name
        ));
        if dir.exists() {
            fs::remove_dir_all(&dir).unwrap();
        }
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Copy named fixtures into a fresh root and scan it once.
    fn scan_fixtures(name: &str, files: &[&str]) -> (ClaudeCodeSource, ScanOutcome) {
        let root = temp_root(name);
        for f in files {
            fs::copy(fixture_dir().join(f), root.join(f)).unwrap();
        }
        let mut source = ClaudeCodeSource::new(root);
        let outcome = source.scan(&SourceCursors::default());
        (source, outcome)
    }

    /// Write inline JSONL content as `<session>.jsonl` and scan the root.
    fn scan_inline(name: &str, files: &[(&str, &str)]) -> (ClaudeCodeSource, ScanOutcome) {
        let root = temp_root(name);
        for (session, content) in files {
            fs::write(root.join(format!("{session}.jsonl")), content).unwrap();
        }
        let mut source = ClaudeCodeSource::new(root);
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

    // Test case (4): basic fixture — AC3 wording "input 합 600", every
    // record keeps all four slots. The fixture is placed in a nested
    // subdirectory to prove the recursive walk (~/.claude/projects/<slug>/).
    #[test]
    fn basic_fixture_sums_input_600_with_slots_preserved() {
        let root = temp_root("basic");
        let nested = root.join("project-slug");
        fs::create_dir_all(&nested).unwrap();
        fs::copy(fixture_dir().join("basic.jsonl"), nested.join("basic.jsonl")).unwrap();
        let mut source = ClaudeCodeSource::new(root);
        let outcome = source.scan(&SourceCursors::default());

        assert_eq!(outcome.events.len(), 3);
        assert!(!outcome.needs_rebuild);
        let input_sum: u64 = outcome.events.iter().map(|e| e.input_tokens).sum();
        assert_eq!(input_sum, 600);
        assert_eq!(slots(&outcome.events[0]), (100, 10, 0, 0));
        assert_eq!(slots(&outcome.events[1]), (200, 20, 0, 0));
        assert_eq!(slots(&outcome.events[2]), (300, 30, 0, 0));
        for ev in &outcome.events {
            assert_eq!(ev.source, SourceId::ClaudeCode);
            assert_eq!(ev.session_id, "basic"); // file stem, not sessionId field
            assert_eq!(ev.model.as_deref(), Some("claude-fable-5"));
        }
        assert_eq!(
            outcome.events[0].dedup_key.as_deref(),
            Some("msg_fixture_basic_01:req_fixture_basic_01")
        );
        assert_eq!(
            outcome.events[0].timestamp,
            Utc.with_ymd_and_hms(2026, 7, 9, 1, 0, 0).unwrap()
        );
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // Test case (4): cache record — slot-wise assertions that fail if cache
    // tokens are lumped into input (adversarial: lumped input would be 1300).
    #[test]
    fn cache_record_preserves_four_disjoint_slots() {
        let (source, outcome) = scan_fixtures("cache", &["cache_record.jsonl"]);
        assert_eq!(outcome.events.len(), 1);
        let ev = &outcome.events[0];
        assert_eq!(ev.input_tokens, 100);
        assert_eq!(ev.output_tokens, 50);
        assert_eq!(ev.cache_read_tokens, 1000);
        assert_eq!(ev.cache_creation_tokens, 200);
        assert_eq!(ev.total_tokens(), 1350);
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // Test case (4) negative: schema drift — missing cache fields become 0,
    // health stays Ok (not a skipped line).
    #[test]
    fn legacy_missing_cache_fields_default_to_zero_health_ok() {
        let (source, outcome) = scan_fixtures("legacy", &["legacy_missing_cache_fields.jsonl"]);
        assert_eq!(outcome.events.len(), 1);
        assert_eq!(slots(&outcome.events[0]), (40, 5, 0, 0));
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // Test case (4) forbidden: malformed JSON lines are skipped and counted,
    // valid records on other lines still aggregate, no panic. The fixture
    // has exactly 2 bad lines (truncated JSON + non-JSON), 1 user line
    // (ignored, NOT counted) and 1 valid assistant record.
    #[test]
    fn malformed_lines_skip_and_report_partial() {
        let (source, outcome) = scan_fixtures("malformed", &["malformed.jsonl"]);
        assert_eq!(outcome.events.len(), 1);
        assert_eq!(slots(&outcome.events[0]), (42, 7, 0, 0));
        match source.health() {
            SourceHealth::Partial { skipped_lines, .. } => assert_eq!(skipped_lines, 2),
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    // Test case (5): resume/continue duplicates dedup globally across files
    // within one scan. Expected == a entirely + b's new record only.
    // Adversarial: naive per-file summation would yield input 900 / output 90.
    #[test]
    fn resume_duplicates_dedup_globally_across_files() {
        let (source, outcome) = scan_fixtures(
            "resume-dup",
            &["resume_duplicate_a.jsonl", "resume_duplicate_b.jsonl"],
        );
        assert_eq!(outcome.events.len(), 3);
        let input_sum: u64 = outcome.events.iter().map(|e| e.input_tokens).sum();
        let output_sum: u64 = outcome.events.iter().map(|e| e.output_tokens).sum();
        assert_eq!(input_sum, 600); // 100 + 200 + 300, copies counted once
        assert_eq!(output_sum, 60); // 10 + 20 + 30
        // Every dedup key appears exactly once.
        let mut keys: Vec<&str> = outcome
            .events
            .iter()
            .map(|e| e.dedup_key.as_deref().expect("fixture records have keys"))
            .collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), 3);
        // Dedup is not a parse failure: health stays Ok.
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // Test case (5) streaming-snapshot rule (T1 (h)): among records sharing
    // a dedup key, the one with the LARGEST output_tokens wins — the whole
    // record, in either arrival order. Inputs differ per snapshot so a
    // "merge maxima" bug would produce input 12 and fail.
    #[test]
    fn duplicate_key_adopts_record_with_max_output_tokens() {
        let rec = |input: u64, output: u64| {
            format!(
                concat!(
                    r#"{{"type": "assistant", "timestamp": "2026-07-09T03:00:00.000Z", "#,
                    r#""requestId": "req_snap", "message": {{"id": "msg_snap", "#,
                    r#""model": "claude-fable-5", "usage": {{"input_tokens": {}, "#,
                    r#""output_tokens": {}}}}}}}"#,
                ),
                input, output
            )
        };
        // Ascending order: final snapshot last.
        let ascending = [rec(10, 5), rec(11, 20), rec(12, 12)].join("\n");
        let (source, outcome) = scan_inline("snapshot-asc", &[("sess-asc", &ascending)]);
        assert_eq!(outcome.events.len(), 1);
        assert_eq!(slots(&outcome.events[0]), (11, 20, 0, 0));
        assert_eq!(source.health(), SourceHealth::Ok);

        // Max output first: later smaller snapshots must not replace it.
        let descending = [rec(11, 20), rec(10, 5)].join("\n");
        let (_, outcome) = scan_inline("snapshot-desc", &[("sess-desc", &descending)]);
        assert_eq!(outcome.events.len(), 1);
        assert_eq!(slots(&outcome.events[0]), (11, 20, 0, 0));
    }

    // Negative: assistant without usage (normal variant) and non-assistant
    // records produce no events and are NOT counted as skipped.
    #[test]
    fn no_usage_assistant_and_non_assistant_records_are_silently_ignored() {
        let content = concat!(
            r#"{"type": "assistant", "timestamp": "2026-07-09T04:00:00.000Z", "requestId": "req_x", "message": {"id": "msg_x", "model": "claude-fable-5"}}"#,
            "\n",
            r#"{"type": "user", "timestamp": "2026-07-09T04:00:01.000Z", "sessionId": "s"}"#,
            "\n",
            r#"{"type": "summary", "summary": "irrelevant"}"#,
            "\n",
        );
        let (source, outcome) = scan_inline("ignored", &[("sess-ignored", content)]);
        assert_eq!(outcome.events.len(), 0);
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // B7: missing message.id or requestId → dedup_key None, and such
    // records aggregate as-is (never merged with each other).
    #[test]
    fn missing_id_fields_yield_none_dedup_key_and_no_dedup() {
        let content = concat!(
            r#"{"type": "assistant", "timestamp": "2026-07-09T05:00:00.000Z", "message": {"id": "msg_same", "model": "claude-fable-5", "usage": {"input_tokens": 5, "output_tokens": 1}}}"#,
            "\n",
            r#"{"type": "assistant", "timestamp": "2026-07-09T05:01:00.000Z", "message": {"id": "msg_same", "model": "claude-fable-5", "usage": {"input_tokens": 5, "output_tokens": 1}}}"#,
            "\n",
        );
        let (source, outcome) = scan_inline("no-key", &[("sess-nokey", content)]);
        assert_eq!(outcome.events.len(), 2);
        assert!(outcome.events.iter().all(|e| e.dedup_key.is_none()));
        let input_sum: u64 = outcome.events.iter().map(|e| e.input_tokens).sum();
        assert_eq!(input_sum, 10);
        assert_eq!(source.health(), SourceHealth::Ok);
    }

    // Filesystem access failure (root unreadable) → Error health, no panic.
    #[test]
    fn missing_root_reports_error_health() {
        let root = temp_root("missing-root").join("does-not-exist");
        let mut source = ClaudeCodeSource::new(root);
        let outcome = source.scan(&SourceCursors::default());
        assert_eq!(outcome.events.len(), 0);
        assert!(matches!(source.health(), SourceHealth::Error { .. }));
    }

    // AC7: the registry entry carries this parser's constructor.
    #[test]
    fn registry_wires_claude_code_constructor() {
        let entries = crate::sources::registry();
        let entry = entries
            .iter()
            .find(|e| e.id == SourceId::ClaudeCode)
            .expect("claude entry");
        let make_source = entry.make_source.expect("claude parser registered");
        let source = make_source(PathBuf::from("/nonexistent"));
        assert_eq!(source.id(), SourceId::ClaudeCode);
        assert_eq!(source.display_name(), "Claude Code");
    }
}
