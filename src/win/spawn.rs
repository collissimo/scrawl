//! Spawning children without console windows, and handing paths and URLs to the shell.

use std::os::windows::process::CommandExt;

use win32::Win32::UI::Shell::ShellExecuteW;
use win32::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use win32::core::{HSTRING, PCWSTR, w};

pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Every child the app spawns runs without a console window (§3 rule 7).
pub fn no_window(cmd: &mut std::process::Command) -> &mut std::process::Command {
    cmd.creation_flags(CREATE_NO_WINDOW)
}

/// Opens a URL, folder, file or `ms-settings:` page with its default handler.
pub fn shell_open(target: &str) -> std::io::Result<()> {
    let file = HSTRING::from(target);
    // SAFETY: both strings outlive the call; no window handle is passed.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &file,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns a value greater than 32 on success.
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "ShellExecuteW(open, {target}) failed with {}",
            result.0 as isize
        )))
    }
}

/// Opens Explorer with `path` selected. Explorer exits with 1 even when it worked.
pub fn reveal_in_explorer(path: &std::path::Path) -> std::io::Result<()> {
    let mut arg = std::ffi::OsString::from("/select,\"");
    arg.push(path.as_os_str());
    arg.push("\"");
    let mut cmd = std::process::Command::new("explorer.exe");
    // `/select,"<path>"` must reach Explorer as one argument, quoted only around the path.
    cmd.raw_arg(&arg);
    cmd.spawn().map(|_| ())
}
