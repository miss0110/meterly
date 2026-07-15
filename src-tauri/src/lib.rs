pub mod aggregate;
pub mod cache;
pub mod commands;
pub mod devicesync;
pub mod model;
pub mod pricing;
pub mod scheduler;
pub mod sources;

use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};
use tauri_plugin_autostart::MacosLauncher;
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

/// Check for updates and drive the user through it with dialogs. `manual` =
/// triggered from the tray menu (show "up to date"/error results); when false
/// (launch check) those quiet outcomes are silent — only an available update
/// prompts. Runs on its own thread so the blocking dialogs and the async
/// check/install don't touch the UI thread or the async executor.
pub(crate) fn check_updates(handle: AppHandle, manual: bool) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
    use tauri_plugin_updater::UpdaterExt;

    std::thread::spawn(move || {
        let current = handle.package_info().version.to_string();
        let updater = match handle.updater() {
            Ok(u) => u,
            Err(err) => {
                if manual {
                    handle
                        .dialog()
                        .message(format!("업데이트를 확인할 수 없습니다.\n{err}"))
                        .title("meterly 업데이트")
                        .blocking_show();
                }
                return;
            }
        };

        match tauri::async_runtime::block_on(updater.check()) {
            Ok(Some(update)) => {
                let new_version = update.version.clone();
                let install = handle
                    .dialog()
                    .message(format!(
                        "새 버전 v{new_version} 이(가) 있습니다. (현재 v{current})\n지금 설치할까요?"
                    ))
                    .title("meterly 업데이트")
                    .buttons(MessageDialogButtons::OkCancelCustom(
                        "지금 설치".into(),
                        "나중에".into(),
                    ))
                    .blocking_show();
                if !install {
                    return;
                }
                match tauri::async_runtime::block_on(
                    update.download_and_install(|_, _| {}, || {}),
                ) {
                    Ok(()) => {
                        let restart = handle
                            .dialog()
                            .message(format!(
                                "v{new_version} 설치 완료. 지금 재시작하여 적용할까요?"
                            ))
                            .title("meterly 업데이트")
                            .buttons(MessageDialogButtons::OkCancelCustom(
                                "재시작".into(),
                                "나중에".into(),
                            ))
                            .blocking_show();
                        if restart {
                            handle.restart();
                        }
                    }
                    Err(err) => {
                        handle
                            .dialog()
                            .message(format!("업데이트 설치에 실패했습니다.\n{err}"))
                            .title("meterly 업데이트")
                            .blocking_show();
                    }
                }
            }
            Ok(None) => {
                if manual {
                    handle
                        .dialog()
                        .message(format!("이미 최신 버전입니다. (v{current})"))
                        .title("meterly 업데이트")
                        .blocking_show();
                }
            }
            Err(err) => {
                if manual {
                    handle
                        .dialog()
                        .message(format!("업데이트 확인에 실패했습니다.\n{err}"))
                        .title("meterly 업데이트")
                        .blocking_show();
                }
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .manage(scheduler::AppState(std::sync::Mutex::new(
            scheduler::Engine::new(),
        )))
        .manage(scheduler::TrayRotation(std::sync::Mutex::new(
            Default::default(),
        )))
        .invoke_handler(tauri::generate_handler![
            commands::get_summary,
            commands::get_dashboard,
            commands::refresh_now,
            commands::open_dashboard,
            commands::get_heatmap,
            commands::export_data,
            commands::get_devices,
            commands::get_settings,
            commands::set_tray_display,
            commands::set_autostart,
            commands::pick_sync_folder,
            commands::clear_sync_folder,
            commands::check_for_updates,
            commands::open_settings
        ])
        .setup(|app| {
            // Menu bar app: hide the Dock icon.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Tray menu: 설정 / 대시보드 / 새로고침 / 종료. Detailed controls
            // (display mode, autostart, sync folder, updates) live in the
            // Settings window. Left-click keeps toggling the popover.
            let menu = MenuBuilder::new(app)
                .item(&MenuItemBuilder::with_id("settings", "설정…").build(app)?)
                .item(&MenuItemBuilder::with_id("dashboard", "대시보드 열기").build(app)?)
                .item(&MenuItemBuilder::with_id("refresh", "지금 새로고침").build(app)?)
                .separator()
                .item(&MenuItemBuilder::with_id("quit", "meterly 종료").build(app)?)
                .build()?;

            // Tray icon. The title shows today's total tokens after the
            // first refresh; "–" is the placeholder until then.
            TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .icon_as_template(true)
                .title("–")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "quit" => app.exit(0),
                    "settings" => {
                        let _ = commands::open_settings(app.clone());
                    }
                    "dashboard" => {
                        let _ = commands::open_dashboard(app.clone());
                    }
                    "refresh" => {
                        let app = app.clone();
                        std::thread::spawn(move || {
                            let _ = scheduler::refresh_and_publish(&app);
                        });
                    }
                    _ => {}
                })
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

            // Quiet update check on launch (release only) — prompts the user
            // only when a newer version exists; silent otherwise.
            #[cfg(not(debug_assertions))]
            check_updates(app.handle().clone(), false);

            // Debug/screenshot helper: METERLY_SHOW=dashboard,popover shows
            // the named windows on launch (normally tray-only).
            if let Ok(show) = std::env::var("METERLY_SHOW") {
                for label in show.split(',') {
                    if let Some(w) = app.get_webview_window(label.trim()) {
                        let _ = w.show();
                    }
                }
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Hide the popover when it loses focus.
            if window.label() == "popover" {
                if let tauri::WindowEvent::Focused(false) = event {
                    let _ = window.hide();
                }
            }
            // Closing the dashboard must HIDE it, not destroy it —
            // a destroyed window can never be reopened from the tray.
            if window.label() == "dashboard" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
