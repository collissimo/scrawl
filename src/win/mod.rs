//! Windows platform layer. Every `crate::win::*` module is declared here; the files that
//! replace a Mac module through `#[cfg_attr(windows, path = "win/<m>.rs")]` in `main.rs`
//! (capture, helper, transcribe, …) are not, see docs/windows-port.md §3.

pub mod claude_dictation;
pub mod clipboard;
pub mod com;
pub mod conpty;
pub mod keys;
pub mod layered;
pub mod netproc;
pub mod process;
pub mod screens;
pub mod spawn;
pub mod text;
pub mod uia;
pub mod ui_host;
pub mod whisper;
pub mod winlist;

use win32::Win32::System::Console::GetConsoleProcessList;
use win32::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

/// Process-wide setup, the first statement of `main()`: Per-Monitor-V2 DPI awareness, so
/// every OS coordinate Scrawl reads is in physical pixels (D3, D11). A second call (or a
/// manifest that already set it) fails with "access denied", which is fine.
pub fn init_process() {
    // SAFETY: no preconditions; failure only means the awareness was already set.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Whether a bare `scrawl` was double-clicked in Explorer rather than typed in a terminal
/// (D10): the parent process is `explorer.exe`. Only when the parent is gone does a console
/// shared with no other process count, because Git Bash/mintty children also see one.
pub fn launched_from_explorer() -> bool {
    let own = process::own_pid();
    match process::parent_pid(own) {
        Some(parent) => match process::exe_path(parent) {
            Some(path) => path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("explorer.exe")),
            None => console_is_ours_alone(),
        },
        None => console_is_ours_alone(),
    }
}

fn console_is_ours_alone() -> bool {
    let mut pids = [0u32; 4];
    // SAFETY: the buffer is valid for its length.
    let count = unsafe { GetConsoleProcessList(&mut pids) };
    count == 1
}
