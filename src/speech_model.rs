//! Apple's on-device speech model. A fresh Mac doesn't have it, and each app has to reserve it
//! for itself (`AssetInventory`, attributed to `scrawl-helper`), so Scrawl checks and installs
//! it in the background at launch (`scrawl-helper speech-model --install`) instead of during the
//! first capture. Onboarding and Settings show how that's going (`view`).
//!
//! A capture that finishes without a model says so in its summary (`TranscriptData::missing`,
//! from `missing`). When the reason can pass (still downloading, a failed download), the bundle
//! gets a `.transcript-pending` marker, and once the model is installed it's transcribed again
//! in place: `scrawl.json` and `summary.md` are rewritten with the transcript.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::model::MissingTranscript;

/// Marks a bundle that should be transcribed again once the model is installed.
pub const PENDING: &str = ".transcript-pending";

/// Waits between failed installs; after the last one it stops until the next launch.
const RETRY_AFTER: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(30 * 60),
];

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    /// Not checked yet (or the configured transcriber is Hex).
    #[default]
    Unknown,
    Checking,
    Downloading,
    Ready,
    /// No Apple speech model for this Mac and language.
    Unavailable,
    Failed,
}

/// What onboarding and Settings show.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub state: State,
    /// `SpeechTranscriber`, or `DictationTranscriber` on Macs without SpeechTranscriber.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// `en_US`; the helper window shows its name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// 0…1 while downloading.
    pub progress: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

static STATUS: Mutex<Status> = Mutex::new(Status {
    state: State::Unknown,
    engine: None,
    locale: None,
    progress: 0.0,
    error: None,
});
static RUNNING: AtomicBool = AtomicBool::new(false);

