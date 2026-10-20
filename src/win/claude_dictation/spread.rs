//! Claude Code hands back plain text without times. The words are spread over the recording's
//! speech islands, as Hex's are (`hex::spread_words`), so keyframes still land near what was
//! said; the times are estimates.

use anyhow::{Result, bail};

use crate::model::TranscriptData;

/// Speech before Claude Code began listening that it missed. More than this and the recording is
/// transcribed from the file instead, which heard all of it.
pub const MISSED_SPEECH_LIMIT_MS: u64 = 500;

/// A transcript of `text`, heard from `heard_from_ms` to the end of a `duration_ms` recording
/// whose speech islands (`audio::speech_islands`) are `islands`.
pub fn transcript(
    text: &str,
    islands: &[(u64, u64)],
    heard_from_ms: u64,
    duration_ms: u64,
    language: &str,
) -> Result<TranscriptData> {
    let clips = crate::hex::plan_clips(islands, duration_ms);
    let missed: u64 = clips
        .iter()
        .map(|&(start, end)| end.min(heard_from_ms).saturating_sub(start))
        .sum();
    if missed > MISSED_SPEECH_LIMIT_MS {
        bail!(
            "Claude Code began listening {heard_from_ms} ms into the recording and missed {missed} ms of speech"
        );
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        bail!("Claude Code returned no text");
    }
    let mut spans: Vec<(u64, u64)> = clips
        .iter()
        .map(|&(start, end)| (start.max(heard_from_ms), end))
        .filter(|(start, end)| end > start)
        .collect();
    if spans.is_empty() {
        spans.push((heard_from_ms.min(duration_ms), duration_ms));
    }
    let mut transcript = TranscriptData {
        source: "claude-code".into(),
        model: Some(format!("Claude Code dictation ({language})")),
        estimated: true,
        text: text.clone(),
        ..Default::default()
    };
    for ((start, end), words) in spans.iter().zip(share(&text, &spans)) {
        if words.is_empty() {
            continue;
        }
        let (segments, timings) = crate::hex::spread_words(&words.join(" "), *start, *end);
        transcript.segments.extend(segments);
        transcript.words.extend(timings);
    }
    Ok(transcript)
}

/// Deals the words out to `spans` in order: each word goes to the span its middle character
/// falls in, with the text's length laid over the spans' combined length.
pub(super) fn share<'a>(text: &'a str, spans: &[(u64, u64)]) -> Vec<Vec<&'a str>> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let weight = |word: &str| word.chars().count().max(1) as u128;
    let total: u128 = words.iter().map(|w| weight(w)).sum();
    let lengths: Vec<u128> = spans.iter().map(|(s, e)| u128::from(e - s)).collect();
    let span_total: u128 = lengths.iter().sum::<u128>().max(1);
    let mut out = vec![Vec::new(); spans.len()];
    let mut before: u128 = 0;
    let mut span = 0;
    let mut span_end = lengths.first().copied().unwrap_or(0);
    for word in words {
        // Midpoint of the word, in span time: (before + w/2) / total * span_total.
        let middle = (2 * before + weight(word)) * span_total / (2 * total.max(1));
        while middle >= span_end && span + 1 < spans.len() {
            span += 1;
            span_end += lengths[span];
        }
        out[span].push(word);
        before += weight(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_go_to_the_islands_in_proportion_to_their_length() {
        let islands = [(1_000, 3_000), (5_000, 7_000)];
        let t = transcript("eins zwei drei vier", &islands, 0, 8_000, "de").unwrap();
        assert_eq!(t.source, "claude-code");
        assert!(t.estimated);
        assert_eq!(t.model.as_deref(), Some("Claude Code dictation (de)"));
        assert_eq!(t.text, "eins zwei drei vier");
        let times: Vec<_> = t.words.iter().map(|w| (w.text.as_str(), w.t0, w.t1)).collect();
        assert_eq!(times[0], ("eins", 1_000, 2_000));
        assert_eq!(times[1].0, "zwei");
        assert_eq!(times[1].2, 3_000);
        assert_eq!(times[2], ("drei", 5_000, 6_000));
        assert_eq!(times[3].2, 7_000);
        assert_eq!(t.segments.len(), 2, "one segment per island");
    }

    #[test]
    fn text_without_islands_covers_what_claude_heard() {
        let t = transcript("  Hallo   Welt ", &[], 400, 2_400, "de").unwrap();
        assert_eq!(t.text, "Hallo Welt");
        assert_eq!(t.words.first().map(|w| w.t0), Some(400));
        assert_eq!(t.words.last().map(|w| w.t1), Some(2_400));
    }

    #[test]
    fn speech_claude_missed_at_the_start_means_the_file_transcribes_it() {
        let islands = [(200, 2_000), (3_000, 4_000)];
        let late = transcript("Hallo", &islands, 2_600, 5_000, "de");
        assert!(late.unwrap_err().to_string().contains("missed 1800 ms"));
        // A short miss is fine: the words go where Claude listened.
        let t = transcript("Hallo Welt", &islands, 500, 5_000, "de").unwrap();
        assert_eq!(t.words.first().map(|w| w.t0), Some(500));
        assert!(transcript("", &islands, 0, 5_000, "de").is_err());
    }

    #[test]
    fn every_word_lands_somewhere_and_in_order() {
        let spans = [(0, 100), (200, 210), (300, 1_300)];
        let shared = share("a bb ccc dddd eeeee ffffff", &spans);
        let flat: Vec<_> = shared.iter().flatten().copied().collect();
        assert_eq!(flat, ["a", "bb", "ccc", "dddd", "eeeee", "ffffff"]);
        assert!(shared[2].len() > shared[0].len());
    }
}
