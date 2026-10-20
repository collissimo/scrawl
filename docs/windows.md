# Windows

Read this before changing `src/capture/windows.rs`, `src/ui/windows.rs`, `src/win/`, `src/onboarding_window.rs`, `src/settings_window.rs`, or anything behind a `platform::` call in `src/capture.rs` and `src/ui.rs`.

## One app, one UI

Every GPUI view (capsule, picker, review, welcome tour) is shared; only the OS shell differs, behind `#[cfg_attr(..., path = ...)] mod platform` in `capture.rs` and `ui.rs`, which expose the same items on both OSes. Never fork a view per OS; add a platform function instead.

Windows global coordinates are physical pixels (Scrawl is per-monitor DPI aware) and `TargetFrame` holds pixels, so `MAX_CAPTURE_SCALE` is 1; views convert with `platform::points_per_pixel`.

## Shortcuts, overlay and windows

Shortcuts are `RegisterHotKey` plus low-level hooks and a key-state poller (injected input is ignored; a masked key is tapped while Alt is held so the app in front doesn't open its menu). The ink overlay is a GPUI window made layered and transparent in its build callback (after the first frame it stops drawing) and resized a turn later (inside an update GPUI misses `WM_SIZE`). It redraws only while each frame asks for the next (`request_animation_frame`), and GPUI drops a request for good when a paint finds the app busy, which leaves the last frame up with no new ink. So a watchdog in `win::overlay` looks every 100 ms: it logs the window's display, client rect and visible/topmost/minimized state once per change, and redraws the overlay and puts it back on top over its display (then the picker and pill above it) when it has drawn nothing for 600 ms, is hidden, minimized, misplaced, no longer topmost or covered by another app's window over the target. It also puts it back as Shift ink starts and as the pointer returns to its display. Search `scrawl.log` for `ink overlay`. Scrawl's popups use `WDA_EXCLUDEFROMCAPTURE` (`SCRAWL_SHOW_OWN_WINDOWS=1` keeps them for screenshots). GPUI quits when its last window closes, so `ui::platform::keep_alive` holds a hidden one.

## Helper, transcription, Settings

`win/helper.rs` stands in for `scrawl-helper`: Media Foundation for movies, `transcribe.cpp` Parakeet for `Engine::Apple`, in the language `handoff::transcription_language` names (config `language`, which Settings › Recording › **Language** and the tray's Transcription › Language write, else `en`; never the Windows locale): Hex's installed model, else Scrawl's own copy in `%LOCALAPPDATA%\Scrawl\models` (`SCRAWL_MODELS_DIR` overrides it), which `win::speech::install` downloads at launch through `speech_model::ensure` (Windows' `curl.exe`, pinned Hugging Face revision, SHA-256 checked, `.verified.json` receipt). English uses Hex's Parakeet Unified English (`PARAKEET`); every other language v3 speaks needs a multilingual model, so `win::speech::models_for` skips English-only ones by name (`-en`, `v2`) and Scrawl downloads Parakeet TDT 0.6b v3 Q8_0 (`PARAKEET_V3`, 25 European languages, German included), whose `general.languages` must list the language before it gets the hint. A language v3 doesn't speak (Japanese, Korean, …) keeps the English model (`win::speech::model_language`). `win/languages.rs` lists what v3 speaks (the Language popup offers exactly those). The first `ensure` call starts a watcher that calls `ensure` again within 5 s of the language or engine changing (Settings, the tray or a hand edit of `config.json`), or of a model landing for a language no longer chosen, so a capture made before the right model is there is transcribed again once it lands (`.transcript-pending`), in the language set at that time. Privacy switches for permissions. The welcome tour is `onboarding_window.rs` on the same JSON protocol as `helper/Onboarding.swift`, and Settings is `settings_window.rs` on the same `View`/`Action` protocol as `helper/Settings.swift` (both run in process through the sessions' `Transport::InProcess`). Launch at login is the user's `Run` key (`win/login.rs`). The native AppKit + Metal pill is macOS only: `native/pill/other.rs` keeps its shared types with a `NativePill` that never opens, and `handoff::native_pill` is always off, so Windows draws the GPUI capsule.

This build has no Claude Code dictation: transcription runs only on this PC (Parakeet, or Hex on `127.0.0.1`). The only download is the speech model from Hugging Face.

## More than one display

`screens::displays()`/`current()`/`pointer()` read `win::monitors()` and the cursor in global pixels (`Screen::id` is the monitor's index, `scale` its DPI scale), so the picker opens on the pointer's monitor and follows it like on macOS: `ui::platform::move_picker_window` moves the picker's HWND, `InkOverlay::move_to` moves the overlay (off the UI thread, as inside an update GPUI misses the resize) and `ui::platform::display_at` maps a monitor to GPUI's display by its DIP origin.

## agentContext and the command-line tool

- `agentContext` looks up what clicks and marks land on through UI Automation (`win/uia.rs`, behind `ax::hit_test`; the rest of `ax.rs`, its prober and `apply`, is shared). It reads names, localized control types and automation ids, never a control's value, so nothing typed into a field (passwords included) is read. `cargo test win::uia -- --ignored --nocapture` names whatever is under the pointer. `ax_other.rs` stays the stub for systems with neither lookup.
- Settings › Agents puts `scrawl` on PATH with a `scrawl.cmd` shim in `%LOCALAPPDATA%\Microsoft\WindowsApps`, which is on every user's PATH (`win/command_line.rs`, the same API as `command_line.rs`): a symbolic link needs Developer Mode and a hard link can't cross drives. The shim runs this `scrawl.exe` with every argument; another program called `scrawl` there is left alone.

## Recording size and storage

`win::media::Encoder` asks the H.264 encoder's `ICodecAPI` (through the sink writer's encoding parameters) for quality mode at `CODECAPI_AVEncCommonQuality` 85, which the AMD hardware encoder and Microsoft's software one both run at QP 20; an encoder that refuses it records with its defaults, and the log line `Recording encoder: … (hardware|software), rate control …, GOP …` says what took effect. Before, only `MF_MT_AVG_BITRATE` was set and the AMD encoder ran near constant bitrate on still screens. Full resolution stays: keyframes and `frames-full/` are decoded from `recording.mp4`. `rate_control_lab` in `win/media.rs` encodes a lossless screenshot as an 8 s recording per setting and prints size, decode time, and PSNR and gradient of the busiest text tile against the screenshot (`SCRAWL_LAB_PNG=shot.png cargo test rate_control_lab -- --ignored --nocapture`; `SCRAWL_LAB_ONLY`, `SCRAWL_LAB_GOP`, `SCRAWL_LAB_BUSY` vary it). Measured on a 2560×1440 screen:

| 8 s, 2560×1440 | before (average only) | quality 85 |
|---|---|---|
| cursor moving 3 s of 8 | 3.9 Mbit/s | 1.1 Mbit/s |
| a frame every 33 ms (caret, clock) | 9.4 Mbit/s | 2.5 Mbit/s |
| text PSNR / gradient vs the screenshot | 52–59 dB / 100–101% | 46 dB / 102–103% (41.5 dB on the last frame before a still stretch) |

Edges stay as crisp (gradient ≥ 100%); what's lost is fine quantization noise. A 1920×1080 screen goes from 2.1 to 0.7 Mbit/s with the cursor moving. Real 8 s captures of a screen playing video went from 4.6 to 1.8–2.7 Mbit/s. The next lever is the key frame interval: the encoder's is 30 frames (1 s while the screen changes), and 120 makes a busy capture 3× smaller again, but `frames` converts every decoded frame to RGB32 on its walk from a key frame (about 30 ms a frame at 1440p), so each far keyframe would take 3.5 s instead of 1 s to decode; decoding to NV12 and converting only the frames it returns would pay for it.

Settings › Storage › **Delete the video after each capture** (`"retention": {"keepVideo": false}`, off by default) deletes `recording.mp4` for good a minute after each capture is processed (`retention::after_capture`, called from `ui.rs`'s finish path and `scrawl record`); the frames, contact sheet, text and `audio.wav` stay. See `docs/export-and-retention.md`. Week and Month move video and audio to the Recycle Bin (`win::shell::recycle`), which frees the space only once it's emptied; the Storage note says so.

## Video export

`scrawl export` (and the menu's **Export Last Capture as Video…**) renders in process: `src/export` writes the same JSON plan as on macOS and `run_helper` hands it to `win/export/` instead of `scrawl-helper`. The Media Foundation source reader decodes BGRA, `pixels.rs` scales each tick's camera rectangle on the CPU, `draw.rs` ports `ExportInk` from `helper/Export.swift` to tiny-skia (pin numbers in Segoe UI Bold), and `encode.rs` writes H.264 (the GPU's encoder when Media Foundation has one, in quality mode 85 like the recording with a key frame every 2 s, BT.709 limited NV12; an encoder failure at any point, creating it, on a sample or finishing, renders everything again once with the software encoder, `with_software_retry`) plus 48 kHz AAC through a sink writer. The default output is `Videos\Scrawl\<bundle>.mp4`; a background (menu) export runs its threads in background mode. HEVC is macOS-only, and the index sits at the end of the file (`MF_MPEG4SINK_MOOV_BEFORE_MDAT` writes a movie Media Foundation can't read back on Windows 10). Decoded frames keep the decoder's 16 px padding, so `win::export::bgra` infers the pitch. `cargo test export` covers the math and the plan; `cargo test win::export -- --ignored --nocapture` encodes, exports and decodes synthetic movies, and `SCRAWL_EXPORT_PROBE=<movie> SCRAWL_EXPORT_FRAMES=<ms,…> cargo test probe_an_export -- --ignored --nocapture` prints a movie's probe and audio level and saves frames beside it as PNGs.

## Not on Windows

The cabled iPhone, Sparkle (**Check for Updates…** opens the releases page) and reading the browser tab URL (the `sdk.js` page bridge still works).

## Building and testing

CI (`.github/workflows/check.yml`) builds and tests on `windows-latest`. A `v*` tag runs `.github/workflows/release-windows.yml`: a `--release` build, `scrawl --help`, the Visual C++ runtime DLLs it imports copied beside it (transcribe.cpp's CMake build uses the DLL runtime, so `crt-static` doesn't link), the real model downloads (`cargo test win::speech -- --ignored`, both Parakeet models, about 1.5 GB), and `Scrawl-<version>-windows-x64.zip` (unsigned `scrawl.exe`, those DLLs, README, LICENSE) attached to the tag's GitHub release. From a Mac, `cargo xwin check --target x86_64-pc-windows-msvc` compiles it (needs `cargo-xwin`, LLVM's `clang-cl`/`lld-link`, CMake and Ninja; `XWIN_ACCEPT_LICENSE=1`).
