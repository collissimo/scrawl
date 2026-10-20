# Windows

Read this before changing `src/capture/windows.rs`, `src/ui/windows.rs`, `src/win/`, `src/onboarding_window.rs`, `src/settings_window.rs`, or anything behind a `platform::` call in `src/capture.rs` and `src/ui.rs`.

## One app, one UI

Every GPUI view (capsule, picker, review, welcome tour) is shared; only the OS shell differs, behind `#[cfg_attr(..., path = ...)] mod platform` in `capture.rs` and `ui.rs`, which expose the same items on both OSes. Never fork a view per OS; add a platform function instead.

Windows global coordinates are physical pixels (Scrawl is per-monitor DPI aware) and `TargetFrame` holds pixels, so `MAX_CAPTURE_SCALE` is 1; views convert with `platform::points_per_pixel`.

## Shortcuts, overlay and windows

Shortcuts are `RegisterHotKey` plus low-level hooks and a key-state poller (injected input is ignored; a masked key is tapped while Alt is held so the app in front doesn't open its menu). The ink overlay is a GPUI window made layered and transparent in its build callback (after the first frame it stops drawing) and resized a turn later (inside an update GPUI misses `WM_SIZE`); Scrawl's popups use `WDA_EXCLUDEFROMCAPTURE` (`SCRAWL_SHOW_OWN_WINDOWS=1` keeps them for screenshots). GPUI quits when its last window closes, so `ui::platform::keep_alive` holds a hidden one.

## Helper, transcription, Settings

`win/helper.rs` stands in for `scrawl-helper`: Media Foundation for movies, `transcribe.cpp` Parakeet for `Engine::Apple`: Hex's installed model, else Scrawl's own copy of Hex's Parakeet Unified English in `%LOCALAPPDATA%\Scrawl\models`, which `win::speech::install` downloads at launch through `speech_model::ensure` (Windows' `curl.exe`, pinned Hugging Face revision, SHA-256 checked, `.verified.json` receipt); a capture made before it finishes is transcribed again once it lands (`.transcript-pending`), privacy switches for permissions. The welcome tour is `onboarding_window.rs` on the same JSON protocol as `helper/Onboarding.swift`, and Settings is `settings_window.rs` on the same `View`/`Action` protocol as `helper/Settings.swift` (both run in process through the sessions' `Transport::InProcess`). Launch at login is the user's `Run` key (`win/login.rs`). The native AppKit + Metal pill is macOS only: `native/pill/other.rs` keeps its shared types with a `NativePill` that never opens, and `handoff::native_pill` is always off, so Windows draws the GPUI capsule.

## More than one display

`screens::displays()`/`current()`/`pointer()` read `win::monitors()` and the cursor in global pixels (`Screen::id` is the monitor's index, `scale` its DPI scale), so the picker opens on the pointer's monitor and follows it like on macOS: `ui::platform::move_picker_window` moves the picker's HWND, `InkOverlay::move_to` moves the overlay (off the UI thread, as inside an update GPUI misses the resize) and `ui::platform::display_at` maps a monitor to GPUI's display by its DIP origin.

## agentContext and the command-line tool

- `agentContext` looks up what clicks and marks land on through UI Automation (`win/uia.rs`, behind `ax::hit_test`; the rest of `ax.rs`, its prober and `apply`, is shared). It reads names, localized control types and automation ids, never a control's value, so nothing typed into a field (passwords included) is read. `cargo test win::uia -- --ignored --nocapture` names whatever is under the pointer. `ax_other.rs` stays the stub for systems with neither lookup.
- Settings › Agents puts `scrawl` on PATH with a `scrawl.cmd` shim in `%LOCALAPPDATA%\Microsoft\WindowsApps`, which is on every user's PATH (`win/command_line.rs`, the same API as `command_line.rs`): a symbolic link needs Developer Mode and a hard link can't cross drives. The shim runs this `scrawl.exe` with every argument; another program called `scrawl` there is left alone.

## Video export

`scrawl export` (and the menu's **Export Last Capture as Video…**) renders in process: `src/export` writes the same JSON plan as on macOS and `run_helper` hands it to `win/export/` instead of `scrawl-helper`. The Media Foundation source reader decodes BGRA, `pixels.rs` scales each tick's camera rectangle on the CPU, `draw.rs` ports `ExportInk` from `helper/Export.swift` to tiny-skia (pin numbers in Segoe UI Bold), and `encode.rs` writes H.264 (the GPU's encoder when Media Foundation has one, BT.709 limited NV12) plus 48 kHz AAC through a sink writer. The default output is `Videos\Scrawl\<bundle>.mp4`; a background (menu) export runs its threads in background mode. HEVC is macOS-only, and the index sits at the end of the file (`MF_MPEG4SINK_MOOV_BEFORE_MDAT` writes a movie Media Foundation can't read back on Windows 10). Decoded frames keep the decoder's 16 px padding, so `win::export::bgra` infers the pitch. `cargo test export` covers the math and the plan; `cargo test win::export -- --ignored --nocapture` encodes, exports and decodes synthetic movies, and `SCRAWL_EXPORT_PROBE=<movie> SCRAWL_EXPORT_FRAMES=<ms,…> cargo test probe_an_export -- --ignored --nocapture` prints a movie's probe and audio level and saves frames beside it as PNGs.

## Not on Windows

The cabled iPhone, Sparkle (**Check for Updates…** opens the releases page) and reading the browser tab URL (the `sdk.js` page bridge still works).

## Building and testing

CI (`.github/workflows/check.yml`) builds and tests on `windows-latest`. A `v*` tag runs `.github/workflows/release-windows.yml`: a `--release` build, `scrawl --help`, the Visual C++ runtime DLLs it imports copied beside it (transcribe.cpp's CMake build uses the DLL runtime, so `crt-static` doesn't link), the real model download (`cargo test win::speech -- --ignored`), and `Scrawl-<version>-windows-x64.zip` (unsigned `scrawl.exe`, those DLLs, README, LICENSE) attached to the tag's GitHub release. From a Mac, `cargo xwin check --target x86_64-pc-windows-msvc` compiles it (needs `cargo-xwin`, LLVM's `clang-cl`/`lld-link`, CMake and Ninja; `XWIN_ACCEPT_LICENSE=1`).
