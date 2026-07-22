//! Tauri IPC commands (T8) — names/payloads fixed by the plan's contract.

use tauri::{AppHandle, Manager, State};

use crate::scheduler::{
    refresh_and_publish, AppState, DashboardData, DevicesData, HeatmapCell, Summary,
};

#[tauri::command]
pub fn get_summary(state: State<'_, AppState>) -> Summary {
    let engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
    engine.summary()
}

/// Per-device today usage for the combined ("전체 N대") view. Reads the sync
/// folder; returns only the current device when sync is off.
#[tauri::command]
pub fn get_devices(state: State<'_, AppState>) -> DevicesData {
    let engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
    engine.get_devices()
}

#[tauri::command]
pub fn get_dashboard(
    state: State<'_, AppState>,
    range: String,
    scope: Option<String>,
) -> DashboardData {
    let engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
    engine.dashboard(&range, scope.as_deref().unwrap_or("local"))
}

#[tauri::command]
pub fn get_heatmap(state: State<'_, AppState>) -> Vec<HeatmapCell> {
    let engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
    engine.heatmap()
}

/// Export aggregate rows for a range to ~/Downloads (csv|json). Returns the
/// written file path.
#[tauri::command]
pub fn export_data(
    state: State<'_, AppState>,
    range: String,
    format: String,
) -> Result<String, String> {
    let engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
    engine.export(&range, &format)
}

/// Manual refresh (popover button). Blocking file IO — run off the main
/// thread via tauri's async command executor.
#[tauri::command(async)]
pub fn refresh_now(app: AppHandle) -> Option<Summary> {
    refresh_and_publish(&app)
}

/// Show the dashboard window (popover "대시보드 열기").
#[tauri::command]
pub fn open_dashboard(app: AppHandle) -> Result<(), String> {
    let Some(dashboard) = app.get_webview_window("dashboard") else {
        return Err("dashboard window not found".into());
    };
    dashboard.show().map_err(|e| e.to_string())?;
    dashboard.set_focus().map_err(|e| e.to_string())?;
    Ok(())
}

// ---- Settings window ----

#[derive(serde::Serialize)]
pub struct SettingsData {
    version: String,
    tray_display: String,
    autostart: bool,
    sync_dir: Option<String>,
    alerts_enabled: bool,
    alert_thresholds: Vec<u8>,
    monthly_budget_tokens: Option<u64>,
    date_format: String,
    percent_display: String,
}

/// Current values for the settings window.
#[tauri::command]
pub fn get_settings(app: AppHandle, state: State<'_, AppState>) -> SettingsData {
    use tauri_plugin_autostart::ManagerExt;
    let engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
    SettingsData {
        version: app.package_info().version.to_string(),
        tray_display: engine.cache.tray_display.clone().unwrap_or_default(),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        sync_dir: engine.cache.sync_dir.clone(),
        alerts_enabled: engine.cache.alerts_enabled.unwrap_or(true),
        alert_thresholds: engine
            .cache
            .alert_thresholds
            .as_deref()
            .map(crate::scheduler::normalize_thresholds)
            .unwrap_or_else(|| crate::scheduler::DEFAULT_LIMIT_THRESHOLDS.to_vec()),
        monthly_budget_tokens: engine.cache.monthly_budget_tokens,
        date_format: engine
            .cache
            .date_format
            .clone()
            .unwrap_or_else(|| "auto".to_string()),
        percent_display: engine
            .cache
            .percent_display
            .clone()
            .unwrap_or_else(|| "used".to_string()),
    }
}

/// Set custom alert thresholds (empty array = reset to defaults).
#[tauri::command]
pub fn set_alert_thresholds(app: AppHandle, thresholds: Vec<u8>) {
    crate::scheduler::set_alert_thresholds(&app, thresholds);
}

/// Limit-gauge display: "used" (사용한 양) | "remaining" (남은 양).
#[tauri::command]
pub fn set_percent_display(app: AppHandle, mode: String) {
    crate::scheduler::set_percent_display(&app, mode);
}

