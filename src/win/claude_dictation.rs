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

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
/// Claude Code transcribes while it listens, so the last words follow the release within a
/// second or two; anything slower falls back to the file engine.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(8);
/// The editor shim runs in about 30 ms.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(3);
/// How long finishing a recording waits for the dictation's text: the settle and the capture,
/// so the session thread is never abandoned in the middle of either.
const FINISH_TIMEOUT: Duration = Duration::from_secs(12);
/// Claude Code reads the emptied prompt file back after the editor exits; keys sent before then
/// can be lost.
const EDITOR_RETURN: Duration = Duration::from_millis(300);
/// The environment variable the editor shim finds its capture file in.
const CAPTURE_ENV: &str = "SCRAWL_CLAUDE_CAPTURE";
/// Copies everything the session prints to this file (for test fixtures).
const TRACE_ENV: &str = "SCRAWL_CLAUDE_TRACE";

// --- finding claude ---------------------------------------------------------------------------

/// `claude.exe` on PATH. Only the first call searches PATH itself; later ones answer from the
/// cache at once and, once it is 30 s old, search again in the background, so a PATH entry on an
/// unreachable drive never stalls a recording's start on the UI thread.
pub fn claude_path() -> Option<PathBuf> {
    static CACHE: Mutex<Option<(Instant, Option<PathBuf>)>> = Mutex::new(None);
    static SEARCHING: AtomicBool = AtomicBool::new(false);
    let store = |found: &Option<PathBuf>| {
        *CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((Instant::now(), found.clone()));
    };
    let cached = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let Some((at, path)) = cached else {
        let found = find_claude();
        store(&found);
        return found;
    };
    if at.elapsed() >= Duration::from_secs(30) && !SEARCHING.swap(true, Ordering::AcqRel) {
        let spawned = std::thread::Builder::new()
            .name("scrawl-claude-path".into())
            .spawn(move || {
                store(&find_claude());
                SEARCHING.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            SEARCHING.store(false, Ordering::Release);
        }
    }
    path
}

fn find_claude() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("claude.exe"))
            .find(|candidate| candidate.is_file())
    })
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
/// Variables that aren't Unicode pass through as they are.
fn claude_environment() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(key, _)| {
            let key = key.to_string_lossy().to_ascii_uppercase();
            !key.starts_with("CLAUDE") && !key.starts_with("ANTHROPIC")
        })
        .collect()
}

/// This process's settings file and a capture file for its `session`-th session. Each process
/// (the app and a `scrawl record` beside it) and each session has its own, so neither a second
/// process nor a killed session's late editor run can swap or delete another's text.
fn session_files(dir: &Path, pid: u32, session: u32) -> (PathBuf, PathBuf) {
    (
        dir.join(format!("claude-settings-{pid}.json")),
        dir.join(format!("capture-{pid}-{session}.txt")),
    )
}

/// The process id in a file name `session_files` made (or the old shared names, `None`).
fn file_pid(name: &str) -> Option<Option<u32>> {
    if matches!(name, "claude-settings.json" | "capture.txt" | "capture.tmp") {
        return Some(None);
    }
    let id = name
        .strip_prefix("claude-settings-")
        .and_then(|rest| rest.strip_suffix(".json"))
        .or_else(|| {
            let rest = name.strip_prefix("capture-")?;
            let rest = rest.strip_suffix(".txt").or_else(|| rest.strip_suffix(".tmp"))?;
            Some(rest.split_once('-')?.0)
        })?;
    Some(Some(id.parse().ok()?))
}

