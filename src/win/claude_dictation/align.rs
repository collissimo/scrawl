//! Claude Code's words with Parakeet's measured times. Parakeet transcribes the same recording
//! alongside Claude Code (`transcribe::claude`); its words are lined up with Claude Code's by a
//! sequence alignment on normalized tokens, and each of Claude Code's words takes the time of
//! the word it lines up with. Words with no partner are spread over the speech between their
//! neighbours. The text stays exactly Claude Code's.

use anyhow::{Result, bail, ensure};

use crate::model::{TranscriptData, TranscriptSegment, WordTiming};

/// Fewer of Claude Code's words found among Parakeet's than this (in percent) and the alignment
/// isn't trusted: the words are spread over the speech islands instead (`spread`).
pub const MIN_MATCHED_PERCENT: usize = 30;

/// A transcript of Claude Code's `text` with the times of `parakeet`'s words, for a
/// `duration_ms` recording with speech `islands`, heard by Claude Code from `heard_from_ms`.
/// Returns it with how many words matched, or why Parakeet's times can't be used.
pub fn transcript(
    text: &str,
    parakeet: &TranscriptData,
    islands: &[(u64, u64)],
    heard_from_ms: u64,
    duration_ms: u64,
    language: &str,
) -> Result<(TranscriptData, usize)> {
    if let Some(missing) = &parakeet.missing {
        bail!("no Parakeet transcript ({})", missing.reason);
    }
    ensure!(!parakeet.estimated, "Parakeet's word times are estimated");
    ensure!(!parakeet.words.is_empty(), "Parakeet heard no words");
    let words: Vec<&str> = text.split_whitespace().collect();
    ensure!(!words.is_empty(), "Claude Code returned no text");
    let (timed, matched) = align(&words, &parakeet.words, islands, heard_from_ms, duration_ms);
    ensure!(
        matched * 100 >= words.len() * MIN_MATCHED_PERCENT,
        "only {matched} of {} words match Parakeet's",
        words.len()
    );
    let model = parakeet.model.as_deref().unwrap_or("Parakeet");
    crate::capture::scrawl_log(format!(
        "Claude Code dictation: {matched} of {} words match {model}'s, using its times",
        words.len()
    ));
    Ok((
        TranscriptData {
            source: "claude-code".into(),
            model: Some(format!(
                "Claude Code dictation ({language}), Parakeet timing"
            )),
            estimated: false,
            text: words.join(" "),
            segments: sentences(&timed),
            words: timed,
            missing: None,
        },
        matched,
    ))
}

/// How a Claude Code token lines up with a Parakeet token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pair {
    /// The same word once normalized.
    Same,
    /// Nearly the same ("Shiloh" and "Shilo").
    Near,
    /// Another word in the same place: the same sound heard differently, most likely.
    Other,
}

/// A piece of a word for the alignment: words split at hyphens and slashes, so "Follow-Knopf"
/// lines up with "Follow Knopf", each piece with its share of the word's time.
struct Token {
    word: usize,
    norm: String,
    t0: u64,
    t1: u64,
}

fn tokens<'a>(words: impl Iterator<Item = (&'a str, u64, u64)>) -> Vec<Token> {
    let mut out = Vec::new();
    for (word, (text, t0, t1)) in words.enumerate() {
        let parts: Vec<String> = text
            .split(['-', '‐', '–', '—', '/'])
            .map(normalize)
            .filter(|part| !part.is_empty())
            .collect();
        let total: u64 = parts.iter().map(|p| p.chars().count() as u64).sum();
        let mut before = 0;
        for norm in parts {
            let at = |chars: u64| t0 + (t1.saturating_sub(t0)) * chars / total.max(1);
            let length = norm.chars().count() as u64;
            out.push(Token {
                word,
                t0: at(before),
                t1: at(before + length),
                norm,
            });
            before += length;
        }
    }
    out
}

/// Lowercase letters and digits only, with umlauts and ß folded ("Glühbirne," → "gluehbirne").
pub(crate) fn normalize(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    for c in word.chars().flat_map(char::to_lowercase) {
        match c {
            'ä' => out.push_str("ae"),
            'ö' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            c if c.is_alphanumeric() => out.push(c),
            _ => {}
        }
    }
    out
}

