//! The ink overlay on Windows: a click-through GPUI window over the display being recorded
//! that paints what `laser_overlay.metal` paints on macOS, from the same telemetry: the
//! picker's dim and cutout, the target's inset border, the start glow, Shift ink with its fade,
//! pins and the Shift tip dot. It is left out of the recording (`WDA_EXCLUDEFROMCAPTURE`).
//!
//! Also the Win32 window helpers Scrawl's other popups use.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, Context, Corners, Edges, Hsla, IntoElement, PathBuilder, PathStyle, Pixels, Point,
    Render, StrokeOptions, Window, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, canvas, point, prelude::*, px, size,
};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMNCRP_DISABLED, DWMWA_BORDER_COLOR, DWMWA_CLOAKED, DWMWA_NCRENDERING_POLICY,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DwmGetWindowAttribute,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::WindowsAndMessaging::{
    GW_HWNDPREV, GWL_EXSTYLE, GetClassNameW, GetClientRect, GetWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowThreadProcessId, HWND_TOPMOST, IsIconic, IsWindowVisible, LWA_ALPHA,
    PostMessageW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetLayeredWindowAttributes,
    SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, ShowWindow, WDA_EXCLUDEFROMCAPTURE,
    WM_CLOSE, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};
use windows::core::{PCSTR, w};

use super::Monitor;
use crate::capture::{SharedCaptureTelemetry, scrawl_log};
use crate::model::RgbaFrame;
use crate::sketch::{slice_laser_window, smooth_ink_points, smooth_laser_points};

const TAIL_MS: u64 = 420;

/// Leaves a window out of every screen capture (Windows 10 2004 and later).
/// `SCRAWL_SHOW_OWN_WINDOWS=1` keeps them in, for screenshots of Scrawl itself.
pub fn exclude_from_capture(hwnd: HWND) {
    if std::env::var_os("SCRAWL_SHOW_OWN_WINDOWS").is_some() {
        return;
    }
    let _ = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) };
}

/// Turns off what Windows adds around a transparent popup: Windows 11's rounded corners,
/// border and shadow (a ghost box), and GPUI's transparent-gradient accent (a faint veil;
/// DirectComposition already gives per-pixel alpha).
pub fn strip_chrome(hwnd: HWND) {
    let round = DWMWCP_DONOTROUND;
    let policy = DWMNCRP_DISABLED;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const round).cast(),
            4,
        );
        let none = 0xFFFF_FFFE_u32; // DWMWA_COLOR_NONE
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, (&raw const none).cast(), 4);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            (&raw const policy).cast(),
            4,
        );
    }
    disable_accent(hwnd);
}

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    gradient: u32,
    animation_id: u32,
}

#[repr(C)]
struct CompositionData {
    attribute: u32,
    data: *mut c_void,
    size: usize,
}

type SetWindowCompositionAttribute = unsafe extern "system" fn(HWND, *mut CompositionData) -> i32;

/// `SetWindowCompositionAttribute(WCA_ACCENT_POLICY, ACCENT_DISABLED)`: user32 exports it
/// without a header.
fn disable_accent(hwnd: HWND) {
    unsafe {
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else {
            return;
        };
        let Some(proc) = GetProcAddress(
            user32,
            PCSTR(c"SetWindowCompositionAttribute".as_ptr().cast()),
        ) else {
            return;
        };
        let set: SetWindowCompositionAttribute = std::mem::transmute(proc);
        let mut policy = AccentPolicy {
            state: 0,
            flags: 0,
            gradient: 0,
            animation_id: 0,
        };
        let mut data = CompositionData {
            attribute: 19,
            data: (&raw mut policy).cast(),
            size: std::mem::size_of::<AccentPolicy>(),
        };
        set(hwnd, &mut data);
    }
}

/// A window's pixels, even when covered (`PrintWindow` with full content).
pub fn capture_window(id: isize) -> anyhow::Result<RgbaFrame> {
    let hwnd = super::hwnd(id);
    let mut rect = Default::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }?;
    let (width, height) = (
        (rect.right - rect.left).max(1),
        (rect.bottom - rect.top).max(1),
    );
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        let old = SelectObject(dc, bitmap.into());
        // PW_RENDERFULLCONTENT captures DirectComposition content.
        let _ = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(2));
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![0_u8; (width * height * 4) as usize];
        GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(bgra.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        SelectObject(dc, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        Ok(RgbaFrame::new(0, width as u32, height as u32, bgra))
    }
}

/// The display an overlay covers: its rect in global pixels and its scale.
#[derive(Clone, Copy)]
struct Placement {
    rect: [f64; 4],
    scale: f32,
}

/// The overlay for one display. Dropping it closes the window.
pub struct InkOverlay {
    hwnd: isize,
    placement: Arc<Mutex<Placement>>,
    /// Cleared on drop: stops the watchdog.
    alive: Arc<AtomicBool>,
}

/// The display holding the centre of `rect` (global pixels), else the one nearest to it, as
/// `MonitorFromPoint(MONITOR_DEFAULTTONEAREST)` picks.
fn display_for(monitors: &[Monitor], rect: [f64; 4]) -> Option<Monitor> {
    let [x, y, w, h] = rect;
    let centre = [x + w / 2.0, y + h / 2.0];
    let distance = |m: &Monitor| {
        let [mx, my, mw, mh] = m.rect;
        let dx = (mx - centre[0]).max(centre[0] - (mx + mw)).max(0.0);
        let dy = (my - centre[1]).max(centre[1] - (my + mh)).max(0.0);
        dx.hypot(dy)
    };
    monitors
        .iter()
        .find(|m| crate::screens::contains(m.rect, centre))
        .or_else(|| {
            monitors
                .iter()
                .min_by(|a, b| distance(a).total_cmp(&distance(b)))
        })
        .cloned()
}

/// Where the overlay goes for `rect`: the display holding it, or `None` when that's the one it
/// covers already (or there's none).
fn next_display(monitors: &[Monitor], covers: [f64; 4], rect: [f64; 4]) -> Option<Monitor> {
    display_for(monitors, rect).filter(|m| !same_rect(m.rect, covers))
}

fn same_rect(a: [f64; 4], b: [f64; 4]) -> bool {
    (0..4).all(|i| (a[i] - b[i]).abs() < 1.0)
}

