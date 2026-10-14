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
    /// Repo attribution (see [`RepoResolver`]): which git repository each
    /// turn actually worked in, instead of the directory Claude was started in.
    repos: RepoResolver,
}

impl ClaudeCodeSource {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            last_health: SourceHealth::Ok,
            repos: RepoResolver::for_home(),
        }
    }
}

/// Attributes Claude Code usage to the git repository a turn actually touched.
///
/// A session's `cwd` is only where Claude was launched; people routinely start
/// in one folder and work across several repos (one user's "claude-admin" held
/// 5.7B tokens, 93% of it spent in other repos). So each tool call's absolute
/// paths (Read/Edit/Write `file_path`, Grep/Glob `path`, `cd …` in Bash) are
/// mapped to their enclosing git root, and usage is billed to the repo of the
/// most recent such call. Paths outside any repo (scratch dirs, ~/Library) are
/// ignored; a session that never touches a repo keeps its `cwd` basename.
///
/// Only the repo's folder NAME is used, like before — never a path.
struct RepoResolver {
    /// Absolute-path prefixes looked for in tool calls (the home dir).
    /// Narrowing to home keeps system paths out.
    prefixes: Vec<String>,
    /// A git root equal to one of these is not a "project" (dotfile repos).
    excluded_roots: Vec<PathBuf>,
    /// Home dir as text, for expanding `~/`, `$HOME/`, `${HOME}/` in commands
    /// (`cd ~/project/x` is the common form — missing it mis-billed whole
    /// sessions to whatever absolute path happened to sit on the same line).
    home: Option<String>,
    /// directory → repo name (None = not inside a repo). Memoized across
    /// scans: a full re-parse runs on every log write, `.git` probes must not.
    cache: HashMap<PathBuf, Option<String>>,
}

impl RepoResolver {
    fn for_home() -> Self {
        let home = dirs::home_dir();
        let prefixes = home
            .as_ref()
            .map(|h| h.to_string_lossy().trim_end_matches(['/', '\\']).to_string())
            .filter(|p| !p.is_empty())
            .into_iter()
            .collect();
        Self {
            prefixes,
            home: home
                .as_ref()
                .map(|h| h.to_string_lossy().trim_end_matches(['/', '\\']).to_string()),
            excluded_roots: home.into_iter().collect(),
            cache: HashMap::new(),
        }
    }

    #[cfg(test)]
    fn with_prefix(prefix: &Path) -> Self {
        Self {
            prefixes: vec![prefix.to_string_lossy().into_owned()],
            home: Some(prefix.to_string_lossy().into_owned()),
            excluded_roots: vec![prefix.to_path_buf()],
            cache: HashMap::new(),
        }
    }

    /// The repo a tool-call line touched, or None. With several repos on one
    /// line (a Bash command spanning two), the most mentioned wins, ties → first.
    ///
    /// Only the tool calls' `input` values are read — never the rest of the
    /// record, whose `cwd` field names the launch folder on every line.
    fn repo_of_line(&mut self, line: &str) -> Option<String> {
        if !line.contains("\"tool_use\"") {
            return None;
        }
        #[derive(Deserialize)]
        struct Rec {
            message: Option<Msg>,
        }
        #[derive(Deserialize)]
        struct Msg {
            #[serde(default)]
            content: Vec<Block>,
        }
        #[derive(Deserialize)]
        struct Block {
            #[serde(rename = "type")]
            kind: Option<String>,
            input: Option<serde_json::Value>,
        }
        // `content` can also be a plain string on some records → no tools.
        let rec: Rec = serde_json::from_str(line).ok()?;
        let mut texts: Vec<String> = Vec::new();
        for block in rec.message?.content {
            if block.kind.as_deref() == Some("tool_use") {
                if let Some(input) = block.input {
                    collect_strings(&input, &mut texts);
                }
            }
        }
        let mut counts: Vec<(String, usize)> = Vec::new();
        let paths: Vec<String> = texts
            .iter()
            .map(|t| expand_home(t, self.home.as_deref()))
            .flat_map(|t| extract_paths(&t, &self.prefixes))
            // A dependency inside another repo (`…/argus/web/node_modules/
            // playwright`) is a tool being borrowed, not work on that repo.
            .filter(|p| !p.contains("/node_modules/") && !p.ends_with("/node_modules"))
            .collect();
        for path in paths {
            if let Some(repo) = self.repo_of_path(Path::new(&path)) {
                match counts.iter_mut().find(|(r, _)| *r == repo) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((repo, 1)),
                }
            }
        }
        let max = counts.iter().map(|(_, n)| *n).max()?;
        counts.into_iter().find(|(_, n)| *n == max).map(|(r, _)| r)
    }

    /// Nearest ancestor holding `.git` (dir, or file for worktrees/submodules).
    /// The path itself need not exist any more.
    fn repo_of_path(&mut self, path: &Path) -> Option<String> {
        if self.cache.len() > 50_000 {
            self.cache.clear(); // bound memory on pathological corpora
        }
        let mut visited: Vec<PathBuf> = Vec::new();
        let mut found: Option<String> = None;
        for dir in path.ancestors() {
            if let Some(hit) = self.cache.get(dir) {
                found = hit.clone();
                break;
            }
            if self.excluded_roots.iter().any(|r| r == dir) || dir.parent().is_none() {
                break;
            }
            visited.push(dir.to_path_buf());
            if dir.join(".git").exists() {
                found = dir.file_name().map(|n| n.to_string_lossy().into_owned());
                break;
            }
        }
        for dir in visited {
            self.cache.insert(dir, found.clone());
        }
        found
    }
}

