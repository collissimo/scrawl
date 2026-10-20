//! On-device transcription on Windows, with measured word timings: Parakeet through
//! `transcribe.cpp`, the engine Hex uses. It loads a model Hex has already installed
//! (`%APPDATA%\voice-control\models`), else Scrawl's own copy (`%LOCALAPPDATA%\Scrawl\models`),
//! which `install` downloads once in the background and checks against its SHA-256. English
//! (`handoff::transcription_language`) uses Hex's Parakeet Unified English; any other language
//! needs a multilingual model, Parakeet TDT 0.6b v3 (25 European languages). Like the Hex client, it transcribes each speech island on its own; unlike
//! it, every word keeps the time the model heard it (`estimated: false`), so spoken cues ("look
//! here") can cut keyframes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use transcribe_cpp::{Model, RunOptions, TimestampKind};

use crate::helper::SpeechModelLine;
use crate::model::{TranscriptData, TranscriptSegment, WordTiming};

const KHZ: u64 = 16;
/// Silence kept around each clip, so first and last words aren't clipped.
const CLIP_PAD_MS: u64 = 200;

/// A model Scrawl downloads when Hex hasn't installed one that speaks the language.
pub struct Artifact {
    pub filename: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// English: the same file, revision and checksum as Hex's `PARAKEET_UNIFIED_ENGLISH_ARTIFACT`.
pub const PARAKEET: Artifact = Artifact {
    filename: "parakeet-unified-en-0.6b-Q8_0.gguf",
    url: "https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/7e948f21b7bdbac698d3318db9d350f1096f3b6c/parakeet-unified-en-0.6b-Q8_0.gguf",
    bytes: 731_357_568,
    sha256: "4b50b6dd862bf6e346929aaf4f5eaacec003bfa3f56462d6c874b41ef2f38795",
};

/// Every other language: Parakeet TDT 0.6b v3 (bg, cs, da, de, el, en, es, et, fi, fr, hr, hu,
/// it, lt, lv, mt, nl, pl, pt, ro, ru, sk, sl, sv, uk), Q8_0 like the English one.
pub const PARAKEET_V3: Artifact = Artifact {
    filename: "parakeet-tdt-0.6b-v3-Q8_0.gguf",
    url: "https://huggingface.co/handy-computer/parakeet-tdt-0.6b-v3-gguf/resolve/d8d8c32cb2017c57e5307767343e97870f8a7f53/parakeet-tdt-0.6b-v3-Q8_0.gguf",
    bytes: 739_508_576,
    sha256: "5859f77944efcd8eafa23a6350731960b2b55b2203df51f319665c807d802cc7",
};

/// The model Scrawl downloads for `language`.
pub fn artifact_for(language: &str) -> &'static Artifact {
    if language == "en" {
        &PARAKEET
    } else {
        &PARAKEET_V3
    }
}

fn models_dir() -> PathBuf {
    crate::hex::support_dir().join("models")
}

/// Where Scrawl keeps the model it downloaded (`SCRAWL_MODELS_DIR` for tests).
pub fn own_models_dir() -> PathBuf {
    std::env::var_os("SCRAWL_MODELS_DIR")
        .map(PathBuf::from)
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| crate::win::local_dir().join("models"))
}

fn receipt_path(model: &Path) -> PathBuf {
    model.with_file_name(format!(
        "{}.verified.json",
        model.file_name().unwrap_or_default().to_string_lossy()
    ))
}

/// `.gguf` files in `dir` that have a `.verified.json` receipt beside them.
fn verified_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "gguf") && receipt_path(path).is_file()
        })
        .collect()
}

/// Hex's installed models (verified downloads), Hex's chosen one first, then Parakeet ones;
/// then the ones Scrawl downloaded.
pub fn installed_models() -> Vec<PathBuf> {
    let mut models = hex_models();
    models.extend(verified_in(&own_models_dir()));
    models
}

/// The installed models that can transcribe `language`, in `installed`'s order. Every model
/// speaks English; another language needs one whose name says it's multilingual (Parakeet v3,
/// Whisper), so Parakeet Unified English and Parakeet v2 never get German speech.
pub fn models_for(installed: &[PathBuf], language: &str) -> Vec<PathBuf> {
    installed
        .iter()
        .filter(|path| language == "en" || multilingual(path))
        .cloned()
        .collect()
}

fn multilingual(path: &Path) -> bool {
    let name = model_name(path).to_ascii_lowercase();
    let english_only = name
        .split(['-', '_', '.'])
        .any(|part| part == "en" || part == "v2");
    !english_only && (name.contains("v3") || name.contains("whisper"))
}

