//! Top-level windows in z-order, front to back, with the facts the window log, the picker and
//! target discovery need. Bounds are physical virtual-screen pixels (D3).

use win32::Win32::Foundation::{HWND, LPARAM, RECT};
use win32::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use win32::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOTOWNER, GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetClassNameW,
    GetForegroundWindow, GetLayeredWindowAttributes, GetWindow, GetWindowLongW, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, LWA_ALPHA,
    LAYERED_WINDOW_ATTRIBUTES_FLAGS, WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
};
use win32::core::BOOL;

#[derive(Clone, Debug)]
pub struct TopWindow {
    pub hwnd: isize,
    pub pid: u32,
    pub class: String,
    pub title: String,
    /// `DWMWA_EXTENDED_FRAME_BOUNDS` (no invisible resize border), else `GetWindowRect`, as
    /// physical `[x, y, w, h]`.
    pub bounds: [f64; 4],
    pub style: u32,
    pub ex_style: u32,
    pub owner: isize,
    pub iconic: bool,
    pub cloaked: bool,
    pub alpha: f64,
}

/// Every visible top-level window, front to back.
pub fn enumerate() -> Vec<TopWindow> {
    let mut hwnds: Vec<isize> = Vec::new();
    // SAFETY: the callback only runs during EnumWindows, while `hwnds` is borrowed.
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&raw mut hwnds as isize)) };
    hwnds
        .into_iter()
        .filter_map(|hwnd| describe(HWND(hwnd as *mut core::ffi::c_void)))
        .collect()
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `Vec` passed by `enumerate`, alive for the whole enumeration.
    let hwnds = unsafe { &mut *(lparam.0 as *mut Vec<isize>) };
    // SAFETY: `hwnd` comes from EnumWindows.
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        hwnds.push(hwnd.0 as isize);
    }
    BOOL(1)
}

fn describe(hwnd: HWND) -> Option<TopWindow> {
    // SAFETY: all calls are plain queries on a window handle; a destroyed window just fails.
    unsafe {
        let mut pid = 0u32;
        if GetWindowThreadProcessId(hwnd, Some(&mut pid)) == 0 {
            return None;
        }
        let mut class_buf = [0u16; 256];
        let class_len = GetClassNameW(hwnd, &mut class_buf).max(0) as usize;
        let class = String::from_utf16_lossy(&class_buf[..class_len]);
        let mut title_buf = [0u16; 512];
        let title_len = GetWindowTextW(hwnd, &mut title_buf).max(0) as usize;
        let title = String::from_utf16_lossy(&title_buf[..title_len]);
        let mut rect = RECT::default();
        let frame = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rect).cast(),
            std::mem::size_of::<RECT>() as u32,
        );
        if frame.is_err() {
            GetWindowRect(hwnd, &mut rect).ok()?;
        }
        let bounds = [
            f64::from(rect.left),
            f64::from(rect.top),
            f64::from(rect.right - rect.left),
            f64::from(rect.bottom - rect.top),
        ];
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let owner = GetWindow(hwnd, GW_OWNER).map_or(0, |owner| owner.0 as isize);
        let iconic = IsIconic(hwnd).as_bool();
        let mut cloaked_value = 0u32;
        let cloaked = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked_value).cast(),
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked_value != 0;
        let mut alpha = 1.0;
        if ex_style & WS_EX_LAYERED.0 != 0 {
            let mut value = 255u8;
            let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
            if GetLayeredWindowAttributes(hwnd, None, Some(&mut value), Some(&mut flags)).is_ok()
                && flags.0 & LWA_ALPHA.0 != 0
            {
                alpha = f64::from(value) / 255.0;
            }
        }
        Some(TopWindow {
            hwnd: hwnd.0 as isize,
            pid,
            class,
            title,
            bounds,
            style,
            ex_style,
            owner,
            iconic,
            cloaked,
            alpha,
        })
    }
}

/// Whether this is a window a person would call an app window: shown, not cloaked on another
/// virtual desktop, unowned and not a tool window (or forced onto the taskbar), and not the
/// desktop or the taskbar.
pub fn is_app_window(w: &TopWindow) -> bool {
    let tool = w.ex_style & WS_EX_TOOLWINDOW.0 != 0;
    let app = w.ex_style & WS_EX_APPWINDOW.0 != 0;
    !w.iconic
        && !w.cloaked
        && ((w.owner == 0 && !tool) || app)
        && !matches!(
            w.class.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        )
}

/// The primary or a secondary taskbar.
pub fn is_taskbar(w: &TopWindow) -> bool {
    matches!(w.class.as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

/// The window in front, as its root owner (a dialog reports the app window that owns it).
pub fn foreground() -> Option<TopWindow> {
    // SAFETY: plain queries; a null handle is checked before use.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let root = GetAncestor(hwnd, GA_ROOTOWNER);
        describe(if root.0.is_null() { hwnd } else { root })
    }
}

/// The `u32` id Scrawl stores for a window (D24).
pub fn id_of(hwnd: isize) -> u32 {
    hwnd as usize as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(class: &str, ex_style: u32, owner: isize) -> TopWindow {
        TopWindow {
            hwnd: 1,
            pid: 1,
            class: class.into(),
            title: String::new(),
            bounds: [0.0, 0.0, 100.0, 100.0],
            style: 0,
            ex_style,
            owner,
            iconic: false,
            cloaked: false,
            alpha: 1.0,
        }
    }

    #[test]
    fn app_windows_exclude_tools_owned_windows_and_the_shell() {
        assert!(is_app_window(&window("Notepad", 0, 0)));
        assert!(!is_app_window(&window("Notepad", WS_EX_TOOLWINDOW.0, 0)));
        assert!(!is_app_window(&window("#32770", 0, 42)));
        assert!(is_app_window(&window("#32770", WS_EX_APPWINDOW.0, 42)));
        assert!(!is_app_window(&window("Shell_TrayWnd", 0, 0)));
        assert!(is_taskbar(&window("Shell_SecondaryTrayWnd", 0, 0)));
        assert_eq!(id_of(0x0001_0234), 0x0001_0234);
    }
}
