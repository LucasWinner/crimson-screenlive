//! Native child surface inside the Tauri "player" window so GStreamer's
//! D3D11/D3D12 VideoOverlay renders hardware video above WebView2 in the top
//! area while keeping the bottom 48px HTML control bar interactive.

pub const BAR_HEIGHT_LOGICAL: f64 = 48.0;

#[cfg(windows)]
mod imp {
    use std::sync::Once;

    use windows::core::w;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{GetStockObject, BLACK_BRUSH, HBRUSH};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, RegisterClassW, SetWindowPos, CS_HREDRAW, CS_VREDRAW,
        HWND_TOP, SWP_NOACTIVATE, WNDCLASSW, WS_CHILD, WS_CLIPSIBLINGS, WS_VISIBLE,
    };

    use super::BAR_HEIGHT_LOGICAL;

    #[derive(Clone, Copy)]
    pub struct PlayerSurface {
        child_hwnd: usize,
    }

    static REGISTER: Once = Once::new();

    unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    impl PlayerSurface {
        pub fn create(win: &tauri::WebviewWindow) -> Option<Self> {
            let parent_raw = win.hwnd().ok()?.0;
            let parent = HWND(parent_raw);
            let size = win.inner_size().ok()?;
            let scale = win.scale_factor().unwrap_or(1.0);
            let bar = (BAR_HEIGHT_LOGICAL * scale).round() as i32;
            let vid_h = (size.height as i32 - bar).max(1);

            unsafe {
                let hinst = GetModuleHandleW(None).ok()?;
                let class_name = w!("CrimsonPlayerSurface");
                REGISTER.call_once(|| {
                    let wc = WNDCLASSW {
                        style: CS_HREDRAW | CS_VREDRAW,
                        lpfnWndProc: Some(wnd_proc),
                        hInstance: hinst.into(),
                        hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                        lpszClassName: class_name,
                        ..Default::default()
                    };
                    let _ = RegisterClassW(&wc);
                });
                let child = CreateWindowExW(
                    Default::default(),
                    class_name,
                    w!(""),
                    WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
                    0,
                    0,
                    size.width as i32,
                    vid_h,
                    Some(parent),
                    None,
                    Some(hinst.into()),
                    None,
                )
                .ok()?;
                let _ = SetWindowPos(child, Some(HWND_TOP), 0, 0, size.width as i32, vid_h, SWP_NOACTIVATE);
                Some(Self { child_hwnd: child.0 as usize })
            }
        }

        pub fn resize(&self, width: u32, height: u32, scale_factor: f64) {
            let bar = (BAR_HEIGHT_LOGICAL * scale_factor).round() as i32;
            let vid_h = (height as i32 - bar).max(1);
            unsafe {
                let _ = SetWindowPos(
                    HWND(self.child_hwnd as *mut core::ffi::c_void),
                    Some(HWND_TOP),
                    0,
                    0,
                    width as i32,
                    vid_h,
                    SWP_NOACTIVATE,
                );
            }
        }

        pub fn hwnd(&self) -> usize {
            self.child_hwnd
        }
    }
}

#[cfg(not(windows))]
mod imp {
    #[derive(Clone, Copy)]
    pub struct PlayerSurface;

    impl PlayerSurface {
        pub fn create(_win: &tauri::WebviewWindow) -> Option<Self> {
            None
        }

        pub fn resize(&self, _width: u32, _height: u32, _scale_factor: f64) {}

        pub fn hwnd(&self) -> usize {
            0
        }
    }
}

pub use imp::PlayerSurface;
