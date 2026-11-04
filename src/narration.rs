//! Experiment `sentenceNarration`: keyframes and their text from whole sentences. Speech and
//! pointing do not run in step: on 160 real clicks the nearest pointing word ("hier", "das") lay
//! anywhere from 3.3 s before to 3 s after the click, and 42% of the clicks came in bursts. So a
//! sentence is never cut at an action. Whole sentences own a keyframe, the actions done over them
//! are listed in front, and a click the screen didn't answer reads as pointing.

use crate::model::{
    BASE_KEYFRAMES, InteractionEvent, Mark, Shape, Stroke, TranscriptData, format_clock,
    keyframe_budget,
};

pub const SENTENCE_NARRATION: &str = "sentenceNarration";

// Thinking pauses run long and the speech goes on where it stopped, so a pause ends only a finished
// sentence, or an unfinished one when it runs past this.
const ABANDONED_GAP_MS: u64 = 5_000;
const MAX_SENTENCE_WORDS: usize = 60;
const BURST_GAP_MS: u64 = 1_500;
/// A screen change this soon after a click is the click's answer.
const REACTION_MS: u64 = 1_000;
const SCROLL_GAP_MS: u64 = 1_500;
const NOTE_REPEAT_MS: u64 = 3_000;
/// An action in a speech pause goes to the sentence it is nearest to, up to this far away.
const ATTACH_MS: u64 = 3_000;
const SETTLE_MS: u64 = 300;
const COVERAGE_MS: u64 = 35_000;
const COVERAGE_WORDS: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mark,
    Clicks,
    Point,
    Drag,
    Scene,
    Scroll,
    /// Said at its place in the speech, but never worth an image of its own.
    Note,
}

