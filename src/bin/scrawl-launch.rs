//! `scrawl-launch`: a tiny GUI-subsystem launcher (D10). The Run key and the
//! Start-menu shortcut point here so autostart never flashes a console: it
//! starts `scrawl.exe app` from its own directory without a console window and
//! exits. Standard library only.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let exe_name = if cfg!(windows) { "scrawl.exe" } else { "scrawl" };
    let Some(scrawl) = std::env::current_exe()
        .ok()
        .and_then(|me| me.parent().map(|dir| dir.join(exe_name)))
    else {
        return ExitCode::FAILURE;
    };
    let mut cmd = Command::new(scrawl);
    cmd.arg("app");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.spawn() {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
