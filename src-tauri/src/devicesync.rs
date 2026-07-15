//! Multi-device usage aggregation over a shared cloud folder.
//!
//! Each device writes its own `usage-<device_id>.json` (raw daily token
//! buckets) into a folder that the user's cloud client (iCloud / Google Drive
//! / Dropbox / OneDrive) syncs across machines. meterly never touches cloud
//! credentials — the cloud handles sync and auth; meterly only reads/writes
//! plain JSON. Cost is not stored (recomputed from tokens with the local
//! pricing table), so files stay minimal and pricing stays consistent.
//!
//! Only token/cost data is aggregated. Rate-limit percentages are already
//! account-global (same on every device) and are never summed here.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::aggregate::DailyBucket;

const PREFIX: &str = "usage-";
const SUFFIX: &str = ".json";

/// One device's contribution: its daily token buckets plus identity/freshness.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceFile {
    pub device_id: String,
    pub hostname: String,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub daily: Vec<DailyBucket>,
}

fn sanitize(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

/// Write this device's file atomically (temp + rename) into `sync_dir`.
pub fn write(sync_dir: &Path, file: &DeviceFile) -> std::io::Result<()> {
    fs::create_dir_all(sync_dir)?;
    let path = sync_dir.join(format!("{PREFIX}{}{SUFFIX}", sanitize(&file.device_id)));
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Read every `usage-*.json` in `sync_dir`; unreadable/unparseable files are
/// skipped (a partially-synced or foreign file must not break the merge).
pub fn read_all(sync_dir: &Path) -> Vec<DeviceFile> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(sync_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with(PREFIX) && name.ends_with(SUFFIX)) {
            continue;
        }
        if let Ok(content) = fs::read_to_string(entry.path()) {
            if let Ok(df) = serde_json::from_str::<DeviceFile>(&content) {
                out.push(df);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SourceId;

    fn bucket(input: u64) -> DailyBucket {
        DailyBucket {
            date: chrono::NaiveDate::from_ymd_opt(2026, 7, 14).unwrap(),
            source: SourceId::ClaudeCode,
            model: Some("claude-sonnet-5".into()),
            project: None,
            input,
            output: 0,
            cache_read: 0,
            cache_creation: 0,
        }
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("meterly-devicesync-{tag}"));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn sample(id: &str, host: &str, input: u64) -> DeviceFile {
        DeviceFile {
            device_id: id.into(),
            hostname: host.into(),
            updated_at: chrono::Utc.timestamp_opt(1_784_000_000, 0).unwrap(),
            daily: vec![bucket(input)],
        }
    }

    use chrono::TimeZone;

    #[test]
    fn write_then_read_all_roundtrips_multiple_devices() {
        let dir = tmp_dir("roundtrip");
        write(&dir, &sample("dev-a", "Mac-A", 100)).unwrap();
        write(&dir, &sample("dev-b", "Win-C", 200)).unwrap();

        let mut all = read_all(&dir);
        all.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].device_id, "dev-a");
        assert_eq!(all[0].daily[0].input, 100);
        assert_eq!(all[1].hostname, "Win-C");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewrite_same_device_overwrites_not_duplicates() {
        let dir = tmp_dir("overwrite");
        write(&dir, &sample("dev-a", "Mac-A", 100)).unwrap();
        write(&dir, &sample("dev-a", "Mac-A", 999)).unwrap();
        let all = read_all(&dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].daily[0].input, 999);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_unrelated_and_bad_files() {
        let dir = tmp_dir("ignore");
        fs::create_dir_all(&dir).unwrap();
        write(&dir, &sample("dev-a", "Mac-A", 100)).unwrap();
        fs::write(dir.join("notes.txt"), "hello").unwrap();
        fs::write(dir.join("usage-bad.json"), "{ not json").unwrap();
        let all = read_all(&dir);
        assert_eq!(all.len(), 1, "only the valid usage-*.json counts");
        let _ = fs::remove_dir_all(&dir);
    }
}