/// Toggle plan-usage threshold notifications (30/50/70/90%).
#[tauri::command]
pub fn set_alerts_enabled(app: AppHandle, enabled: bool) {
    crate::scheduler::set_alerts_enabled(&app, enabled);
}

/// Set (or clear with 0) the monthly token budget.
#[tauri::command]
pub fn set_monthly_budget(app: AppHandle, tokens: u64) {
    crate::scheduler::set_monthly_budget(&app, (tokens > 0).then_some(tokens));
}

/// Set the date-format preference ("auto" | "iso" | "us" | "eu").
#[tauri::command]
pub fn set_date_format(app: AppHandle, format: String) {
    crate::scheduler::set_date_format(&app, format);
}

#[tauri::command]
pub fn set_tray_display(app: AppHandle, mode: String) {
    crate::scheduler::set_tray_display(&app, &mode);
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) {
    use tauri_plugin_autostart::ManagerExt;
    let mgr = app.autolaunch();
    let _ = if enabled { mgr.enable() } else { mgr.disable() };
}

/// Native folder picker → persist as the sync folder. Returns the chosen path.
/// `async` so it runs OFF the main thread — `blocking_pick_folder` blocks its
/// caller and must not sit on the main thread (that deadlocks the panel).
#[tauri::command(async)]
pub fn pick_sync_folder(app: AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let path = app
        .dialog()
        .file()
        .blocking_pick_folder()
        .and_then(|f| f.into_path().ok())?;
    let s = path.to_string_lossy().to_string();
    crate::scheduler::set_sync_dir(&app, Some(s.clone()));
    Some(s)
}

#[tauri::command]
pub fn clear_sync_folder(app: AppHandle) {
    crate::scheduler::set_sync_dir(&app, None);
}

#[tauri::command]
pub fn check_for_updates(app: AppHandle) {
    crate::check_updates(app, true);
}