impl Kind {
    fn score(self) -> u32 {
        match self {
            Kind::Mark => 100,
            Kind::Clicks => 60,
            Kind::Point => 55,
            Kind::Drag => 50,
            Kind::Scene => 40,
            Kind::Scroll => 30,
            Kind::Note => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Detail {
    /// Index into the capture's marks.
    Mark {
        mark: usize,
        flag: bool,
    },
    Clicks {
        /// What each click was, as `clicked button “Area”` would say it, without the verb.
        targets: Vec<Option<String>>,
        verb: &'static str,
        points: Vec<[f32; 2]>,
        /// False when the screen did not react: the hand was pointing, not operating anything.
        changed: Option<bool>,
    },
    Text(String),
    Scroll(Vec<&'static str>),
    Scene,
    Note {
        text: String,
        count: usize,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub kind: Kind,
    pub t0: u64,
    pub t1: u64,
    pub detail: Detail,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Sentence {
    t0: u64,
    t1: u64,
    words: Vec<String>,
    actions: Vec<usize>,
}

/// One keyframe's worth of narration.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub t0: u64,
    pub t1: u64,
    /// When the keyframe of this line should be taken.
    pub frame_t: u64,
    pub text: String,
    pub actions: Vec<Action>,
    /// Whole sentences with the marks drawn over each (indexes into the capture's marks).
    pub sentences: Vec<(String, Vec<usize>)>,
}

fn ends_sentence(word: &str) -> bool {
    word.trim_end_matches(['"', '”', '’', ')', ']'])
        .ends_with(['.', '!', '?', ';', ':'])
}

fn ends_clause(word: &str) -> bool {
    word.trim_end_matches(['"', '”', '’', ')', ']'])
        .ends_with(',')
}

/// Joins recognizer words, keeping punctuation tokens on the word before.
fn join(words: &[String]) -> String {
    let mut text = String::new();
    for unit in words {
        let attached = unit.starts_with([',', '.', '!', '?', ';', ':', '’', '\'', ')', ']', '”']);
        let open = text.ends_with(['(', '[', '“']);
        if !text.is_empty() && !attached && !open {
            text.push(' ');
        }
        text.push_str(unit);
    }
    text
}

/// Timed units: words, else segments; a transcriber that can't time anything still said it, so
/// the speech stays whole over the capture.
fn units(transcript: &TranscriptData, duration_ms: u64) -> Vec<(u64, u64, String)> {
    let mut units: Vec<(u64, u64, String)> = if transcript.words.is_empty() {
        transcript
            .segments
            .iter()
            .map(|s| (s.t0, s.t1, s.text.clone()))
            .collect()
    } else {
        transcript
            .words
            .iter()
            .map(|w| (w.t0, w.t1, w.text.clone()))
            .collect()
    };
    units.retain(|unit| !unit.2.trim().is_empty());
    if units.is_empty() && !transcript.text.trim().is_empty() {
        units.push((0, duration_ms, transcript.text.trim().to_owned()));
    }
    units.sort_by_key(|unit| unit.0);
    units
}

fn split_sentences(units: &[(u64, u64, String)]) -> Vec<Sentence> {
    let mut sentences: Vec<Sentence> = Vec::new();
    let mut open: Option<usize> = None;
    for (t0, t1, text) in units {
        let text = text.trim();
        if let Some(i) = open
            && t0.saturating_sub(sentences[i].t1) >= ABANDONED_GAP_MS
        {
            open = None;
        }
        // The model also ends sentences that go on: German starts a new one with a capital.
        if open.is_none()
            && let Some(last) = sentences.last()
            && t0.saturating_sub(last.t1) < ABANDONED_GAP_MS
            && text.chars().next().is_some_and(char::is_lowercase)
        {
            open = Some(sentences.len() - 1);
        }
        let i = match open {
            Some(i) => {
                let sentence = &mut sentences[i];
                sentence.t1 = sentence.t1.max(*t1);
                sentence.words.push(text.to_owned());
                i
            }
            None => {
                sentences.push(Sentence {
                    t0: *t0,
                    t1: *t1,
                    words: vec![text.to_owned()],
                    actions: Vec::new(),
                });
                sentences.len() - 1
            }
        };
        open = Some(i);
        let too_long = sentences[i].words.len() >= MAX_SENTENCE_WORDS && ends_clause(text);
        if ends_sentence(text) || too_long {
            open = None;
        }
    }
    sentences
}

fn note(notes: &mut Vec<Action>, t: u64, text: String) {
    if let Some(same) = notes
        .iter_mut()
        .rev()
        .find(|n| matches!(&n.detail, Detail::Note { text: other, .. } if *other == text))
        && t.saturating_sub(same.t1) < NOTE_REPEAT_MS
    {
        same.t1 = t;
        if let Detail::Note { count, .. } = &mut same.detail {
            *count += 1;
        }
        return;
    }
    notes.push(Action {
        kind: Kind::Note,
        t0: t,
        t1: t,
        detail: Detail::Note { text, count: 1 },
    });
}

fn click_target(role: Option<&str>, label: Option<&str>) -> Option<String> {
    let parts: Vec<String> = [
        role.map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_owned),
        label
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(crate::sketch::quote_label),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Marks, click bursts, drags, scrolls, laser points and notes (shortcuts, app switches), in time
/// order. The app's own events stay out: the summary lists them already.
pub fn fold_actions(
    events: &[InteractionEvent],
    marks: &[Mark],
    strokes: &[Stroke],
) -> Vec<Action> {
    let mut actions: Vec<Action> = marks
        .iter()
        .enumerate()
        .map(|(i, mark)| Action {
            kind: Kind::Mark,
            t0: mark.t0,
            t1: mark.t1,
            detail: Detail::Mark {
                mark: i,
                flag: mark.shape == Shape::Flag,
            },
        })
        .collect();
    let mut bursts: Vec<Action> = Vec::new();
    let mut scrolls: Vec<Action> = Vec::new();
    let mut notes: Vec<Action> = Vec::new();
    let mut events: Vec<&InteractionEvent> = events.iter().collect();
    events.sort_by_key(|event| event.timestamp_ms());
    for event in events {
        match event {
            InteractionEvent::Click {
                t,
                x,
                y,
                button,
                label,
                role,
            } => {
                let verb = if button.as_deref() == Some("touch") {
                    "tapped"
                } else {
                    "clicked"
                };
                let target = click_target(role.as_deref(), label.as_deref());
                match bursts.last_mut() {
                    Some(Action {
                        t1,
                        detail:
                            Detail::Clicks {
                                targets,
                                points,
                                verb: same,
                                ..
                            },
                        ..
                    }) if t.saturating_sub(*t1) <= BURST_GAP_MS && *same == verb => {
                        *t1 = *t;
                        targets.push(target);
                        points.push([*x, *y]);
                    }
                    _ => bursts.push(Action {
                        kind: Kind::Clicks,
                        t0: *t,
                        t1: *t,
                        detail: Detail::Clicks {
                            targets: vec![target],
                            verb,
                            points: vec![[*x, *y]],
                            changed: None,
                        },
                    }),
                }
            }
            InteractionEvent::Scroll { t, dx, dy, .. } => {
                let joins = scrolls
                    .last()
                    .is_some_and(|s| t.saturating_sub(s.t1) <= SCROLL_GAP_MS);
                if !joins {
                    scrolls.push(Action {
                        kind: Kind::Scroll,
                        t0: *t,
                        t1: *t,
                        detail: Detail::Scroll(Vec::new()),
                    });
                }
                let scroll = scrolls.last_mut().unwrap();
                scroll.t1 = *t;
                if let Detail::Scroll(directions) = &mut scroll.detail {
                    // As `sketch::describe_event` says it: a positive `dy` goes down the page.
                    for (on, direction) in [
                        (*dy < 0.0, "up"),
                        (*dy > 0.0, "down"),
                        (*dx < 0.0, "left"),
                        (*dx > 0.0, "right"),
                    ] {
                        if on && !directions.contains(&direction) {
                            directions.push(direction);
                        }
                    }
                }
            }
            InteractionEvent::Key { t, shortcut } => {
                note(&mut notes, *t, format!("pressed {shortcut}"))
            }
            InteractionEvent::AppChange { t, app, .. } => {
                note(&mut notes, *t, format!("switched to {app}"))
            }
            InteractionEvent::Point { t, label, .. } => actions.push(Action {
                kind: Kind::Point,
                t0: *t,
                t1: *t,
                detail: Detail::Text(pointed(label.as_deref())),
            }),
            InteractionEvent::Drag { t, t1, .. } => {
                if let Some(text) = crate::agent_context::drag_line(event) {
                    actions.push(Action {
                        kind: Kind::Drag,
                        t0: *t,
                        t1: *t1,
                        detail: Detail::Text(text),
                    });
                }
            }
            InteractionEvent::AppEvent { .. } => {}
        }
    }
    for stroke in strokes.iter().filter(|s| s.laser) {
        actions.push(Action {
            kind: Kind::Point,
            t0: stroke.t0,
            t1: stroke.t1,
            detail: Detail::Text(pointed(stroke.on.as_deref())),
        });
    }
    actions.extend(bursts);
    actions.extend(scrolls);
    actions.extend(notes);
    actions.sort_by_key(|a| a.t0);
    actions
}

fn pointed(label: Option<&str>) -> String {
    match label.map(str::trim).filter(|l| !l.is_empty()) {
        Some(label) => format!("pointed at {}", crate::sketch::quote_label(label)),
        None => "pointed".to_owned(),
    }
}

/// Clicks the screen answered stay clicks; the rest are pointing. A change no click or scroll
/// answers for is a scene of its own.
fn with_reactions(mut actions: Vec<Action>, scene_changes: &[u64]) -> Vec<Action> {
    let answered = |a: &Action, t: u64| t >= a.t0 && t <= a.t1 + REACTION_MS;
    let scenes: Vec<Action> = scene_changes
        .iter()
        .filter(|&&t| {
            !actions
                .iter()
                .any(|a| matches!(a.kind, Kind::Clicks | Kind::Scroll) && answered(a, t))
        })
        .map(|&t| Action {
            kind: Kind::Scene,
            t0: t,
            t1: t,
            detail: Detail::Scene,
        })
        .collect();
    for action in &mut actions {
        let reacted = scene_changes.iter().any(|&t| answered(action, t));
        if let Detail::Clicks { changed, .. } = &mut action.detail {
            *changed = Some(reacted);
        }
    }
    actions.extend(scenes);
    actions.sort_by_key(|a| a.t0);
    actions
}

fn attach(sentences: &mut Vec<Sentence>, actions: &[Action]) {
    for (index, action) in actions.iter().enumerate() {
        let at = (action.t0 + action.t1) / 2;
        let nearest = sentences
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.t0.saturating_sub(at).max(at.saturating_sub(s.t1))))
            .min_by_key(|&(i, distance)| (distance, i));
        match nearest {
            Some((i, distance)) if distance <= ATTACH_MS => sentences[i].actions.push(index),
            _ => {
                let at = sentences.partition_point(|s| s.t0 <= action.t0);
                sentences.insert(
                    at,
                    Sentence {
                        t0: action.t0,
                        t1: action.t1,
                        words: Vec::new(),
                        actions: vec![index],
                    },
                );
            }
        }
    }
}

fn score_of(sentence: &Sentence, actions: &[Action]) -> u32 {
    sentence
        .actions
        .iter()
        .map(|&i| actions[i].kind.score())
        .max()
        .unwrap_or(0)
}

/// Sentences inside a group of marks: its keyframe starts earlier and nothing cuts into it.
fn inside_mark_groups(
    sentences: &[Sentence],
    actions: &[Action],
    groups: &[Vec<usize>],
) -> Vec<bool> {
    let mut inside = vec![false; sentences.len()];
    for group in groups {
        let holding: Vec<usize> = sentences
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.actions.iter().any(|&i| {
                    matches!(actions[i].detail, Detail::Mark { mark, .. } if group.contains(&mark))
                })
            })
            .map(|(i, _)| i)
            .collect();
        if let (Some(&first), Some(&last)) = (holding.first(), holding.last()) {
            for flag in &mut inside[first + 1..=last] {
                *flag = true;
            }
        }
    }
    inside
}

/// The sentences that own a keyframe: every mark, then long stretches that show something, then the
/// strongest actions. Ordinary actions fill only the base budget; the rest is for marks and coverage.
fn choose_owners(
    sentences: &[Sentence],
    actions: &[Action],
    budget: usize,
    inside: &[bool],
) -> Vec<bool> {
    let mut owners = vec![false; sentences.len()];
    let count = |owners: &[bool]| owners.iter().filter(|&&o| o).count();
    let mut ranked: Vec<(usize, u32)> = sentences
        .iter()
        .enumerate()
        .map(|(i, s)| (i, score_of(s, actions)))
        .filter(|&(i, score)| score > 0 && !inside[i])
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    for &(i, score) in &ranked {
        if score >= Kind::Mark.score() {
            owners[i] = true;
        }
    }
    let mut stretch_start = 0;
    let mut stretch_words = 0;
    let mut best: Option<usize> = None;
    for (i, sentence) in sentences.iter().enumerate() {
        if owners[i] {
            stretch_start = sentence.t0;
            stretch_words = sentence.words.len();
            best = None;
            continue;
        }
        stretch_words += sentence.words.len();
        // The latest of equals splits the stretch most evenly.
        let score = score_of(sentence, actions);
        if score > 0 && !inside[i] && best.is_none_or(|b| score >= score_of(&sentences[b], actions))
        {
            best = Some(i);
        }
        let long = sentence.t1.saturating_sub(stretch_start) > COVERAGE_MS
            || stretch_words > COVERAGE_WORDS;
        if long
            && let Some(b) = best
            && count(&owners) < budget
        {
            owners[b] = true;
            stretch_start = sentences[b].t0;
            stretch_words = sentences[b..=i].iter().map(|s| s.words.len()).sum();
            best = None;
        }
    }
    for &(i, _) in &ranked {
        if count(&owners) >= budget.min(BASE_KEYFRAMES) {
            break;
        }
        owners[i] = true;
    }
    // Speech with nothing on screen still gets one frame to be read against.
    if count(&owners) == 0 && !sentences.is_empty() {
        owners[0] = true;
    }
    owners
}

fn frame_time(t0: u64, actions: &[Action], duration_ms: u64) -> u64 {
    let moment = actions
        .iter()
        .rev()
        .find(|a| a.kind == Kind::Mark)
        .or_else(|| actions.iter().rev().find(|a| a.kind.score() > 0));
    match moment {
        None => t0,
        Some(a) if a.kind == Kind::Mark => a.t1.min(duration_ms),
        Some(a) => (a.t1 + SETTLE_MS).min(duration_ms),
    }
}

/// The capture as keyframe lines. `scene_changes` are when the screen changed (without it every
/// click counts as operating something); `mark_groups` are mark indexes that share one keyframe.
pub fn build(
    transcript: &TranscriptData,
    events: &[InteractionEvent],
    marks: &[Mark],
    strokes: &[Stroke],
    duration_ms: u64,
    scene_changes: Option<&[u64]>,
    mark_groups: &[Vec<usize>],
    budget: usize,
) -> Vec<Line> {
    let mut sentences = split_sentences(&units(transcript, duration_ms));
    let folded = fold_actions(events, marks, strokes);
    let actions = match scene_changes {
        Some(changes) => with_reactions(folded, changes),
        None => folded,
    };
    attach(&mut sentences, &actions);
    // Nothing said, done or changed: the picture is still what the capture shows.
    if sentences.is_empty() {
        return vec![Line {
            t0: 0,
            t1: duration_ms,
            frame_t: duration_ms / 2,
            text: String::new(),
            actions: Vec::new(),
            sentences: Vec::new(),
        }];
    }
    let inside = inside_mark_groups(&sentences, &actions, mark_groups);
    let owners = choose_owners(&sentences, &actions, budget, &inside);
    let mut groups: Vec<Vec<&Sentence>> = Vec::new();
    let mut owner_seen = false;
    for (sentence, &owner) in sentences.iter().zip(&owners) {
        // Speech ahead of the first keyframe belongs to it.
        if groups.is_empty() || (owner && owner_seen) {
            groups.push(vec![sentence]);
        } else {
            groups.last_mut().unwrap().push(sentence);
        }
        owner_seen |= owner;
    }
    groups
        .into_iter()
        .map(|group| {
            let line_actions: Vec<Action> = group
                .iter()
                .flat_map(|s| s.actions.iter().map(|&i| actions[i].clone()))
                .collect();
            let t0 = group[0].t0;
            let words: Vec<String> = group.iter().flat_map(|s| s.words.clone()).collect();
            Line {
                t0,
                t1: group.iter().map(|s| s.t1).max().unwrap_or(t0),
                frame_t: frame_time(t0, &line_actions, duration_ms),
                text: join(&words),
                sentences: group
                    .iter()
                    .map(|s| {
                        let marks = s
                            .actions
                            .iter()
                            .filter_map(|&i| match actions[i].detail {
                                Detail::Mark { mark, .. } => Some(mark),
                                _ => None,
                            })
                            .collect();
                        (join(&s.words), marks)
                    })
                    .collect(),
                actions: line_actions,
            }
        })
        .collect()
}

/// `build` with Scrawl's keyframe budget for the capture's length.
pub fn build_for(
    transcript: &TranscriptData,
    events: &[InteractionEvent],
    marks: &[Mark],
    strokes: &[Stroke],
    duration_ms: u64,
    scene_changes: Option<&[u64]>,
    mark_groups: &[Vec<usize>],
) -> Vec<Line> {
    build(
        transcript,
        events,
        marks,
        strokes,
        duration_ms,
        scene_changes,
        mark_groups,
        keyframe_budget(duration_ms),
    )
}

/// What was done over a line, as one bracket: `marked, clicked ×2, pointed at “Save”`.
pub fn describe(actions: &[Action], marks: &[Mark]) -> String {
    let mut drawn: Vec<(String, usize)> = Vec::new();
    let mut clicks: Vec<(String, usize)> = Vec::new();
    let mut pointing: Vec<(String, usize)> = Vec::new();
    let mut notes: Vec<(String, usize)> = Vec::new();
    let mut drags: Vec<String> = Vec::new();
    let mut directions: Vec<&str> = Vec::new();
    let mut scrolled = false;
    let add = |into: &mut Vec<(String, usize)>, text: String, n: usize| match into
        .iter_mut()
        .find(|(t, _)| *t == text)
    {
        Some((_, count)) => *count += n,
        None => into.push((text, n)),
    };
    for action in actions {
        match &action.detail {
            Detail::Mark { mark, flag: false } => add(
                &mut drawn,
                crate::agent_context::mark_line(&marks[*mark]),
                1,
            ),
            Detail::Mark { flag: true, .. } | Detail::Scene => {}
            Detail::Clicks {
                targets,
                verb,
                changed,
                ..
            } => {
                for target in targets {
                    if *changed == Some(false) {
                        add(&mut pointing, pointed(target.as_deref()), 1);
                    } else {
                        let text = match target {
                            Some(target) => format!("{verb} {target}"),
                            None => (*verb).to_owned(),
                        };
                        add(&mut clicks, text, 1);
                    }
                }
            }
            Detail::Text(text) if action.kind == Kind::Point => add(&mut pointing, text.clone(), 1),
            Detail::Text(text) => {
                if !drags.contains(text) {
                    drags.push(text.clone());
                }
            }
            Detail::Scroll(ds) => {
                scrolled = true;
                for d in ds {
                    if !directions.contains(d) {
                        directions.push(d);
                    }
                }
            }
            Detail::Note { text, count } => add(&mut notes, text.clone(), *count),
        }
    }
    let counted = |(text, n): (String, usize)| if n > 1 { format!("{text} ×{n}") } else { text };
    let mut parts: Vec<String> = Vec::new();
    parts.extend(drawn.into_iter().map(counted));
    parts.extend(clicks.into_iter().map(counted));
    parts.extend(pointing.into_iter().map(counted));
    parts.extend(drags);
    if scrolled {
        parts.push(if directions.is_empty() {
            "scrolled".to_owned()
        } else {
            format!("scrolled {}", directions.join(" and "))
        });
    }
    parts.extend(notes.into_iter().map(counted));
    parts.join(", ")
}

/// What `summary.md` says for a keyframe built from a line, after its `- #N (M:SS)` head.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Narrated {
    /// What was done over its sentences, without the marks a `marks` line names.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub actions: String,
    /// With several marks on the keyframe: the words said before the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead: Option<String>,
    /// With several marks on the keyframe: each one's label and the whole sentences said from it
    /// on (`✎① 0:05 circled “Area” “this one”`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<String>,
}

impl Line {
    /// The marks this line draws (not flags), in drawing order.
    pub fn drawn(&self, marks: &[Mark]) -> Vec<usize> {
        let mut drawn: Vec<usize> = self
            .actions
            .iter()
            .filter_map(|a| match a.detail {
                Detail::Mark { mark, flag: false } => Some(mark),
                _ => None,
            })
            .collect();
        drawn.sort_by_key(|&m| (marks[m].t0, m));
        drawn
    }

