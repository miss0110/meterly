pub mod aggregate;
pub mod cache;
pub mod commands;
pub mod model;
pub mod pricing;
pub mod scheduler;
pub mod sources;

use tauri::{
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};
use tauri_plugin_positioner::{Position, WindowExt};

/// Toggle the popover window: hide when visible, otherwise position it near
/// the tray icon and show it.
fn toggle_popover(app: &AppHandle) {
    let Some(popover) = app.get_webview_window("popover") else {
        return;
    };
    if popover.is_visible().unwrap_or(false) {
        let _ = popover.hide();
    } else {
        let _ = popover.move_window(Position::TrayCenter);
        let _ = popover.show();
        let _ = popover.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_positioner::init())
        .manage(scheduler::AppState(std::sync::Mutex::new(
            scheduler::Engine::new(),
        )))
        .invoke_handler(tauri::generate_handler![
            commands::get_summary,
            commands::get_dashboard,
            commands::refresh_now,
            commands::open_dashboard
        ])
        .setup(|app| {
            // Menu bar app: hide the Dock icon.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Tray icon. The title will show today's total tokens later
            // (T9); "–" is the placeholder until then.
            TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .icon_as_template(true)
                .title("–")
                .on_tray_icon_event(|tray, event| {
                    // Feed tray events to the positioner plugin so
                    // Position::TrayCenter knows where the tray icon is.
                    tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_popover(tray.app_handle());
                    }
                })
                .build(app)?;

            // Background polling (default 3 min) — first refresh runs
            // immediately, so the tray title fills in shortly after launch.
            scheduler::start(app.handle().clone());

            Ok(())
        })
        .on_window_event(|window, event| {
            // Hide the popover when it loses focus.
            if window.label() == "popover" {
                if let tauri::WindowEvent::Focused(false) = event {
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
