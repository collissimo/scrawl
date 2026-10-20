//! Terminal output captured from real Claude Code sessions (`SCRAWL_CLAUDE_TRACE`, cut at the
//! moment each state was on screen by `cut_fixtures`).

/// ESC `[`, which starts a control sequence.
const CSI: &[u8] = &[0x1b, b'['];

pub fn get(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/win/claude_dictation/fixtures")
        .join(format!("{name}.vt"));
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// `template` (the captured "No speech detected" screen) showing `message` instead, right-aligned
/// in the footer as Claude Code places its messages there (the cursor move before it is redone).
pub fn with_error(template: &[u8], message: &str) -> Vec<u8> {
    let needle = b"No speech detected";
    let at = template
        .windows(needle.len())
        .rposition(|window| window == needle)
        .expect("the template shows No speech detected");
    let moved = template[..at]
        .windows(CSI.len())
        .rposition(|window| window == CSI)
        .expect("a cursor move before the message");
    let row = std::str::from_utf8(&template[moved + CSI.len()..at])
        .ok()
        .and_then(|cup| cup.strip_suffix('H')?.split(';').next()?.parse::<u16>().ok())
        .expect("the move is CSI row;col H");
    let column = super::screen::COLS as usize - message.chars().count();
    let mut bytes = template[..moved].to_vec();
    bytes.extend_from_slice(CSI);
    bytes.extend_from_slice(format!("{row};{column}H{message}").as_bytes());
    bytes.extend_from_slice(&template[at + needle.len()..]);
    bytes
}

/// Replays `SCRAWL_CLAUDE_TRACE_IN` and writes each state's fixture, cut where it first showed:
/// `SCRAWL_CLAUDE_TRACE_IN=trace.vt cargo test cut_fixtures -- --ignored --nocapture`.
#[test]
#[ignore]
fn cut_fixtures() {
    use super::screen::{COLS, ROWS, State, Trust, rows};
    let trace = std::fs::read(std::env::var("SCRAWL_CLAUDE_TRACE_IN").unwrap()).unwrap();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/win/claude_dictation/fixtures");
    std::fs::create_dir_all(&dir).unwrap();
    let mut parser = vt100::Parser::new(ROWS, COLS, 0);
    let mut seen = std::collections::BTreeSet::new();
    let mut last = String::new();
    let mut at = 0;
    while at < trace.len() {
        let end = (at + 16).min(trace.len());
        parser.process(&trace[at..end]);
        at = end;
        let state = State::of(&rows(&parser));
        let summary = state.summary();
        if summary != last {
            println!("{at:>7}: {summary}");
            last = summary;
        }
        let ready = state.prompt && !state.busy() && state.trust.is_none() && state.error.is_none();
        let names = [
            (state.trust == Some(Trust::NotSelected), "trust"),
            (state.trust == Some(Trust::Selected), "trust-selected"),
            (ready, "ready"),
            (state.listening, "listening"),
            (state.processing, "processing"),
            (state.error.is_some() && !state.busy(), "no-speech"),
        ];
        for (on, name) in names {
            if on && seen.insert(name) {
                std::fs::write(dir.join(format!("{name}.vt")), &trace[..at]).unwrap();
                println!("         wrote {name}.vt");
            }
        }
    }
    println!("--- final screen ---\n{}", rows(&parser).join("\n"));
}

/// `template` (a captured screen with the prompt box on rows 9-11) after Claude Code redrew the
/// prompt row with `text` and blanked the footer's dictation status, as when dictation is done.
pub fn with_prompt(template: &[u8], text: &str) -> Vec<u8> {
    let mut bytes = template.to_vec();
    bytes.extend_from_slice(format!("\x1b[10;3H\x1b[K{text}\x1b[12;100H\x1b[K").as_bytes());
    bytes
}

