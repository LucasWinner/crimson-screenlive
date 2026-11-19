//! Finding processes for per-application audio capture (Windows).
//!
//! Windows 10 build 20348+ / Windows 11 can loopback-capture a single process
//! tree, or everything except one process tree. We use that to keep Discord's
//! call audio (friends' voices) out of the stream, or to capture only a game.

#[derive(Debug, Clone, serde::Serialize)]
pub struct AppInfo {
    pub pid: u32,
    /// Executable file name, e.g. "eldenring.exe" (stable across restarts).
    pub exe: String,
    /// Main window title, for display.
    pub title: String,
}

/// Discord flavours whose call audio must never be streamed.
const DISCORD_EXES: &[&str] = &["discord.exe", "discordptb.exe", "discordcanary.exe"];

/// Windowed apps that aren't useful audio sources.
const IGNORED_EXES: &[&str] = &[
    "explorer.exe",
    "applicationframehost.exe",
    "textinputhost.exe",
    "shellexperiencehost.exe",
    "searchhost.exe",
    "startmenuexperiencehost.exe",
    "systemsettings.exe",
    "crimson-screenlive.exe",
];

/// Process-loopback capture needs Windows build 20348 or newer.
pub fn per_app_audio_supported() -> bool {
    imp::windows_build() >= 20348
}

/// Visible top-level windowed apps, one entry per process, sorted by title.
pub fn list_apps() -> Vec<AppInfo> {
    imp::list_apps()
}

/// PID of the top Discord process (its children render the call audio), if running.
pub fn discord_root_pid() -> Option<u32> {
    imp::discord_root_pid()
}

/// PID of a running windowed app with this exe name (case-insensitive).
pub fn find_app(exe: &str) -> Option<u32> {
    list_apps().into_iter().find(|a| a.exe.eq_ignore_ascii_case(exe)).map(|a| a.pid)
}

/// What the user picked for stream audio, independent of current PIDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioChoice {
    /// Whole PC, including voice chat.
    All,
    /// Whole PC except Discord (friends' voices).
    AllButDiscord,
    /// Whole PC except one program (and its child processes), by exe name.
    AllExcept(String),
    /// Only one program, by exe name.
    App(String),
    None,
}

/// Root PID of the process tree for an exe name (the instance whose parent
/// is a different program), so child processes are covered too.
pub fn find_root(exe: &str) -> Option<u32> {
    if DISCORD_EXES.contains(&exe.to_lowercase().as_str()) {
        return discord_root_pid();
    }
    imp::find_root(exe)
}

/// Turns the user's choice into a concrete capture source right now.
/// Falls back to whole-PC capture on Windows builds without per-app loopback.
pub fn resolve_audio(choice: &AudioChoice) -> anyhow::Result<crate::capture::AudioSource> {
    use crate::capture::AudioSource;
    match choice {
        AudioChoice::None => return Ok(AudioSource::None),
        AudioChoice::All => return Ok(AudioSource::System),
        _ => {}
    }
    if !per_app_audio_supported() {
        log::warn!("per-app audio capture needs Windows 10 build 20348+/Windows 11; capturing the whole PC");
        return Ok(AudioSource::System);
    }
    Ok(match choice {
        AudioChoice::AllButDiscord => match discord_root_pid() {
            Some(pid) => {
                log::info!("audio: whole PC except Discord (pid {pid})");
                AudioSource::SystemExcept(pid)
            }
            // No Discord: exclude ourselves so watching while streaming doesn't echo.
            None => AudioSource::SystemExcept(std::process::id()),
        },
        AudioChoice::App(exe) => match find_app(exe) {
            Some(pid) => {
                log::info!("audio: only {exe} (pid {pid})");
                AudioSource::App(pid)
            }
            None => anyhow::bail!("{exe} is not running"),
        },
        AudioChoice::AllExcept(exe) => match find_root(exe) {
            Some(pid) => {
                log::info!("audio: whole PC except {exe} (pid {pid})");
                AudioSource::SystemExcept(pid)
            }
            None => AudioSource::SystemExcept(std::process::id()),
        },
        AudioChoice::All | AudioChoice::None => unreachable!(),
    })
}

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;

    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, GWL_EXSTYLE,
        WS_EX_TOOLWINDOW,
    };
    use windows::core::{w, BOOL};
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

    use super::{AppInfo, DISCORD_EXES, IGNORED_EXES};

    struct Proc {
        exe: String,
        parent: u32,
    }

    fn processes() -> HashMap<u32, Proc> {
        let mut out = HashMap::new();
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
            let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut ok = Process32FirstW(snap, &mut e).is_ok();
            while ok {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                let exe = String::from_utf16_lossy(&e.szExeFile[..len]);
                out.insert(e.th32ProcessID, Proc { exe, parent: e.th32ParentProcessID });
                ok = Process32NextW(snap, &mut e).is_ok();
            }
            let _ = CloseHandle(snap);
        }
        out
    }

    pub fn windows_build() -> u32 {
        // CurrentBuildNumber is reliable for a feature gate (no manifest quirks).
        let mut buf = [0u16; 32];
        let mut size = std::mem::size_of_val(&buf) as u32;
        let res = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
                w!("CurrentBuildNumber"),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if res.is_err() {
            return 0;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len]).trim().parse().unwrap_or(0)
    }

    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<(u32, String)>);
        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }
        if (GetWindowLongW(hwnd, GWL_EXSTYLE) as u32) & WS_EX_TOOLWINDOW.0 != 0 {
            return true.into();
        }
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n <= 0 {
            return true.into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        list.push((pid, String::from_utf16_lossy(&buf[..n as usize])));
        true.into()
    }

    pub fn list_apps() -> Vec<AppInfo> {
        let mut windows: Vec<(u32, String)> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(collect), LPARAM(&mut windows as *mut _ as isize));
        }
        let procs = processes();
        let me = std::process::id();
        let mut by_pid: HashMap<u32, AppInfo> = HashMap::new();
        for (pid, title) in windows {
            let Some(p) = procs.get(&pid) else { continue };
            let exe_l = p.exe.to_lowercase();
            if pid == me || IGNORED_EXES.contains(&exe_l.as_str()) || DISCORD_EXES.contains(&exe_l.as_str()) {
                continue;
            }
            let entry = by_pid.entry(pid).or_insert_with(|| AppInfo { pid, exe: p.exe.clone(), title: String::new() });
            if title.len() > entry.title.len() {
                entry.title = title;
            }
        }
        let mut apps: Vec<AppInfo> = by_pid.into_values().collect();
        apps.sort_by_key(|a| a.title.to_lowercase());
        apps
    }

    pub fn find_root(exe: &str) -> Option<u32> {
        let procs = processes();
        let named = |pid: &u32| procs.get(pid).is_some_and(|p| p.exe.eq_ignore_ascii_case(exe));
        procs.iter().find(|(pid, p)| named(pid) && !named(&p.parent)).map(|(pid, _)| *pid)
    }

    pub fn discord_root_pid() -> Option<u32> {
        let procs = processes();
        let is_discord = |pid: &u32| procs.get(pid).is_some_and(|p| DISCORD_EXES.contains(&p.exe.to_lowercase().as_str()));
        procs.iter().find(|(pid, p)| is_discord(pid) && !is_discord(&p.parent)).map(|(pid, _)| *pid)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::AppInfo;
    pub fn windows_build() -> u32 {
        0
    }
    pub fn list_apps() -> Vec<AppInfo> {
        Vec::new()
    }
    pub fn discord_root_pid() -> Option<u32> {
        None
    }
    pub fn find_root(_exe: &str) -> Option<u32> {
        None
    }
}