    pub fn flagged(&self) -> bool {
        self.actions
            .iter()
            .any(|a| matches!(a.detail, Detail::Mark { flag: true, .. }))
    }

    pub fn narrated(&self, marks: &[Mark]) -> Narrated {
        let drawn = self.drawn(marks);
        if drawn.len() <= 1 {
            let mut actions = describe(&self.actions, marks);
            // Only the screen changed: the keyframe shows what it changed to.
            if actions.is_empty()
                && self.text.is_empty()
                && !self.flagged()
                && self.actions.iter().any(|a| a.kind == Kind::Scene)
            {
                actions = "screen changed".to_owned();
            }
            return Narrated {
                actions,
                ..Default::default()
            };
        }
        let others: Vec<Action> = self
            .actions
            .iter()
            .filter(|a| a.kind != Kind::Mark)
            .cloned()
            .collect();
        let mut lead: Vec<&str> = Vec::new();
        let mut lines: Vec<(String, Vec<&str>)> = Vec::new();
        for (text, on) in &self.sentences {
            let mut on: Vec<usize> = on.iter().copied().filter(|m| drawn.contains(m)).collect();
            on.sort_by_key(|&m| (marks[m].t0, m));
            for m in on {
                let number = drawn.iter().position(|&d| d == m).unwrap_or(0) + 1;
                lines.push((
                    format!(
                        "✎{} {} {}",
                        crate::agent_context::circled(number),
                        format_clock(marks[m].t0),
                        crate::agent_context::mark_line(&marks[m])
                    ),
                    Vec::new(),
                ));
            }
            if !text.is_empty() {
                match lines.last_mut() {
                    Some((_, said)) => said.push(text),
                    None => lead.push(text),
                }
            }
        }
        Narrated {
            actions: describe(&others, marks),
            lead: (!lead.is_empty()).then(|| lead.join(" ")),
            marks: lines
                .into_iter()
                .map(|(label, said)| {
                    if said.is_empty() {
                        label
                    } else {
                        format!("{label} “{}”", said.join(" "))
                    }
                })
                .collect(),
        }
    }
}

/// The text after a keyframe's head (`- #2 (0:09) ⚑`), one or more lines.
pub fn summary_text(head: &str, said: Option<&str>, flagged: bool, narrated: &Narrated) -> String {
    let actions = if narrated.actions.is_empty() {
        String::new()
    } else {
        format!(" [{}]", narrated.actions)
    };
    let quoted = |text: &str| format!(" “{text}”");
    if !narrated.marks.is_empty() {
        let mut out = format!(
            "{head}{actions}{}",
            narrated.lead.as_deref().map(quoted).unwrap_or_default()
        );
        for mark in &narrated.marks {
            out.push_str(&format!("\n  {mark}"));
        }
        return out;
    }
    match said.map(str::trim).filter(|s| !s.is_empty()) {
        Some(said) => format!("{head}{actions}{}", quoted(said)),
        None if !narrated.actions.is_empty() => format!("{head} {}", narrated.actions),
        None if flagged => format!("{head} flagged this moment"),
        None => format!("{head} nothing said or done"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WordTiming;

    // Ported one to one from T3 Code's `narration.test.ts`, where these rules were measured.

    fn say_at(text: &str, start: u64, word_ms: u64) -> Vec<WordTiming> {
        text.split(' ')
            .enumerate()
            .map(|(i, w)| WordTiming {
                t0: start + i as u64 * word_ms,
                t1: start + (i as u64 + 1) * word_ms,
                text: w.to_owned(),
            })
            .collect()
    }

    fn say(text: &str, start: u64) -> Vec<WordTiming> {
        say_at(text, start, 300)
    }

    fn transcript(words: Vec<WordTiming>) -> TranscriptData {
        TranscriptData {
            source: "test".into(),
            text: words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            words,
            ..Default::default()
        }
    }

    fn joined(words: &[WordTiming]) -> String {
        words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn click(t: u64, x: f32, y: f32) -> InteractionEvent {
        InteractionEvent::Click {
            t,
            x,
            y,
            button: None,
            label: None,
            role: None,
        }
    }

    fn named_click(t: u64, role: &str, label: &str) -> InteractionEvent {
        InteractionEvent::Click {
            t,
            x: 0.1,
            y: 0.1,
            button: None,
            label: Some(label.into()),
            role: Some(role.into()),
        }
    }

    fn key(t: u64, shortcut: &str) -> InteractionEvent {
        InteractionEvent::Key {
            t,
            shortcut: shortcut.into(),
        }
    }

    fn mark(t0: u64, t1: u64, shape: Shape) -> Mark {
        Mark {
            id: String::new(),
            strokes: vec![0],
            t0,
            t1,
            box_norm: [0.1, 0.1, 0.2, 0.2],
            shape,
            from: None,
            to: None,
            pin: None,
            on: None,
            description: String::new(),
            frame: None,
        }
    }

    fn drag(
        t: u64,
        t1: u64,
        from: [f32; 2],
        to: [f32; 2],
        label: Option<&str>,
    ) -> InteractionEvent {
        InteractionEvent::Drag {
            t,
            t1,
            x0: from[0],
            y0: from[1],
            x1: to[0],
            y1: to[1],
            label: label.map(Into::into),
            button: None,
            path: Vec::new(),
        }
    }

    struct Case<'a> {
        words: Vec<WordTiming>,
        events: &'a [InteractionEvent],
        marks: &'a [Mark],
        duration_ms: u64,
        scenes: Option<&'a [u64]>,
        groups: &'a [Vec<usize>],
    }

    impl<'a> Case<'a> {
        fn new(words: Vec<WordTiming>, duration_ms: u64) -> Self {
            Self {
                words,
                events: &[],
                marks: &[],
                duration_ms,
                scenes: None,
                groups: &[],
            }
        }

        fn lines(&self) -> Vec<Line> {
            build_for(
                &transcript(self.words.clone()),
                self.events,
                self.marks,
                &[],
                self.duration_ms,
                self.scenes,
                self.groups,
            )
        }

        fn text(&self) -> Vec<String> {
            self.lines()
                .iter()
                .enumerate()
                .map(|(i, line)| summary(line, i, self.marks))
                .collect()
        }
    }

    fn summary(line: &Line, index: usize, marks: &[Mark]) -> String {
        let head = format!(
            "- #{} ({}){}",
            index + 1,
            format_clock(line.frame_t),
            if line.flagged() { " ⚑" } else { "" }
        );
        let said = (!line.text.is_empty()).then_some(line.text.as_str());
        summary_text(&head, said, line.flagged(), &line.narrated(marks))
    }

    fn sentences_of(words: Vec<WordTiming>) -> Vec<String> {
        let units: Vec<_> = words.into_iter().map(|w| (w.t0, w.t1, w.text)).collect();
        split_sentences(&units)
            .into_iter()
            .map(|s| join(&s.words))
            .collect()
    }

    #[test]
    fn a_thinking_pause_and_a_wrong_full_stop_stay_inside_the_sentence() {
        let mut words = say("Ich will hier bitte", 0);
        words.extend(say("eine andere Ansicht. der einzelnen Tage.", 3_000));
        words.extend(say("Neuer Satz.", 6_000));
        assert_eq!(
            sentences_of(words),
            [
                "Ich will hier bitte eine andere Ansicht. der einzelnen Tage.",
                "Neuer Satz."
            ]
        );
    }

    #[test]
    fn an_unfinished_sentence_ends_after_a_long_silence() {
        let mut words = say("Ich will hier", 0);
        words.extend(say("Neuer Satz.", 10_000));
        assert_eq!(sentences_of(words).len(), 2);
    }

    #[test]
    fn click_bursts_fold_and_the_hands_small_drags_drop() {
        let actions = fold_actions(
            &[
                click(1_000, 0.1, 0.1),
                click(1_500, 0.2, 0.1),
                click(2_900, 0.3, 0.1),
                click(9_000, 0.5, 0.5),
                drag(12_000, 12_200, [0.5, 0.5], [0.51, 0.5], None),
                drag(13_000, 13_400, [0.5, 0.5], [0.1, 0.5], None),
            ],
            &[],
            &[],
        );
        let shape: Vec<(Kind, u64, u64, usize)> = actions
            .iter()
            .map(|a| {
                let n = match &a.detail {
                    Detail::Clicks { targets, .. } => targets.len(),
                    _ => 0,
                };
                (a.kind, a.t0, a.t1, n)
            })
            .collect();
        assert_eq!(
            shape,
            [
                (Kind::Clicks, 1_000, 2_900, 3),
                (Kind::Clicks, 9_000, 9_000, 1),
                (Kind::Drag, 13_000, 13_400, 0)
            ]
        );
        assert_eq!(actions[2].detail, Detail::Text("dragged left".into()));
    }

    #[test]
    fn a_sentence_is_never_cut_at_a_click_and_no_word_click_or_mark_is_lost() {
        let words = say(
            "Bitte diese Boxen hier entfernen und den Text darunter auch.",
            0,
        );
        let marks = [mark(900, 1_100, Shape::Mark)];
        let case = Case {
            events: &[click(600, 0.1, 0.1), click(2_000, 0.4, 0.4)],
            marks: &marks,
            ..Case::new(words.clone(), 4_000)
        };
        let lines = case.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, joined(&words));
        let mut kinds: Vec<_> = lines[0].actions.iter().map(|a| a.kind.score()).collect();
        kinds.sort();
        assert_eq!(kinds, [Kind::Clicks.score(), Kind::Mark.score()]);
        assert_eq!(
            case.text(),
            [
                "- #1 (0:01) [marked, clicked ×2] “Bitte diese Boxen hier entfernen und den Text darunter auch.”"
            ]
        );
    }

    #[test]
    fn a_click_the_screen_answered_is_told_from_a_hand_that_was_pointing() {
        let case = Case {
            events: &[
                click(100, 0.1, 0.1),
                click(200, 0.2, 0.1),
                click(5_000, 0.5, 0.5),
            ],
            scenes: Some(&[5_400]),
            ..Case::new(say("Das hier und das hier.", 0), 7_000)
        };
        let text = case.text();
        assert!(text[0].contains("[pointed ×2]"), "{text:?}");
        assert_eq!(text[1], "- #2 (0:05) clicked");
    }

    #[test]
    fn a_keyframe_without_words_or_actions_says_what_it_shows() {
        let case = Case {
            scenes: Some(&[2_000]),
            ..Case::new(Vec::new(), 3_000)
        };
        assert_eq!(case.text(), ["- #1 (0:02) screen changed"]);
        assert_eq!(
            Case::new(Vec::new(), 4_000).text(),
            ["- #1 (0:02) nothing said or done"]
        );
    }

    #[test]
    fn speech_the_model_could_not_time_stays_whole() {
        let transcript = TranscriptData {
            source: "test".into(),
            text: "Das hier bitte weg. Und das auch.".into(),
            ..Default::default()
        };
        let lines = build_for(
            &transcript,
            &[click(2_000, 0.5, 0.5)],
            &[],
            &[],
            8_000,
            None,
            &[],
        );
        let texts: Vec<_> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["Das hier bitte weg. Und das auch."]);
    }

    #[test]
    fn a_click_over_no_speech_gets_its_own_line() {
        let marks = [mark(100, 300, Shape::Mark)];
        let case = Case {
            events: &[click(20_000, 0.5, 0.5)],
            marks: &marks,
            ..Case::new(say("Hallo Welt.", 0), 25_000)
        };
        assert_eq!(case.text().last().unwrap(), "- #2 (0:20) clicked");
    }

    #[test]
    fn a_group_of_marks_is_one_keyframe_numbered_without_cutting_a_sentence() {
        let mut words = say("Erst etwas Einleitung.", 0);
        words.extend(say("Dieses Feld hier soll weg.", 2_000));
        words.extend(say("Und dann noch dieser.", 5_000));
        words.extend(say("Das hier auch noch.", 7_000));
        let mut underline = mark(3_000, 3_200, Shape::Underline);
        underline.strokes = vec![1, 2];
        let marks = [
            mark(2_200, 2_800, Shape::Circle),
            underline,
            mark(7_200, 7_800, Shape::Arrow),
        ];
        let case = Case {
            events: &[click(12_000, 0.5, 0.5)],
            marks: &marks,
            scenes: Some(&[4_000]),
            groups: &[vec![0, 1, 2]],
            ..Case::new(words.clone(), 14_000)
        };
        let lines = case.lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].frame_t, 7_800);
        assert_eq!(lines[0].text, joined(&words));
        assert_eq!(
            case.text()[0],
            [
                "- #1 (0:07) “Erst etwas Einleitung.”",
                "  ✎① 0:02 circled",
                "  ✎② 0:03 underlined (2 strokes) “Dieses Feld hier soll weg. Und dann noch dieser.”",
                "  ✎③ 0:07 drew an arrow “Das hier auch noch.”",
            ]
            .join("\n")
        );
    }

    #[test]
    fn a_lone_mark_is_named_by_its_shape_and_unnumbered() {
        let marks = [mark(100, 300, Shape::Circle)];
        let case = Case {
            marks: &marks,
            ..Case::new(say("Das hier.", 0), 2_000)
        };
        assert_eq!(case.text(), ["- #1 (0:00) [circled] “Das hier.”"]);
    }

    #[test]
    fn a_click_names_what_it_landed_on_and_still_says_pointed_when_nothing_answered() {
        let mut saved = mark(400, 500, Shape::Mark);
        saved.on = Some("button “Save”".into());
        let marks = [saved];
        let case = Case {
            events: &[
                named_click(100, "button", "Area"),
                named_click(200, "button", "Area"),
                click(300, 0.2, 0.1),
                named_click(9_000, "tab item", "Files"),
            ],
            marks: &marks,
            scenes: Some(&[9_400]),
            ..Case::new(say("Das hier und das hier.", 0), 12_000)
        };
        let text = case.text();
        assert!(
            text[0].contains("[marked button “Save”, pointed at button “Area” ×2, pointed]"),
            "{text:?}"
        );
        assert_eq!(text[1], "- #2 (0:09) clicked tab item “Files”");
    }

    #[test]
    fn shortcuts_and_app_switches_are_said_in_place_without_an_image_or_a_cut() {
        let sentence = "Bitte diese Boxen hier entfernen und den Text darunter auch.";
        let case = Case {
            events: &[
                click(300, 0.1, 0.1),
                key(600, "Ctrl+C"),
                key(900, "Ctrl+C"),
                InteractionEvent::AppChange {
                    t: 1_200,
                    app: "Helium".into(),
                    window_title: None,
                },
            ],
            ..Case::new(say(sentence, 0), 4_000)
        };
        let lines = case.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, sentence);
        assert_eq!(lines[0].frame_t, 600);
        assert!(
            case.text()[0].contains("[clicked, pressed Ctrl+C ×2, switched to Helium]"),
            "{:?}",
            case.text()
        );
    }

    #[test]
    fn a_shortcut_in_a_silence_gets_no_frame_and_joins_the_keyframe_before() {
        let marks = [mark(100, 300, Shape::Mark)];
        let case = Case {
            events: &[key(20_000, "Ctrl+S")],
            marks: &marks,
            ..Case::new(say("Hallo Welt.", 0), 25_000)
        };
        let lines = case.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].frame_t, 300);
        assert!(case.text()[0].contains("pressed Ctrl+S"));
    }

    #[test]
    fn the_wheel_says_its_direction_and_the_screen_answering_it_is_no_scene() {
        let scroll = |t, dy| InteractionEvent::Scroll {
            t,
            x: 0.5,
            y: 0.5,
            dx: 0.0,
            dy,
            label: None,
        };
        let case = Case {
            events: &[
                scroll(1_000, 1.0),
                scroll(1_200, 1.0),
                scroll(9_000, -1.0),
                scroll(9_200, 1.0),
            ],
            scenes: Some(&[1_100, 1_900, 9_300]),
            ..Case::new(Vec::new(), 12_000)
        };
        assert_eq!(
            case.text(),
            [
                "- #1 (0:01) scrolled down",
                "- #2 (0:09) scrolled up and down"
            ]
        );
    }

    #[test]
    fn a_flag_gets_its_own_keyframe_and_marker_and_is_told_from_a_mark() {
        let flag = [mark(20_000, 20_000, Shape::Flag)];
        let said = Case {
            marks: &flag,
            ..Case::new(say("Hallo Welt.", 19_800), 25_000)
        };
        assert_eq!(said.text(), ["- #1 (0:20) ⚑ “Hallo Welt.”"]);
        let silent = Case {
            marks: &flag,
            ..Case::new(Vec::new(), 25_000)
        };
        assert_eq!(silent.text(), ["- #1 (0:20) ⚑ flagged this moment"]);
    }

    #[test]
    fn a_named_drag_is_said_however_short_and_a_scrub_without_its_direction() {
        let case = Case {
            events: &[
                drag(100, 200, [0.5, 0.5], [0.51, 0.5], Some("slider “Volume”")),
                drag(400, 500, [0.5, 0.5], [0.6, 0.5], Some("button “Tab”")),
                drag(700, 800, [0.5, 0.5], [0.51, 0.5], None),
            ],
            ..Case::new(Vec::new(), 1_000)
        };
        assert_eq!(
            case.text(),
            ["- #1 (0:00) scrubbed slider “Volume”, dragged button “Tab” right"]
        );
    }

    #[test]
    fn every_mark_over_budget_keeps_its_keyframe_and_speech_ahead_of_the_first_joins_it() {
        let marks: Vec<Mark> = (0..9u64)
            .map(|i| mark(2_000 + i * 2_000, 2_100 + i * 2_000, Shape::Mark))
            .collect();
        let mut words = say_at("Erster Satz ohne Aktion.", 0, 100);
        for m in &marks {
            words.extend(say_at("Das hier markiert.", m.t0, 100));
        }
        let case = Case {
            marks: &marks,
            ..Case::new(words, 22_000)
        };
        let lines = case.lines();
        assert_eq!(lines.len(), 9);
        assert_eq!(lines[0].text, "Erster Satz ohne Aktion. Das hier markiert.");
    }

    fn acted(times: &[u64]) -> (Vec<WordTiming>, Vec<InteractionEvent>) {
        let words = times
            .iter()
            .flat_map(|&t| say_at("Hier klicke ich jetzt.", t, 100))
            .collect();
        let events = times.iter().map(|&t| click(t + 100, 0.5, 0.5)).collect();
        (words, events)
    }

    #[test]
    fn ordinary_clicks_fill_only_the_base_budget() {
        let times: Vec<u64> = (0..12).map(|i| 500 + i * 3_000).collect();
        let (words, events) = acted(&times);
        for duration_ms in [39_999, 40_000, 139_999, 600_000] {
            let case = Case {
                events: &events,
                ..Case::new(words.clone(), duration_ms)
            };
            assert_eq!(case.lines().len(), BASE_KEYFRAMES, "{duration_ms} ms");
        }
    }

    #[test]
    fn the_budget_past_the_base_goes_to_long_stretches_that_show_something() {
        let late = [50_000, 90_000, 130_000];
        let mut times: Vec<u64> = (0..6).map(|i| 500 + i * 2_000).collect();
        times.extend(late);
        let (words, events) = acted(&times);
        let case = Case {
            events: &events,
            ..Case::new(words, 140_000)
        };
        let starts: Vec<u64> = case.lines().iter().map(|l| l.t0).collect();
        for t in late {
            assert!(starts.contains(&t), "{starts:?}");
        }
    }

    #[test]
    fn long_speech_over_a_screen_where_nothing_happens_takes_no_keyframe() {
        let words: Vec<WordTiming> = (0..6u64)
            .flat_map(|i| {
                say_at(
                    "Ich rede hier einfach weiter ohne etwas zu tun.",
                    i * 20_000,
                    200,
                )
            })
            .collect();
        let n = words.len();
        let lines = Case::new(words, 120_000).lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text.split(' ').count(), n);
    }

    #[test]
    fn a_laser_point_reads_as_pointing() {
        let stroke = Stroke {
            t0: 300,
            t1: 600,
            fw: 100,
            fh: 100,
            on: Some("Header".into()),
            pin: None,
            laser: true,
            pts: vec![1000, 1000, 0],
        };
        let lines = build_for(
            &transcript(say("Das da.", 0)),
            &[],
            &[],
            &[stroke],
            3_000,
            None,
            &[],
        );
        assert_eq!(
            summary(&lines[0], 0, &[]),
            "- #1 (0:00) [pointed at “Header”] “Das da.”"
        );
    }
}