/// `~/x`, `$HOME/x`, `${HOME}/x` → `<home>/x` where they start a word.
fn expand_home(text: &str, home: Option<&str>) -> String {
    let Some(home) = home else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut prev: Option<char> = None;
    'outer: while !rest.is_empty() {
        let at_word_start = !matches!(prev, Some(c) if c.is_alphanumeric() || c == '/' || c == '.' || c == '-' || c == '_');
        if at_word_start {
            for token in ["~/", "$HOME/", "${HOME}/"] {
                if rest.starts_with(token) {
                    out.push_str(home);
                    out.push('/');
                    rest = &rest[token.len()..];
                    prev = Some('/');
                    continue 'outer;
                }
            }
        }
        let c = rest.chars().next().expect("non-empty");
        out.push(c);
        prev = Some(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Every string leaf of a JSON value (tool inputs nest: MultiEdit, Agent…).
fn collect_strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_strings(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| collect_strings(x, out)),
        _ => {}
    }
}

/// Absolute paths under any of `prefixes` inside a (decoded) tool-input
/// string. A path ends at a shell delimiter; spaces end it too, which only
/// costs the tail — the `.git` walk goes upward anyway.
fn extract_paths(text: &str, prefixes: &[String]) -> Vec<String> {
    const DELIMS: &[char] = &['"', '\'', ' ', '\t', '\n', '\r', ',', ';', ')', '(', '<', '>', '|', '&', '`', '*', '{', '}', '[', ']', '='];
    let mut out = Vec::new();
    for prefix in prefixes {
        for (idx, _) in text.match_indices(prefix.as_str()) {
            // Must start a path (not sit inside a longer one).
            let before = text[..idx].chars().next_back();
            if matches!(before, Some(c) if c.is_alphanumeric() || c == '/' || c == '.' || c == '-') {
                continue;
            }
            let rest = &text[idx..];
            let end = rest
                .char_indices()
                // On unix a backslash is an escape (`a\ b`, line continuation),
                // never part of the path.
                .find(|&(_, c)| DELIMS.contains(&c) || (cfg!(not(windows)) && c == '\\'))
                .map(|(i, _)| i)
                .unwrap_or(rest.len());
            let path = rest[..end].trim_end_matches(['/', '\\', ':', '.']).to_string();
            if path.len() > prefix.len() {
                out.push(path);
            }
        }
    }
    out
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
            // Project per turn: the repo of the most recent tool call (see
            // RepoResolver). Turns before the first tool call take the first
            // repo the session touches; no repo at all → the launch cwd.
            let lines: Vec<&str> = content.lines().collect();
            let touched: Vec<Option<String>> =
                lines.iter().map(|l| self.repos.repo_of_line(l)).collect();
            let mut project = touched
                .iter()
                .flatten()
                .next()
                .cloned()
                .or_else(|| file_project(&content));
            for (line, repo) in lines.into_iter().zip(touched) {
                if repo.is_some() {
                    project = repo;
                }
                if line.trim().is_empty() {
                    continue;
                }
                match parse_line(line, &session_id, &project) {
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
            // Full-parse source: events REPLACE claude aggregates each scan
            // (54MB corpus — re-parse is cheap and self-healing), so no
            // cursors and no persistent seen_keys are needed.
            cursors: None,
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

/// The session's project (basename of its `cwd`). Claude stamps `cwd` on many
/// records; the first one we find applies to the whole session file.
fn file_project(content: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct CwdOnly {
        cwd: Option<String>,
    }
    for line in content.lines() {
        if !line.contains("\"cwd\"") {
            continue;
        }
        if let Ok(c) = serde_json::from_str::<CwdOnly>(line) {
            if let Some(cwd) = c.cwd {
                if let Some(p) = crate::sources::project_from_cwd(&cwd) {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn parse_line(line: &str, session_id: &str, project: &Option<String>) -> LineOutcome {
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
        project: project.clone(),
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

    /// One assistant record with usage, optionally carrying a tool call that
    /// touches `tool_path` (Edit `file_path`).
    fn assistant_line(n: u32, cwd: &str, tool_path: Option<&str>) -> String {
        let content = match tool_path {
            Some(p) => format!(
                r#"[{{"type":"tool_use","id":"t{n}","name":"Edit","input":{{"file_path":"{p}","old_string":"a","new_string":"b"}}}}]"#
            ),
            None => r#"[{"type":"text","text":"hi"}]"#.to_string(),
        };
        format!(
            r#"{{"type":"assistant","cwd":"{cwd}","timestamp":"2026-07-13T01:0{n}:00Z","requestId":"r{n}","message":{{"id":"m{n}","model":"claude-opus-5","content":{content},"usage":{{"input_tokens":{n},"output_tokens":1}}}}}}"#
        )
    }

    /// Repo attribution: turns follow the repo of the latest tool call; the
    /// leading turn borrows the first repo; non-repo paths don't switch.
    #[test]
    fn usage_is_attributed_to_the_repo_each_turn_touched_not_the_launch_cwd() {
        let base = temp_root("repo-attrib");
        for repo in ["alpha", "beta"] {
            fs::create_dir_all(base.join(repo).join(".git")).unwrap();
            fs::create_dir_all(base.join(repo).join("src")).unwrap();
        }
        fs::create_dir_all(base.join("scratch")).unwrap(); // not a repo
        let b = base.to_string_lossy();
        let launch = format!("{b}/launcher");
        let lines = [
            assistant_line(1, &launch, None),                                   // before any tool → alpha (lookahead)
            assistant_line(2, &launch, Some(&format!("{b}/alpha/src/x.rs"))), // alpha
            assistant_line(3, &launch, None),                                   // stays alpha
            assistant_line(4, &launch, Some(&format!("{b}/scratch/n.txt"))),  // non-repo → stays alpha
            assistant_line(5, &launch, Some(&format!("{b}/beta/src/y.rs"))),  // beta
            assistant_line(6, &launch, None),                                   // stays beta
        ]
        .join("\n");
        let root = base.join("logs");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("s1.jsonl"), lines).unwrap();
        let mut source = ClaudeCodeSource::new(root);
        source.repos = RepoResolver::with_prefix(&base);
        let mut events = source.scan(&SourceCursors::default()).events;
        events.sort_by_key(|e| e.input_tokens);
        let projects: Vec<_> = events.iter().map(|e| e.project.as_deref()).collect();
        assert_eq!(
            projects,
            vec![Some("alpha"), Some("alpha"), Some("alpha"), Some("alpha"), Some("beta"), Some("beta")]
        );
    }

    /// Live check against the real ~/.claude/projects: per-project totals and
    /// scan time. `cargo test --lib live_claude_project_breakdown -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_claude_project_breakdown() {
        let root = dirs::home_dir().unwrap().join(".claude/projects");
        let mut source = ClaudeCodeSource::new(root);
        let t = std::time::Instant::now();
        let events = source.scan(&SourceCursors::default()).events;
        let first = t.elapsed();
        let t = std::time::Instant::now();
        let _ = source.scan(&SourceCursors::default());
        let second = t.elapsed();
        // Baseline: same scan with repo attribution off (cwd-only, as before).
        let mut plain = ClaudeCodeSource::new(dirs::home_dir().unwrap().join(".claude/projects"));
        plain.repos.prefixes.clear();
        let _ = plain.scan(&SourceCursors::default());
        let t = std::time::Instant::now();
        let _ = plain.scan(&SourceCursors::default());
        println!("baseline(cwd-only) scan={:?}", t.elapsed());
        let mut by: HashMap<String, u64> = HashMap::new();
        for e in &events {
            *by.entry(e.project.clone().unwrap_or_else(|| "(none)".into())).or_default() += e.total_tokens();
        }
        let mut v: Vec<_> = by.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        println!("events={} scan1={first:?} scan2(cached)={second:?}", events.len());
        // Yesterday's sessions, split by project (dashboard "어제" check).
        let yday = chrono::Local::now().date_naive() - chrono::Duration::days(1);
        let mut sess: HashMap<String, HashMap<String, u64>> = HashMap::new();
        for e in events.iter().filter(|e| e.timestamp.with_timezone(&chrono::Local).date_naive() == yday) {
            *sess.entry(e.session_id[..8].to_string()).or_default()
                .entry(e.project.clone().unwrap_or_default()).or_default() += e.total_tokens();
        }
        for (sid, split) in &sess {
            let mut v: Vec<_> = split.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            let total: u64 = v.iter().map(|x| *x.1).sum();
            println!("  {yday} {sid} {:.0}M {:?}", total as f64 / 1e6,
                v.iter().map(|(p, t)| format!("{p} {:.0}M", **t as f64 / 1e6)).collect::<Vec<_>>());
        }
        for (p, t) in v.iter().take(15) {
            println!("  {p:28} {:.2}B", *t as f64 / 1e9);
        }
    }

    /// No tool call ever touches a repo → keep the launch cwd basename.
    #[test]
    fn session_without_repo_paths_keeps_cwd_project() {
        let base = temp_root("repo-none");
        fs::create_dir_all(base.join("scratch")).unwrap();
        let b = base.to_string_lossy();
        let launch = format!("{b}/launcher");
        let lines = [
            assistant_line(1, &launch, None),
            assistant_line(2, &launch, Some(&format!("{b}/scratch/n.txt"))),
        ]
        .join("\n");
        let root = base.join("logs");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("s2.jsonl"), lines).unwrap();
        let mut source = ClaudeCodeSource::new(root);
        source.repos = RepoResolver::with_prefix(&base);
        let events = source.scan(&SourceCursors::default()).events;
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.project.as_deref() == Some("launcher")));
    }

    #[test]
    fn extract_paths_stops_at_shell_delimiters() {
        let cmd = r#"cd /h/u/proj/a && cat "/h/u/proj/b/f.rs"; ls /h/u"#;
        let got = extract_paths(cmd, &["/h/u".to_string()]);
        assert_eq!(got, vec!["/h/u/proj/a".to_string(), "/h/u/proj/b/f.rs".to_string()]);
        // Inside a longer path (or a slugged dir name) is not a match.
        assert!(extract_paths("/x/h/u/proj", &["/h/u".to_string()]).is_empty());
        assert!(extract_paths("/tmp/-h/u-proj", &["/h/u".to_string()]).is_empty());
    }

    /// `cd ~/proj/x && … /abs/other/node_modules/lib` → x (tilde expanded,
    /// borrowed dependency ignored). The argus/goat-keeper case from 2026-10-13.
    #[test]
    fn tilde_paths_count_and_node_modules_do_not() {
        let base = temp_root("repo-tilde");
        for repo in ["keeper", "argus"] {
            fs::create_dir_all(base.join(repo).join(".git")).unwrap();
        }
        let b = base.to_string_lossy().into_owned();
        let mut r = RepoResolver::with_prefix(&base);
        let cmd = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Bash","input":{{"command":"cd ~/keeper && PLAYWRIGHT_MODULE={b}/argus/web/node_modules/playwright/index.mjs node x.mjs"}}}}]}}}}"#
        );
        assert_eq!(r.repo_of_line(&cmd).as_deref(), Some("keeper"));
        assert_eq!(expand_home("cd ~/a; ls $HOME/b ${HOME}/c x~/d", Some("/H")), "cd /H/a; ls /H/b /H/c x~/d");
    }

    /// The record's own `cwd` (on every line) must not count as a tool path.
    #[test]
    fn cwd_field_on_tool_lines_is_not_a_repo_signal() {
        let base = temp_root("repo-cwd");
        for repo in ["launcher", "alpha"] {
            fs::create_dir_all(base.join(repo).join(".git")).unwrap();
        }
        let b = base.to_string_lossy();
        let launch = format!("{b}/launcher"); // a real repo, but only the launch dir
        let lines = [
            assistant_line(1, &launch, Some(&format!("{b}/alpha/x.rs"))),
            assistant_line(2, &launch, Some("/tmp/not-home/y.rs")),
            assistant_line(3, &launch, None),
        ]
        .join("\n");
        let root = base.join("logs");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("s3.jsonl"), lines).unwrap();
        let mut source = ClaudeCodeSource::new(root);
        source.repos = RepoResolver::with_prefix(&base);
        let events = source.scan(&SourceCursors::default()).events;
        assert!(events.iter().all(|e| e.project.as_deref() == Some("alpha")), "{:?}",
            events.iter().map(|e| e.project.clone()).collect::<Vec<_>>());
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