pub fn current() -> Status {
    STATUS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn update(change: impl FnOnce(&mut Status)) {
    change(
        &mut STATUS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
}

/// Checks the model in the background and installs it if it's missing, unless the transcriber
/// is Hex (whose users don't need it) or a check is already running. Cheap to call again.
pub fn ensure() {
    if crate::transcribe::Engine::configured() == crate::transcribe::Engine::Hex {
        return;
    }
    if settled(&current()) || RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("scrawl-speech-model".into())
        .spawn(|| {
            work();
            RUNNING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        RUNNING.store(false, Ordering::Release);
    }
}

/// Nothing left to install. On Windows the model follows the transcription language, so a model
/// that's ready for another language (Settings changed it) doesn't count: `work` checks again,
/// which costs nothing when the right model is already there.
fn settled(status: &Status) -> bool {
    match status.state {
        State::Unavailable => true,
        State::Ready if cfg!(target_os = "windows") => {
            status.locale.as_deref() == Some(crate::handoff::transcription_language().as_str())
        }
        State::Ready => true,
        _ => false,
    }
}

fn work() {
    for attempt in 0..=RETRY_AFTER.len() {
        update(|s| {
            if s.state != State::Downloading {
                s.state = State::Checking;
            }
        });
        let result = crate::helper::install_speech_model(|line| {
            update(|s| {
                s.engine = line.engine.clone().or(s.engine.take());
                s.locale = line.locale.clone().or(s.locale.take());
                s.state = match line.status.as_str() {
                    "installed" => State::Ready,
                    "unsupported" => State::Unavailable,
                    "failed" => State::Failed,
                    _ => State::Downloading,
                };
                s.progress = line.progress.unwrap_or(s.progress);
                s.error = line.error.clone();
            });
        });
        let status = current();
        if let Err(error) = result {
            update(|s| {
                s.state = State::Failed;
                s.error = Some(format!("{error:#}"));
            });
        }
        crate::capture::scrawl_log(format!(
            "speech model: {:?} {} {}{}",
            current().state,
            status.engine.as_deref().unwrap_or("?"),
            status.locale.as_deref().unwrap_or("?"),
            current()
                .error
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        ));
        match current().state {
            State::Ready => {
                transcribe_pending(&crate::handoff::default_captures_dir());
                return;
            }
            State::Unavailable => return,
            _ => {}
        }
        if let Some(wait) = RETRY_AFTER.get(attempt) {
            std::thread::sleep(*wait);
        }
    }
}

/// The plain sentence for a capture without a transcript, from the helper's `missing`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn missing(why: &str, locale: &str, error: Option<&str>) -> MissingTranscript {
    let model = if cfg!(target_os = "windows") {
        format!("The Parakeet speech model ({locale})")
    } else {
        format!("Apple's speech model ({locale})")
    };
    match why {
        "downloading" => MissingTranscript {
            reason: format!(
                "{model} was still downloading. The audio is saved, and Scrawl adds the \
                 transcript to this bundle when the download finishes."
            ),
            retry: true,
        },
        "unsupported" => MissingTranscript {
            reason: format!(
                "this Mac has no Apple speech model for {locale}. Hex can transcribe instead \
                 (Scrawl Settings › Recording)."
            ),
            retry: false,
        },
        _ => MissingTranscript {
            reason: format!(
                "{model} couldn't be installed{}. The audio is saved, and Scrawl adds the \
                 transcript to this bundle if a later try works.",
                error.map(|e| format!(" ({e})")).unwrap_or_default()
            ),
            retry: true,
        },
    }
}

/// Marks a freshly written bundle whose transcript can come later; if the model has arrived
/// meanwhile, transcribes it now.
pub fn mark_pending(bundle_dir: &Path) {
    let _ = std::fs::write(bundle_dir.join(PENDING), b"");
    if current().state == State::Ready {
        let dir = bundle_dir.to_path_buf();
        std::thread::spawn(move || transcribe_one(&dir));
    }
}

/// Every marked bundle in `captures`.
pub fn transcribe_pending(captures: &Path) {
    let Ok(entries) = std::fs::read_dir(captures) else {
        return;
    };
    let pending: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|dir| dir.join(PENDING).is_file())
        .collect();
    for dir in pending {
        transcribe_one(&dir);
    }
}

fn transcribe_one(dir: &Path) {
    match transcribe_again(dir) {
        Ok(done) => crate::capture::scrawl_log(format!(
            "transcribed again {}: {}",
            dir.display(),
            if done { "done" } else { "still no model" }
        )),
        Err(error) => {
            crate::capture::scrawl_log(format!(
                "transcribing {} again failed: {error:#}",
                dir.display()
            ));
            let _ = std::fs::remove_file(dir.join(PENDING));
        }
    }
}

/// Transcribes a bundle's `audio.wav` with Apple Speech and rewrites its transcript, each
/// keyframe's words, `scrawl.json` and `summary.md`. Returns `false` (and keeps the marker)
/// while there's still no model.
pub fn transcribe_again(dir: &Path) -> Result<bool> {
    let (dir, mut bundle) = crate::handoff::load_bundle(dir)?;
    let wav = dir.join(bundle.audio.as_deref().unwrap_or("audio.wav"));
    anyhow::ensure!(wav.is_file(), "the bundle has no audio.wav any more");
    let transcript = crate::helper::transcribe(&wav)?;
    if transcript
        .missing
        .as_ref()
        .is_some_and(|missing| missing.retry)
    {
        return Ok(false);
    }
    let times: Vec<u64> = bundle.keyframes.iter().map(|k| k.t).collect();
    let said = crate::keyframes::said_per_keyframe(&times, &[], &transcript);
    for (frame, said) in bundle.keyframes.iter_mut().zip(said) {
        frame.said = said;
    }
    bundle.transcript = transcript;
    std::fs::write(
        dir.join("scrawl.json"),
        serde_json::to_vec_pretty(&bundle).context("Could not encode scrawl.json")?,
    )?;
    std::fs::write(
        dir.join("summary.md"),
        crate::sketch::summary_markdown(&bundle, &dir),
    )?;
    let _ = std::fs::remove_file(dir.join(PENDING));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_ready_for_another_language_is_checked_again_on_windows() {
        let status = |state: State, locale: &str| Status {
            state,
            locale: Some(locale.into()),
            ..Default::default()
        };
        let language = crate::handoff::transcription_language();
        assert!(settled(&status(State::Ready, &language)));
        assert_eq!(
            settled(&status(State::Ready, "xx")),
            cfg!(not(target_os = "windows"))
        );
        assert!(settled(&status(State::Unavailable, "xx")));
        assert!(!settled(&status(State::Downloading, &language)));
        assert!(!settled(&status(State::Failed, &language)));
    }

    #[test]
    fn a_download_in_progress_will_be_retried_but_a_missing_language_will_not() {
        let downloading = missing("downloading", "en_US", None);
        assert!(downloading.retry);
        assert!(downloading.reason.contains("still downloading"));
        let unsupported = missing("unsupported", "xx_XX", None);
        assert!(!unsupported.retry);
        assert!(
            unsupported
                .reason
                .contains("no Apple speech model for xx_XX")
        );
        let failed = missing("failed", "en_US", Some("asset unavailable"));
        assert!(failed.retry && failed.reason.contains("(asset unavailable)"));
    }
}
