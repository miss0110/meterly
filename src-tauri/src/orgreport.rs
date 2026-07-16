//! Optional org usage reporting.
//!
//! Off by default — the personal app is unchanged. When an org config exists
//! (managed file deployed by IT, or values entered in Settings), meterly
//! periodically POSTs a usage snapshot to the org's collection server. The
//! stats/search admin is a separate system; this module only implements the
//! agent side of the contract (see ORG_REPORTING.md).
//!
//! Identity = user-entered identifier (e.g. 사번) + hostname. A one-time
//! registration call records that pair server-side before reporting starts.
//! Payload = per-day/source/model token totals only — projects, prompts and
//! account emails are NOT sent.
//!
//! Managed config (`managed.json`) wins over Settings values so IT can deploy
//! the endpoint fleet-wide; the identifier is always personal (Settings).

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::aggregate::DailyBucket;
use crate::model::SourceId;

/// Seconds between usage reports.
pub const REPORT_INTERVAL_SECS: i64 = 3600;
/// Hard cap per HTTP call.
const HTTP_TIMEOUT_SECS: u64 = 15;

/// IT-deployed managed config. System-wide file wins over the user-level one;
/// `METERLY_MANAGED_CONFIG` overrides both (tests).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ManagedConfig {
    pub url: Option<String>,
    pub token: Option<String>,
}

fn managed_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os("METERLY_MANAGED_CONFIG") {
        v.push(PathBuf::from(p));
        return v;
    }
    #[cfg(target_os = "macos")]
    v.push(PathBuf::from(
        "/Library/Application Support/meterly/managed.json",
    ));
    if let Some(dir) = dirs::data_dir() {
        v.push(dir.join("com.meterly.app").join("managed.json"));
    }
    v
}

pub fn managed_config() -> Option<ManagedConfig> {
    for p in managed_paths() {
        if let Ok(content) = std::fs::read_to_string(&p) {
            if let Ok(cfg) = serde_json::from_str::<ManagedConfig>(&content) {
                return Some(cfg);
            }
            crate::logging::warn(&format!("org: managed config unparseable: {}", p.display()));
        }
    }
    None
}

/// Effective org config: managed url/token win over Settings; the identifier
/// always comes from Settings (it's personal).
#[derive(Debug, Clone)]
pub struct OrgConfig {
    pub url: String,
    pub token: Option<String>,
    pub user_id: String,
    /// True when url/token came from a managed file (Settings shows read-only).
    pub managed: bool,
}

pub fn resolve(cache: &crate::cache::CacheV1) -> Option<OrgConfig> {
    let managed = managed_config();
    let (url, token, is_managed) = match &managed {
        Some(m) if m.url.is_some() => (m.url.clone(), m.token.clone(), true),
        _ => (cache.org_url.clone(), cache.org_token.clone(), false),
    };
    let url = url?.trim().trim_end_matches('/').to_string();
    if url.is_empty() {
        return None;
    }
    let user_id = cache.org_user_id.clone()?.trim().to_string();
    if user_id.is_empty() {
        return None;
    }
    Some(OrgConfig {
        url,
        token,
        user_id,
        managed: is_managed,
    })
}

// ---- Payload ----

/// One (date, source, model) row — deliberately WITHOUT project.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageRow {
    pub date: chrono::NaiveDate,
    pub source: SourceId,
    pub model: Option<String>,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsagePayload {
    pub schema: u32,
    pub user_id: String,
    pub hostname: String,
    pub app_version: String,
    pub reported_at: chrono::DateTime<chrono::Utc>,
    pub daily: Vec<UsageRow>,
}

/// Merge project-keyed buckets into (date, source, model) rows.
pub fn merge_rows(buckets: &[&DailyBucket]) -> Vec<UsageRow> {
    let mut rows: Vec<UsageRow> = Vec::new();
    for b in buckets {
        match rows
            .iter_mut()
            .find(|r| r.date == b.date && r.source == b.source && r.model == b.model)
        {
            Some(r) => {
                r.input += b.input;
                r.output += b.output;
                r.cache_read += b.cache_read;
                r.cache_creation += b.cache_creation;
            }
            None => rows.push(UsageRow {
                date: b.date,
                source: b.source,
                model: b.model.clone(),
                input: b.input,
                output: b.output,
                cache_read: b.cache_read,
                cache_creation: b.cache_creation,
            }),
        }
    }
    rows.sort_by(|a, b| {
        (a.date, a.source.as_str(), &a.model).cmp(&(b.date, b.source.as_str(), &b.model))
    });
    rows
}

// ---- HTTP ----

fn client() -> Option<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .ok()
}

fn post_inner(cfg: &OrgConfig, path: &str, body: &serde_json::Value) -> Result<u16, String> {
    let client = client().ok_or("http client init failed")?;
    let mut req = client
        .post(format!("{}{path}", cfg.url))
        .json(body)
        .header("User-Agent", format!("meterly/{}", env!("CARGO_PKG_VERSION")));
    if let Some(t) = &cfg.token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let resp = req.send().map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    if resp.status().is_success() {
        Ok(status)
    } else {
        let body = resp.text().unwrap_or_default();
        Err(format!("HTTP {status} {}", body.chars().take(200).collect::<String>()))
    }
}

