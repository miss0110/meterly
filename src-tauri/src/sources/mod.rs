//! Usage-source abstraction: the `UsageSource` trait every parser implements
//! and the static registry (AC7: adding a source = one new file in
//! `sources/` + one entry here).
//!
//! `SourceCursors`, `ScanOutcome` and `RecentEvents` are deliberately
//! minimal; T5/T6 concretize their semantics.

pub mod claude_code;
pub mod codex;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::{RateLimitStatus, SourceHealth, SourceId, UsageEvent};

/// Incremental-scan cursor for one log file (cache schema `cursors` entry).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorEntry {
    pub offset: u64,
    pub size: u64,
    pub mtime_epoch: i64,
}

/// Cursor map for one source. Key semantics are per-source: Claude uses the
/// absolute file path, Codex uses the session uuid (plan C1).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceCursors(pub BTreeMap<String, CursorEntry>);

/// Result of one incremental scan.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScanOutcome {
    pub events: Vec<UsageEvent>,
    /// True when cursors are untrustworthy (e.g. truncation) and the cache
    /// must be rebuilt from scratch.
    pub needs_rebuild: bool,
}

/// Recent events window handed to `rate_limit` (kept by the scheduler, T6).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecentEvents(pub Vec<UsageEvent>);

/// Contract every usage source implements (plan: Contract surface).
pub trait UsageSource: Send {
    fn id(&self) -> SourceId;
    fn display_name(&self) -> &str;
    /// New events since the cursors, or `needs_rebuild`.
    fn scan(&mut self, cursors: &SourceCursors) -> ScanOutcome;
    fn health(&self) -> SourceHealth;
    fn rate_limit(&self, recent: &RecentEvents) -> RateLimitStatus;
}

/// Registry entry: source metadata plus the resolved log root — adding a
/// source stays a one-line change here.
#[derive(Debug, Clone)]
pub struct SourceEntry {
    pub id: SourceId,
    pub display_name: &'static str,
    pub root_path: PathBuf,
    /// Parser constructor (AC7). `None` until the parser task lands.
    pub make_source: Option<fn(PathBuf) -> Box<dyn UsageSource>>,
}

/// Manual impl: metadata fields only — fn-pointer addresses are not
/// meaningful to compare (and `id` determines the constructor anyway).
impl PartialEq for SourceEntry {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.display_name == other.display_name
            && self.root_path == other.root_path
    }
}

/// All registered sources with their log roots resolved once.
///
/// Root resolution (AC4 manual scenarios / tests): `METERLY_CLAUDE_DIR` /
/// `METERLY_CODEX_DIR` env overrides win; otherwise `~/.claude/projects` /
/// `~/.codex`.
pub fn registry() -> Vec<SourceEntry> {
    vec![
        SourceEntry {
            id: SourceId::ClaudeCode,
            display_name: "Claude Code",
            root_path: resolve_root("METERLY_CLAUDE_DIR", &[".claude", "projects"]),
            make_source: Some(claude_code::make),
        },
        SourceEntry {
            id: SourceId::Codex,
            display_name: "Codex",
            root_path: resolve_root("METERLY_CODEX_DIR", &[".codex"]),
            make_source: Some(codex::make),
        },
        // New sources register here: one `SourceEntry` per parser file (AC7).
    ]
}

/// Env override wins; otherwise `~/<default_segments...>`. An unresolvable
/// home yields a relative fallback path — the scan layer reports it as a
/// per-source `Error` health instead of panicking.
fn resolve_root(env_var: &str, default_segments: &[&str]) -> PathBuf {
    if let Some(dir) = std::env::var_os(env_var) {
        return PathBuf::from(dir);
    }
    let mut root = dirs::home_dir().unwrap_or_default();
    for segment in default_segments {
        root.push(segment);
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    /// Compile-time proof the trait is object safe (scheduler holds
    /// `Box<dyn UsageSource>`).
    fn _assert_object_safe(_: &mut dyn UsageSource) {}

    /// Minimal mock proving the trait is implementable as specified (AC7).
    struct MockSource;

    impl UsageSource for MockSource {
        fn id(&self) -> SourceId {
            SourceId::Codex
        }
        fn display_name(&self) -> &str {
            "Mock"
        }
        fn scan(&mut self, _cursors: &SourceCursors) -> ScanOutcome {
            ScanOutcome {
                events: vec![UsageEvent {
                    source: SourceId::Codex,
                    session_id: "0195aaaa-0000-0000-0000-000000000001".into(),
                    dedup_key: None,
                    timestamp: Utc.with_ymd_and_hms(2026, 7, 13, 0, 0, 0).unwrap(),
                    model: Some("gpt-5.5".into()),
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                }],
                needs_rebuild: false,
            }
        }
        fn health(&self) -> SourceHealth {
            SourceHealth::Ok
        }
        fn rate_limit(&self, _recent: &RecentEvents) -> RateLimitStatus {
            RateLimitStatus::Unavailable
        }
    }

    #[test]
    fn usage_source_trait_is_implementable_and_boxable() {
        let mut boxed: Box<dyn UsageSource> = Box::new(MockSource);
        assert_eq!(boxed.id(), SourceId::Codex);
        assert_eq!(boxed.display_name(), "Mock");
        let outcome = boxed.scan(&SourceCursors::default());
        assert_eq!(outcome.events.len(), 1);
        assert!(!outcome.needs_rebuild);
        assert_eq!(boxed.health(), SourceHealth::Ok);
        assert_eq!(
            boxed.rate_limit(&RecentEvents::default()),
            RateLimitStatus::Unavailable
        );
    }

    /// Env override and default resolution live in one test: cargo runs
    /// tests in parallel threads and process env is shared, so the set /
    /// remove sequence must not be split across tests.
    #[test]
    fn registry_resolves_roots_with_env_override_and_defaults() {
        // Override.
        std::env::set_var("METERLY_CLAUDE_DIR", "/tmp/meterly-test/claude");
        std::env::set_var("METERLY_CODEX_DIR", "/tmp/meterly-test/codex");
        let entries = registry();
        assert_eq!(entries.len(), 2);
        let claude = entries
            .iter()
            .find(|e| e.id == SourceId::ClaudeCode)
            .expect("claude entry");
        let codex = entries
            .iter()
            .find(|e| e.id == SourceId::Codex)
            .expect("codex entry");
        assert_eq!(claude.display_name, "Claude Code");
        assert_eq!(codex.display_name, "Codex");
        assert_eq!(claude.root_path, PathBuf::from("/tmp/meterly-test/claude"));
        assert_eq!(codex.root_path, PathBuf::from("/tmp/meterly-test/codex"));

        // Defaults.
        std::env::remove_var("METERLY_CLAUDE_DIR");
        std::env::remove_var("METERLY_CODEX_DIR");
        let entries = registry();
        assert_eq!(entries.len(), 2);
        let home = dirs::home_dir().expect("home dir");
        let claude = entries
            .iter()
            .find(|e| e.id == SourceId::ClaudeCode)
            .expect("claude entry");
        let codex = entries
            .iter()
            .find(|e| e.id == SourceId::Codex)
            .expect("codex entry");
        assert_eq!(claude.root_path, home.join(".claude").join("projects"));
        assert_eq!(codex.root_path, home.join(".codex"));
    }
}