/// Deletes files left by Scrawl processes that are no longer running.
fn remove_stale_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let own = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let stale = match file_pid(&name.to_string_lossy()) {
            Some(None) => true,
            Some(Some(pid)) => pid != own && !process_running(pid),
            None => false,
        };
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn process_running(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: opens the process for a status query only and closes the handle.
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0_u32;
        let running = GetExitCodeProcess(process, &mut code).is_ok()
            && code == STILL_ACTIVE.0 as u32;
        let _ = CloseHandle(process);
        running
    }
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
    /// A step failed and left the prompt in an unknown state (old text, a late editor run), so
    /// the session is not used again (`give_back`).
    broken: AtomicBool,
    /// When Claude Code is back from the last editor run (`EDITOR_RETURN`).
    editor_back: Mutex<Option<Instant>>,
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
        remove_stale_files(&dir);
        static SESSIONS: AtomicU32 = AtomicU32::new(0);
        let (settings, capture_file) = session_files(
            &dir,
            std::process::id(),
            SESSIONS.fetch_add(1, Ordering::Relaxed),
        );
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
        let editor = editor_command()?;
        let mut env = claude_environment();
        env.retain(|(key, _)| {
            !["EDITOR", "VISUAL", CAPTURE_ENV]
                .contains(&key.to_string_lossy().to_ascii_uppercase().as_str())
        });
        env.push(("EDITOR".into(), editor.clone().into()));
        env.push(("VISUAL".into(), editor.into()));
        env.push((CAPTURE_ENV.into(), capture_file.clone().into_os_string()));

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
            broken: AtomicBool::new(false),
            editor_back: Mutex::new(None),
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
    /// runs the moment it is (to note how much of the recording it missed) and returns false
    /// when that was too late (the recording had already stopped). `give_up` ends the wait
    /// early. Any failure leaves the session broken: dictation may still start a moment later.
    pub fn begin(&self, give_up: &AtomicBool, mut on_hearing: impl FnMut() -> bool) -> Result<()> {
        self.wait_editor_back();
        self.holding.store(true, Ordering::Release);
        let deadline = Instant::now() + LISTEN_TIMEOUT;
        let mut late = false;
        while Instant::now() < deadline && !give_up.load(Ordering::Acquire) {
            if self.state().listening {
                if on_hearing() {
                    return Ok(());
                }
                late = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.holding.store(false, Ordering::Release);
        self.broken.store(true, Ordering::Release);
        if late || give_up.load(Ordering::Acquire) {
            bail!("the recording ended before Claude Code was listening");
        }
        bail!(
            self.state()
                .error
                .unwrap_or_else(|| "Claude Code did not start listening".into())
        )
    }

    /// Releases the key, waits for the text to settle, and takes it out of the session. Empty
    /// text is an error with Claude Code's own reason (e.g. "No speech detected"); a timeout
    /// leaves the session broken.
    pub fn end(&self) -> Result<String> {
        self.holding.store(false, Ordering::Release);
        self.wait_settled(SETTLE_TIMEOUT)
            .inspect_err(|_| self.broken.store(true, Ordering::Release))?;
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

    /// The prompt's text, through Ctrl+G and the editor shim, which also empties the prompt. A
    /// timeout leaves the session broken (the editor may still write the file later).
    pub fn capture(&self, timeout: Duration) -> Result<String> {
        self.wait_editor_back();
        let _ = std::fs::remove_file(&self.capture_file);
        self.write(CTRL_G)
            .inspect_err(|_| self.broken.store(true, Ordering::Release))?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(&self.capture_file) {
                let _ = std::fs::remove_file(&self.capture_file);
                *self.editor_back_lock() = Some(Instant::now() + EDITOR_RETURN);
                return Ok(text);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.broken.store(true, Ordering::Release);
        bail!("Could not read the text from Claude Code (Ctrl+G editor)")
    }

    fn editor_back_lock(&self) -> std::sync::MutexGuard<'_, Option<Instant>> {
        self.editor_back
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Waits until Claude Code has read the prompt back from the last editor run, which is
    /// paid before the next keys rather than while a recording finishes.
    fn wait_editor_back(&self) {
        let back = self.editor_back_lock().take();
        if let Some(wait) = back.and_then(|at| at.checked_duration_since(Instant::now())) {
            std::thread::sleep(wait);
        }
    }

    pub fn is_broken(&self) -> bool {
        self.broken.load(Ordering::Acquire)
    }

    fn clear_prompt(&self) {
        if let Err(error) = self.capture(CAPTURE_TIMEOUT) {
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
    let spawned = std::thread::Builder::new()
        .name("scrawl-claude-start".into())
        .spawn(move || {
            let starting = Starting::new(&POOL);
            match Session::start(&language) {
                Ok(session) => starting.ready(session),
                Err(error) => crate::capture::scrawl_log(format!(
                    "Claude Code dictation: could not start the session: {error:#}"
                )),
            }
        });
    if let Err(error) = spawned {
        crate::capture::scrawl_log(format!(
            "Claude Code dictation: could not start the session: {error}"
        ));
        drop(Starting::new(&POOL));
    }
}

/// A session being started into `slot`. Dropped without `ready` (an error, a panic, a thread
/// that never ran), it empties the slot and notes the failure, so the slot never stays
/// `Starting` and later recordings wait for nothing.
struct Starting<'a> {
    slot: &'a Mutex<Slot>,
    done: bool,
}

impl<'a> Starting<'a> {
    fn new(slot: &'a Mutex<Slot>) -> Self {
        Self { slot, done: false }
    }

    fn ready(mut self, session: Session) {
        self.done = true;
        self.set(Slot::Ready(session));
    }

    fn set(&self, to: Slot) {
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = to;
        CHANGED.notify_all();
    }
}

impl Drop for Starting<'_> {
    fn drop(&mut self) {
        if !self.done {
            *LAST_FAILURE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
            self.set(Slot::Empty);
        }
    }
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

/// Puts a session back for the next recording; a dead or broken one is replaced, and one no
/// longer wanted ends.
fn give_back(session: Session) {
    let (alive, broken, wanted) = (session.is_alive(), session.is_broken(), wanted());
    let keep = keep(alive, broken, wanted);
    {
        let mut slot = pool();
        if matches!(&*slot, Slot::InUse) {
            *slot = if keep {
                Slot::Ready(session)
            } else {
                Slot::Empty
            };
            CHANGED.notify_all();
        }
    }
    if !keep && wanted {
        crate::capture::scrawl_log(if alive {
            "Claude Code dictation: replacing the session after a failed step"
        } else {
            "Claude Code dictation: the session ended"
        });
        warm();
    }
}

/// Whether a session goes back into the slot for the next recording.
fn keep(alive: bool, broken: bool, wanted: bool) -> bool {
    alive && !broken && wanted
}

// --- one recording ----------------------------------------------------------------------------

/// What the session made of one recording.
pub(crate) struct Heard {
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

    /// Releases the dictation key: the recording has stopped, and nothing said from now on is
    /// part of it (`AudioRecorder::stop`, before its buffer is taken).
    pub fn release(&self) {
        self.released.store(true, Ordering::Release);
    }

    /// Releases the key and turns the text into a transcript of `wav_bytes`, the recording. A
    /// recording without speech gives up at once, without waiting for Claude Code.
    pub fn finish(&mut self, wav_bytes: &[u8]) -> Result<Finished> {
        let finishing = Instant::now();
        self.finished = true;
        let samples = crate::audio::decode_wav_16k(wav_bytes);
        let islands = samples
            .as_deref()
            .map(crate::audio::speech_islands)
            .unwrap_or_default();
        if islands.is_empty() {
            self.cancelled.store(true, Ordering::Release);
        }
        self.release();
        let samples = samples?;
        if islands.is_empty() {
            bail!("the recording has no speech");
        }
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
        let duration_ms = samples.len() as u64 / 16;
        let heard_from_ms = heard.from_sample as u64 / 16;
        crate::capture::scrawl_log(format!(
            "Claude Code dictation: listening from {heard_from_ms} of {duration_ms} ms, {} characters",
            heard.text.chars().count()
        ));
        let transcript = spread::transcript(
            &heard.text,
            &islands,
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
    if let Some(reason) = other_microphone() {
        static LOGGED: Mutex<Option<String>> = Mutex::new(None);
        let mut logged = LOGGED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if logged.as_deref() != Some(reason.as_str()) {
            crate::capture::scrawl_log(format!(
                "Claude Code dictation: {reason}; transcribing recordings from their file"
            ));
            *logged = Some(reason);
        }
        return Err(anyhow!(Skipped));
    }
    let session = acquire(released)?;
    let mut from_sample = 0;
    let began = session.begin(released, || match hearing_point(samples, released) {
        Some(at) => {
            from_sample = at;
            true
        }
        None => false,
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

/// Claude Code dictation wasn't tried for this recording, for a reason already logged once
/// (`transcribe::finish_live` stays quiet about it).
#[derive(Debug)]
pub struct Skipped;

impl std::fmt::Display for Skipped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Claude Code dictation skipped")
    }
}

impl std::error::Error for Skipped {}

/// Why Claude Code would hear another microphone than the recording, if it would: it always
/// records the system default, and Scrawl records the one chosen in Settings (or the default
/// when the chosen one isn't connected, as `AudioRecorder::open_stream` does).
fn other_microphone() -> Option<String> {
    let choice = crate::mic::choice()?;
    other_microphone_of(&choice, &crate::mic::inputs())
}

fn other_microphone_of(choice: &crate::mic::Choice, inputs: &crate::mic::Inputs) -> Option<String> {
    let used = inputs.find(&choice.id)?;
    (inputs.default.as_deref() != Some(used.id.as_str())).then(|| {
        format!(
            "Scrawl records {} but Claude Code dictates from the default microphone",
            used.name
        )
    })
}

/// How many samples the recording has as Claude Code starts listening, or `None` once it has
/// stopped. `released` is read under the buffer's lock, which `AudioRecorder::stop` takes only
/// after releasing, so an emptied buffer is never read as "listening from the start".
fn hearing_point(samples: &Mutex<Vec<f32>>, released: &AtomicBool) -> Option<usize> {
    let samples = samples.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    (!released.load(Ordering::Acquire)).then(|| samples.len())
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
    match session.begin(&never, || true) {
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

    match session.begin(&never, || true) {
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

/// A take without a session: its thread's answer is sent by hand on the returned sender (or
/// never), and its `released` and `cancelled` flags are returned to look at.
#[cfg(test)]
pub(crate) fn test_take() -> (
    Take,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
    std::sync::mpsc::Sender<Result<Heard>>,
) {
    let released = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    let (answer, result) = std::sync::mpsc::channel();
    let take = Take {
        released: Arc::clone(&released),
        cancelled: Arc::clone(&cancelled),
        result,
        finished: false,
    };
    (take, released, cancelled, answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    /// 16 kHz samples, quiet room noise with louder noise (speech, as `speech_islands` sees it)
    /// in `bursts` (ms).
    fn recording(duration_ms: u64, bursts: &[(u64, u64)]) -> Vec<f32> {
        let mut seed = 12_345_u32;
        (0..duration_ms * 16)
            .map(|i| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let noise = (seed >> 16) as f32 / 32_768.0 - 1.0;
                let ms = i / 16;
                let loud = bursts.iter().any(|&(from, to)| (from..to).contains(&ms));
                noise * if loud { 0.3 } else { 0.000_5 }
            })
            .collect()
    }

    #[test]
    fn only_a_sound_session_goes_back_for_the_next_recording() {
        assert!(keep(true, false, true));
        assert!(
            !keep(true, true, true),
            "a failed step leaves old text or a late editor run"
        );
        assert!(!keep(false, false, true));
        assert!(!keep(true, false, false));
    }

    #[test]
    fn a_session_that_fails_or_panics_while_starting_empties_the_slot() {
        let slot = Mutex::new(Slot::Starting);
        drop(Starting::new(&slot));
        assert!(matches!(*slot.lock().unwrap(), Slot::Empty), "an error");

        *slot.lock().unwrap() = Slot::Starting;
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _starting = Starting::new(&slot);
            panic!("a non-Unicode variable");
        }));
        assert!(panicked.is_err());
        let slot = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(matches!(*slot, Slot::Empty), "a panic");
    }

    #[test]
    fn every_process_and_session_has_its_own_files() {
        let dir = Path::new(r"C:\x");
        let (settings, capture) = session_files(dir, 41, 0);
        assert_eq!(settings, dir.join("claude-settings-41.json"));
        assert_eq!(capture, dir.join("capture-41-0.txt"));
        assert_ne!(
            session_files(dir, 42, 0),
            (settings.clone(), capture.clone())
        );
        assert_eq!(session_files(dir, 41, 1).0, settings);
        assert_ne!(
            session_files(dir, 41, 1).1,
            capture,
            "a killed session's late editor run"
        );
        for (name, pid) in [
            ("claude-settings-41.json", Some(Some(41))),
            ("capture-41-3.txt", Some(Some(41))),
            ("capture-41-3.tmp", Some(Some(41))),
            ("capture.txt", Some(None)),
            ("claude-settings.json", Some(None)),
            ("claude-cwd", None),
            ("capture-x-1.txt", None),
        ] {
            assert_eq!(file_pid(name), pid, "{name}");
        }
        assert!(process_running(std::process::id()));
    }

    #[test]
    fn listening_that_starts_after_the_recording_stopped_does_not_count() {
        let samples = Mutex::new(vec![0.0; 3_200]);
        let released = AtomicBool::new(false);
        assert_eq!(hearing_point(&samples, &released), Some(3_200));
        released.store(true, Ordering::Release);
        // `AudioRecorder::stop` has emptied the buffer: never "listening from the start".
        samples.lock().unwrap().clear();
        assert_eq!(hearing_point(&samples, &released), None);
    }

    #[test]
    fn claude_is_used_only_with_the_default_microphone() {
        let input = |id: &str| crate::mic::Input {
            id: id.into(),
            name: format!("Mic {id}"),
        };
        let inputs = crate::mic::Inputs {
            devices: vec![input("a"), input("b")],
            default: Some("a".into()),
        };
        let choice = |id: &str| crate::mic::Choice {
            id: id.into(),
            name: "old name".into(),
        };
        assert_eq!(other_microphone_of(&choice("a"), &inputs), None);
        assert_eq!(
            other_microphone_of(&choice("gone"), &inputs),
            None,
            "the recorder uses the default then"
        );
        assert_eq!(
            other_microphone_of(&choice("b"), &inputs).as_deref(),
            Some("Scrawl records Mic b but Claude Code dictates from the default microphone")
        );
    }

    #[test]
    fn a_recording_without_speech_falls_back_at_once() {
        let (mut take, released, cancelled, _answer) = test_take();
        let wav = crate::audio::encode_wav_16k(&recording(3_000, &[])).unwrap();
        let started = Instant::now();
        let error = take.finish(&wav).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(error.to_string(), "the recording has no speech");
        assert!(cancelled.load(Ordering::Acquire) && released.load(Ordering::Acquire));
    }

    /// Release → Claude Code's text → spread over the speech → `summary.md`, without `claude`.
    #[test]
    fn dictated_text_reaches_the_summary_under_the_keyframe_it_was_said_at() {
        let samples = recording(8_000, &[(1_000, 2_500), (5_000, 6_500)]);
        let islands = crate::audio::speech_islands(&samples);
        assert_eq!(islands.len(), 2, "{islands:?}");
        let (take, _released, _cancelled, answer) = test_take();
        answer
            .send(Ok(Heard {
                text: "Erstens das hier oben. Zweitens das dort unten.".into(),
                from_sample: 0,
            }))
            .unwrap();
        let audio = crate::audio::RecordedAudio {
            sample_count: samples.len(),
            duration_ms: 8_000,
            wav_bytes: crate::audio::encode_wav_16k(&samples).unwrap(),
            live: crate::live_transcribe::Slot::new(Some(crate::live_transcribe::Live::claude(
                take,
            ))),
        };
        let transcript =
            crate::transcribe::transcribe(Some(Path::new("never-read.wav")), Some(&audio));
        assert_eq!(transcript.source, "claude-code");
        assert!(transcript.estimated);
        assert_eq!(
            transcript.text,
            "Erstens das hier oben. Zweitens das dort unten."
        );
        let (first, last) = (&transcript.words[0], transcript.words.last().unwrap());
        assert!(first.t0 >= 900 && last.t1 <= 6_600, "{first:?} {last:?}");

        let times = [1_500, 4_800];
        let said = crate::keyframes::said_per_keyframe(&times, &[], &transcript);
        let keyframes = times
            .iter()
            .zip(said)
            .enumerate()
            .map(|(i, (&t, said))| Keyframe {
                index: i + 1,
                t,
                reason: KeyframeReason::SceneChange,
                file: format!("frames/frame-{:02}.png", i + 1),
                event: "scene changed".into(),
                said,
                mark_id: None,
                cursor: None,
            })
            .collect();
        let bundle = ScrawlBundle {
            version: 1,
            id: "t".into(),
            created_at_ms: 0,
            duration_ms: 8_000,
            target: TargetInfo {
                mode: CaptureMode::Display,
                app: None,
                bundle_id: None,
                window_title: None,
                window_id: None,
                display_id: 1,
                frame: TargetFrame {
                    x: 0.0,
                    y: 0.0,
                    width: 1000.0,
                    height: 1000.0,
                    scale: 1.0,
                },
            },
            transcript,
            note: None,
            strokes: Vec::new(),
            marks: Vec::new(),
            events: Vec::new(),
            keyframes,
            contact_sheet: "contact-sheet.png".into(),
            video: None,
            video_offset_ms: 0,
            audio: None,
            source: None,
            device: None,
            context: Default::default(),
            context_changes: Vec::new(),
            screens: Vec::new(),
            windows: Vec::new(),
            cursor: Vec::new(),
            autocrop: None,
            destination: None,
        };
        let summary = crate::sketch::summary_markdown(&bundle, Path::new("/recordings/t"));
        assert!(summary.contains("Word times are estimated"), "{summary}");
        assert!(
            summary.contains("Erstens das hier oben.")
                && summary.find("- #1 (0:01)") < summary.find("Erstens")
                && summary.find("Erstens") < summary.find("- #2 (0:04)")
                && summary.find("- #2 (0:04)") < summary.find("Zweitens das dort unten."),
            "{summary}"
        );
    }
}
