//! Reading Claude Code's terminal screen: is it listening, processing, asking to trust the
//! folder, showing a dictation error, or waiting at its prompt. Pure functions over the screen's
//! rows, so the tests run on captured terminal output without starting `claude`.

use std::time::{Duration, Instant};

pub const ROWS: u16 = 40;
pub const COLS: u16 = 160;

pub const STATUS_WARMUP: &str = "keep holding";
pub const STATUS_LISTENING: &str = "listening";
pub const STATUS_PROCESSING: &str = "Voice: processing";
pub const TRUST_PROMPT: &str = "Yes, I trust this folder";
/// The CLI falls back to a plain `>` when it runs with `NO_COLOR` set.
pub const PROMPT_MARKS: [&str; 2] = ["❯", ">"];
const LEVEL_BARS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// What the CLI prints when dictation fails, as documented for `/voice`.
pub const VOICE_ERRORS: [&str; 8] = [
    "No speech detected",
    "No audio detected from microphone",
    "Voice connection failed",
    "Voice stream error",
    "Microphone access is denied",
    "Voice mode requires",
    "Voice mode is disabled",
    "Voice input is failing repeatedly",
];
/// How long the screen must hold still after dictation before the text is final.
pub const SETTLE: Duration = Duration::from_millis(600);
/// The same once the footer shows a dictation error ("No speech detected"): no text follows.
pub const SETTLE_ON_ERROR: Duration = Duration::from_millis(150);

/// The screen's rows as text, trailing blanks trimmed.
pub fn rows(parser: &vt100::Parser) -> Vec<String> {
    parser
        .screen()
        .rows(0, COLS)
        .map(|row| row.trim_end().to_owned())
        .collect()
}

/// The trust dialog's choice line, if the dialog is open, and whether the selection mark is on
/// "Yes, I trust this folder".
#[derive(Debug, PartialEq, Eq)]
pub enum Trust {
    Selected,
    NotSelected,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct State {
    pub warming: bool,
    pub listening: bool,
    pub processing: bool,
    pub trust: Option<Trust>,
    pub prompt: bool,
    /// A dictation error with the text after it, e.g. "No speech detected".
    pub error: Option<String>,
}

impl State {
    /// Status and errors come only from the footer below the prompt box, and the prompt only
    /// from the box's first row, so dictated words ("listening", "No speech detected") and
    /// Claude Code's own selection dialogs (one rule, numbered `❯ 1.` rows) never count.
    pub fn of(rows: &[String]) -> Self {
        let layout = Layout::of(rows);
        let footer = layout.footer(rows);
        let has = |needle: &str| footer.iter().any(|row| row.contains(needle));
        Self {
            warming: has(STATUS_WARMUP),
            listening: has(STATUS_LISTENING),
            processing: has(STATUS_PROCESSING),
            trust: rows
                .iter()
                .find(|row| row.contains(TRUST_PROMPT))
                .map(|row| {
                    if starts_with_mark(row.trim()) {
                        Trust::Selected
                    } else {
                        Trust::NotSelected
                    }
                }),
            prompt: layout
                .prompt_box(rows)
                .iter()
                .find(|row| !row.trim().is_empty())
                .is_some_and(|row| starts_with_mark(row) && !is_option(row)),
            error: voice_error(footer),
        }
    }

    /// Recording, or turning what it recorded into text.
    pub fn busy(&self) -> bool {
        self.warming || self.listening || self.processing
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for (on, name) in [
            (self.warming, "warming up"),
            (self.listening, "listening"),
            (self.processing, "processing"),
            (self.prompt, "prompt"),
        ] {
            if on {
                parts.push(name.to_owned());
            }
        }
        if let Some(trust) = &self.trust {
            parts.push(format!("trust dialog ({trust:?})"));
        }
        if let Some(error) = &self.error {
            parts.push(format!("error: {error}"));
        }
        if parts.is_empty() {
            "nothing recognised".into()
        } else {
            parts.join(", ")
        }
    }
}

fn starts_with_mark(row: &str) -> bool {
    PROMPT_MARKS.iter().any(|mark| row.starts_with(mark))
}

/// A dialog's numbered choice, e.g. `❯ 1. Dark mode`.
fn is_option(row: &str) -> bool {
    let rest = PROMPT_MARKS
        .iter()
        .find_map(|mark| row.trim().strip_prefix(mark))
        .unwrap_or(row)
        .trim_start();
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && rest[digits..].starts_with('.')
}

/// Where the horizontal rules are: the prompt box sits between the last two, and Claude Code's
/// footer (mode, dictation status, errors) below the last.
struct Layout {
    rules: Vec<usize>,
}

impl Layout {
    fn of(rows: &[String]) -> Self {
        Self {
            rules: rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.chars().count() > 20 && row.trim().chars().all(|c| c == '─'))
                .map(|(i, _)| i)
                .collect(),
        }
    }

