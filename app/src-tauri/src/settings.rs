//! User settings persisted as JSON in the OS app-config directory.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// Default signaling server (Cloudflare Worker in `signaling/`).
pub const DEFAULT_SERVER: &str = "https://seu-worker.workers.dev";

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
    /// "auto" | "av1" | "h264"
    pub codec: String,
    /// Manual bitrate override in kbps (None = automatic).
    pub bitrate_kbps: Option<u32>,
    /// Preferred viewer playback volume (0.0..=2.0, default 1.0).
    pub watch_volume: f64,
    pub watch_muted: bool,
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
            codec: "auto".into(),
            bitrate_kbps: None,
            watch_volume: 1.0,
            watch_muted: false,
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

    pub fn codec_choice(&self) -> screenlive_core::encoder::Codec {
        use screenlive_core::encoder::Codec;
        match self.codec.as_str() {
            "av1" => Codec::Av1,
            "h264" => Codec::H264,
            _ => Codec::Auto,
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
    if s.server_url.trim().is_empty() {
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
pub fn save_settings(app: AppHandle, mut settings: Settings) -> Result<(), String> {
    settings.nickname = settings.nickname.trim().to_string();
    settings.server_url = settings.server_url.trim().trim_end_matches('/').to_string();
    if settings.server_url.is_empty() {
        settings.server_url = DEFAULT_SERVER.into();
    } else {
        screenlive_core::signaling::Server::new(&settings.server_url).map_err(|e| e.to_string())?;
    }
    if !matches!(settings.codec.as_str(), "auto" | "av1" | "h264") {
        settings.codec = "auto".into();
    }
    settings.bitrate_kbps = settings.bitrate_kbps.map(|b| b.clamp(1000, 30_000));
    settings.watch_volume = if settings.watch_volume.is_finite() {
        settings.watch_volume.clamp(0.0, 2.0)
    } else {
        1.0
    };
    let p = path(&app).ok_or("no config directory")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(p, json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use screenlive_core::procs::AudioChoice;

    #[test]
    fn audio_choice_mapping() {
        let mut s = Settings::default();
        assert_eq!(s.audio_choice(), AudioChoice::AllExcept("Discord.exe".into()));

        s.audio_exclude.clear();
        assert_eq!(s.audio_choice(), AudioChoice::AllButDiscord);

        s.audio_mode = "app".into();
        s.audio_app = "game.exe".into();
        assert_eq!(s.audio_choice(), AudioChoice::App("game.exe".into()));

        s.audio_mode = "all".into();
        assert_eq!(s.audio_choice(), AudioChoice::All);

        s.audio_mode = "none".into();
        assert_eq!(s.audio_choice(), AudioChoice::None);
    }

    #[test]
    fn keeps_local_server_url() {
        let s: Settings = serde_json::from_str(r#"{"serverUrl":"http://127.0.0.1:8787"}"#).unwrap();
        assert_eq!(s.server_url, "http://127.0.0.1:8787");
        assert_eq!(s.codec, "auto");
        assert_eq!(s.bitrate_kbps, None);
    }

    #[test]
    fn codec_choice_mapping() {
        use screenlive_core::encoder::Codec;
        let mut s = Settings::default();
        assert_eq!(s.codec_choice(), Codec::Auto);
        s.codec = "av1".into();
        assert_eq!(s.codec_choice(), Codec::Av1);
        s.codec = "h264".into();
        assert_eq!(s.codec_choice(), Codec::H264);
        s.codec = "unknown".into();
        assert_eq!(s.codec_choice(), Codec::Auto);
    }
}