impl InkOverlay {
    /// Covers the display holding the centre of `on` (global pixels), else the primary.
    pub fn open<T: 'static>(
        telemetry: Arc<SharedCaptureTelemetry>,
        on: Option<[f64; 4]>,
        cx: &mut Context<T>,
    ) -> Result<Self, String> {
        let monitor = on
            .and_then(|rect| display_for(&super::monitors(), rect))
            .or_else(super::primary_monitor)
            .ok_or("No display")?;
        let display_id = crate::ui::gpui_display_for(&monitor, cx).map(|d| d.id());
        let scale = monitor.scale as f32;
        let bounds = Bounds {
            origin: point(
                px(monitor.rect[0] as f32 / scale),
                px(monitor.rect[1] as f32 / scale),
            ),
            size: size(
                px(monitor.rect[2] as f32 / scale),
                px(monitor.rect[3] as f32 / scale),
            ),
        };
        let rect = monitor.rect;
        let placement = Arc::new(Mutex::new(Placement { rect, scale }));
        let view_placement = Arc::clone(&placement);
        // When it last drew, for the watchdog: GPUI draws it again only while each frame asks
        // for the next one (`request_animation_frame`), and one lost request stops it for good.
        let drawn = Arc::new(AtomicU64::new(clock_ms()));
        let view_drawn = Arc::clone(&drawn);
        let watch_telemetry = Arc::clone(&telemetry);
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    display_id,
                    titlebar: None,
                    focus: false,
                    show: false,
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    is_resizable: false,
                    is_minimizable: false,
                    window_background: WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                },
                move |window, cx| {
                    // Layered before GPUI's first frame, or DirectComposition never shows it.
                    // Clicks fall through only a window that is both layered and transparent;
                    // alpha 255 keeps GPUI's per-pixel alpha as it is.
                    if let Some(hwnd) = crate::ui::hwnd_of(window) {
                        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
                        let extra = WS_EX_TRANSPARENT.0
                            | WS_EX_LAYERED.0
                            | WS_EX_TOOLWINDOW.0
                            | WS_EX_NOACTIVATE.0
                            | WS_EX_TOPMOST.0;
                        unsafe {
                            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (style | extra) as isize);
                            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
                        }
                        strip_chrome(hwnd);
                        exclude_from_capture(hwnd);
                    }
                    cx.new(|_| OverlayView {
                        telemetry,
                        placement: view_placement,
                        drawn: view_drawn,
                    })
                },
            )
            .map_err(|error| format!("{error:#}"))?;
        let hwnd = handle
            .update(cx, |_, window, _| crate::ui::hwnd_of(window))
            .ok()
            .flatten()
            .ok_or("The overlay has no window")?;
        // Resized on the next turn of the event loop: called from inside an update, GPUI's
        // resize callback can't reach the app and the window keeps its default layout size.
        let id = hwnd.0 as isize;
        scrawl_log(format!(
            "ink overlay opens on display {rect:?} (scale {scale}) for {on:?}"
        ));
        cx.spawn(async move |_, _| place(id, rect)).detach();
        let alive = Arc::new(AtomicBool::new(true));
        let watchdog = Watchdog {
            id,
            handle,
            telemetry: watch_telemetry,
            placement: Arc::clone(&placement),
            drawn,
            alive: Arc::clone(&alive),
        };
        cx.spawn(async move |_, cx| watchdog.run(cx).await).detach();
        Ok(Self {
            hwnd: hwnd.0 as isize,
            placement,
            alive,
        })
    }

    pub fn window_number(&self) -> u32 {
        self.hwnd as usize as u32
    }

    /// The display it covers, global pixels.
    pub fn screen(&self) -> [f64; 4] {
        self.placement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rect
    }

    /// Moves to the display holding the centre of `rect` (global pixels). Returns whether it
    /// moved: false when it's on that display already or there's none there.
    pub fn move_to(&mut self, rect: [f64; 4]) -> bool {
        let monitor = {
            let mut placement = self
                .placement
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(monitor) = next_display(&super::monitors(), placement.rect, rect) else {
                return false;
            };
            *placement = Placement {
                rect: monitor.rect,
                scale: monitor.scale as f32,
            };
            monitor
        };
        scrawl_log(format!(
            "ink overlay moves to display {:?} (scale {}) for {rect:?}",
            monitor.rect, monitor.scale
        ));
        // Off the UI thread: this runs inside a GPUI update, where the resize callback can't
        // reach the app (see `open`); the window's thread handles the move from its loop.
        let id = self.hwnd;
        std::thread::spawn(move || place(id, monitor.rect));
        true
    }
}

/// Puts one of Scrawl's GPUI windows (the overlay, picker or pill) over `rect` (global pixels)
/// on top, and shows it without activating it. Call it outside GPUI updates: the moves it makes
/// reach GPUI's resize and DPI handlers at once.
pub(crate) fn place(id: isize, rect: [f64; 4]) {
    let hwnd = super::hwnd(id);
    // A minimized window is restored first: moved while minimized, it would come back where it
    // was before.
    if unsafe { IsIconic(hwnd) }.as_bool() {
        let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
    }
    let [x, y, w, h] = rect.map(|v| v.round() as i32);
    let _ = unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE) };
    // GPUI keeps a resize frame around the client area; grow the window by it so the client
    // area (what GPUI draws) covers `rect` exactly. Measured from where the window is now: a
    // move onto a monitor of another DPI has GPUI resize it to Windows' suggested rect.
    let (mut window, mut client, mut origin) = (RECT::default(), RECT::default(), POINT::default());
    let placed = unsafe {
        GetWindowRect(hwnd, &mut window).is_ok()
            && GetClientRect(hwnd, &mut client).is_ok()
            && ClientToScreen(hwnd, &mut origin).as_bool()
    };
    if placed {
        let [x, y, w, h] = grown_by_frame([x, y, w, h], window, origin, client);
        let _ = unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE) };
    }
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
}

/// The window rect whose client area is `target`, given where the window is now (`window`), its
/// client area's screen origin and its client rect.
fn grown_by_frame(target: [i32; 4], window: RECT, origin: POINT, client: RECT) -> [i32; 4] {
    let [x, y, w, h] = target;
    let (left, top) = (origin.x - window.left, origin.y - window.top);
    let right = window.right - origin.x - client.right;
    let bottom = window.bottom - origin.y - client.bottom;
    [x - left, y - top, w + left + right, h + top + bottom]
}

