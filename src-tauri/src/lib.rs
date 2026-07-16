pub mod accounts;
pub mod aggregate;
pub mod cache;
pub mod commands;
pub mod devicesync;
pub mod logging;
pub mod model;
pub mod pricing;
pub mod scheduler;
pub mod sources;

use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager,
};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_positioner::{Position, WindowExt};

/// Latest known available update version (e.g. "0.1.14"), set by the periodic
/// scan. `None` = up to date (or not checked yet). Drives the popover banner.
pub struct UpdateState(pub std::sync::Mutex<Option<String>>);

/// Seconds between background update scans.
const UPDATE_SCAN_INTERVAL_SECS: u64 = 6 * 3600;

/// Quiet background update check: no dialogs. When a newer version exists,
/// remember it (popover banner via `get_update_status` / "update-available"
/// event) and notify once per version (dedup persisted in the cache).
/// `METERLY_FAKE_UPDATE=<ver>` fakes an available update for dev testing.
fn update_scan(handle: &AppHandle) {
    use tauri_plugin_notification::NotificationExt;
    use tauri_plugin_updater::UpdaterExt;

    let found: Option<String> = if let Ok(v) = std::env::var("METERLY_FAKE_UPDATE") {
        Some(v)
    } else {
        let Ok(updater) = handle.updater() else {
            return;
        };
        match tauri::async_runtime::block_on(updater.check()) {
            Ok(Some(update)) => Some(update.version.clone()),
            Ok(None) => None,
            Err(err) => {
                crate::logging::warn(&format!("update scan failed: {err}"));
                return; // keep previous state on transient failure
            }
        }
    };

    {
        let state = handle.state::<UpdateState>();
        *state.0.lock().unwrap_or_else(|e| e.into_inner()) = found.clone();
    }
    let Some(version) = found else {
        return;
    };
    crate::logging::info(&format!("update scan: v{version} available"));
    let _ = handle.emit("update-available", &version);

    // Notify once per version across restarts.
    let already = {
        let state = handle.state::<crate::scheduler::AppState>();
        let mut engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
        if engine.cache.last_notified_update.as_deref() == Some(version.as_str()) {
            true
        } else {
            engine.cache.last_notified_update = Some(version.clone());
            engine.save_cache_best_effort();
            false
        }
    };
    if !already {
        let _ = handle
            .notification()
            .builder()
            .title("meterly 업데이트")
            .body(format!(
                "새 버전 v{version}이(가) 있습니다. 메뉴바 팝오버에서 설치할 수 있어요."
            ))
            .show();
    }
}

/// Warn (once, at startup) when the app runs from a location where the
/// auto-updater cannot replace it — macOS App Translocation (launched from
/// Downloads/DMG without moving) or any read-only bundle. Field failure:
/// "업데이트 설치에 실패했습니다. Read-only file system (os error 30)".
#[cfg(target_os = "macos")]
fn check_app_location(handle: &AppHandle) {
    use tauri_plugin_dialog::DialogExt;

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let translocated = exe.to_string_lossy().contains("/AppTranslocation/");
    // <bundle>.app/Contents/MacOS/<exe> → ancestor(3) is the .app bundle.
    let writable = exe.ancestors().nth(3).map_or(true, |app_dir| {
        let probe = app_dir.join(".meterly-write-probe");
        match std::fs::OpenOptions::new().create(true).write(true).open(&probe) {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
                true
            }
            Err(_) => false,
        }
    });
    if !translocated && writable {
        return;
    }
    crate::logging::warn(&format!(
        "app location not updatable (translocated: {translocated}, writable: {writable}) — {}",
        exe.display()
    ));
    let handle = handle.clone();
    std::thread::spawn(move || {
        handle
            .dialog()
            .message(
                "meterly가 읽기 전용 위치에서 실행 중이라 자동 업데이트가 동작하지 \
                 않습니다.\n\nmeterly를 종료한 뒤 '응용 프로그램' 폴더로 옮기고 \
                 다시 실행해 주세요.",
            )
            .title("meterly 위치 안내")
            .blocking_show();
    });
}

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
                crate::logging::info(&format!(
                    "update available: v{new_version} (current v{current})"
                ));
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
                        crate::logging::info(&format!("update installed: v{new_version}"));
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
                        crate::logging::error(&format!("update install failed: {err}"));
                        let msg = err.to_string();
                        let body = if msg.contains("os error 30")
                            || msg.to_lowercase().contains("read-only")
                        {
                            "업데이트 설치에 실패했습니다.\n\nmeterly가 읽기 전용 \
                             위치에서 실행 중입니다. meterly를 종료한 뒤 '응용 프로그램' \
                             폴더로 옮기고 다시 시도해 주세요."
                                .to_string()
                        } else {
                            format!("업데이트 설치에 실패했습니다.\n{err}")
                        };
                        handle
                            .dialog()
                            .message(body)
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
                crate::logging::warn(&format!("update check failed: {err}"));
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
        .manage(UpdateState(std::sync::Mutex::new(None)))
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
            commands::set_alerts_enabled,
            commands::set_alert_thresholds,
            commands::set_percent_display,
            commands::set_monthly_budget,
            commands::set_date_format,
            commands::pick_sync_folder,
            commands::clear_sync_folder,
            commands::check_for_updates,
            commands::open_settings,
            commands::open_log_dir,
            commands::get_update_status
        ])
        .setup(|app| {
            // Menu bar app: hide the Dock icon.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Local daily logs (last week kept) for field diagnosis.
            crate::logging::prune();
            crate::logging::info(&format!(
                "meterly {} starting ({})",
                app.package_info().version,
                std::env::consts::OS
            ));

            // Warn early when the app can't self-update from where it runs.
            #[cfg(target_os = "macos")]
            check_app_location(&app.handle().clone());

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

            // Tray icon. A dedicated monochrome sparkline glyph (transparent
            // background) used as a macOS template image — the colorful app
            // icon would render as a solid black box in the menu bar. The
            // title shows today's total tokens after the first refresh; "–"
            // is the placeholder until then.
            let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
            TrayIconBuilder::with_id("main-tray")
                .icon(tray_icon)
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

            // Periodic quiet update scan (launch + every 6h): no dialogs —
            // an available update surfaces as a popover banner + one native
            // notification per version. Install runs when the user clicks.
            let scan_handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(10));
                loop {
                    update_scan(&scan_handle);
                    std::thread::sleep(std::time::Duration::from_secs(
                        UPDATE_SCAN_INTERVAL_SECS,
                    ));
                }
            });

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
            // Closing the dashboard/settings must HIDE, not destroy — a
            // destroyed window can never be reopened from the tray.
            if window.label() == "dashboard" || window.label() == "settings" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