/// Run the blocking HTTP POST on a dedicated plain thread and log each phase.
/// reqwest's blocking client panics when entered from an async-runtime thread
/// (tauri async commands) — the isolation makes callers context-independent,
/// and `join` converts a panic into an error instead of a hung command.
fn post(cfg: &OrgConfig, path: &str, body: &serde_json::Value) -> Result<u16, String> {
    crate::logging::info(&format!(
        "org: POST {}{path} (user {}, body {}B)…",
        cfg.url,
        cfg.user_id,
        body.to_string().len()
    ));
    let cfg2 = cfg.clone();
    let path2 = path.to_string();
    let body2 = body.clone();
    let outcome = std::thread::spawn(move || post_inner(&cfg2, &path2, &body2)).join();
    match outcome {
        Ok(Ok(status)) => {
            crate::logging::info(&format!("org: POST {path} → HTTP {status}"));
            Ok(status)
        }
        Ok(Err(e)) => {
            crate::logging::warn(&format!("org: POST {path} failed: {e}"));
            Err(e)
        }
        Err(_) => {
            crate::logging::error(&format!("org: POST {path} panicked (internal bug)"));
            Err("내부 오류 — 로그 폴더의 org 항목을 확인하세요".into())
        }
    }
}

/// One-time registration: records (user_id, hostname) server-side. Any 2xx =
/// registered. The server dedups/flags identifier reuse — hostname is sent so
/// the same identifier on two machines is distinguishable.
pub fn register(cfg: &OrgConfig, hostname: &str) -> Result<(), String> {
    let body = serde_json::json!({
        "schema": 1,
        "user_id": cfg.user_id,
        "hostname": hostname,
        "app_version": env!("CARGO_PKG_VERSION"),
    });
    post(cfg, "/register", &body).map(|_| ())
}

/// Send a usage snapshot. Snapshot-style upsert: the whole retention window
/// every time, so the server heals from missed reports.
pub fn report(cfg: &OrgConfig, payload: &UsagePayload) -> Result<(), String> {
    let body = serde_json::to_value(payload).map_err(|e| e.to_string())?;
    post(cfg, "/usage", &body).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn bucket(date: (i32, u32, u32), source: SourceId, model: &str, project: &str, input: u64) -> DailyBucket {
        DailyBucket {
            date: NaiveDate::from_ymd_opt(date.0, date.1, date.2).unwrap(),
            source,
            model: Some(model.into()),
            project: Some(project.into()),
            input,
            output: 1,
            cache_read: 2,
            cache_creation: 3,
        }
    }

    #[test]
    fn merge_rows_collapses_projects_keeps_date_source_model() {
        let owned = vec![
            bucket((2026, 7, 15), SourceId::ClaudeCode, "claude-sonnet-5", "meterly", 10),
            bucket((2026, 7, 15), SourceId::ClaudeCode, "claude-sonnet-5", "goaterm", 20),
            bucket((2026, 7, 15), SourceId::Codex, "gpt-5.5", "meterly", 5),
            bucket((2026, 7, 16), SourceId::ClaudeCode, "claude-sonnet-5", "meterly", 1),
        ];
        let refs: Vec<&DailyBucket> = owned.iter().collect();
        let rows = merge_rows(&refs);
        assert_eq!(rows.len(), 3); // two projects merged into one row
        let merged = &rows[0];
        assert_eq!(merged.input, 30);
        assert_eq!(merged.cache_read, 4);
        // Serialization must not leak a project field.
        let json = serde_json::to_string(&rows).unwrap();
        assert!(!json.contains("project"), "{json}");
    }

    #[test]
    fn register_round_trip_against_local_server() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            // Read until the JSON body has arrived (ends with '}').
            loop {
                match s.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        req.extend_from_slice(&buf[..n]);
                        if req.ends_with(b"}") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            s.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\n\r\n{\"status\":\"ok\"}",
            )
            .unwrap();
            String::from_utf8_lossy(&req).to_string()
        });

        let cfg = OrgConfig {
            url: format!("http://{addr}"),
            token: Some("tkn".into()),
            user_id: "E1".into(),
            managed: false,
        };
        register(&cfg, "test-host").expect("register should succeed");
        let req = server.join().unwrap();
        assert!(req.contains("POST /register"), "{req}");
        assert!(req.contains("Bearer tkn"), "{req}");
        assert!(req.contains("test-host"), "{req}");
    }

    #[test]
    fn resolve_requires_url_and_user_id() {
        let mut cache = crate::cache::CacheV1::default();
        assert!(resolve(&cache).is_none());
        cache.org_url = Some("https://collect.example.com/".into());
        assert!(resolve(&cache).is_none()); // identifier still missing
        cache.org_user_id = Some("E12345".into());
        let cfg = resolve(&cache).expect("config");
        assert_eq!(cfg.url, "https://collect.example.com"); // trailing slash trimmed
        assert_eq!(cfg.user_id, "E12345");
        assert!(!cfg.managed);
    }
}