fn hex_models() -> Vec<PathBuf> {
    let chosen = std::fs::read(crate::hex::support_dir().join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|settings| {
            settings["transcription"]["model"]
                .as_str()
                .map(str::to_owned)
        })
        .map(|id| id.replace('_', "-"));
    let mut models = verified_in(&models_dir());
    let rank = |path: &PathBuf| {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let picked = chosen.as_deref().is_some_and(|id| name.starts_with(id));
        (!picked, !name.starts_with("parakeet"), name.into_owned())
    };
    models.sort_by_key(rank);
    models
}

/// The model's name for `TranscriptData.model`: its file name without the extension.
fn model_name(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn transcribe(audio: &Path) -> Result<TranscriptData> {
    let language = crate::handoff::transcription_language();
    let candidates = models_for(&installed_models(), &language);
    if candidates.is_empty() {
        // The background download hasn't finished (or failed): the bundle is transcribed again
        // once it has.
        let status = crate::speech_model::current();
        let why = match status.state {
            crate::speech_model::State::Failed => "failed",
            _ => "downloading",
        };
        return Ok(TranscriptData {
            source: "offline".into(),
            missing: Some(crate::speech_model::missing(
                why,
                &language,
                status.error.as_deref(),
            )),
            ..Default::default()
        });
    }
    let wav =
        std::fs::read(audio).with_context(|| format!("Could not read {}", audio.display()))?;
    let samples = crate::audio::decode_wav_16k(&wav)?;
    let duration_ms = samples.len() as u64 / KHZ;
    let mut clips = crate::hex::plan_clips(&crate::audio::speech_islands(&samples), duration_ms);
    if clips.is_empty() && duration_ms >= 480 {
        clips.push((0, duration_ms));
    }
    let named = |path: &Path| {
        let name = model_name(path);
        TranscriptData {
            source: format!("parakeet:{name}"),
            model: Some(name),
            estimated: false,
            ..Default::default()
        }
    };
    if clips.is_empty() {
        return Ok(named(&candidates[0]));
    }

    transcribe_cpp::disable_logging();
    transcribe_cpp::init_backends_default().context("transcribe.cpp backends")?;
    let (path, model, hint) = load_for(&candidates, &language)?;
    let mut transcript = named(&path);
    let mut session = model.session()?;
    let options = RunOptions {
        timestamps: TimestampKind::Word,
        language: hint,
        ..Default::default()
    };
    let mut texts = Vec::new();
    for (start, end) in clips {
        let from_ms = start.saturating_sub(CLIP_PAD_MS);
        let from = (from_ms * KHZ) as usize;
        let to = (((end + CLIP_PAD_MS) * KHZ) as usize).min(samples.len());
        if from >= to {
            continue;
        }
        let result = session.run(&samples[from..to], &options)?;
        let text = result.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            continue;
        }
        let at = |ms: i64| from_ms + ms.max(0) as u64;
        let words: Vec<WordTiming> = result
            .words
            .iter()
            .filter(|word| !word.text.trim().is_empty())
            .map(|word| WordTiming {
                t0: at(word.t0_ms),
                t1: at(word.t1_ms.max(word.t0_ms)),
                text: word.text.trim().to_owned(),
            })
            .collect();
        if words.is_empty() {
            // No word times from this model: spread the text over the clip, and say so.
            let (segments, words) = crate::hex::spread_words(&text, start, end);
            transcript.estimated = true;
            transcript.segments.extend(segments);
            transcript.words.extend(words);
        } else {
            let segments: Vec<TranscriptSegment> = result
                .segments
                .iter()
                .filter(|s| !s.text.trim().is_empty())
                .map(|s| TranscriptSegment {
                    t0: at(s.t0_ms),
                    t1: at(s.t1_ms.max(s.t0_ms)),
                    text: s.text.trim().to_owned(),
                })
                .collect();
            transcript.segments.extend(if segments.is_empty() {
                vec![TranscriptSegment {
                    t0: words[0].t0,
                    t1: words[words.len() - 1].t1,
                    text: text.clone(),
                }]
            } else {
                segments
            });
            transcript.words.extend(words);
        }
        texts.push(text);
    }
    transcript.text = texts.join(" ");
    Ok(transcript)
}

