//! The Settings and onboarding windows as a GPUI UI-host process (`scrawl.exe settings …`,
//! `scrawl.exe onboarding …`), speaking the Swift helper's stdin/stdout JSON protocol (D12).
//! Stub until P5.

use std::path::PathBuf;

/// `scrawl settings [--pane P] [--icon F]`.
pub fn run_settings(pane: Option<String>, icon: Option<PathBuf>) -> anyhow::Result<()> {
    let _ = (pane, icon);
    anyhow::bail!("the Settings window isn't available on Windows yet")
}

/// `scrawl onboarding --shortcut S --paste P [--icon F]`.
pub fn run_onboarding(shortcut: String, paste: String, icon: Option<PathBuf>) -> anyhow::Result<()> {
    let _ = (shortcut, paste, icon);
    anyhow::bail!("onboarding isn't available on Windows yet")
}
