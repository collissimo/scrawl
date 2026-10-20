//! Claude Code dictation: one warm hidden `claude` in a ConPTY, driven like T3 Voice (§7).
//! Stub until P8.

/// `scrawl claude-editor <file>`: the `EDITOR` Claude Code opens under Ctrl+G.
pub fn run_editor(args: &[std::ffi::OsString]) -> anyhow::Result<()> {
    let _ = args;
    anyhow::bail!("Claude Code dictation isn't available on Windows yet")
}

/// `scrawl dictation-selftest`.
pub fn selftest() -> anyhow::Result<()> {
    anyhow::bail!("Claude Code dictation isn't available on Windows yet")
}