/// Version of an available update found by the background scan, if any —
/// drives the popover's update banner.
#[tauri::command]
pub fn get_update_status(state: State<'_, crate::UpdateState>) -> Option<String> {
    state.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

// ---- Org reporting ----

#[derive(serde::Serialize)]
pub struct OrgStatus {
    /// Effective endpoint (managed file wins). `None` = feature off.
    url: Option<String>,
    /// True when url/token come from an IT-managed file (read-only in UI).
    managed: bool,
    user_id: Option<String>,
    registered: bool,
    last_report: Option<chrono::DateTime<chrono::Utc>>,
    /// Reporting cadence (fixed) — shown in the Settings status panel.
    interval_secs: i64,
    /// This device's hostname (sent alongside the identifier).
    hostname: String,
    /// Sources included in reports (resolved; default = all known sources).
    sources: Vec<String>,
    /// Last actionable server rejection (e.g. unknown identifier), if any —
    /// shown so the user can correct their input. `None` when all is well.
    last_error: Option<String>,
}

#[tauri::command]
pub fn get_org_status(state: State<'_, AppState>) -> OrgStatus {
    let engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
    let managed = crate::orgreport::managed_config();
    let (url, is_managed) = match &managed {
        Some(m) if m.url.is_some() => (m.url.clone(), true),
        _ => (engine.cache.org_url.clone(), false),
    };
    OrgStatus {
        url,
        managed: is_managed,
        user_id: engine.cache.org_user_id.clone(),
        registered: engine.cache.org_registered,
        last_report: engine.cache.last_org_report,
        interval_secs: crate::orgreport::REPORT_INTERVAL_SECS,
        hostname: crate::scheduler::hostname(),
        sources: engine
            .cache
            .org_sources
            .clone()
            .unwrap_or_else(|| vec!["claude_code".into(), "codex".into()]),
        last_error: engine.cache.org_last_error.clone(),
    }
}

/// Choose which sources are included in org reports. Unknown ids are dropped;
/// an empty (or all-unknown) list resets to "all". Takes effect on the next
/// report — no re-registration needed.
#[tauri::command]
pub fn set_org_sources(state: State<'_, AppState>, sources: Vec<String>) {
    let mut engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
    let known = ["claude_code", "codex"];
    let mut cleaned: Vec<String> = sources
        .into_iter()
        .filter(|s| known.contains(&s.as_str()))
        .collect();
    cleaned.dedup();
    engine.cache.org_sources = if cleaned.is_empty() { None } else { Some(cleaned) };
    engine.save_cache_best_effort();
}

/// Send a usage report immediately (Settings "지금 전송" — connectivity check).
/// Returns the number of rows sent. `async` — network off the main thread.
#[tauri::command(async)]
pub fn org_report_now(app: AppHandle) -> Result<usize, String> {
    crate::scheduler::send_org_report(&app)
}

/// Save org settings (url/token ignored while a managed file exists). Any
/// change resets the registered flag — the identity must re-register.
#[tauri::command]
pub fn set_org_config(
    state: State<'_, AppState>,
    url: Option<String>,
    token: Option<String>,
    user_id: Option<String>,
) {
    let mut engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    engine.cache.org_url = clean(url);
    engine.cache.org_token = clean(token);
    engine.cache.org_user_id = clean(user_id);
    engine.cache.org_registered = false;
    engine.cache.org_last_error = None; // fresh identifier → clear stale notice
    engine.save_cache_best_effort();
}

/// One-time registration: POST /register with (identifier, hostname). On 2xx
/// the device starts reporting. `async` — network call off the main thread.
#[tauri::command(async)]
pub fn org_register(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let cfg = {
        let engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
        crate::orgreport::resolve(&engine.cache)
            .ok_or("서버 주소와 식별자를 먼저 입력하세요")?
    };
    let host = crate::scheduler::hostname();
    if let Err(e) = crate::orgreport::register(&cfg, &host) {
        crate::logging::warn(&format!("org register failed: {e}"));
        // Persist an actionable rejection (e.g. an unknown identifier) so the
        // status panel keeps showing what to fix, not just the toast.
        let msg = e.message();
        let mut engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
        engine.cache.org_registered = false;
        engine.cache.org_last_error = if e.is_unknown_user() {
            Some(msg.clone())
        } else {
            None
        };
        engine.save_cache_best_effort();
        return Err(msg);
    }
    crate::logging::info(&format!(
        "org registered: {} @ {} → {}",
        cfg.user_id, host, cfg.url
    ));
    let mut engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
    engine.cache.org_registered = true;
    engine.cache.org_last_error = None;
    engine.save_cache_best_effort();
    Ok(())
}

/// Turn org reporting off and clear the stored settings.
#[tauri::command]
pub fn org_disable(state: State<'_, AppState>) {
    let mut engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
    engine.cache.org_url = None;
    engine.cache.org_token = None;
    engine.cache.org_user_id = None;
    engine.cache.org_registered = false;
    engine.cache.org_sources = None;
    engine.cache.last_org_report = None;
    engine.cache.org_last_error = None;
    engine.save_cache_best_effort();
}

/// Show the settings window (tray "설정" / Cmd+,).
#[tauri::command]
pub fn open_settings(app: AppHandle) -> Result<(), String> {
    let Some(w) = app.get_webview_window("settings") else {
        return Err("settings window not found".into());
    };
    w.show().map_err(|e| e.to_string())?;
    w.set_focus().map_err(|e| e.to_string())?;
    Ok(())
}

/// Reveal the local log folder in Finder/Explorer so a user can grab the logs.
#[tauri::command]
pub fn open_log_dir() -> Result<(), String> {
    let dir = crate::logging::log_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(target_os = "windows")]
    let opener = "explorer";
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let opener = "xdg-open";
    std::process::Command::new(opener)
        .arg(&dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}
