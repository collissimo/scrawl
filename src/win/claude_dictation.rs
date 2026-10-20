//! Transcription through Claude Code's voice dictation (`"transcriber": "claude"`), as Collin's
//! T3 Voice does it. A hidden, unmodified `claude` session runs in a pseudo console
//! (`win::conpty`) with dictation on in hold mode. While Scrawl records, a held space bar is
//! typed into it (a space every 30 ms, like key repeat); Claude Code records the default
//! microphone and transcribes it through Anthropic with the user's own claude.ai login. When the
//! recording stops the key is released, and once the screen settles the text is taken out
//! through Claude Code's external editor hook (Ctrl+G runs `scrawl.exe claude-editor`, which
//! copies the prompt to a file and empties it). Nothing is ever submitted, and the session has
//! no tools and runs in plan mode, so even a stray Enter could not act.
//!
//! One session is kept warm while Claude is the engine (`watch`), and reused after each
//! recording. Every failure returns an error and the recording is transcribed from its file
//! instead (`transcribe::transcribe`).

mod screen;
mod spread;
#[cfg(test)]
mod fixtures;

use std::fs::File;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
};

use crate::live_transcribe::Finished;
use screen::{COLS, ROWS, Settle, State, Trust};

const KEY_REPEAT: Duration = Duration::from_millis(30);
const CTRL_G: &[u8] = b"\x07";
const ESC: &[u8] = b"\x1b";
const ARROW_DOWN: &[u8] = b"\x1b[B";
const START_TIMEOUT: Duration = Duration::from_secs(60);
const LISTEN_TIMEOUT: Duration = Duration::from_secs(6);
const SETTLE_TIMEOUT: Duration = Duration::from_secs(45);
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long finishing a recording waits for the dictation's text.
const FINISH_TIMEOUT: Duration = Duration::from_secs(70);
/// The environment variable the editor shim finds its capture file in.
const CAPTURE_ENV: &str = "SCRAWL_CLAUDE_CAPTURE";
/// Copies everything the session prints to this file (for test fixtures).
const TRACE_ENV: &str = "SCRAWL_CLAUDE_TRACE";

// --- finding claude ---------------------------------------------------------------------------

/// `claude.exe` on PATH, looked up again at most every 30 s.
pub fn claude_path() -> Option<PathBuf> {
    static CACHE: Mutex<Option<(Instant, Option<PathBuf>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((at, path)) = cache.as_ref()
        && at.elapsed() < Duration::from_secs(30)
    {
        return path.clone();
    }
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("claude.exe"))
            .find(|candidate| candidate.is_file())
    });
    *cache = Some((Instant::now(), found.clone()));
    found
}

/// Why Claude Code dictation can't be chosen now, or `None` when it can.
pub fn unavailable_reason() -> Option<String> {
    claude_path()
        .is_none()
        .then(|| "claude.exe not found on PATH".to_owned())
}

pub fn available() -> bool {
    claude_path().is_some()
}

/// `%LOCALAPPDATA%\Scrawl\claude-dictation`, or `SCRAWL_CLAUDE_DIR`.
fn runtime_dir() -> PathBuf {
    std::env::var_os("SCRAWL_CLAUDE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| super::local_dir().join("claude-dictation"))
}

/// The environment for `claude`: a session started from inside another Claude Code session
/// inherits its identity and auth overrides, and this one needs the user's own login.
fn claude_environment() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(key, _)| {
            let key = key.to_ascii_uppercase();
            !key.starts_with("CLAUDE") && !key.starts_with("ANTHROPIC")
        })
        .collect()
}

/// The `EDITOR` command: this executable's `claude-editor`, quoted only when it has to be.
fn editor_command() -> Result<String> {
    let exe = std::env::current_exe().context("Could not find scrawl.exe")?;
    Ok(super::conpty::command_line(&exe, &["claude-editor".into()]))
}

// --- the session ------------------------------------------------------------------------------

pub struct Session {
    pty: Arc<super::conpty::Pty>,
    screen: Arc<Mutex<vt100::Parser>>,
    holding: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    capture_file: PathBuf,
    pub language: String,
}

