//! Shortcut labels for people on Windows. Labels are stored in glyph form (`⌥⇧S`, D8) so the
//! shared parsing keeps working; this renders them the Windows way (`Alt+Shift+S`).

/// `⌥⇧S` → `Alt+Shift+S`, modifiers in the order Ctrl, Alt, Shift, Win (⌘ is the Win key,
/// D9). Arrow and editing-key glyphs get their Windows names; anything else passes through.
pub fn display_label(label: &str) -> String {
    let (mods, key) = crate::capture::shortcut_parts(label);
    let mut parts: Vec<&str> = Vec::new();
    for (glyph, name) in [('⌃', "Ctrl"), ('⌥', "Alt"), ('⇧', "Shift"), ('⌘', "Win")] {
        if mods.contains(glyph) {
            parts.push(name);
        }
    }
    let mut out = parts.join("+");
    let key = key_name(key);
    if !key.is_empty() {
        if !out.is_empty() {
            out.push('+');
        }
        out.push_str(key);
    }
    out
}

fn key_name(key: &str) -> &str {
    match key {
        "↩" | "⏎" => "Enter",
        "⇥" => "Tab",
        "⌫" => "Backspace",
        "⌦" => "Delete",
        "⎋" => "Esc",
        "←" => "Left",
        "→" => "Right",
        "↑" => "Up",
        "↓" => "Down",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::display_label;

    #[test]
    fn glyph_labels_read_the_windows_way() {
        assert_eq!(display_label("⌥⇧S"), "Alt+Shift+S");
        assert_eq!(display_label("⌥⇧V"), "Alt+Shift+V");
        assert_eq!(display_label("⇧⌘⌃⌥K"), "Ctrl+Alt+Shift+Win+K");
        assert_eq!(display_label("⌃↩"), "Ctrl+Enter");
        assert_eq!(display_label("F5"), "F5");
        assert_eq!(display_label("⌥Space"), "Alt+Space");
        assert_eq!(display_label(""), "");
    }
}
