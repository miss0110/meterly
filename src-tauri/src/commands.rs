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
pub fn get_dashboard(state: State<'_, AppState>, range: String) -> DashboardData {
    let engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
    engine.dashboard(&range)
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
    }
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
