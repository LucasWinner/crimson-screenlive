// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod player_win;
mod room;
mod settings;

use std::sync::Mutex;

use player_win::PlayerSurface;
use room::AppState;
use tauri::menu::MenuBuilder;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

#[derive(Default)]
struct ShortcutIds {
    mute: Option<u32>,
    stop: Option<u32>,
}

pub fn apply_shortcuts(app: &tauri::AppHandle, s: &settings::Settings) -> Result<(), String> {
    let parse = |raw: &str, label: &str| -> Result<Option<Shortcut>, String> {
        let t = raw.trim();
        if t.is_empty() {
            return Ok(None);
        }
        t.parse::<Shortcut>()
            .map(Some)
            .map_err(|e| format!("atalho inválido para {label} ('{t}'): {e}"))
    };
    let mute = parse(&s.shortcut_mute, "silenciar")?;
    let stop = parse(&s.shortcut_stop, "parar transmissão")?;
    if let (Some(a), Some(b)) = (&mute, &stop) {
        if a.id() == b.id() {
            return Err("os dois atalhos não podem ser iguais".into());
        }
    }

    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let state = app.state::<Mutex<ShortcutIds>>();
    let mut ids = state.lock().unwrap();
    ids.mute = None;
    ids.stop = None;

    if let Some(sc) = mute {
        let id = sc.id();
        gs.register(sc).map_err(|e| format!("não foi possível registrar atalho de mudo: {e}"))?;
        ids.mute = Some(id);
    }
    if let Some(sc) = stop {
        let id = sc.id();
        gs.register(sc).map_err(|e| format!("não foi possível registrar atalho de parar transmissão: {e}"))?;
        ids.stop = Some(id);
    }
    Ok(())
}

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
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let state = app.state::<Mutex<ShortcutIds>>();
                    let ids = state.lock().unwrap();
                    let (mute_id, stop_id) = (ids.mute, ids.stop);
                    drop(ids);
                    if Some(shortcut.id()) == mute_id {
                        app.state::<AppState>().toggle_watch_mute(app);
                    } else if Some(shortcut.id()) == stop_id {
                        app.state::<AppState>().stop_active_stream(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState::default())
        .manage(Mutex::new(ShortcutIds::default()))
        .manage(Mutex::new(None::<PlayerSurface>))
        .setup(|app| {
            if let Some(player_win) = app.get_webview_window("player") {
                let surf = PlayerSurface::create(&player_win);
                *app.state::<Mutex<Option<PlayerSurface>>>().lock().unwrap() = surf;
            }
            let saved = settings::load(app.handle());
            if let Err(e) = apply_shortcuts(app.handle(), &saved) {
                log::warn!("could not register global shortcuts: {e}");
            }
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
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                if window.label() == "main" && window.app_handle().state::<AppState>().is_media_active() {
                    api.prevent_close();
                    let _ = window.hide();
                } else if window.label() == "player" {
                    api.prevent_close();
                    let _ = window.set_fullscreen(false);
                    let _ = window.hide();
                    window.app_handle().state::<AppState>().stop_active_watch(window.app_handle());
                }
            }
            tauri::WindowEvent::Resized(size) if window.label() == "player" => {
                let scale = window.scale_factor().unwrap_or(1.0);
                let surf_state = window.app_handle().state::<Mutex<Option<PlayerSurface>>>();
                let guard = surf_state.lock().unwrap();
                if let Some(surf) = guard.as_ref() {
                    surf.resize(size.width, size.height, scale);
                }
            }
            tauri::WindowEvent::ScaleFactorChanged { scale_factor, .. } if window.label() == "player" => {
                if let Ok(size) = window.inner_size() {
                    let surf_state = window.app_handle().state::<Mutex<Option<PlayerSurface>>>();
                    let guard = surf_state.lock().unwrap();
                    if let Some(surf) = guard.as_ref() {
                        surf.resize(size.width, size.height, *scale_factor);
                    }
                }
            }
            _ => {}
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
            room::get_watch_state,
            room::get_watch_volume,
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