    /// The rows below the last rule (none without a rule).
    fn footer<'a>(&self, rows: &'a [String]) -> &'a [String] {
        self.rules.last().map_or(&[], |&last| &rows[last + 1..])
    }

    /// The rows between the last two rules (none without two rules).
    fn prompt_box<'a>(&self, rows: &'a [String]) -> &'a [String] {
        match self.rules[..] {
            [.., above, below] => &rows[above + 1..below],
            _ => &[],
        }
    }
}

/// The first dictation error in `rows` (the footer). It shares its row with unrelated footer
/// text to its left.
pub fn voice_error(rows: &[String]) -> Option<String> {
    rows.iter().find_map(|row| {
        VOICE_ERRORS
            .iter()
            .find_map(|message| row.find(message).map(|at| row[at..].trim().to_owned()))
    })
}

/// The interim text in the prompt box (between the last two horizontal rules), wrapped rows
/// rejoined and the level meter trimmed.
pub fn preview(rows: &[String]) -> String {
    let text = Layout::of(rows)
        .prompt_box(rows)
        .iter()
        .map(|row| row.trim())
        .filter(|row| !row.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut text = text.as_str();
    for mark in PROMPT_MARKS {
        text = text.strip_prefix(mark).unwrap_or(text);
    }
    text.trim().trim_end_matches(LEVEL_BARS).trim().to_owned()
}

/// Waits for the screen to settle: not busy, and unchanged for `SETTLE` (`SETTLE_ON_ERROR`
/// with a dictation error showing).
pub struct Settle {
    last: Option<String>,
    since: Instant,
}

impl Settle {
    pub fn new(now: Instant) -> Self {
        Self {
            last: None,
            since: now,
        }
    }

    /// Feeds the screen as it is at `now`; true once it has settled.
    pub fn observe(&mut self, rows: &[String], now: Instant) -> bool {
        let text = rows.join("\n");
        if self.last.as_deref() != Some(text.as_str()) {
            self.last = Some(text);
            self.since = now;
        }
        let state = State::of(rows);
        let quiet_for = if state.error.is_some() {
            SETTLE_ON_ERROR
        } else {
            SETTLE
        };
        !state.busy() && now.duration_since(self.since) >= quiet_for
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(bytes: &[u8]) -> Vec<String> {
        let mut parser = vt100::Parser::new(ROWS, COLS, 0);
        parser.process(bytes);
        rows(&parser)
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn captured_screens_read_as_what_claude_code_showed() {
        let fixture = |name: &str| super::super::fixtures::get(name);
        let listening = State::of(&screen(&fixture("listening")));
        assert!(listening.listening && listening.busy(), "{listening:?}");
        assert_eq!(listening.error, None);

        let processing = State::of(&screen(&fixture("processing")));
        assert!(processing.processing && processing.busy(), "{processing:?}");

        let trust = State::of(&screen(&fixture("trust")));
        assert_eq!(trust.trust, Some(Trust::NotSelected), "{trust:?}");
        let trusted = State::of(&screen(&fixture("trust-selected")));
        assert_eq!(trusted.trust, Some(Trust::Selected), "{trusted:?}");

        let ready = State::of(&screen(&fixture("ready")));
        assert!(ready.prompt && !ready.busy() && ready.trust.is_none(), "{ready:?}");

        let silent = State::of(&screen(&fixture("no-speech")));
        assert!(!silent.busy(), "{silent:?}");
        assert!(
            silent
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("No speech detected")),
            "{silent:?}"
        );
    }

    #[test]
    fn every_documented_dictation_error_is_recognised_in_the_footer() {
        let template = super::super::fixtures::get("no-speech");
        for message in VOICE_ERRORS {
            let bytes = super::super::fixtures::with_error(&template, message);
            let error = State::of(&screen(&bytes)).error;
            assert!(
                error.as_deref().is_some_and(|e| e.starts_with(message)),
                "{message}: {error:?}"
            );
        }
    }

    #[test]
    fn the_error_is_cut_from_unrelated_footer_text() {
        let rows = lines("  ? for shortcuts      Voice input is failing repeatedly. Try /voice");
        assert_eq!(
            voice_error(&rows).as_deref(),
            Some("Voice input is failing repeatedly. Try /voice")
        );
        assert_eq!(voice_error(&lines("all fine")), None);
    }

    #[test]
    fn the_preview_is_the_prompt_box_without_mark_or_meter() {
        let rule = "─".repeat(60);
        let rows = lines(&format!(
            "Welcome\n{rule}\n❯ Hallo Welt, das ist\n  ein Test ▁▃▅\n{rule}\n  listening"
        ));
        assert_eq!(preview(&rows), "Hallo Welt, das ist ein Test");
        assert_eq!(preview(&lines("no box")), "");
    }

    #[test]
    fn spoken_status_words_in_the_prompt_are_not_the_status() {
        let spoken = "Ich sage listening… und keep holding, dann Voice: processing und No speech detected";
        let bytes = super::super::fixtures::with_prompt(
            &super::super::fixtures::get("processing"),
            spoken,
        );
        let rows = screen(&bytes);
        let state = State::of(&rows);
        assert!(!state.busy(), "{state:?}");
        assert_eq!(state.error, None, "{state:?}");
        assert!(state.prompt, "{state:?}");
        assert_eq!(preview(&rows), spoken);
        // The real footer still counts when it says so.
        let bytes = [bytes, b"\x1b[12;140Hlistening\xe2\x80\xa6".to_vec()].concat();
        assert!(State::of(&screen(&bytes)).listening);
    }

    #[test]
    fn selection_dialogs_are_not_the_prompt() {
        let rule = "─".repeat(60);
        let one_rule = lines(&format!(
            "{rule}\n Select model\n\n ❯ 1. Default (recommended)\n   2. Opus\n\n Enter to confirm · Esc to exit"
        ));
        assert!(!State::of(&one_rule).prompt);
        let boxed = lines(&format!(
            "{rule}\n❯ 1. Dark mode\n  2. Light mode\n{rule}\n  Enter to confirm"
        ));
        assert!(!State::of(&boxed).prompt);
        let trust = State::of(&screen(&super::super::fixtures::get("trust-selected")));
        assert!(!trust.prompt, "{trust:?}");
        let ready = lines(&format!("{rule}\n❯ 1. ist eine Zahl\n{rule}\n  ? for shortcuts"));
        assert!(!State::of(&ready).prompt, "a numbered prompt reads as a dialog: fail safe");
        let ready = lines(&format!("{rule}\n❯ Hallo\n{rule}\n  ? for shortcuts"));
        assert!(State::of(&ready).prompt);
    }

    #[test]
    fn settling_needs_a_quiet_screen_for_the_whole_window() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut settle = Settle::new(start);
        let rule = "─".repeat(60);
        let busy = lines(&format!("{rule}\n❯ text\n{rule}\n  Voice: processing"));
        let done = lines(&format!("{rule}\n❯ Hallo Welt\n{rule}\n  ? for shortcuts"));
        assert!(!settle.observe(&busy, at(0)));
        assert!(!settle.observe(&busy, at(900)), "busy never settles");
        assert!(!settle.observe(&done, at(1_000)));
        assert!(!settle.observe(&done, at(1_500)));
        assert!(settle.observe(&done, at(1_600)));
        let changed = lines(&format!("{rule}\n❯ Hallo Welt!\n{rule}\n  ? for shortcuts"));
        assert!(!settle.observe(&changed, at(1_650)), "a change restarts it");

        let mut settle = Settle::new(start);
        let failed = lines(&format!("{rule}\n❯\n{rule}\n  ? for shortcuts   No speech detected."));
        assert!(!settle.observe(&busy, at(0)));
        assert!(!settle.observe(&failed, at(100)));
        assert!(settle.observe(&failed, at(250)), "an error has no text to wait for");
    }
}