/// Milliseconds on a process-wide monotonic clock (never 0).
fn clock_ms() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// How often the watchdog looks, how long the overlay may go without drawing, and how long a
/// repair gets before the next one.
const WATCH_MS: u64 = 100;
const STALL_MS: u64 = 600;
const RETRY_MS: u64 = 1_000;

/// What Windows says about the overlay's window.
#[derive(Clone, Debug, PartialEq)]
struct WindowState {
    visible: bool,
    minimized: bool,
    topmost: bool,
    /// Its client area (what GPUI draws into), global pixels.
    client: [i32; 4],
}

impl std::fmt::Display for WindowState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "visible={} topmost={} minimized={} client={:?}",
            self.visible, self.topmost, self.minimized, self.client
        )
    }
}

fn window_state(id: isize) -> Option<WindowState> {
    let hwnd = super::hwnd(id);
    let (mut client, mut origin) = (RECT::default(), POINT::default());
    unsafe {
        GetClientRect(hwnd, &mut client).ok()?;
        if !ClientToScreen(hwnd, &mut origin).as_bool() {
            return None;
        }
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        Some(WindowState {
            visible: IsWindowVisible(hwnd).as_bool(),
            minimized: IsIconic(hwnd).as_bool(),
            topmost: style & WS_EX_TOPMOST.0 != 0,
            client: [origin.x, origin.y, client.right, client.bottom],
        })
    }
}

/// Windows that sit above everything on purpose, at a display's edge or only briefly: the
/// taskbars, menus and tooltips.
const SHELL_BARS: [&str; 4] = [
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "#32768",
    "tooltips_class32",
];

/// The first window above the overlay (in z-order) that overlaps `area` (global pixels) and
/// isn't process `own`'s, as "class (pid N)".
fn covering_window(id: isize, area: [f64; 4], own: u32) -> Option<String> {
    let mut next = unsafe { GetWindow(super::hwnd(id), GW_HWNDPREV) };
    for _ in 0..256 {
        let above = next.ok().filter(|h| !h.is_invalid())?;
        next = unsafe { GetWindow(above, GW_HWNDPREV) };
        let mut cloaked = 0_u32;
        let _ = unsafe {
            DwmGetWindowAttribute(
                above,
                DWMWA_CLOAKED,
                (&raw mut cloaked).cast(),
                std::mem::size_of::<u32>() as u32,
            )
        };
        let mut pid = 0_u32;
        unsafe { GetWindowThreadProcessId(above, Some(&mut pid)) };
        let shown = unsafe { IsWindowVisible(above).as_bool() && !IsIconic(above).as_bool() };
        if !shown || cloaked != 0 || pid == own {
            continue;
        }
        let Some(frame) = super::window_frame(above).or_else(|| window_rect(above)) else {
            continue;
        };
        if !overlaps(frame, area) {
            continue;
        }
        let mut buffer = [0_u16; 128];
        let read = unsafe { GetClassNameW(above, &mut buffer) };
        let class = String::from_utf16_lossy(&buffer[..read.max(0) as usize]);
        if SHELL_BARS.contains(&class.as_str()) {
            continue;
        }
        return Some(format!("{class} (pid {pid})"));
    }
    None
}

fn window_rect(hwnd: HWND) -> Option<[f64; 4]> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r) }.ok()?;
    Some([r.left, r.top, r.right - r.left, r.bottom - r.top].map(f64::from))
}

fn overlaps(a: [f64; 4], b: [f64; 4]) -> bool {
    let w = (a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0]);
    let h = (a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1]);
    w >= 1.0 && h >= 1.0
}

/// What's wrong with the overlay's window, for the display it should cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    Hidden,
    Minimized,
    NotTopmost,
    /// Its client area isn't the display's rect.
    Misplaced,
}

fn faults(state: &WindowState, display: [f64; 4]) -> Vec<Fault> {
    let mut faults = Vec::new();
    if !state.visible {
        faults.push(Fault::Hidden);
    }
    if state.minimized {
        faults.push(Fault::Minimized);
    }
    if !state.topmost {
        faults.push(Fault::NotTopmost);
    }
    if (0..4).any(|i| (f64::from(state.client[i]) - display[i]).abs() > 2.0) {
        faults.push(Fault::Misplaced);
    }
    faults
}

/// One look at the overlay.
struct Look {
    now: u64,
    /// When it last drew.
    drawn: u64,
    state: Option<WindowState>,
    /// The display it should cover, global pixels.
    display: [f64; 4],
    covered_by: Option<String>,
    /// Shift ink is being drawn.
    shift: bool,
    pointer_on_display: bool,
}

/// What to do about a look: lines for the log, a redraw (restarting GPUI's frame requests), and
/// putting the window back over its display on top.
#[derive(Debug, Default, PartialEq)]
struct Repair {
    logs: Vec<String>,
    redraw: bool,
    reassert: bool,
}

/// The watchdog's memory between looks, so it logs each change once.
#[derive(Default)]
struct Watch {
    logged: Option<WindowState>,
    stalled_since: Option<u64>,
    faults: Vec<Fault>,
    covered_by: Option<String>,
    /// Covering windows already named in the log.
    named: Vec<String>,
    last_repair: Option<u64>,
    shift: bool,
    pointer_on_display: bool,
}

