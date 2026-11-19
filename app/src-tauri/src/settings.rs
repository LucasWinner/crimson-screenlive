//! User settings persisted as JSON in the OS app-config directory.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// Default signaling server (Cloudflare Worker in `signaling/`).
pub const DEFAULT_SERVER: &str = "https://seu-worker.workers.dev";
/// Pre-deploy default; settings still pointing here are migrated to DEFAULT_SERVER.
const OLD_LOCAL_DEFAULT: &str = "http://127.0.0.1:8787";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub nickname: String,
    pub server_url: String,
    /// Monitor id from `system_info`, or None for the primary one.
    pub monitor_id: Option<u64>,
    pub quality: String,
    pub audio: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            nickname: String::new(),
            server_url: DEFAULT_SERVER.into(),
            monitor_id: None,
            quality: "1080p60".into(),
            audio: true,
        }
    }
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("settings.json"))
}

pub fn load(app: &AppHandle) -> Settings {
    let mut s: Settings = path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if s.server_url.is_empty() || s.server_url == OLD_LOCAL_DEFAULT {
        s.server_url = DEFAULT_SERVER.into();
    }
    s
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Settings {
    load(&app)
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    let p = path(&app).ok_or("no config directory")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(p, json).map_err(|e| e.to_string())
}