/// Loads the first candidate whose own language list (`general.languages`, when it has one)
/// includes `language`, with the language as the hint. When every candidate lists others, the
/// last one runs without a hint and detects the language itself.
fn load_for(candidates: &[PathBuf], language: &str) -> Result<(PathBuf, Model, Option<String>)> {
    let mut last = None;
    for path in candidates {
        let model =
            Model::load(path).with_context(|| format!("Could not load {}", path.display()))?;
        let languages = model.capabilities().languages;
        if languages.is_empty() || languages.iter().any(|l| l == language) {
            return Ok((path.clone(), model, Some(language.to_owned())));
        }
        crate::capture::scrawl_log(format!(
            "{} doesn't list {language} ({})",
            path.display(),
            languages.join(", ")
        ));
        last = Some((path.clone(), model));
    }
    let (path, model) = last.context("no speech model")?;
    Ok((path, model, None))
}

/// Makes sure a model for the transcription language is on disk: `installed` when Hex's or
/// Scrawl's own is there, else downloads `artifact_for` it into `own_models_dir()`, reporting
/// progress, and keeps it only if its SHA-256 matches. The same lines as macOS's
/// `scrawl-helper speech-model --install`, with the language as the locale.
pub fn install(mut line: impl FnMut(SpeechModelLine)) -> Result<()> {
    let language = crate::handoff::transcription_language();
    let report = |status: &str, progress: Option<f64>, error: Option<String>| SpeechModelLine {
        status: status.into(),
        engine: Some("Parakeet".into()),
        locale: Some(language.clone()),
        progress,
        error,
    };
    if models_for(&installed_models(), &language).is_empty() {
        let artifact = artifact_for(&language);
        let mut progress = |done: f64| line(report("downloading", Some(done), None));
        if let Err(error) = download(artifact, &own_models_dir(), &mut progress) {
            line(report("failed", None, Some(format!("{error:#}"))));
            return Ok(());
        }
    }
    line(report("installed", Some(1.0), None));
    Ok(())
}

/// Downloads `artifact` into `dir` (resuming a `.partial` file), checks it, and writes the
/// `.verified.json` receipt that `installed_models` looks for. A file that fails the check is
/// deleted, so the next try starts over.
pub fn download(artifact: &Artifact, dir: &Path, progress: &mut dyn FnMut(f64)) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("Could not create {}", dir.display()))?;
    let model = dir.join(artifact.filename);
    if !model.is_file() {
        let partial = dir.join(format!("{}.partial", artifact.filename));
        if std::fs::metadata(&partial).map_or(0, |m| m.len()) < artifact.bytes {
            fetch(artifact.url, &partial, artifact.bytes, progress)?;
        }
        std::fs::rename(&partial, &model)?;
    }
    if let Err(error) = verify(&model, artifact) {
        let _ = std::fs::remove_file(&model);
        return Err(error);
    }
    let receipt = serde_json::json!({
        "sha256": artifact.sha256,
        "bytes": artifact.bytes,
        "url": artifact.url,
    });
    std::fs::write(receipt_path(&model), receipt.to_string())?;
    Ok(model)
}

fn verify(path: &Path, artifact: &Artifact) -> Result<()> {
    use sha2::{Digest, Sha256};
    let len = std::fs::metadata(path)?.len();
    if len != artifact.bytes {
        bail!(
            "{} is {len} bytes, expected {}",
            artifact.filename,
            artifact.bytes
        );
    }
    let mut hasher = Sha256::new();
    std::io::copy(&mut std::fs::File::open(path)?, &mut hasher)?;
    let sha256 = format!("{:x}", hasher.finalize());
    if sha256 != artifact.sha256 {
        bail!("{} failed its checksum ({sha256})", artifact.filename);
    }
    Ok(())
}