impl Session {
    /// Starts `claude` and waits (up to a minute) for its prompt, accepting the folder trust
    /// dialog for the empty folder it runs in.
    pub fn start(language: &str) -> Result<Self> {
        let started = Instant::now();
        let claude = claude_path().context("claude.exe not found on PATH")?;
        let dir = runtime_dir();
        let cwd = dir.join("claude-cwd");
        std::fs::create_dir_all(&cwd).context("Could not create the Claude Code folder")?;
        let settings = dir.join("claude-settings.json");
        std::fs::write(
            &settings,
            serde_json::json!({
                "voice": {"enabled": true, "mode": "hold", "autoSubmit": false},
                "language": language,
                // The fullscreen renderer never finishes starting in a pseudo console (it asks
                // the terminal questions conhost doesn't answer), and a failed start makes the
                // user's own next `claude` fall back to the classic renderer.
                "tui": "default",
            })
            .to_string(),
        )
        .context("Could not write the Claude Code settings")?;
        let args = [
            "--settings".to_owned(),
            settings.to_string_lossy().into_owned(),
            "--permission-mode".into(),
            "plan".into(),
            "--strict-mcp-config".into(),
            "--tools".into(),
            String::new(),
        ];
        let capture_file = dir.join("capture.txt");
        let editor = editor_command()?;
        let mut env = claude_environment();
        env.retain(|(key, _)| {
            !["EDITOR", "VISUAL", CAPTURE_ENV].contains(&key.to_ascii_uppercase().as_str())
        });
        env.push(("EDITOR".into(), editor.clone()));
        env.push(("VISUAL".into(), editor));
        env.push((CAPTURE_ENV.into(), capture_file.to_string_lossy().into_owned()));

        // SAFETY: reads which window has the foreground.
        let in_front = unsafe { GetForegroundWindow() }.0 as isize;
        let (pty, output) = super::conpty::Pty::spawn(&claude, &args, &cwd, &env, ROWS, COLS)?;
        let pty = Arc::new(pty);
        let child = pty.pid;
        std::thread::spawn(move || give_back_foreground(in_front, child));

        let screen = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let stopped = Arc::new(AtomicBool::new(false));
        let holding = Arc::new(AtomicBool::new(false));
        spawn_reader(output, Arc::clone(&screen));
        spawn_key_repeat(Arc::clone(&pty), Arc::clone(&holding), Arc::clone(&stopped));
        let session = Self {
            pty,
            screen,
            holding,
            stopped,
            capture_file,
            language: language.to_owned(),
        };
        session.wait_ready(START_TIMEOUT)?;
        crate::capture::scrawl_log(format!(
            "Claude Code dictation: session ready in {} ms (language {language})",
            started.elapsed().as_millis()
        ));
        Ok(session)
    }

    pub fn is_alive(&self) -> bool {
        self.pty.is_alive()
    }

    fn rows(&self) -> Vec<String> {
        let parser = self
            .screen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        screen::rows(&parser)
    }

    pub fn state(&self) -> State {
        State::of(&self.rows())
    }

    fn text(&self) -> String {
        self.rows().join("\n").trim().to_owned()
    }

    fn write(&self, bytes: &[u8]) -> Result<()> {
        self.pty
            .write(bytes)
            .context("The Claude Code session stopped")
    }

