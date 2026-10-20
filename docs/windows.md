# Windows

Read this before changing `src/capture/windows.rs`, `src/ui/windows.rs`, `src/win/`, `src/onboarding_window.rs`, `src/settings_window.rs`, or anything behind a `platform::` call in `src/capture.rs` and `src/ui.rs`.

## One app, one UI

Every GPUI view (capsule, picker, review, welcome tour) is shared; only the OS shell differs, behind `#[cfg_attr(..., path = ...)] mod platform` in `capture.rs` and `ui.rs`, which expose the same items on both OSes. Never fork a view per OS; add a platform function instead.

Windows global coordinates are physical pixels (Scrawl is per-monitor DPI aware) and `TargetFrame` holds pixels, so `MAX_CAPTURE_SCALE` is 1; views convert with `platform::points_per_pixel`.

## Shortcuts, overlay and windows

Shortcuts are `RegisterHotKey` plus low-level hooks and a key-state poller (injected input is ignored; a masked key is tapped while Alt is held so the app in front doesn't open its menu). The ink overlay is a GPUI window made layered and transparent in its build callback (after the first frame it stops drawing) and resized a turn later (inside an update GPUI misses `WM_SIZE`); Scrawl's popups use `WDA_EXCLUDEFROMCAPTURE` (`SCRAWL_SHOW_OWN_WINDOWS=1` keeps them for screenshots). GPUI quits when its last window closes, so `ui::platform::keep_alive` holds a hidden one.

## Helper, transcription, Settings

`win/helper.rs` stands in for `scrawl-helper`: Media Foundation for movies, `transcribe.cpp` Parakeet for `Engine::Apple`, in the language `handoff::transcription_language` names (config `language`, which Settings › Recording › **Language** and the tray's Transcription › Language write, else `en`; never the Windows locale): Hex's installed model, else Scrawl's own copy in `%LOCALAPPDATA%\Scrawl\models` (`SCRAWL_MODELS_DIR` overrides it), which `win::speech::install` downloads at launch through `speech_model::ensure` (Windows' `curl.exe`, pinned Hugging Face revision, SHA-256 checked, `.verified.json` receipt). English uses Hex's Parakeet Unified English (`PARAKEET`); every other language v3 speaks needs a multilingual model, so `win::speech::models_for` skips English-only ones by name (`-en`, `v2`) and Scrawl downloads Parakeet TDT 0.6b v3 Q8_0 (`PARAKEET_V3`, 25 European languages, German included), whose `general.languages` must list the language before it gets the hint. A language v3 doesn't speak (Japanese, Korean, …) keeps the English model (`win::speech::model_language`). `win/languages.rs` lists what Claude Code and v3 each speak (the Language popup offers both; Claude Code greys out for a language outside its list). The first `ensure` call starts a watcher that calls `ensure` again within 5 s of the language or engine changing (Settings, the tray or a hand edit of `config.json`), or of a model landing for a language no longer chosen, so a capture made before the right model is there is transcribed again once it lands (`.transcript-pending`), in the language set at that time. Privacy switches for permissions. The welcome tour is `onboarding_window.rs` on the same JSON protocol as `helper/Onboarding.swift`, and Settings is `settings_window.rs` on the same `View`/`Action` protocol as `helper/Settings.swift` (both run in process through the sessions' `Transport::InProcess`). Launch at login is the user's `Run` key (`win/login.rs`). The native AppKit + Metal pill is macOS only: `native/pill/other.rs` keeps its shared types with a `NativePill` that never opens, and `handoff::native_pill` is always off, so Windows draws the GPUI capsule.

Claude Code dictation (`"transcriber": "claude"`, `Engine::Claude`, Windows only) is `win/claude_dictation.rs` over `win/conpty.rs`: a hidden `claude` in a ConPTY (kill-on-close job object, `CLAUDE*`/`ANTHROPIC*` variables removed, `--settings` with voice hold mode and `language`, `--permission-mode plan --strict-mcp-config --tools ""`), its screen read through `vt100` (`claude_dictation/screen.rs`, tested on captured output in `claude_dictation/fixtures/`). It runs through `live_transcribe::Live`:
- `start` holds the key (a space every 30 ms) on a thread of its own.
- `AudioRecorder::stop` calls `Live::release()` before the microphone closes. If Claude Code only starts listening after that, the recording falls back (`hearing_point` reads `released` under the sample buffer's lock).
- `transcribe::claude` runs `Take::finish` while Parakeet (`helper::transcribe`) transcribes the recording's `audio.wav` on another thread, so finishing takes as long as the slower of the two (25 s of German: about 1.7 s, where Claude Code alone answered in 1.1 s and falling back used to take 2.8 s).
- `finish` handles the rest. A recording without speech islands falls back at once. Otherwise it waits for the screen to settle, takes the prompt out with Ctrl+G and `scrawl.exe claude-editor`, and gives the words Parakeet's measured times (`claude_dictation/align.rs`): a Needleman-Wunsch alignment on normalized tokens (lowercase, no punctuation, umlauts and ß folded, compounds split at hyphens, near spellings like "Shilo"/"Shiloh" counted) pairs each of Claude Code's words with Parakeet's, and words without a partner are spread over the speech between their neighbours. The text stays Claude Code's; `scrawl.json` says `"model": "Claude Code dictation (de), Parakeet timing"` and `"estimated": false`, so spoken cues (English words only, `keyframes::spoken_cue`) can cut keyframes. When under 30 % of the words match, or Parakeet has no model or fails, the words are spread over the speech islands instead (`claude_dictation/spread.rs`, `"estimated": true`).
- Timeouts:
  - Settling waits for 600 ms of stillness, or 150 ms once the footer shows an error, and gives up after 8 s.
  - The capture gives up after 3 s, and the whole finish after 12 s.
- Any error (and a skipped session) uses the Parakeet transcript made alongside; the recording is never transcribed twice. A session whose begin, settle, capture or clear failed is dropped and replaced.

Claude Code is skipped, with the reason logged once (`claude_dictation::Skipped`), in two cases:
- the language isn't on its list (`win::languages::claude_supports`);
- Scrawl records a microphone other than the system default, which is the only one Claude Code hears. A chosen mic that isn't connected counts as the default.

The screen parser reads status and errors only from the footer below the last horizontal rule. The prompt is the first row of the box between the last two rules; numbered rows (selection dialogs) don't count. Only the island spread's times are estimated, and then `summary.md` says so. `reprocess`/`import` reject `--transcriber claude`.

Each process has its own settings file (`claude-settings-<pid>.json`), and each session its own capture file (`capture-<pid>-<session>.txt`). Files left by processes that are no longer running are removed at session start. The PATH lookup for `claude` is cached and refreshed in the background, so recording start never scans PATH. `claude_dictation::watch` (started in `run_app`) keeps one session warm. `SCRAWL_CLAUDE_TRACE=<file>` copies the session's terminal output to a file (cut fixtures with `cargo test cut_fixtures -- --ignored`), `SCRAWL_CLAUDE_DIR` moves its folder, and `scrawl dictation-selftest` checks a real session.

## More than one display

`screens::displays()`/`current()`/`pointer()` read `win::monitors()` and the cursor in global pixels (`Screen::id` is the monitor's index, `scale` its DPI scale), so the picker opens on the pointer's monitor and follows it like on macOS: `ui::platform::move_picker_window` moves the picker's HWND, `InkOverlay::move_to` moves the overlay (off the UI thread, as inside an update GPUI misses the resize) and `ui::platform::display_at` maps a monitor to GPUI's display by its DIP origin.

## agentContext and the command-line tool

- `agentContext` looks up what clicks and marks land on through UI Automation (`win/uia.rs`, behind `ax::hit_test`; the rest of `ax.rs`, its prober and `apply`, is shared). It reads names, localized control types and automation ids, never a control's value, so nothing typed into a field (passwords included) is read. `cargo test win::uia -- --ignored --nocapture` names whatever is under the pointer. `ax_other.rs` stays the stub for systems with neither lookup.
- Settings › Agents puts `scrawl` on PATH with a `scrawl.cmd` shim in `%LOCALAPPDATA%\Microsoft\WindowsApps`, which is on every user's PATH (`win/command_line.rs`, the same API as `command_line.rs`): a symbolic link needs Developer Mode and a hard link can't cross drives. The shim runs this `scrawl.exe` with every argument; another program called `scrawl` there is left alone.

## Video export

`scrawl export` (and the menu's **Export Last Capture as Video…**) renders in process: `src/export` writes the same JSON plan as on macOS and `run_helper` hands it to `win/export/` instead of `scrawl-helper`. The Media Foundation source reader decodes BGRA, `pixels.rs` scales each tick's camera rectangle on the CPU, `draw.rs` ports `ExportInk` from `helper/Export.swift` to tiny-skia (pin numbers in Segoe UI Bold), and `encode.rs` writes H.264 (the GPU's encoder when Media Foundation has one, BT.709 limited NV12; an encoder failure at any point, creating it, on a sample or finishing, renders everything again once with the software encoder, `with_software_retry`) plus 48 kHz AAC through a sink writer. The default output is `Videos\Scrawl\<bundle>.mp4`; a background (menu) export runs its threads in background mode. HEVC is macOS-only, and the index sits at the end of the file (`MF_MPEG4SINK_MOOV_BEFORE_MDAT` writes a movie Media Foundation can't read back on Windows 10). Decoded frames keep the decoder's 16 px padding, so `win::export::bgra` infers the pitch. `cargo test export` covers the math and the plan; `cargo test win::export -- --ignored --nocapture` encodes, exports and decodes synthetic movies, and `SCRAWL_EXPORT_PROBE=<movie> SCRAWL_EXPORT_FRAMES=<ms,…> cargo test probe_an_export -- --ignored --nocapture` prints a movie's probe and audio level and saves frames beside it as PNGs.

## Not on Windows

The cabled iPhone, Sparkle (**Check for Updates…** opens the releases page) and reading the browser tab URL (the `sdk.js` page bridge still works).

## Building and testing

CI (`.github/workflows/check.yml`) builds and tests on `windows-latest`. A `v*` tag runs `.github/workflows/release-windows.yml`: a `--release` build, `scrawl --help`, the Visual C++ runtime DLLs it imports copied beside it (transcribe.cpp's CMake build uses the DLL runtime, so `crt-static` doesn't link), the real model downloads (`cargo test win::speech -- --ignored`, both Parakeet models, about 1.5 GB), and `Scrawl-<version>-windows-x64.zip` (unsigned `scrawl.exe`, those DLLs, README, LICENSE) attached to the tag's GitHub release. From a Mac, `cargo xwin check --target x86_64-pc-windows-msvc` compiles it (needs `cargo-xwin`, LLVM's `clang-cl`/`lld-link`, CMake and Ninja; `XWIN_ACCEPT_LICENSE=1`).