/// Windows' own `curl.exe` (in System32 since Windows 10 1803), without a console window.
fn fetch(url: &str, partial: &Path, total: u64, progress: &mut dyn FnMut(f64)) -> Result<()> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let curl = std::env::var_os("SystemRoot")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join(r"System32\curl.exe");
    let mut child = std::process::Command::new(&curl)
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--retry",
            "3",
            "--continue-at",
            "-",
            "--output",
        ])
        .arg(partial)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("Could not run {}", curl.display()))?;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                progress(1.0);
                return Ok(());
            }
            let mut error = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut error);
            }
            bail!("the download failed ({status}): {}", error.trim());
        }
        let have = std::fs::metadata(partial).map_or(0, |m| m.len());
        progress((have as f64 / total as f64).min(0.99));
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A five-byte artifact whose checksum is `sha256("hello")`.
    const HELLO: Artifact = Artifact {
        filename: "fixture.gguf",
        url: "http://127.0.0.1:9/never-fetched",
        bytes: 5,
        sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    };

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scrawl-speech-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_finished_download_that_matches_gets_a_receipt_and_counts_as_installed() {
        let dir = temp_dir("good");
        std::fs::write(dir.join("fixture.gguf.partial"), b"hello").unwrap();
        let model = download(&HELLO, &dir, &mut |_| {}).unwrap();
        assert_eq!(model, dir.join("fixture.gguf"));
        assert!(receipt_path(&model).is_file());
        assert_eq!(verified_in(&dir), vec![model]);
    }

    #[test]
    fn a_download_that_fails_its_checksum_is_deleted_and_not_installed() {
        let dir = temp_dir("bad");
        std::fs::write(dir.join("fixture.gguf.partial"), b"hellp").unwrap();
        let error = download(&HELLO, &dir, &mut |_| {}).unwrap_err();
        assert!(format!("{error:#}").contains("checksum"));
        assert!(!dir.join("fixture.gguf").exists());
        assert!(verified_in(&dir).is_empty());
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn english_takes_any_model_in_order() {
        let installed = paths(&[
            r"hex\parakeet-unified-en-0.6b-Q8_0.gguf",
            r"own\parakeet-tdt-0.6b-v3-Q8_0.gguf",
        ]);
        assert_eq!(models_for(&installed, "en"), installed);
        assert_eq!(artifact_for("en").filename, PARAKEET.filename);
    }

    #[test]
    fn german_never_gets_an_english_only_model() {
        let english = paths(&[
            r"hex\parakeet-unified-en-0.6b-Q8_0.gguf",
            r"hex\parakeet-tdt-0.6b-v2-Q8_0.gguf",
            r"hex\whisper-small.en-Q8_0.gguf",
        ]);
        assert!(models_for(&english, "de").is_empty());
        assert_eq!(artifact_for("de").filename, PARAKEET_V3.filename);
        assert_eq!(artifact_for("fr").filename, PARAKEET_V3.filename);
    }

    #[test]
    fn german_prefers_hexs_multilingual_model_then_scrawls_own() {
        let installed = paths(&[
            r"hex\parakeet-unified-en-0.6b-Q8_0.gguf",
            r"hex\parakeet-v3-Q8_0.gguf",
            r"own\parakeet-tdt-0.6b-v3-Q8_0.gguf",
        ]);
        let picked = paths(&[r"hex\parakeet-v3-Q8_0.gguf", r"own\parakeet-tdt-0.6b-v3-Q8_0.gguf"]);
        assert_eq!(models_for(&installed, "de"), picked);
        assert_eq!(models_for(&installed, "fr"), picked);
        let whisper = paths(&[r"hex\whisper-large-v3-turbo-Q8_0.gguf"]);
        assert_eq!(models_for(&whisper, "de"), whisper);
    }

    /// `SCRAWL_MODELS_DIR` keeps the downloads there; else a temporary folder that's removed.
    fn download_dir(name: &str) -> (PathBuf, bool) {
        match std::env::var_os("SCRAWL_MODELS_DIR").filter(|dir| !dir.is_empty()) {
            Some(dir) => (PathBuf::from(dir), false),
            None => (temp_dir(name), true),
        }
    }

    /// Downloads the real English model (731 MB) and loads it. CI's release job runs it.
    #[test]
    #[ignore]
    fn downloads_and_loads_parakeet() {
        let (dir, remove) = download_dir("parakeet");
        let mut last = 0.0;
        let model = download(&PARAKEET, &dir, &mut |done| last = done).unwrap();
        assert_eq!(last, 1.0);
        transcribe_cpp::init_backends_default().unwrap();
        Model::load(&model).unwrap();
        if remove {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Downloads the real multilingual model (740 MB), checks it lists German, and runs it with
    /// the German hint on a second of silence. CI's release job runs it.
    #[test]
    #[ignore]
    fn downloads_and_loads_parakeet_v3_for_german() {
        let (dir, remove) = download_dir("parakeet-v3");
        let mut last = 0.0;
        let path = download(&PARAKEET_V3, &dir, &mut |done| last = done).unwrap();
        assert_eq!(last, 1.0);
        assert_eq!(models_for(&[path.clone()], "de"), vec![path.clone()]);
        transcribe_cpp::init_backends_default().unwrap();
        let (_, model, hint) = load_for(&[path], "de").unwrap();
        let caps = model.capabilities();
        eprintln!(
            "languages {:?}, detects {}",
            caps.languages, caps.supports_language_detect
        );
        assert_eq!(hint.as_deref(), Some("de"));
        let options = RunOptions {
            timestamps: TimestampKind::Word,
            language: hint,
            ..Default::default()
        };
        model.session().unwrap().run(&[0.0; 16_000], &options).unwrap();
        drop(model);
        if remove {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