impl Watch {
    fn look(&mut self, look: &Look) -> Repair {
        let mut repair = Repair::default();
        let retry_due = self
            .last_repair
            .is_none_or(|t| look.now.saturating_sub(t) >= RETRY_MS);
        let described = look
            .state
            .as_ref()
            .map_or_else(|| "no window".to_owned(), ToString::to_string);
        if look.state != self.logged {
            repair.logs.push(format!(
                "ink overlay window for display {:?}: {described}",
                look.display
            ));
            self.logged = look.state.clone();
        }

        // GPUI draws the overlay only while each frame asks for the next; a request it drops
        // (a paint while the app is busy) leaves the last frame up with no ink.
        let quiet = look.now.saturating_sub(look.drawn);
        if quiet >= STALL_MS {
            if self.stalled_since.is_none() {
                self.stalled_since = Some(look.drawn);
                repair.logs.push(format!(
                    "ink overlay stopped drawing {quiet} ms ago ({described}): redrawing it"
                ));
                repair.redraw = true;
                repair.reassert = true;
            } else if retry_due {
                repair.redraw = true;
                repair.reassert = true;
            }
        } else if let Some(since) = self.stalled_since.take() {
            repair.logs.push(format!(
                "ink overlay draws again after {} ms",
                look.drawn.saturating_sub(since)
            ));
        }

        let faults = look
            .state
            .as_ref()
            .map_or_else(Vec::new, |state| faults(state, look.display));
        if faults != self.faults {
            if !faults.is_empty() {
                repair.logs.push(format!(
                    "ink overlay is {faults:?} ({described}): putting it back over {:?}",
                    look.display
                ));
                repair.reassert = true;
            }
            self.faults = faults;
        } else if !faults.is_empty() && retry_due {
            repair.reassert = true;
        }

        if look.covered_by != self.covered_by {
            if let Some(by) = &look.covered_by {
                if !self.named.contains(by) {
                    repair
                        .logs
                        .push(format!("ink overlay is covered by {by}: raising it"));
                    self.named.push(by.clone());
                }
                repair.reassert = true;
            }
            self.covered_by = look.covered_by.clone();
        }

        // Cheap insurance where it matters: as Shift ink starts and as the pointer comes back
        // to the overlay's display.
        if (look.shift && !self.shift) || (look.pointer_on_display && !self.pointer_on_display) {
            repair.reassert = true;
        }
        self.shift = look.shift;
        self.pointer_on_display = look.pointer_on_display;
        if repair.redraw || repair.reassert {
            self.last_repair = Some(look.now);
        }
        repair
    }
}

/// Watches the overlay while it's open: logs its window's state once per change, and puts it
/// back when it stops drawing or isn't on top over its display any more.
struct Watchdog {
    id: isize,
    handle: gpui::WindowHandle<OverlayView>,
    telemetry: Arc<SharedCaptureTelemetry>,
    placement: Arc<Mutex<Placement>>,
    drawn: Arc<AtomicU64>,
    alive: Arc<AtomicBool>,
}

impl Watchdog {
    async fn run(self, cx: &mut gpui::AsyncApp) {
        let mut watch = Watch::default();
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(WATCH_MS))
                .await;
            if !self.alive.load(Ordering::Acquire) {
                return;
            }
            let display = self
                .placement
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .rect;
            let t = &self.telemetry;
            let frame = *t
                .target_frame
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let area = if t.active.load(Ordering::Acquire) && frame.x > -10_000.0 {
                [frame.x, frame.y, frame.width, frame.height]
            } else {
                display
            };
            let look = Look {
                now: clock_ms(),
                drawn: self.drawn.load(Ordering::Acquire),
                state: window_state(self.id),
                display,
                covered_by: covering_window(self.id, area, std::process::id()),
                shift: t.shift_held.load(Ordering::Acquire),
                pointer_on_display: super::cursor_position()
                    .is_some_and(|(x, y)| crate::screens::contains(display, [x, y])),
            };
            let repair = watch.look(&look);
            for line in &repair.logs {
                scrawl_log(line);
            }
            if repair.redraw {
                let _ = self.handle.update(cx, |_, window, cx| {
                    cx.notify();
                    window.refresh();
                });
            }
            if repair.reassert {
                // Off the UI thread, like `move_to`: the moves reach GPUI's handlers at once.
                let id = self.id;
                std::thread::spawn(move || {
                    place(id, display);
                    crate::ui::raise_floating_windows();
                });
            }
        }
    }
}