    fn wait_ready(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.is_alive() {
                bail!("Claude Code exited during startup:\n{}", self.text());
            }
            let state = self.state();
            match state.trust {
                // The session folder is Scrawl's own and empty, so trusting it is safe. The
                // dialog opens on "No, exit" and drops keys for a moment, so Enter waits until
                // the mark has moved.
                Some(Trust::Selected) => {
                    self.write(b"\r")?;
                    std::thread::sleep(Duration::from_secs(1));
                }
                Some(Trust::NotSelected) => {
                    self.write(ARROW_DOWN)?;
                    std::thread::sleep(Duration::from_millis(300));
                }
                None if state.prompt => {
                    std::thread::sleep(Duration::from_secs(1));
                    return Ok(());
                }
                None => {}
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        bail!("Claude Code did not become ready:\n{}", self.text())
    }

    /// Starts holding the dictation key and returns once Claude Code is listening. `on_hearing`
    /// runs the moment it is (to note how much of the recording it missed). `give_up` ends the
    /// wait early.
    pub fn begin(&self, give_up: &AtomicBool, mut on_hearing: impl FnMut()) -> Result<()> {
        self.holding.store(true, Ordering::Release);
        let deadline = Instant::now() + LISTEN_TIMEOUT;
        while Instant::now() < deadline && !give_up.load(Ordering::Acquire) {
            if self.state().listening {
                on_hearing();
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.holding.store(false, Ordering::Release);
        let reason = self.state().error;
        self.clear_prompt();
        if give_up.load(Ordering::Acquire) {
            bail!("the recording ended before Claude Code was listening");
        }
        bail!(reason.unwrap_or_else(|| "Claude Code did not start listening".into()))
    }

    /// Releases the key, waits for the text to settle, and takes it out of the session. Empty
    /// text is an error with Claude Code's own reason (e.g. "No speech detected").
    pub fn end(&self) -> Result<String> {
        self.holding.store(false, Ordering::Release);
        self.wait_settled(SETTLE_TIMEOUT)?;
        let text = self.capture(CAPTURE_TIMEOUT)?.trim().to_owned();
        if text.is_empty() {
            bail!(
                self.state()
                    .error
                    .unwrap_or_else(|| "No speech detected".into())
            );
        }
        Ok(text)
    }

    /// Throws away a running dictation and leaves the prompt empty.
    pub fn cancel(&self) {
        if !self.is_alive() {
            return;
        }
        if self.holding.swap(false, Ordering::AcqRel) {
            // Esc discards the running dictation and restores the prompt.
            let _ = self.write(ESC);
            std::thread::sleep(Duration::from_millis(300));
        }
        self.clear_prompt();
    }

    fn wait_settled(&self, timeout: Duration) -> Result<()> {
        let started = Instant::now();
        let mut settle = Settle::new(started);
        while started.elapsed() < timeout {
            if settle.observe(&self.rows(), Instant::now()) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        bail!("Timed out waiting for Claude Code's transcript")
    }

    /// The prompt's text, through Ctrl+G and the editor shim, which also empties the prompt.
    pub fn capture(&self, timeout: Duration) -> Result<String> {
        let _ = std::fs::remove_file(&self.capture_file);
        self.write(CTRL_G)?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(&self.capture_file) {
                let _ = std::fs::remove_file(&self.capture_file);
                // Claude Code reads the emptied file back after the editor exits.
                std::thread::sleep(Duration::from_millis(300));
                return Ok(text);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        bail!("Could not read the text from Claude Code (Ctrl+G editor)")
    }

    fn clear_prompt(&self) {
        if let Err(error) = self.capture(Duration::from_secs(5)) {
            crate::capture::scrawl_log(format!(
                "Claude Code dictation: could not clear the prompt: {error:#}"
            ));
        }
    }

    /// Types `text` into the prompt (the self-test).
    fn type_text(&self, text: &str) -> Result<()> {
        self.write(text.as_bytes())?;
        std::thread::sleep(Duration::from_secs(1));
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.holding.store(false, Ordering::Release);
        self.stopped.store(true, Ordering::Release);
    }
}

fn spawn_reader(mut output: File, screen: Arc<Mutex<vt100::Parser>>) {
    let mut trace = std::env::var_os(TRACE_ENV).and_then(|path| File::create(path).ok());
    let _ = std::thread::Builder::new()
        .name("scrawl-claude-read".into())
        .spawn(move || {
            let mut buffer = [0_u8; 8192];
            // Ends when the console closes its side.
            while let Ok(n @ 1..) = output.read(&mut buffer) {
                if let Some(trace) = trace.as_mut() {
                    let _ = trace.write_all(&buffer[..n]);
                }
                screen
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .process(&buffer[..n]);
            }
        });
}

fn spawn_key_repeat(
    pty: Arc<super::conpty::Pty>,
    holding: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
) {
    let _ = std::thread::Builder::new()
        .name("scrawl-claude-key".into())
        .spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                if holding.load(Ordering::Acquire) {
                    if pty.write(b" ").is_err() {
                        holding.store(false, Ordering::Release);
                    }
                    std::thread::sleep(KEY_REPEAT);
                } else {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        });
}

/// A new console can take the foreground, which would leave the user typing into nothing; hand
/// it back if a window of this process or the session's took it.
fn give_back_foreground(previous: isize, child: u32) {
    let previous = HWND(previous as *mut std::ffi::c_void);
    if previous.is_invalid() {
        return;
    }
    let deadline = Instant::now() + Duration::from_millis(1_500);
    while Instant::now() < deadline {
        // SAFETY: reads the foreground window, its owner and its class name.
        unsafe {
            let current = GetForegroundWindow();
            if !current.is_invalid() && current != previous {
                let mut owner = 0_u32;
                GetWindowThreadProcessId(current, Some(&mut owner));
                let mut class = [0_u16; 64];
                let len = GetClassNameW(current, &mut class).max(0) as usize;
                let class = String::from_utf16_lossy(&class[..len]);
                if owner == std::process::id()
                    || owner == child
                    || class == "PseudoConsoleWindow"
                    || class == "ConsoleWindowClass"
                {
                    let _ = SetForegroundWindow(previous);
                    crate::capture::scrawl_log(
                        "Claude Code dictation: returned the foreground to the previous window",
                    );
                }
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// --- one warm session -------------------------------------------------------------------------

enum Slot {
    Empty,
    Starting,
    Ready(Session),
    /// A recording has it.
    InUse,
}

static POOL: Mutex<Slot> = Mutex::new(Slot::Empty);
static CHANGED: Condvar = Condvar::new();
/// When starting last failed, so the watcher doesn't start `claude` every few seconds.
static LAST_FAILURE: Mutex<Option<Instant>> = Mutex::new(None);
const RETRY_AFTER: Duration = Duration::from_secs(60);

fn pool() -> std::sync::MutexGuard<'static, Slot> {
    POOL.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn wanted() -> bool {
    crate::transcribe::Engine::preferred() == crate::transcribe::Engine::Claude && available()
}

/// Starts a session in the background unless one is ready or starting.
pub fn warm() {
    let language = crate::handoff::transcription_language();
    {
        let mut slot = pool();
        match &*slot {
            Slot::Starting | Slot::InUse => return,
            Slot::Ready(session) if session.is_alive() && session.language == language => return,
            _ => {}
        }
        *slot = Slot::Starting;
    }
    let _ = std::thread::Builder::new()
        .name("scrawl-claude-start".into())
        .spawn(move || {
            let started = Session::start(&language);
            let mut slot = pool();
            *slot = match started {
                Ok(session) => Slot::Ready(session),
                Err(error) => {
                    crate::capture::scrawl_log(format!(
                        "Claude Code dictation: could not start the session: {error:#}"
                    ));
                    *LAST_FAILURE
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
                    Slot::Empty
                }
            };
            CHANGED.notify_all();
        });
}

/// Ends the warm session, if any.
pub fn shut_down() {
    let mut slot = pool();
    if matches!(&*slot, Slot::Ready(_)) {
        *slot = Slot::Empty;
        crate::capture::scrawl_log("Claude Code dictation: session ended");
    }
}

/// Keeps one session warm while Claude Code is the engine, and none otherwise (the app).
pub fn watch() {
    let _ = std::thread::Builder::new()
        .name("scrawl-claude-watch".into())
        .spawn(|| {
            loop {
                if wanted() {
                    if !failed_recently() {
                        warm();
                    }
                } else {
                    shut_down();
                }
                std::thread::sleep(Duration::from_secs(5));
            }
        });
}

/// The warm session, else one started in the background and waited for. `give_up` (the
/// recording ended) stops the wait at once; the session goes on starting for the next one.
fn acquire(give_up: &AtomicBool) -> Result<Session> {
    let deadline = Instant::now() + START_TIMEOUT + Duration::from_secs(5);
    let language = crate::handoff::transcription_language();
    let mut slot = pool();
    loop {
        match std::mem::replace(&mut *slot, Slot::Empty) {
            Slot::Ready(session) if session.is_alive() && session.language == language => {
                *slot = Slot::InUse;
                return Ok(session);
            }
            Slot::Ready(_) | Slot::Empty => {
                if failed_recently() {
                    bail!("the Claude Code session failed to start less than a minute ago");
                }
                drop(slot);
                warm();
                slot = pool();
            }
            Slot::InUse => {
                *slot = Slot::InUse;
                bail!("the Claude Code session is busy with another recording");
            }
            Slot::Starting => {
                *slot = Slot::Starting;
                if give_up.load(Ordering::Acquire) {
                    bail!("the recording ended before the Claude Code session was ready");
                }
                if Instant::now() > deadline {
                    bail!("the Claude Code session took too long to start");
                }
                slot = CHANGED
                    .wait_timeout(slot, Duration::from_millis(100))
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
            }
        }
    }
}

fn failed_recently() -> bool {
    LAST_FAILURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some_and(|at| at.elapsed() < RETRY_AFTER)
}

/// Puts a session back for the next recording; a dead one is replaced, and one no longer
/// wanted ends.
fn give_back(session: Session) {
    let alive = session.is_alive();
    let wanted = wanted();
    {
        let mut slot = pool();
        if matches!(&*slot, Slot::InUse) {
            *slot = if alive && wanted {
                Slot::Ready(session)
            } else {
                Slot::Empty
            };
            CHANGED.notify_all();
        }
    }
    if !alive {
        crate::capture::scrawl_log("Claude Code dictation: the session ended");
        if wanted {
            warm();
        }
    }
}

// --- one recording ----------------------------------------------------------------------------

/// What the session made of one recording.
struct Heard {
    text: String,
    /// Samples the recorder had when Claude Code started listening.
    from_sample: usize,
}

/// Dictation running alongside one recording (`live_transcribe::Live`). Starting never waits;
/// the session is found and the key held on a thread of its own.
pub struct Take {
    released: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    result: Receiver<Result<Heard>>,
    finished: bool,
}

impl Take {
    pub fn start(samples: Arc<Mutex<Vec<f32>>>) -> Result<Self> {
        let released = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, result) = std::sync::mpsc::channel();
        let (thread_released, thread_cancelled) = (Arc::clone(&released), Arc::clone(&cancelled));
        std::thread::Builder::new()
            .name("scrawl-claude-take".into())
            .spawn(move || {
                let _ = sender.send(dictate(&samples, &thread_released, &thread_cancelled));
            })
            .context("Could not spawn the Claude Code dictation thread")?;
        Ok(Self {
            released,
            cancelled,
            result,
            finished: false,
        })
    }

    /// Releases the key and turns the text into a transcript of `wav_bytes`, the recording.
    pub fn finish(&mut self, wav_bytes: &[u8]) -> Result<Finished> {
        let finishing = Instant::now();
        self.finished = true;
        self.released.store(true, Ordering::Release);
        let heard = match self.result.recv_timeout(FINISH_TIMEOUT) {
            Ok(heard) => heard?,
            Err(RecvTimeoutError::Timeout) => {
                self.cancelled.store(true, Ordering::Release);
                bail!(
                    "Claude Code took over {} s to finish",
                    FINISH_TIMEOUT.as_secs()
                )
            }
            Err(RecvTimeoutError::Disconnected) => bail!("the Claude Code dictation vanished"),
        };
        let samples = crate::audio::decode_wav_16k(wav_bytes)?;
        let duration_ms = samples.len() as u64 / 16;
        let heard_from_ms = heard.from_sample as u64 / 16;
        crate::capture::scrawl_log(format!(
            "Claude Code dictation: listening from {heard_from_ms} of {duration_ms} ms, {} characters",
            heard.text.chars().count()
        ));
        let transcript = spread::transcript(
            &heard.text,
            &crate::audio::speech_islands(&samples),
            heard_from_ms,
            duration_ms,
            &crate::handoff::transcription_language(),
        )?;
        Ok(Finished {
            transcript,
            tail_ms: finishing.elapsed().as_millis() as u64,
            streamed_samples: samples.len().saturating_sub(heard.from_sample),
        })
    }
}

impl Drop for Take {
    fn drop(&mut self) {
        if !self.finished {
            self.cancelled.store(true, Ordering::Release);
            self.released.store(true, Ordering::Release);
        }
    }
}

fn dictate(
    samples: &Mutex<Vec<f32>>,
    released: &AtomicBool,
    cancelled: &AtomicBool,
) -> Result<Heard> {
    let session = acquire(released)?;
    let mut from_sample = 0;
    let began = session.begin(released, || {
        from_sample = samples
            .lock()
            .map(|samples| samples.len())
            .unwrap_or_default();
    });
    if let Err(error) = began {
        give_back(session);
        return Err(error);
    }
    crate::capture::scrawl_log(format!(
        "Claude Code dictation: listening {} ms into the recording",
        from_sample / 16
    ));
    while !released.load(Ordering::Acquire) {
        if !session.is_alive() {
            give_back(session);
            return Err(anyhow!("the Claude Code session stopped while recording"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if cancelled.load(Ordering::Acquire) {
        session.cancel();
        give_back(session);
        bail!("cancelled");
    }
    let text = session.end();
    give_back(session);
    Ok(Heard {
        text: text?,
        from_sample,
    })
}

// --- the editor shim and the self-test ----------------------------------------------------------

/// `scrawl.exe claude-editor <file>`, run by Claude Code as `$EDITOR` on its prompt: copies the
/// file to the capture file and empties it, so the text leaves exactly as dictated.
pub fn editor_main(file: &Path) -> Result<()> {
    let target = PathBuf::from(
        std::env::var_os(CAPTURE_ENV).context("SCRAWL_CLAUDE_CAPTURE is not set")?,
    );
    let text = std::fs::read(file).with_context(|| format!("Could not read {}", file.display()))?;
    let pending = target.with_extension("tmp");
    std::fs::write(&pending, &text)?;
    std::fs::rename(&pending, &target)?;
    File::create(file)?;
    Ok(())
}

/// `scrawl.exe dictation-selftest`: starts a session, types text and takes it back out through
/// the editor hook, holds dictation for 2 s, then cancels one. Prints what it saw.
pub fn selftest() -> Result<bool> {
    let language = crate::handoff::transcription_language();
    println!(
        "claude: {}",
        claude_path().map_or("not found on PATH".into(), |p| p.display().to_string())
    );
    println!("language: {language}");
    let started = Instant::now();
    let session = Session::start(&language)?;
    println!("ready after {:.1} s", started.elapsed().as_secs_f64());
    let mut ok = true;

    let sample = "Hallo Welt, das ist ein Test mit Umlauten: äöü ß.";
    session.type_text(sample)?;
    let captured = session.capture(CAPTURE_TIMEOUT)?;
    let exact = captured == sample;
    ok &= exact;
    println!("capture exact: {exact} {captured:?}");
    let empty = session.capture(CAPTURE_TIMEOUT)?;
    ok &= empty.is_empty();
    println!("prompt empty after capture: {:?}", empty);

    let never = AtomicBool::new(false);
    let started = Instant::now();
    match session.begin(&never, || {}) {
        Ok(()) => {
            println!(
                "listening after {:.2} s",
                started.elapsed().as_secs_f64()
            );
            std::thread::sleep(Duration::from_secs(2));
            println!("while holding: {}", session.state().summary());
            println!("preview: {:?}", screen::preview(&session.rows()));
            let started = Instant::now();
            let result = session.end();
            println!(
                "end after {:.2} s: {}",
                started.elapsed().as_secs_f64(),
                match &result {
                    Ok(text) => format!("text {text:?}"),
                    Err(error) => format!("no text ({error:#})"),
                }
            );
            println!("after release: {}", session.state().summary());
        }
        Err(error) => {
            ok = false;
            println!("did not start listening: {error:#}");
            println!("screen: {}", session.state().summary());
        }
    }

    match session.begin(&never, || {}) {
        Ok(()) => {
            std::thread::sleep(Duration::from_secs(1));
            session.cancel();
            let left = session.capture(CAPTURE_TIMEOUT)?;
            ok &= left.trim().is_empty();
            println!("cancelled, prompt: {left:?}");
        }
        Err(error) => {
            ok = false;
            println!("second hold did not start: {error:#}");
        }
    }
    println!("alive at the end: {}", session.is_alive());
    println!("{}", if ok { "PASS" } else { "FAIL" });
    Ok(ok)
}
