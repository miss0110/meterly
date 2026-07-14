pub mod aggregate;
pub mod cache;
pub mod commands;
pub mod model;
pub mod pricing;
pub mod scheduler;
pub mod sources;

use tauri::{
    menu::{CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder, SubmenuBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
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

/// Check GitHub Releases for a newer version on launch; if found, download and
/// install it in the background, then notify the user to restart. Non-fatal:
/// any failure (offline, no update, bad signature) is logged and ignored.
#[cfg(not(debug_assertions))]
fn spawn_update_check(handle: AppHandle) {
    use tauri_plugin_updater::UpdaterExt;
    tauri::async_runtime::spawn(async move {
        let updater = match handle.updater() {
            Ok(u) => u,
            Err(err) => {
                eprintln!("meterly: updater unavailable: {err}");
                return;
            }
        };
        match updater.check().await {
            Ok(Some(update)) => {
                let version = update.version.clone();
                match update.download_and_install(|_, _| {}, || {}).await {
                    Ok(()) => {
                        use tauri_plugin_notification::NotificationExt;
                        let _ = handle
                            .notification()
                            .builder()
                            .title("meterly 업데이트 준비됨")
                            .body(format!("v{version} 설치됨 — 앱을 재시작하면 적용됩니다."))
                            .show();
                    }
                    Err(err) => eprintln!("meterly: update install failed: {err}"),
                }
            }
            Ok(None) => {}
            Err(err) => eprintln!("meterly: update check failed: {err}"),
        }
    });
}

/// Tray "meterly 정보" (About): show the app name, version and a one-line
/// description in a native dialog — like a standard macOS About panel.
fn show_about(app: &AppHandle) {
    use tauri_plugin_dialog::DialogExt;
    let version = app.package_info().version.to_string();
    let body = format!(
        "meterly v{version}\n\n로컬 AI CLI 사용량 추적 · Claude Code · Codex\ncom.meterly.app"
    );
    app.dialog()
        .message(body)
        .title("meterly 정보")
        .show(|_| {});
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
        .invoke_handler(tauri::generate_handler![
            commands::get_summary,
            commands::get_dashboard,
            commands::refresh_now,
            commands::open_dashboard,
            commands::get_heatmap,
            commands::export_data
        ])
        .setup(|app| {
            // Menu bar app: hide the Dock icon.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Tray context menu (right-click): 대시보드 / 새로고침 /
            // 자동 시작 토글 / 종료. Left-click keeps toggling the popover.
            let autostart_enabled = app.autolaunch().is_enabled().unwrap_or(false);
            let autostart_item =
                CheckMenuItemBuilder::with_id("autostart", "로그인 시 자동 시작")
                    .checked(autostart_enabled)
                    .build(app)?;
            // 트레이 표시 모드 (radio-style check items).
            let saved_display = {
                let state = app.state::<scheduler::AppState>();
                let engine = state.0.lock().unwrap_or_else(|e| e.into_inner());
                engine.cache.tray_display.clone().unwrap_or_default()
            };
            let disp_tokens = CheckMenuItemBuilder::with_id("disp_tokens", "토큰 표시")
                .checked(saved_display != "cost" && saved_display != "icon")
                .build(app)?;
            let disp_cost = CheckMenuItemBuilder::with_id("disp_cost", "비용 표시 (API 환산)")
                .checked(saved_display == "cost")
                .build(app)?;
            let disp_icon = CheckMenuItemBuilder::with_id("disp_icon", "아이콘만")
                .checked(saved_display == "icon")
                .build(app)?;
            let display_menu = SubmenuBuilder::new(app, "트레이 표시")
                .item(&disp_tokens)
                .item(&disp_cost)
                .item(&disp_icon)
                .build()?;
            let menu = MenuBuilder::new(app)
                .item(&MenuItemBuilder::with_id("about", "meterly 정보").build(app)?)
                .separator()
                .item(&MenuItemBuilder::with_id("dashboard", "대시보드 열기").build(app)?)
                .item(&MenuItemBuilder::with_id("refresh", "지금 새로고침").build(app)?)
                .separator()
                .item(&display_menu)
                .item(&autostart_item)
                .separator()
                .item(&MenuItemBuilder::with_id("quit", "meterly 종료").build(app)?)
                .build()?;
            let autostart_check = autostart_item.clone();
            let disp_items = (disp_tokens.clone(), disp_cost.clone(), disp_icon.clone());

            // Tray icon. The title shows today's total tokens after the
            // first refresh; "–" is the placeholder until then.
            TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .icon_as_template(true)
                .title("–")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id().as_ref() {
                    "quit" => app.exit(0),
                    "about" => show_about(app),
                    "dashboard" => {
                        let _ = commands::open_dashboard(app.clone());
                    }
                    "refresh" => {
                        let app = app.clone();
                        std::thread::spawn(move || {
                            let _ = scheduler::refresh_and_publish(&app);
                        });
                    }
                    id @ ("disp_tokens" | "disp_cost" | "disp_icon") => {
                        let mode = match id {
                            "disp_cost" => "cost",
                            "disp_icon" => "icon",
                            _ => "tokens",
                        };
                        let _ = disp_items.0.set_checked(mode == "tokens");
                        let _ = disp_items.1.set_checked(mode == "cost");
                        let _ = disp_items.2.set_checked(mode == "icon");
                        scheduler::set_tray_display(app, mode);
                    }
                    "autostart" => {
                        let launcher = app.autolaunch();
                        let now_enabled = launcher.is_enabled().unwrap_or(false);
                        let result = if now_enabled {
                            launcher.disable()
                        } else {
                            launcher.enable()
                        };
                        if result.is_ok() {
                            let _ = autostart_check.set_checked(!now_enabled);
                        }
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

            // Check for updates on launch (release builds only — dev builds
            // have no signed artifacts to compare against).
            #[cfg(not(debug_assertions))]
            spawn_update_check(app.handle().clone());

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