impl Drop for InkOverlay {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        let _ =
            unsafe { PostMessageW(Some(super::hwnd(self.hwnd)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}

struct OverlayView {
    telemetry: Arc<SharedCaptureTelemetry>,
    /// The display it covers (moved by `InkOverlay::move_to`).
    placement: Arc<Mutex<Placement>>,
    /// When it last drew (`clock_ms`), for the watchdog.
    drawn: Arc<AtomicU64>,
}

/// One frame's geometry, in the window's points.
struct Scene {
    target: Option<([f32; 4], f32)>,
    uniform_dim: Option<f32>,
    now_sec: f32,
    picker_fade: f32,
    glow: bool,
    ink: Vec<(Vec<[f32; 2]>, f32)>,
    laser: Vec<Vec<([f32; 2], f32)>>,
    pins: Vec<[f32; 2]>,
    tip: Option<[f32; 2]>,
    screen: [f32; 2],
}

fn ink_color(alpha: f32) -> Hsla {
    gpui::Rgba {
        r: 1.0,
        g: 0.23,
        b: 0.19,
        a: alpha,
    }
    .into()
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> Hsla {
    gpui::Rgba { r, g, b, a }.into()
}

impl OverlayView {
    fn scene(&self, screen: [f32; 2]) -> Option<Scene> {
        let t = &self.telemetry;
        let active = t.active.load(Ordering::Acquire);
        let selecting = t.selecting.load(Ordering::Acquire);
        let mode = t.capture_mode.load(Ordering::Acquire);
        if !active && !selecting && mode != 3 {
            return None;
        }
        let now_ms = t.elapsed_ms();
        let now_sec = if selecting {
            0.0
        } else {
            now_ms as f32 / 1000.0
        };
        let picker_fade = if selecting {
            let elapsed = t
                .picker_started_at
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .elapsed()
                .as_secs_f32();
            1.0 - (1.0 - (elapsed / 0.15).clamp(0.0, 1.0)).powi(3)
        } else {
            1.0
        };
        let placement = *self
            .placement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let k = 1.0 / placement.scale;
        let [ox, oy] = [placement.rect[0] as f32, placement.rect[1] as f32];
        let local = |x: f32, y: f32| [(x - ox) * k, (y - oy) * k];
        let picker = if selecting {
            *t.picker_rect
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        } else {
            None
        };
        let frame = *t
            .target_frame
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (target, uniform_dim) = if selecting {
            match picker {
                Some(([x, y, w, h], kind)) => {
                    let [lx, ly] = local(x, y);
                    (
                        Some(([lx, ly, w.max(1.0) * k, h.max(1.0) * k], f32::from(kind))),
                        None,
                    )
                }
                None => (None, Some(picker_fade)),
            }
        } else {
            let [lx, ly] = local(frame.x as f32, frame.y as f32);
            let constrained = mode > 0 && frame.x > -10_000.0;
            let rect = [
                lx,
                ly,
                (frame.width as f32).max(1.0) * k,
                (frame.height as f32).max(1.0) * k,
            ];
            (constrained.then_some((rect, mode as f32)), None)
        };
        let to_screen = |nx: f32, ny: f32| -> [f32; 2] {
            let [lx, ly] = local(frame.x as f32, frame.y as f32);
            [
                lx + nx * frame.width as f32 * k,
                ly + ny * frame.height as f32 * k,
            ]
        };

        let mut strokes: Vec<(crate::model::Stroke, bool)> = t
            .strokes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .map(|s| (s, false))
            .collect();
        if let Some(laser) = t
            .active_laser
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            strokes.push((laser, true));
        }
        if let Some(ink) = t
            .live_ink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            strokes.push((ink, true));
        }
        let mut ink = Vec::new();
        let mut laser = Vec::new();
        let mut pins = Vec::new();
        if active {
            for (stroke, live) in &strokes {
                if stroke.pin.is_some() {
                    if let Some([nx, ny]) = stroke.normalized_points().first().copied() {
                        pins.push(to_screen(nx, ny));
                    }
                    continue;
                }
                if stroke.laser {
                    if stroke.t1 + TAIL_MS < now_ms {
                        continue;
                    }
                    let raw =
                        slice_laser_window(&stroke.timed_points(), now_ms, TAIL_MS, to_screen);
                    laser.push(smooth_laser_points(&raw));
                    continue;
                }
                let alpha = if *live {
                    1.0
                } else {
                    let age = now_ms.saturating_sub(stroke.t1);
                    if age >= 1_300 {
                        continue;
                    } else if age <= 650 {
                        1.0
                    } else {
                        let t = ((age - 650) as f32 / 650.0).clamp(0.0, 1.0);
                        (1.0 - t) * (1.0 - t)
                    }
                };
                let pts: Vec<[f32; 2]> = stroke
                    .normalized_points()
                    .into_iter()
                    .map(|[nx, ny]| to_screen(nx, ny))
                    .collect();
                let smoothed: Vec<[f32; 2]> = smooth_ink_points(&pts)
                    .into_iter()
                    .map(|(p, _)| p)
                    .collect();
                ink.push((smoothed, alpha));
            }
        }
        let tip = (active && t.shift_held.load(Ordering::Acquire)).then(|| {
            let [x, y] = *t
                .pointer_screen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            local(x, y)
        });
        Some(Scene {
            target,
            uniform_dim,
            now_sec,
            picker_fade,
            glow: active && t.start_glow.load(Ordering::Acquire),
            ink,
            laser,
            pins,
            tip,
            screen,
        })
    }
}

fn pt(p: [f32; 2], origin: Point<Pixels>) -> Point<Pixels> {
    point(origin.x + px(p[0]), origin.y + px(p[1]))
}

fn rect_bounds(r: [f32; 4], origin: Point<Pixels>) -> Bounds<Pixels> {
    Bounds {
        origin: pt([r[0], r[1]], origin),
        size: size(px(r[2].max(0.0)), px(r[3].max(0.0))),
    }
}

/// A rounded rectangle as a closed path (for the dim's even-odd hole).
fn add_rounded_rect(builder: &mut PathBuilder, r: [f32; 4], radius: f32, origin: Point<Pixels>) {
    let [x, y, w, h] = r;
    let rad = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    if rad <= 0.0 {
        builder.add_polygon(
            &[
                pt([x, y], origin),
                pt([x + w, y], origin),
                pt([x + w, y + h], origin),
                pt([x, y + h], origin),
            ],
            true,
        );
        return;
    }
    builder.move_to(pt([x + rad, y], origin));
    builder.line_to(pt([x + w - rad, y], origin));
    builder.curve_to(pt([x + w, y + rad], origin), pt([x + w, y], origin));
    builder.line_to(pt([x + w, y + h - rad], origin));
    builder.curve_to(pt([x + w - rad, y + h], origin), pt([x + w, y + h], origin));
    builder.line_to(pt([x + rad, y + h], origin));
    builder.curve_to(pt([x, y + h - rad], origin), pt([x, y + h], origin));
    builder.line_to(pt([x, y + rad], origin));
    builder.curve_to(pt([x + rad, y], origin), pt([x, y], origin));
    builder.close();
}

fn round_stroke(width: f32) -> PathBuilder {
    let mut options = StrokeOptions::default().with_line_width(width);
    options.line_join = lyon::tessellation::LineJoin::Round;
    options.start_cap = lyon::tessellation::LineCap::Round;
    options.end_cap = lyon::tessellation::LineCap::Round;
    PathBuilder::stroke(px(width)).with_style(PathStyle::Stroke(options))
}

fn paint_polyline(
    window: &mut Window,
    points: &[[f32; 2]],
    width: f32,
    color: Hsla,
    o: Point<Pixels>,
) {
    if points.len() < 2 {
        if let Some(&p) = points.first() {
            paint_dot(window, p, width / 2.0, color, o);
        }
        return;
    }
    let mut builder = round_stroke(width);
    builder.move_to(pt(points[0], o));
    for &p in &points[1..] {
        builder.line_to(pt(p, o));
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn paint_dot(window: &mut Window, p: [f32; 2], radius: f32, color: Hsla, o: Point<Pixels>) {
    window.paint_quad(gpui::quad(
        rect_bounds(
            [p[0] - radius, p[1] - radius, radius * 2.0, radius * 2.0],
            o,
        ),
        Corners::all(px(radius)),
        color,
        Edges::default(),
        gpui::transparent_black(),
        gpui::BorderStyle::Solid,
    ));
}

fn paint_scene(scene: &Scene, bounds: Bounds<Pixels>, window: &mut Window) {
    let o = bounds.origin;
    let [sw, sh] = scene.screen;
    let intro = 1.0 - (1.0 - (scene.now_sec / 0.24).clamp(0.0, 1.0)).powi(3);
    let intro_fade = 1.0 - intro;
    let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;

    if let Some((rect, kind)) = scene.target {
        let device = kind > 2.5;
        let window_mode = kind > 1.5 && !device;
        let radius = if device {
            rect[2].min(rect[3]) * 0.135
        } else if window_mode {
            10.0
        } else {
            0.0
        };
        // Outside the target: 0.32 dim while picking, easing to 0.22 while recording.
        let dim = mix(0.32, 0.22, intro) * scene.picker_fade;
        let mut hole = PathBuilder::fill();
        add_rounded_rect(&mut hole, [0.0, 0.0, sw, sh], 0.0, o);
        add_rounded_rect(&mut hole, rect, radius, o);
        if let Ok(path) = hole.build() {
            window.paint_path(path, rgba(0.0, 0.0, 0.0, dim));
        }
        // Inside a picked window: the picker's blue tint, dissolving as recording starts.
        if window_mode && intro_fade > 0.002 {
            window.paint_quad(gpui::quad(
                rect_bounds(rect, o),
                Corners::all(px(radius)),
                rgba(0.231, 0.361, 0.965, 0.13 * intro_fade * scene.picker_fade),
                Edges::default(),
                gpui::transparent_black(),
                gpui::BorderStyle::Solid,
            ));
        }
        if !device {
            let width = if window_mode {
                mix(2.0, 1.0, intro)
            } else {
                1.0
            };
            let picker_border = if window_mode {
                [0.357, 0.486, 0.980]
            } else {
                [1.0, 1.0, 1.0]
            };
            let alpha = mix(if window_mode { 0.93 } else { 0.92 }, 0.28, intro) * scene.picker_fade;
            let c = [
                mix(picker_border[0], 1.0, intro),
                mix(picker_border[1], 1.0, intro),
                mix(picker_border[2], 1.0, intro),
            ];
            window.paint_quad(gpui::quad(
                rect_bounds(rect, o),
                Corners::all(px(radius)),
                gpui::transparent_black(),
                Edges::all(px(width)),
                rgba(c[0], c[1], c[2], alpha),
                gpui::BorderStyle::Solid,
            ));
        }
    } else if let Some(fade) = scene.uniform_dim {
        window.paint_quad(gpui::fill(bounds, rgba(0.0, 0.0, 0.0, 0.32 * fade)));
    } else if scene.glow {
        // A shortcut recording: a red glow along the display's edges, in by 0.12 s, gone by 1.1 s.
        let t = scene.now_sec;
        let smooth = |a: f32, b: f32, x: f32| {
            let s = ((x - a) / (b - a)).clamp(0.0, 1.0);
            s * s * (3.0 - 2.0 * s)
        };
        let env = smooth(0.0, 0.12, t) * (1.0 - smooth(0.35, 1.1, t));
        if env > 0.002 {
            for step in 0..24 {
                let d = step as f32 * 3.0;
                let a = ((-d / 22.0).exp() - (-(d + 3.0) / 22.0).exp()) * 0.45 * env;
                window.paint_quad(gpui::quad(
                    rect_bounds([d, d, sw - 2.0 * d, sh - 2.0 * d], o),
                    Corners::default(),
                    gpui::transparent_black(),
                    Edges::all(px(3.0)),
                    ink_color(a.max(0.0) * 1.8),
                    gpui::BorderStyle::Solid,
                ));
            }
        }
    } else if intro_fade > 0.002 {
        window.paint_quad(gpui::fill(bounds, rgba(0.0, 0.0, 0.0, 0.32 * intro_fade)));
    }

    for pin in &scene.pins {
        paint_dot(window, *pin, 11.5 + 1.4, rgba(1.0, 1.0, 1.0, 0.82), o);
        paint_dot(window, *pin, 11.5, ink_color(1.0), o);
    }
    for (points, alpha) in &scene.ink {
        paint_polyline(window, points, 4.5, ink_color(*alpha), o);
    }
    for points in &scene.laser {
        for pair in points.windows(2) {
            let w = (pair[0].1 + pair[1].1) / 2.0;
            if w <= 0.002 {
                continue;
            }
            let glow = rgba(1.0, 0.16, 0.19, 0.18 * w);
            paint_polyline(
                window,
                &[pair[0].0, pair[1].0],
                12.0 * (0.28 + 0.72 * w),
                glow,
                o,
            );
            paint_polyline(
                window,
                &[pair[0].0, pair[1].0],
                4.9 * w.powf(0.68),
                rgba(1.0, 0.16, 0.19, 0.88 * w.powf(0.4)),
                o,
            );
            paint_polyline(
                window,
                &[pair[0].0, pair[1].0],
                1.8 * w.powf(0.82),
                rgba(1.0, 0.97, 0.97, 0.95 * w.powf(0.72)),
                o,
            );
        }
    }
    if let Some(tip) = scene.tip {
        paint_dot(window, tip, 2.0, ink_color(1.0), o);
    }
}

impl Render for OverlayView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.drawn.store(clock_ms(), Ordering::Release);
        let size = window.viewport_size();
        let scene = self.scene([f32::from(size.width), f32::from(size.height)]);
        // Telemetry changes on other threads; follow it every frame while the overlay is up.
        window.request_animation_frame();
        canvas(
            |_, _, _| {},
            move |bounds, (), window: &mut Window, _: &mut App| {
                if let Some(scene) = scene {
                    paint_scene(&scene, bounds, window);
                }
            },
        )
        .size_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_is_grown_by_its_frame_as_it_is_now() {
        let rect = |left, top, right, bottom| RECT {
            left,
            top,
            right,
            bottom,
        };
        // Asked for 1920×1080 at the origin, then resized by a DPI change to 2880×1620, with a
        // 1 px frame left, right and bottom and 2 px on top.
        let grown = grown_by_frame(
            [0, 0, 1920, 1080],
            rect(0, 0, 2880, 1620),
            POINT { x: 1, y: 2 },
            rect(0, 0, 2878, 1617),
        );
        assert_eq!(grown, [-1, -2, 1922, 1083]);
        // No frame: the target itself.
        let none = grown_by_frame(
            [-1920, 0, 400, 100],
            rect(-1920, 0, -1520, 100),
            POINT { x: -1920, y: 0 },
            rect(0, 0, 400, 100),
        );
        assert_eq!(none, [-1920, 0, 400, 100]);
    }

    /// Collin's desk: a 1080p primary and a 1440p display to its right, both at 100 %.
    fn desk() -> Vec<Monitor> {
        let monitor = |handle, rect: [f64; 4], primary| Monitor {
            handle,
            rect,
            work: [rect[0], rect[1], rect[2], rect[3] - 40.0],
            scale: 1.0,
            primary,
        };
        vec![
            monitor(1, [0.0, 0.0, 1920.0, 1080.0], true),
            monitor(2, [1920.0, 0.0, 2560.0, 1440.0], false),
        ]
    }

    const LEFT: [f64; 4] = [0.0, 0.0, 1920.0, 1080.0];
    const RIGHT: [f64; 4] = [1920.0, 0.0, 2560.0, 1440.0];

    #[test]
    fn the_overlay_covers_the_display_holding_the_targets_centre() {
        let desk = desk();
        let on = |rect| display_for(&desk, rect).map(|m| m.rect);
        assert_eq!(on(RIGHT), Some(RIGHT));
        // The window Collin recorded.
        assert_eq!(on([2713.0, 151.0, 892.0, 739.0]), Some(RIGHT));
        assert_eq!(on([100.0, 100.0, 400.0, 300.0]), Some(LEFT));
        // Straddling both: its centre decides.
        assert_eq!(on([1500.0, 100.0, 1000.0, 300.0]), Some(RIGHT));
        assert_eq!(on([1000.0, 100.0, 1000.0, 300.0]), Some(LEFT));
        // Below the shorter display: the nearest one.
        assert_eq!(on([100.0, 1200.0, 200.0, 100.0]), Some(LEFT));
        assert_eq!(on([-900.0, 100.0, 200.0, 100.0]), Some(LEFT));
        assert_eq!(display_for(&[], RIGHT).map(|m| m.rect), None);
    }

    #[test]
    fn a_retarget_on_the_same_display_keeps_the_overlay_where_it_is() {
        let desk = desk();
        // Collin's run: a Display recording of the right display, Window opens the picker there,
        // then a Region of a window on it. The overlay never moves.
        let mut covers = display_for(&desk, RIGHT).unwrap().rect;
        assert_eq!(covers, RIGHT);
        for rect in [RIGHT, [2713.0, 151.0, 892.0, 739.0]] {
            assert!(next_display(&desk, covers, rect).is_none());
        }
        // The picker followed the pointer to the left display, and the window picked is on the
        // right one: it moves there and back.
        let to_left = next_display(&desk, covers, LEFT).unwrap();
        assert_eq!(to_left.rect, LEFT);
        covers = to_left.rect;
        let back = next_display(&desk, covers, [2713.0, 151.0, 892.0, 739.0]).unwrap();
        assert_eq!(back.rect, RIGHT);
    }

    fn healthy() -> WindowState {
        WindowState {
            visible: true,
            minimized: false,
            topmost: true,
            client: [1920, 0, 2560, 1440],
        }
    }

    #[test]
    fn faults_name_whatever_keeps_the_overlay_off_its_display() {
        assert!(faults(&healthy(), RIGHT).is_empty());
        // A pixel of rounding is fine.
        let nudged = WindowState {
            client: [1921, 0, 2559, 1440],
            ..healthy()
        };
        assert!(faults(&nudged, RIGHT).is_empty());
        let lost = WindowState {
            visible: false,
            minimized: true,
            topmost: false,
            client: [0, 0, 1920, 1080],
        };
        assert_eq!(
            faults(&lost, RIGHT),
            vec![
                Fault::Hidden,
                Fault::Minimized,
                Fault::NotTopmost,
                Fault::Misplaced
            ]
        );
        assert_eq!(faults(&healthy(), LEFT), vec![Fault::Misplaced]);
    }

    fn look(now: u64, drawn: u64) -> Look {
        Look {
            now,
            drawn,
            state: Some(healthy()),
            display: RIGHT,
            covered_by: None,
            shift: false,
            pointer_on_display: true,
        }
    }

    #[test]
    fn the_watchdog_logs_the_window_once_per_change() {
        let mut watch = Watch::default();
        let first = watch.look(&look(1_000, 990));
        assert_eq!(first.logs.len(), 1, "{:?}", first.logs);
        assert!(first.logs[0].contains("visible=true topmost=true"));
        assert!(watch.look(&look(1_100, 1_090)).logs.is_empty());
        let moved = Look {
            state: Some(WindowState {
                client: [1920, 0, 2560, 1400],
                ..healthy()
            }),
            ..look(1_200, 1_190)
        };
        let repair = watch.look(&moved);
        assert_eq!(repair.logs.len(), 2, "{:?}", repair.logs);
        assert!(repair.logs[1].contains("Misplaced"));
        assert!(repair.reassert);
    }

    #[test]
    fn a_stalled_overlay_is_redrawn_and_its_recovery_logged() {
        let mut watch = Watch::default();
        watch.look(&look(1_000, 990));
        // 600 ms without a frame: logged once, redrawn and put back.
        let stalled = watch.look(&look(1_700, 1_000));
        assert!(stalled.redraw && stalled.reassert);
        assert_eq!(stalled.logs.len(), 1);
        assert!(stalled.logs[0].contains("stopped drawing 700 ms ago"));
        // Still stalled: quiet, and retried once a second.
        let quiet = watch.look(&look(1_800, 1_000));
        assert_eq!(quiet, Repair::default());
        let retry = watch.look(&look(2_700, 1_000));
        assert!(retry.redraw && retry.reassert && retry.logs.is_empty());
        let back = watch.look(&look(2_800, 2_790));
        assert_eq!(back.logs, vec!["ink overlay draws again after 1790 ms".to_owned()]);
        assert!(!back.redraw);
    }

    #[test]
    fn the_overlay_is_put_back_when_it_loses_its_place() {
        let mut watch = Watch::default();
        watch.look(&look(1_000, 990));
        let demoted = Look {
            state: Some(WindowState {
                topmost: false,
                ..healthy()
            }),
            ..look(1_100, 1_090)
        };
        let repair = watch.look(&demoted);
        assert!(repair.reassert);
        assert!(repair.logs.iter().any(|l| l.contains("NotTopmost")));
        // Same fault a moment later: no new log, no new repair until the retry is due.
        let again = Look {
            now: 1_200,
            drawn: 1_190,
            ..demoted
        };
        assert_eq!(watch.look(&again), Repair::default());
        let due = Look {
            now: 2_100,
            drawn: 2_090,
            ..again
        };
        let retry = watch.look(&due);
        assert!(retry.reassert && retry.logs.is_empty());
    }

    #[test]
    fn a_covering_window_is_named_once_and_the_overlay_raised_each_time() {
        let mut watch = Watch::default();
        watch.look(&look(1_000, 990));
        let covered = |now| Look {
            covered_by: Some("Chrome_WidgetWin_1 (pid 42)".into()),
            ..look(now, now - 10)
        };
        let first = watch.look(&covered(1_100));
        assert!(first.reassert);
        assert_eq!(first.logs.len(), 1);
        assert!(first.logs[0].contains("covered by Chrome_WidgetWin_1 (pid 42)"));
        assert!(!watch.look(&look(1_200, 1_190)).reassert);
        let second = watch.look(&covered(1_300));
        assert!(second.reassert && second.logs.is_empty());
    }

    #[test]
    fn shift_ink_and_the_pointer_coming_back_put_the_overlay_on_top() {
        let mut watch = Watch::default();
        watch.look(&look(1_000, 990));
        assert!(!watch.look(&look(1_100, 1_090)).reassert);
        let shift = |now| Look {
            shift: true,
            ..look(now, now - 10)
        };
        assert!(watch.look(&shift(1_200)).reassert);
        assert!(!watch.look(&shift(1_300)).reassert);
        let away = Look {
            pointer_on_display: false,
            ..look(1_400, 1_390)
        };
        assert!(!watch.look(&away).reassert);
        let back = watch.look(&look(1_500, 1_490));
        assert!(back.reassert && back.logs.is_empty());
    }

    /// A popup with the overlay's styles, fully transparent (alpha 0), so nothing shows on
    /// screen and every click falls through.
    fn ghost_window(name: &str) -> HWND {
        use windows::Win32::Foundation::LRESULT;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, RegisterClassW, WNDCLASSW, WS_POPUP,
        };
        unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
            unsafe { DefWindowProcW(h, m, w, l) }
        }
        let class = w!("ScrawlOverlayTest");
        let instance = unsafe { GetModuleHandleW(None) }.unwrap();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        unsafe { RegisterClassW(&wc) };
        let title: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let ex = WS_EX_TRANSPARENT
            | WS_EX_LAYERED
            | WS_EX_TOOLWINDOW
            | WS_EX_NOACTIVATE
            | WS_EX_TOPMOST;
        let hwnd = unsafe {
            CreateWindowExW(
                ex,
                class,
                windows::core::PCWSTR(title.as_ptr()),
                WS_POPUP,
                0,
                0,
                100,
                100,
                None,
                None,
                Some(instance.into()),
                None,
            )
        }
        .unwrap();
        unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA) }.unwrap();
        hwnd
    }

    /// Opens two invisible, click-through, never-activating windows for about a second: the
    /// overlay goes to the second display and back, is hidden, minimized, demoted from the top
    /// band and covered, and `place` puts it back each time. Needs two displays.
    /// `cargo test overlay_window -- --ignored --nocapture`
    #[test]
    #[ignore = "opens windows on the desktop"]
    fn overlay_window_is_put_back_on_its_display() {
        use windows::Win32::UI::WindowsAndMessaging::{
            DestroyWindow, HWND_NOTOPMOST, SW_HIDE, SW_SHOWMINNOACTIVE, SWP_NOMOVE, SWP_NOSIZE,
            SWP_SHOWWINDOW,
        };
        let monitors = super::super::monitors();
        let Some(primary) = monitors.iter().find(|m| m.primary).cloned() else {
            panic!("no primary display");
        };
        let Some(other) = monitors.iter().find(|m| !m.primary).cloned() else {
            eprintln!("one display only: nothing to move across");
            return;
        };
        let overlay = ghost_window("overlay");
        let id = overlay.0 as isize;
        let check = |step: &str, display: [f64; 4]| {
            let state = window_state(id).expect("the window is gone");
            println!("{step}: {state}");
            assert_eq!(faults(&state, display), vec![], "{step}");
        };
        place(id, other.rect);
        check("placed on the other display", other.rect);
        place(id, primary.rect);
        check("moved to the primary", primary.rect);
        place(id, other.rect);
        check("moved back", other.rect);

        let _ = unsafe { ShowWindow(overlay, SW_HIDE) };
        let hidden = window_state(id).unwrap();
        assert!(faults(&hidden, other.rect).contains(&Fault::Hidden));
        place(id, other.rect);
        check("shown again", other.rect);

        let _ = unsafe { ShowWindow(overlay, SW_SHOWMINNOACTIVE) };
        let minimized = window_state(id).unwrap();
        println!("minimized: {minimized}");
        assert!(faults(&minimized, other.rect).contains(&Fault::Minimized));
        place(id, other.rect);
        check("restored", other.rect);

        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
        unsafe { SetWindowPos(overlay, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags) }.unwrap();
        let demoted = window_state(id).unwrap();
        assert!(faults(&demoted, other.rect).contains(&Fault::NotTopmost));
        place(id, other.rect);
        check("back on top", other.rect);

        // Another topmost window over the middle of the display, raised above the overlay.
        let [x, y, w, h] = other.rect;
        let area = [x + w / 4.0, y + h / 4.0, w / 2.0, h / 2.0];
        let cover = ghost_window("cover");
        let [cx, cy, cw, ch] = area.map(|v| v as i32);
        unsafe {
            SetWindowPos(
                cover,
                Some(HWND_TOPMOST),
                cx,
                cy,
                cw,
                ch,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        }
        .unwrap();
        let by = covering_window(id, area, 0);
        println!("covered by: {by:?}");
        assert!(by.is_some_and(|by| by.starts_with("ScrawlOverlayTest")));
        assert_eq!(covering_window(id, area, std::process::id()), None);
        place(id, other.rect);
        assert_eq!(covering_window(id, area, 0), None, "raised above the cover");
        check("raised above the cover", other.rect);

        unsafe {
            let _ = DestroyWindow(cover);
            let _ = DestroyWindow(overlay);
        }
    }
}
