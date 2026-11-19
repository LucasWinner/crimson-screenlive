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
    /// Legacy on/off switch (v0.1/0.2); `false` migrates to audio_mode "none".
    pub audio: bool,
    /// "all" | "all-except" | "app" | "none"
    pub audio_mode: String,
    /// Exe left out in "all-except" mode (default: Discord, i.e. friends' voices).
    pub audio_exclude: String,
    /// Exe captured alone in "app" mode.
    pub audio_app: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            nickname: String::new(),
            server_url: DEFAULT_SERVER.into(),
            monitor_id: None,
            quality: "1080p60".into(),
            audio: true,
            audio_mode: "all-except".into(),
            audio_exclude: "Discord.exe".into(),
            audio_app: String::new(),
        }
    }
}

impl Settings {
    pub fn audio_choice(&self) -> screenlive_core::procs::AudioChoice {
        use screenlive_core::procs::AudioChoice;
        match self.audio_mode.as_str() {
            "none" => AudioChoice::None,
            "all" => AudioChoice::All,
            "app" if !self.audio_app.is_empty() => AudioChoice::App(self.audio_app.clone()),
            _ if self.audio_exclude.is_empty() => AudioChoice::AllButDiscord,
            _ => AudioChoice::AllExcept(self.audio_exclude.clone()),
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
    if !s.audio {
        s.audio_mode = "none".into();
        s.audio = true;
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