fn pair(a: &str, b: &str) -> Pair {
    if a == b {
        return Pair::Same;
    }
    let shorter = a.chars().count().min(b.chars().count());
    if shorter >= 4 && edit_distance(a, b) <= (shorter / 4).max(1) {
        Pair::Near
    } else {
        Pair::Other
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let next = (diagonal + usize::from(ca != cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// Needleman-Wunsch over the tokens: the partner of each of `a`'s tokens in `b`, if it has one.
fn partners(a: &[Token], b: &[Token]) -> Vec<Option<(usize, Pair)>> {
    const GAP: i32 = -1;
    let score = |pair: Pair| match pair {
        Pair::Same => 2,
        Pair::Near => 1,
        Pair::Other => -1,
    };
    let (n, m) = (a.len(), b.len());
    let width = m + 1;
    let mut best = vec![0_i32; (n + 1) * width];
    for i in 0..=n {
        best[i * width] = GAP * i as i32;
    }
    for j in 0..=m {
        best[j] = GAP * j as i32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let diagonal =
                best[(i - 1) * width + j - 1] + score(pair(&a[i - 1].norm, &b[j - 1].norm));
            let up = best[(i - 1) * width + j] + GAP;
            let left = best[i * width + j - 1] + GAP;
            best[i * width + j] = diagonal.max(up).max(left);
        }
    }
    let mut out = vec![None; n];
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        let here = best[i * width + j];
        let pair = pair(&a[i - 1].norm, &b[j - 1].norm);
        if here == best[(i - 1) * width + j - 1] + score(pair) {
            out[i - 1] = Some((j - 1, pair));
            i -= 1;
            j -= 1;
        } else if here == best[(i - 1) * width + j] + GAP {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    out
}

/// Claude Code's `words` with times, and how many of them have a same or near partner among
/// Parakeet's `measured` words. A word takes its partners' times. Words without one (the
/// alignment pairs leftovers on both sides, so Parakeet heard nothing there) are spread over the
/// speech `islands` between their neighbours, else over all the time between them.
fn align(
    words: &[&str],
    measured: &[WordTiming],
    islands: &[(u64, u64)],
    heard_from_ms: u64,
    duration_ms: u64,
) -> (Vec<WordTiming>, usize) {
    let ours = tokens(words.iter().map(|w| (*w, 0, 0)));
    let theirs = tokens(measured.iter().map(|w| (w.text.as_str(), w.t0, w.t1)));
    let partners = partners(&ours, &theirs);

    // Per word: its time from its partners, and whether one of them is the same word (or nearly).
    let mut anchors: Vec<Option<(u64, u64)>> = vec![None; words.len()];
    let mut matched = vec![false; words.len()];
    for (token, partner) in ours.iter().zip(&partners) {
        let Some((j, pair)) = *partner else { continue };
        let theirs = &theirs[j];
        let anchor = &mut anchors[token.word];
        *anchor = Some(match *anchor {
            None => (theirs.t0, theirs.t1),
            Some((t0, t1)) => (t0.min(theirs.t0), t1.max(theirs.t1)),
        });
        matched[token.word] |= pair != Pair::Other;
    }

    let mut times: Vec<(u64, u64)> = vec![(0, 0); words.len()];
    let mut word = 0;
    while word < words.len() {
        if let Some((t0, t1)) = anchors[word] {
            times[word] = (t0, t1);
            word += 1;
            continue;
        }
        let run_end = (word..words.len())
            .find(|&w| anchors[w].is_some())
            .unwrap_or(words.len());
        let before = word.checked_sub(1).and_then(|w| anchors[w]);
        let after = anchors.get(run_end).copied().flatten();
        let from = before.map_or(heard_from_ms, |(t0, t1)| t1.max(t0));
        let to = after.map_or(duration_ms, |(t0, _)| t0).max(from);
        let speech: Vec<(u64, u64)> = islands
            .iter()
            .map(|&(s, e)| (s.max(from), e.min(to)))
            .filter(|(s, e)| e > s)
            .collect();
        let windows = if speech.is_empty() {
            vec![(from, to)]
        } else {
            speech
        };
        let run = &words[word..run_end];
        for ((start, end), share) in windows
            .iter()
            .zip(super::spread::share(&run.join(" "), &windows))
        {
            if share.is_empty() {
                continue;
            }
            let (_, spread) = crate::hex::spread_words(&share.join(" "), *start, *end);
            for timing in spread {
                times[word] = (timing.t0, timing.t1);
                word += 1;
            }
        }
        // `share` hands out every word, so the run is done.
        word = run_end;
    }

    // Partners that cross (rare, and only for "Other" pairs) must not move time backwards.
    let mut floor = 0;
    let timed = words
        .iter()
        .zip(times)
        .map(|(text, (t0, t1))| {
            let t0 = t0.max(floor);
            floor = t0;
            WordTiming {
                t0,
                t1: t1.max(t0),
                text: (*text).to_owned(),
            }
        })
        .collect();
    (timed, matched.iter().filter(|m| **m).count())
}

/// One segment per sentence, so `said_per_keyframe` breaks clauses where Claude Code's text does.
fn sentences(words: &[WordTiming]) -> Vec<TranscriptSegment> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, word) in words.iter().enumerate() {
        let end = word
            .text
            .trim_end_matches(['"', '\'', '”', '’', '»', '«', ')']);
        if end.ends_with(['.', '!', '?']) || i + 1 == words.len() {
            let phrase = &words[start..=i];
            out.push(TranscriptSegment {
                t0: phrase[0].t0,
                t1: phrase.iter().map(|w| w.t1).max().unwrap_or(phrase[0].t1),
                text: phrase
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            });
            start = i + 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parakeet's words, each `(start ms, text)`, 300 ms long.
    fn heard(words: &[(u64, &str)]) -> TranscriptData {
        TranscriptData {
            source: "parakeet:parakeet-tdt-0.6b-v3-Q8_0".into(),
            model: Some("parakeet-tdt-0.6b-v3-Q8_0".into()),
            words: words
                .iter()
                .map(|&(t0, text)| WordTiming {
                    t0,
                    t1: t0 + 300,
                    text: text.into(),
                })
                .collect(),
            ..Default::default()
        }
    }

    fn starts(t: &TranscriptData) -> Vec<(&str, u64)> {
        t.words.iter().map(|w| (w.text.as_str(), w.t0)).collect()
    }

    const ISLANDS: [(u64, u64); 2] = [(1_000, 4_000), (6_000, 9_000)];

    #[test]
    fn german_with_punctuation_and_umlauts_takes_parakeets_times() {
        let parakeet = heard(&[
            (1_000, "Hier"),
            (1_300, "ist"),
            (1_600, "der"),
            (1_900, "Follow"),
            (2_200, "Knopf"),
            (6_000, "und"),
            (6_300, "hier"),
            (6_600, "eine"),
            (6_900, "Gluehbirne"),
        ]);
        let text = "Hier ist der „Follow-Knopf“, und hier eine Glühbirne.";
        let (t, matched) = transcript(text, &parakeet, &ISLANDS, 0, 10_000, "de").unwrap();
        assert_eq!(t.text, text, "the text stays Claude Code's");
        assert_eq!(matched, 8);
        assert!(!t.estimated);
        assert_eq!(t.source, "claude-code");
        assert_eq!(
            t.model.as_deref(),
            Some("Claude Code dictation (de), Parakeet timing")
        );
        assert_eq!(
            starts(&t),
            [
                ("Hier", 1_000),
                ("ist", 1_300),
                ("der", 1_600),
                ("„Follow-Knopf“,", 1_900),
                ("und", 6_000),
                ("hier", 6_300),
                ("eine", 6_600),
                ("Glühbirne.", 6_900)
            ]
        );
        assert_eq!(t.words[3].t1, 2_500, "both halves of the compound");
        assert_eq!(t.segments.len(), 1);
    }

    #[test]
    fn a_word_heard_differently_keeps_its_place_and_near_words_count() {
        let parakeet = heard(&[
            (1_000, "Hier"),
            (1_300, "ist"),
            (1_600, "Shilo,"),
            (6_000, "hier"),
            (6_300, "steht"),
            (6_600, "Germani."),
        ]);
        let text = "Hier ist Shiloh, hier steht Jermany.";
        let (t, matched) = transcript(text, &parakeet, &ISLANDS, 0, 10_000, "de").unwrap();
        // "Shilo"/"Shiloh" is near; "Germani"/"Jermany" is another word in the same place.
        assert_eq!(matched, 5);
        assert_eq!(
            starts(&t),
            [
                ("Hier", 1_000),
                ("ist", 1_300),
                ("Shiloh,", 1_600),
                ("hier", 6_000),
                ("steht", 6_300),
                ("Jermany.", 6_600)
            ]
        );
    }

    #[test]
    fn a_word_claude_left_out_leaves_the_others_where_they_were_said() {
        let parakeet = heard(&[
            (1_000, "Ich"),
            (1_300, "scroll"),
            (1_600, "jetzt"),
            (1_900, "äh"),
            (2_200, "runter."),
            (6_000, "Hier"),
            (6_300, "ist"),
            (6_600, "Shiloh."),
        ]);
        let (t, matched) = transcript(
            "Ich scroll jetzt runter. Hier ist Shiloh.",
            &parakeet,
            &ISLANDS,
            0,
            10_000,
            "de",
        )
        .unwrap();
        assert_eq!(matched, 7);
        assert_eq!(
            starts(&t),
            [
                ("Ich", 1_000),
                ("scroll", 1_300),
                ("jetzt", 1_600),
                ("runter.", 2_200),
                ("Hier", 6_000),
                ("ist", 6_300),
                ("Shiloh.", 6_600)
            ]
        );
        assert_eq!(t.segments.len(), 2);
        assert_eq!(
            (t.segments[1].t0, t.segments[1].text.as_str()),
            (6_000, "Hier ist Shiloh.")
        );
    }

    #[test]
    fn a_word_parakeet_missed_goes_between_its_neighbours() {
        // Parakeet heard nothing between "ist" (ends 1 600) and "Shiloh" (6 000): the extra word
        // goes into the speech there, the rest of the first island.
        let parakeet = heard(&[(1_000, "Hier"), (1_300, "ist"), (6_000, "Shiloh")]);
        let (t, _) = transcript(
            "Hier ist wirklich Shiloh",
            &parakeet,
            &ISLANDS,
            0,
            10_000,
            "de",
        )
        .unwrap();
        let extra = &t.words[2];
        assert_eq!(extra.text, "wirklich");
        assert!(extra.t0 >= 1_600 && extra.t1 <= 4_000, "{extra:?}");
        assert_eq!(t.words[3].t0, 6_000);

        // Words before the first match start where Claude Code began listening.
        let (t, _) =
            transcript("Also hier ist Shiloh", &parakeet, &ISLANDS, 0, 10_000, "de").unwrap();
        assert_eq!(
            (t.words[0].t0, t.words[0].t1, t.words[1].t0),
            (0, 1_000, 1_000)
        );
    }

    #[test]
    fn nothing_from_parakeet_or_too_little_in_common_keeps_the_estimate() {
        let none = heard(&[]);
        let error = transcript("Hallo Welt", &none, &ISLANDS, 0, 10_000, "de").unwrap_err();
        assert_eq!(error.to_string(), "Parakeet heard no words");

        let mut estimated = heard(&[(1_000, "Hallo"), (1_300, "Welt")]);
        estimated.estimated = true;
        assert!(transcript("Hallo Welt", &estimated, &ISLANDS, 0, 10_000, "de").is_err());

        let missing = TranscriptData {
            missing: Some(crate::model::MissingTranscript {
                reason: "The speech model is downloading.".into(),
                retry: true,
            }),
            ..Default::default()
        };
        assert!(transcript("Hallo Welt", &missing, &ISLANDS, 0, 10_000, "de").is_err());

        // 2 of 7 words in common is under 30 %; 3 of 7 is over.
        let parakeet = heard(&[(1_000, "eins"), (1_300, "zwei"), (1_600, "drei")]);
        let text = "eins zwei vier fünf sechs sieben acht";
        let error = transcript(text, &parakeet, &ISLANDS, 0, 10_000, "de").unwrap_err();
        assert_eq!(error.to_string(), "only 2 of 7 words match Parakeet's");
        let text = "eins zwei drei fünf sechs sieben acht";
        assert!(transcript(text, &parakeet, &ISLANDS, 0, 10_000, "de").is_ok());
    }

    #[test]
    fn times_never_go_backwards() {
        let parakeet = heard(&[(6_000, "zwei"), (1_000, "eins"), (6_300, "drei")]);
        let (t, _) = transcript("eins zwei drei", &parakeet, &ISLANDS, 0, 10_000, "de").unwrap();
        assert!(
            t.words.windows(2).all(|w| w[0].t0 <= w[1].t0),
            "{:?}",
            t.words
        );
    }

    #[test]
    fn normalizing_folds_case_punctuation_and_umlauts() {
        assert_eq!(normalize("„Glühbirne“,"), "gluehbirne");
        assert_eq!(normalize("Straße."), "strasse");
        assert_eq!(normalize("ÖL"), "oel");
        assert_eq!(normalize("—"), "");
        assert_eq!(edit_distance("shiloh", "shilo"), 1);
        assert_eq!(pair("germany", "germani"), Pair::Near);
        assert_eq!(pair("ist", "isst"), Pair::Other);
    }
}
