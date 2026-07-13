//! Tauri IPC commands (T8) — names/payloads fixed by the plan's contract.

use tauri::{AppHandle, Manager, State};

use crate::scheduler::{refresh_and_publish, AppState, DashboardData, HeatmapCell, Summary};

#[tauri::command]
pub fn get_summary(state: State<'_, AppState>) -> Summary {
    let engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
    engine.summary()
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
