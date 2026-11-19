//! Crimson ScreenLive portable launcher.
//!
//! One .exe that carries the whole app (exe + GStreamer DLLs + plugins) as an
//! embedded ZIP. On first run it unpacks to
//! %LOCALAPPDATA%\CrimsonScreenLive\portable\<version-crc>\ and starts the app;
//! later runs start it directly. Older unpacked versions are cleaned up.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.zip"));
const KEY: &str = concat!(env!("CARGO_PKG_VERSION"), "-", env!("PAYLOAD_CRC"));
const APP_DIR: &str = "Crimson ScreenLive";
const APP_EXE: &str = "Crimson ScreenLive.exe";
const DONE_MARKER: &str = ".complete";

fn cache_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("CrimsonScreenLive")
        .join("portable")
}

fn unpack(root: &Path, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| format!("não foi possível criar {}: {e}", root.display()))?;
    // Unpack beside the final folder, then rename: a crash mid-way never leaves
    // a half-unpacked folder that looks complete.
    let tmp = root.join(format!("{KEY}.tmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut zip = zip::ZipArchive::new(Cursor::new(PAYLOAD)).map_err(|e| format!("pacote corrompido: {e}"))?;
    zip.extract(&tmp).map_err(|e| format!("falha ao descompactar: {e}"))?;
    std::fs::write(tmp.join(DONE_MARKER), KEY).map_err(|e| e.to_string())?;
    if dir.exists() {
        let _ = std::fs::remove_dir_all(dir); // leftover from an interrupted run
    }
    if let Err(e) = std::fs::rename(&tmp, dir) {
        let _ = std::fs::remove_dir_all(&tmp);
        // Another instance may have finished first; that's fine.
        if !dir.join(DONE_MARKER).exists() {
            return Err(format!("falha ao preparar o app: {e}"));
        }
    }
    Ok(())
}

/// Best-effort removal of other versions (skips anything still in use).
fn cleanup_old(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy() != KEY {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn run() -> Result<(), String> {
    if PAYLOAD.is_empty() {
        return Err("este lançador foi compilado sem o app embutido (use npm run build).".into());
    }
    let root = cache_root();
    let dir = root.join(KEY);
    if !dir.join(DONE_MARKER).exists() {
        unpack(&root, &dir)?;
        cleanup_old(&root);
    }
    let app = dir.join(APP_DIR);
    Command::new(app.join(APP_EXE))
        .args(std::env::args_os().skip(1))
        .current_dir(&app)
        .spawn()
        .map_err(|e| format!("não foi possível abrir o app: {e}"))?;
    Ok(())
}

fn main() {
    if let Err(msg) = run() {
        show_error(&msg);
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn show_error(msg: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (text, title) = (wide(msg), wide("Crimson ScreenLive"));
    unsafe {
        MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
fn show_error(msg: &str) {
    eprintln!("Crimson ScreenLive: {msg}");
}
