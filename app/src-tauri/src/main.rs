// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod room;
mod settings;

use room::AppState;
use tauri::menu::MenuBuilder;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;

/// Installed builds ship GStreamer next to the exe (DLLs) with plugins in
/// `gstreamer/plugins`. Point GStreamer there so it ignores any system install.
/// Must run before any other thread starts (env vars) and before gst::init.
fn use_bundled_gstreamer() {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) else {
        return;
    };
    let plugins = dir.join("gstreamer").join("plugins");
    if !plugins.is_dir() {
        return; // dev build: use the system GStreamer
    }
    std::env::set_var("GST_PLUGIN_SYSTEM_PATH_1_0", &plugins);
    std::env::set_var("GST_PLUGIN_PATH_1_0", "");
    // Scan plugins in-process; we don't ship gst-plugin-scanner.exe.
    std::env::set_var("GST_REGISTRY_FORK", "no");
    let cache = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("CrimsonScreenLive");
    let _ = std::fs::create_dir_all(&cache);
    std::env::set_var("GST_REGISTRY_1_0", cache.join("gst-registry.bin"));
}

fn main() {
    use_bundled_gstreamer();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if let Err(e) = screenlive_core::init() {
        eprintln!("GStreamer failed to initialize: {e}");
        std::process::exit(1);
    }

    fn show_main(app: &tauri::AppHandle) {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.unminimize();
            let _ = win.show();
            let _ = win.set_focus();
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState::default())
        .setup(|app| {
            let menu = MenuBuilder::new(app)
                .text("open", "Abrir Crimson ScreenLive")
                .separator()
                .text("quit", "Sair")
                .build()?;
            let mut tray = TrayIconBuilder::with_id("main")
                .tooltip("Crimson ScreenLive")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_main(app),
                    "quit" => {
                        app.state::<AppState>().shutdown();
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                    | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = event
                    {
                        show_main(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon().cloned() {
                tray = tray.icon(icon);
            }
            tray.build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" && window.app_handle().state::<AppState>().is_media_active() {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            settings::get_settings,
            settings::save_settings,
            room::system_info,
            room::audio_sources,
            room::turn_usage,
            room::create_room,
            room::join_room,
            room::leave_room,
            room::start_stream,
            room::stop_stream,
            room::watch,
            room::stop_watch,
            room::toggle_fullscreen,
            room::set_watch_volume,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Crimson ScreenLive")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                app.state::<AppState>().shutdown();
            }
        });
}
