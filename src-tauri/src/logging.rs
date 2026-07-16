//! Lightweight local logging for field diagnosis.
//!
//! A bundled app's `eprintln!` goes nowhere the user can retrieve, so we append
//! to a per-day file under [`log_dir`] and keep the last [`RETENTION_DAYS`]
//! days (older files are pruned on startup / each poll cycle). No external log
//! crate: writes are `TIMESTAMP [LEVEL] message` lines behind a global lock.
//! Logging failures are swallowed — diagnostics must never crash the app.
//!
//! (Remote reporting is intentionally out of scope for now — this just makes a
//! local log exist so it can be inspected or, later, uploaded.)

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Local;

/// Days of history to keep (including today).
pub const RETENTION_DAYS: i64 = 7;

/// Serializes appends across the scheduler/watcher threads.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Where daily log files live. `METERLY_LOG_DIR` overrides (tests); otherwise
/// `~/Library/Logs/meterly` on macOS (Console.app-discoverable), else the
/// platform data dir.
pub fn log_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("METERLY_LOG_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        return home.join("Library").join("Logs").join("meterly");
    }
    dirs::data_dir()
        .map(|d| d.join("meterly").join("logs"))
        .unwrap_or_else(|| PathBuf::from(".meterly-logs"))
}

fn today_file() -> PathBuf {
    log_dir().join(format!("meterly-{}.log", Local::now().format("%Y-%m-%d")))
}

#[derive(Clone, Copy)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// Append one line to today's log file. Best-effort.
pub fn write(level: Level, msg: &str) {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let line = format!(
        "{} [{}] {}\n",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        level.tag(),
        msg
    );
    let _ = fs::create_dir_all(log_dir());
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(today_file()) {
        let _ = f.write_all(line.as_bytes());
    }
    #[cfg(debug_assertions)]
    eprint!("{line}");
}

pub fn info(msg: &str) {
    write(Level::Info, msg);
}
pub fn warn(msg: &str) {
    write(Level::Warn, msg);
}
pub fn error(msg: &str) {
    write(Level::Error, msg);
}

/// Delete log files older than the retention window. Filenames not matching
/// `meterly-YYYY-MM-DD.log` are left untouched.
pub fn prune() {
    let cutoff = Local::now().date_naive() - chrono::Duration::days(RETENTION_DAYS - 1);
    let Ok(entries) = fs::read_dir(log_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(date) = name
            .strip_prefix("meterly-")
            .and_then(|s| s.strip_suffix(".log"))
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        {
            if date < cutoff {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_keeps_recent_removes_old() {
        let dir = std::env::temp_dir().join(format!(
            "meterly-log-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("METERLY_LOG_DIR", &dir);

        let today = Local::now().date_naive();
        let recent = today - chrono::Duration::days(2);
        let old = today - chrono::Duration::days(10);
        for d in [recent, old] {
            fs::write(dir.join(format!("meterly-{d}.log")), "x").unwrap();
        }
        // A foreign file must survive pruning.
        fs::write(dir.join("notes.txt"), "keep").unwrap();

        prune();

        assert!(dir.join(format!("meterly-{recent}.log")).exists());
        assert!(!dir.join(format!("meterly-{old}.log")).exists());
        assert!(dir.join("notes.txt").exists());

        std::env::remove_var("METERLY_LOG_DIR");
        let _ = fs::remove_dir_all(&dir);
    }
}
