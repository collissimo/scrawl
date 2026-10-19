# Scrawl on Windows: port architecture

This is the port plan, written 2026-10-19 against commit `b677c2c` on branch `t3/2c2a4260`. It is binding for every work package. Line numbers refer to `b677c2c`.

- **Target:** Windows 10 22H2 (19045), x64, MSVC toolchain, Rust 1.99.
- **This PC:** DISPLAY1 is 1920x1080, primary, at (0,0). DISPLAY2 is 2560x1440 at (1920,0). Both run at 100% DPI. The keyboard layout is US only.
- **Goal:** native Windows build with feature parity wherever Windows allows. Today the crate does not compile on Windows: the mac crates are target-gated in `Cargo.toml`, but the code uses them unconditionally.
- **Ownership:** after the foundation commit, this file is edited only by the orchestrator. Packages report in their commit messages.

## 1. Decisions (binding)

| # | Topic | Decision | Tie-breaker |
|---|---|---|---|
| D1 | Mechanism | Modules that are mostly platform code get a parallel file: `#[cfg_attr(windows, path = "win/<m>.rs")] mod <m>;`. Mostly portable files get inline `#[cfg(target_os = "macos")]` / `#[cfg(windows)]` twins. Mac bytes stay unchanged; Windows code is only added. | macOS can't be compiled here |
| D2 | `windows` crate | Aliased as `win32 = { package = "windows", version = "0.61.3" }` under `[target.'cfg(windows)'.dependencies]`. | `src/windows.rs` already claims the module name `windows`; 0.61.3 is gpui's locked version |
| D3 | Global coordinates | Virtual-screen **physical pixels**, process Per-Monitor-V2 aware. `TargetFrame.scale = 1.0`. `Screen.scale = DPI/96`, used only to size UI. Conversion to GPUI happens only at the GPUI boundary (§5). | Every OS API consumed is physical under PMv2 |
| D4 | `Screen.id` | `n` from `\\.\DISPLAYn` (1-based, stable, never 0). `ui.rs display_at` gets a Windows twin that matches by bounds. | `phone.rs:440` and `ui/lab.rs:129` use `display_id: 0` as "no display", and the label reads "Display {id}"; gpui's `DisplayId` is the 0-based `available_monitors()` index |
| D5 | `bundle_id` / `frontmost_app_info().2` / `AppMeta.bundle_id` / `TargetInfo.bundle_id` | Lowercase exe stem without `.exe`: `chrome`, `msedge`, `windowsterminal`. | `look.rs:199` and `handoff.rs:1004` take `rsplit('.')`, so `WindowsTerminal.exe` would yield `exe` |
| D6 | Config | `%USERPROFILE%\.config\scrawl\config.json`, i.e. `handoff::default_config_path` unchanged (`dirs::home_dir`). The `SCRAWL_CONFIG` override is kept. Same for `tune.json`. | No code change; SKILL.md and docs stay true; the ingest token does not roam |
| D7 | Data | Captures in `%LOCALAPPDATA%\scrawl\captures`. Logs in `%LOCALAPPDATA%\scrawl\logs\scrawl.log`. Inbox at `%LOCALAPPDATA%\scrawl\inbox.json` (follows the captures dir). Full table in §6. | Per-user, not roaming |
| D8 | Shortcut storage | `ShortcutConfig.key_code` = Windows VK. `carbon_modifiers` keeps the Carbon bit layout (CMD = Win, OPTION = Alt). The stored `label` stays in glyph form (`⌥⇧S`), so `is_default_for` and `shortcut_parts` work unchanged. The UI renders labels with `crate::win::keys::display_label` ("Alt+Shift+S"). `InteractionEvent::Key.shortcut` in summary.md uses plain form ("Ctrl+Shift+K"). The settings map's suggestion that `paste_label()` default to `"Alt+Shift+V"` is **rejected**: the default stays `⌥⇧V`, rendered at display time. | Logic and tests parse glyphs; agents read text |
| D9 | ⌘ semantics | Win key = the `cmd` bit for matching and aliases. Undo = Ctrl+Z. Logged chords = ctrl or win held. App keys: Ctrl+Q quit, Ctrl+, settings, Ctrl+W close review. | Windows conventions |
| D10 | Exe subsystem | Console subsystem (no `windows_subsystem`). In `main()`, the `None` arm (bare `scrawl`) checks `crate::win::launched_from_explorer()`: the parent process (`CreateToolhelp32Snapshot` → `th32ParentProcessID` → `QueryFullProcessImageNameW`) is `explorer.exe`, with `GetConsoleProcessList()==1` only as a fallback when the parent is gone. If so it calls `FreeConsole()` and runs `Command::App`. Otherwise it keeps the Mac logic. The Run key and shortcuts always pass `app` explicitly, so the heuristic only matters for a raw double-click. Every child the app spawns uses `CREATE_NO_WINDOW`. | CLI pipes and `cmd /c` waits work. A second tiny `[[bin]] scrawl-launch` (`src/bin/scrawl-launch.rs`, `#![windows_subsystem = "windows"]`, std only) spawns `scrawl.exe app` beside it with `CREATE_NO_WINDOW` and exits; the Run key and Start-menu shortcut point at it, so autostart never flashes a console |
| D11 | DPI awareness | `crate::win::init_process()` calls `SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)` as the first statement of `main()`. build.rs adds no manifest. | gpui's `windows-manifest` runs `embed_resource::compile`, which emits `rustc-link-arg-bins` in gpui's own build script and never reaches scrawl.exe |
| D12 | Settings / Onboarding UI | GPUI UI-host process: `scrawl.exe settings …` / `scrawl.exe onboarding …` (hidden subcommands). On Windows `helper::path()` returns `current_exe()`. The stdin/stdout JSON protocol is unchanged. `settings::Session` and `onboarding::Session` stay byte-identical apart from one `#[cfg(windows)] creation_flags(CREATE_NO_WINDOW)` line at each spawn. The UI host logs via `scrawl_log`, never to stderr (stderr is null). | `Session::open/sync/show` take no `cx` |
| D13 | Transcription | `win/transcribe.rs` with `Engine { #[default] Claude, Whisper, Hex }`. `parse("apple")` returns `None`, so the default applies. `MenuState` keeps all five Mac fields and adds `engine`, `claude_ready`, `whisper_ready`. Live = Claude Code dictation; file = whisper.cpp. `win/helper.rs::transcribe` is a `bail!`, because nothing calls it on Windows. | The user picks whisper-only without touching Claude |
| D14 | Text rasterizer | One: `src/win/text.rs`, DirectWrite (Segoe UI) with a ttf-parser + tiny-skia fallback inside the same file. Consumers: `native/pill_win/atlas.rs` and `render.rs native_text`. | Crisp 13 px pill text at 100% DPI |
| D15 | Window log chrome | `windows.rs` (Windows) emits the taskbar (`Shell_TrayWnd`, `Shell_SecondaryTrayWnd`) as `app:"Dock", layer:20`. Menus (`#32768`) and own-process topmost/tool windows are layer 101. Normal windows are 0. `autocrop.rs` is not edited. | Zero edits to `autocrop::Win::chrome` |
| D16 | Ingest server bind | `127.0.0.1:47777` by default on Windows; `SCRAWL_INGEST_LAN=1` binds `0.0.0.0` (see questions). | Avoids a Defender Firewall prompt at first launch |
| D17 | iPhone | Dropped. `helper::devices()` returns `Ok(vec![])`. `PhoneSession::start*` returns Err "iPhone capture isn't available on Windows". `record --iphone` bails before `create_dir_all`. `serve --usb` warns and is ignored. | No CoreMediaIO |
| D18 | Single instance | `run_app` holds the named mutex `Local\scrawl-app`. A second app instance logs and exits, because a second instance would fail `RegisterHotKey` and add a second tray icon. The UI host and the CLI don't take the mutex. | |
| D19 | Pill | The native layered tiny-skia pill is the default. The GPUI `HudPillWindow` fallback is used when `NativePill::open` errs or `nativePill:false`. The `Glass` style renders as Black. | |
| D20 | Exclusion from recording | Each Scrawl window owner calls `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` at creation. Ids are still pushed to `excluded_window_ids` / `passthrough_window_ids` (as HWND `u32`) for the own-window click filter. | `reset_for_session` clears `excluded_window_ids`; DDA can't filter |
| D21 | Video codecs | H.264 only. `--hevc` export bails "HEVC export isn't available on this PC" unless an HEVC encoder MFT enumerates. Retention shrink is off on Windows (`shrink_video` → `Ok(None)`). | Win10 has no inbox HEVC encoder |
| D22 | Window mode | Windows.Graphics.Capture `CreateForWindow` (via `IGraphicsCaptureItemInterop`): follows the window and captures it un-occluded, like SCK's window filter; `IsCursorCaptureEnabled` keeps the cursor. Win10 draws a yellow border around the window on screen (not in the frames). Fallback when WGC fails: fixed DDA crop of the window's `DWMWA_EXTENDED_FRAME_BOUNDS` at start. Screen and Area stay on DDA (no border). | Parity with macOS beats the on-screen border |
| D23 | Timer | `timeBeginPeriod(1)` for the app's lifetime (in the `run_app` prologue). | gpui's timer ticks at ~15.6 ms, which would make the 8 ms UI loop run at 64 Hz |
| D24 | Window ids | Every "window id/number" on Windows = `hwnd.0 as usize as u32`. User HWNDs fit in 32 bits. | Matches existing `u32` fields |

## 2. Module map

| Mac module (file) | Windows | Mechanism | Owner |
|---|---|---|---|
| `capture` (capture.rs) | `src/win/capture.rs` (+ `src/win/capture/*.rs`) | cfg_attr path | P1 |
| `screens` (screens.rs) | inline: `displays()`/`current()` re-exported from `crate::win::screens` | inline cfg | P1 |
| `hold` | inline cfg in `next()` (Window → Screen, skipping iPhone) + test gate | inline cfg | P1 |
| `helper` (helper.rs + helper/*.swift) | `src/win/helper.rs` (+ `src/win/helper/*.rs`, Media Foundation) | cfg_attr path | P2 |
| `export` (export/mod.rs) | inline cfg: `run_helper`, `default_output` | inline cfg | P2 |
| `retention` | inline cfg: `trash`, `changed_ago`, `lower_priority`, `shrink_video` | inline cfg | P2 |
| `autocrop_lab` | inline cfg: `open` URL at :253 | inline cfg | P2 |
| `laser_overlay` (+ .metal) | `src/win/laser_overlay.rs` (+ `src/win/laser_overlay/*.rs`) | cfg_attr path | P3 |
| `native::pill` (native/pill/*) | `src/native/pill_win/{mod.rs, atlas.rs}`; `model.rs` reused via `#[path = "../pill/model.rs"]` | cfg_attr path in native/mod.rs | P3 |
| `render` | inline `#[cfg(windows)] mod native_text` backed by `crate::win::text` | inline cfg | P3 |
| `ui`, `ui/lab`, `ui/motion`, `icons` | ui.rs inline cfg + `src/ui/win.rs` (twins, tray) | inline cfg + child module | P4 |
| `settings`, `onboarding` | inline cfg + GPUI UI host `src/win/ui_host.rs` (+ `src/win/ui_host/*.rs`) | inline cfg + new module | P5 |
| `command_line` | `src/win/command_line.rs` | cfg_attr path | P5 |
| `sparkle` | `src/win/sparkle.rs` (no-op) | cfg_attr path | P5 |
| `ax` | inline cfg; `hit_test` from `crate::win::uia` | inline cfg | P6 |
| `windows` (window log) | inline twins: `raw_windows`, `app_meta_of`, `dock_rect`, `snapshot` | inline cfg | P6 |
| `routing`, `routing/look`, `routing/smart`, `web` | inline cfg twins; `crate::win::uia`, `crate::win::netproc` | inline cfg | P6 |
| `autocrop` | unchanged (D15); P6 owns it in case a fallback edit is needed | none | P6 |
| `handoff`, `inbox`, `phone`, `delivery` | inline cfg; `crate::win::clipboard` | inline cfg | P7 |
| `transcribe` | `src/win/transcribe.rs` | cfg_attr path | P8 |
| `live_transcribe` | `src/win/live_transcribe.rs` | cfg_attr path | P8 |
| `mic` | inline: polling `Watch` on non-mac | inline cfg | P8 |
| `hex`, `audio` | unchanged (portable) | none | P8 |
| `model`, `keyframes`, `sketch`, `zoom`, `agent_context`, `page`, `export/{camera,crop,ink}` | unchanged | none | — |
| `main.rs`, `Cargo.toml`, `Cargo.lock`, `build.rs`, `src/native/mod.rs`, `src/win/mod.rs`, `src/win/{com,spawn,keys}.rs` | foundation only | — | F |

## 3. Module rules

1. **Replacement files.** A file loaded by `#[cfg_attr(windows, path = "win/x.rs")]` is never declared in `src/win/mod.rs`. Its children live in `src/win/x/` and are declared with an explicit `#[path = "x/child.rs"] mod child;`. Path-attributed files resolve nested modules like `mod.rs` files, and an explicit path resolves to `src/win/x/child.rs` under either reading.
2. **`src/win/mod.rs` is foundation-only.** It pre-declares every `crate::win::*` module. A package adds files only as children of modules it owns: `src/win/uia.rs` may declare `mod tree;`, which resolves to `src/win/uia/tree.rs`.
3. **`#[cfg(windows)] mod win;`** is declared at the crate root. Its helpers are reached as `crate::win::…`. The windows crate is `win32::Win32::…`. Inside `ui.rs`, the child twin module is `self::win` (`src/ui/win.rs`), which is unrelated to `crate::win`.
4. **`native::pill` on Windows:** `src/native/pill_win/mod.rs` contains `#[path = "../pill/model.rs"] pub mod model;` and `pub mod atlas;`.
5. **Inline twins** sit next to the mac item with the same name and signature: `#[cfg(target_os = "macos")] fn f(..)` / `#[cfg(windows)] fn f(..)`. If a mac signature names an AppKit type, the Windows twin substitutes `win32::Win32::Foundation::HWND` (`find_picker_ns_window`, `move_window_to_screen`, `configure_floating_window`).
6. **Never panic.** Platform failures return `Err`/`None` and `scrawl_log`. No `todo!()`, `unimplemented!()` or `unwrap()` on OS results.
7. **Spawned children** (opencode2, whisper-cli, tailscale, the UI host, the explorer reveal) use `crate::win::spawn::no_window(&mut cmd)`.
8. **Threads and COM.** Every thread that touches COM, Media Foundation or UI Automation creates `crate::win::com::ComGuard::mta()` first (it tolerates `RPC_E_CHANGED_MODE`). Low-level hooks and `RegisterHotKey` live on the dedicated `scrawl-input` thread, never on the gpui main thread.
9. **Tests.** Mac-only tests that fail to compile on Windows are gated `#[cfg(target_os = "macos")]`, never deleted. Pure logic copied into a Windows file brings its tests along. Tests that need a desktop, hooks, a GPU or a microphone are `#[ignore]` and run by hand.

## 4. Platform API contracts

Signatures are copied from the Mac code. A Windows file must expose exactly these, with the same visibility. "Verbatim" means: copy the Mac lines unchanged.

### 4.1 `crate::capture` → `src/win/capture.rs` (P1)

**Copied verbatim** from capture.rs (with these exceptions: the KEY_* consts become VKs and `scrawl_log` gets the Windows dir):
- 21-110: consts, `CandidateBuffer`, `ScreenRecording`.
- 140-143: `CARBON_*`.
- 292-313: `LiveOverlayEvent`.
- 421-518: `ShortcutConfig`, `ShortcutKind`, `modifier_symbols`.
- 527-546: `scrawl_log`.
- 667-691: `hotkey_combos`.
- 747-801: `set_shortcuts`, `pause_shortcuts`, `update_global_shortcut`; `CARBON_HOTKEYS.get()` becomes "the hotkey window exists".
- 803-863: `shortcut_parts`, `is_capture_shortcut`, `shortcut_matches`.
- 1062-1071: `clean_label`.
- 1078-1094: `pick_display`.
- 1247-1252: `selectable_window_at`.
- 1763-1806: `region_target`; the fallback becomes the primary `Screen.id` and the scale becomes 1.0.
- 2134-2144: `capture_size`.
- 2237-2739: `SharedCaptureTelemetry` and its impl, keeping the private field names. Swap only `on_scrawl_window`, `probing` and `check_key_tap`.
- 2924-2952: `poll_clicks`, `same_target_frame`.
- 3115-3266: HID poller body, with API swaps only.
- 3323-3363: `write_bundle_artifacts`.
- The pure tests listed in §9.

```rust
pub const FRAME_SAMPLE_INTERVAL: Duration; pub const CANDIDATE_EDGE: f64 = 480.0; pub const CANDIDATE_BUDGET_BYTES: usize = 384 << 20;
pub struct CandidateBuffer; fn new(budget_bytes: usize) -> Self; fn interval(&self) -> Duration; fn push(&mut self, frame: RgbaFrame, urgent: bool); fn into_frames(self) -> Vec<RgbaFrame>
pub struct ScreenRecording { pub frames: Vec<RgbaFrame>, pub video: Option<PathBuf>, pub duration_ms: u64, pub video_offset_ms: u64 }
pub const CARBON_CMD_KEY: u32 = 1<<8; CARBON_SHIFT_KEY = 1<<9; CARBON_OPTION_KEY = 1<<11; CARBON_CONTROL_KEY = 1<<12;
pub fn modifier_symbols(carbon_modifiers: u32) -> String
pub enum LiveOverlayEvent { CaptureShortcut, PasteLatestShortcut, UndoStroke, CancelCapture, RequestSnapshot, PhoneEnded, Onboarding(crate::onboarding::Event), Settings(crate::settings::Event), IngestedBundle { dir: PathBuf, bundle: Box<crate::model::ScrawlBundle>, status: Option<String> } }
pub struct ShortcutConfig { pub key_code: u32, pub carbon_modifiers: u32, pub label: String }  // Serialize/Deserialize, Default = default_record
  fn default_record() -> Self; fn default_paste() -> Self; fn is_default_for(&self, kind: ShortcutKind) -> bool; fn key_label(&self) -> &str
pub enum ShortcutKind { Record, Paste }  // serde kebab-case; const ALL: [Self; 2]; fn default_config(self) -> ShortcutConfig; fn name(self) -> &'static str
pub fn scrawl_log(msg: impl AsRef<str>)                      // %LOCALAPPDATA%\scrawl\logs\scrawl.log
pub fn install_global_hotkeys(telemetry: &Arc<SharedCaptureTelemetry>)
pub fn set_shortcuts(telemetry: &SharedCaptureTelemetry, record: ShortcutConfig, paste: ShortcutConfig) -> Result<(), ShortcutKind>
pub fn pause_shortcuts(telemetry: &SharedCaptureTelemetry, paused: bool)   // also makes the LL key hook pass through
pub fn update_global_shortcut(telemetry: &SharedCaptureTelemetry, config: ShortcutConfig)
pub fn shortcut_parts(label: &str) -> (&str, &str)
pub fn is_capture_shortcut(cfg: &ShortcutConfig, s_code: u16, keycode: u16, cmd: bool, ctrl: bool, alt: bool, shift: bool) -> bool
pub fn shortcut_matches(kind: ShortcutKind, cfg: &ShortcutConfig, layout_code: u16, keycode: u16, cmd: bool, ctrl: bool, alt: bool, shift: bool) -> bool
pub fn init_keyboard_layout_map() -> &'static std::collections::HashMap<char, u16>   // VkKeyScanExW(ch, GetKeyboardLayout(0)) low byte
pub fn key_code_for_char(ch: char) -> Option<u16>; pub fn layout_s_keycode() -> u16; pub fn layout_z_keycode() -> u16; pub fn layout_v_keycode() -> u16
pub fn shortcut_from_key_and_mods(key: &str, cmd: bool, ctrl: bool, alt: bool, shift: bool) -> Option<ShortcutConfig>  // gpui key names; 'space'->0x20, f1..f12->0x70..0x7B
pub fn screen_capture_granted() -> bool          // true
pub fn screen_capture_usable(probe: bool) -> bool // true
pub fn screen_capture_probe() -> Result<(u32, u32), String>  // D3D11 + DuplicateOutput(primary) + AcquireNextFrame(500ms) -> output size, or Err with HRESULT
pub fn request_screen_capture()                  // no-op
pub fn accessibility_granted() -> bool           // true (gates ax probing)
pub fn modifier_flags() -> u64                   // Mac bit layout: SHIFT 1<<17, CONTROL 1<<18, ALTERNATE 1<<19, COMMAND 1<<20, from GetAsyncKeyState
pub fn set_escape_hotkey_enabled(_enabled: bool) // no-op
pub fn wait_for_modifiers_released(max_wait_ms: u64)
pub fn discover_target(window_query: Option<&str>) -> TargetInfo
pub fn discover_target_at(window_query: Option<&str>, point: Option<[f64; 2]>) -> TargetInfo
pub fn pick_display(displays: &[[f64; 4]], point: Option<[f64; 2]>, focused: Option<[f64; 4]>) -> usize
pub fn list_selectable_windows() -> Vec<TargetInfo>            // via crate::win::winlist; front to back
pub fn selectable_window_at(windows: &[TargetInfo], x: f64, y: f64) -> Option<&TargetInfo>
pub fn region_target(x: f64, y: f64, width: f64, height: f64, windows: &[TargetInfo]) -> TargetInfo
pub fn record_external_frontmost_app()   // cheap: called every 8 ms while the island is visible
pub fn ensure_external_app_active()      // SetForegroundWindow(last external hwnd), poll <=220 ms
pub fn frontmost_external_pid() -> Option<i32>
pub fn frontmost_app_info() -> Option<(i32, String, Option<String>)>  // (pid, app_name, Some(app_id)) per D5
pub fn is_frontmost_terminal() -> bool   // app_id/class in: windowsterminal, openconsole, conhost, cmd, powershell, pwsh, mintty, wezterm-gui, alacritty; classes CASCADIA_HOSTING_WINDOW_CLASS, ConsoleWindowClass, mintty
pub struct ScreenCaptureSession { started_at: Instant, stopping: Arc<AtomicBool>, urgent_snapshot: Arc<AtomicBool>, frames: Arc<Mutex<CandidateBuffer>>, video_offset_ms: Arc<AtomicU64>, sampler_thread: Option<JoinHandle<Option<PathBuf>>>, _recording: crate::retention::Recording }
  pub fn start(target: TargetInfo, video_output_path: Option<PathBuf>, excluded_window_ids: Arc<Mutex<Vec<u32>>>) -> Self
  pub fn request_immediate_frame(&self); pub fn elapsed_ms(&self) -> u64; pub fn stop(mut self) -> ScreenRecording; impl Drop
pub fn capture_size(target: &TargetInfo, long_edge: f64) -> (u32, u32)
pub struct SharedCaptureTelemetry { pub active, selecting, shift_held, capture_mode: AtomicU64, shortcut, paste_shortcut, shortcuts_paused, started_at, picker_started_at, picker_rect: Mutex<Option<([f32; 4], u8)>>, target_frame: Mutex<TargetFrame>, pointer_screen: Mutex<[f32; 2]>, events, strokes, active_laser, live_ink, excluded_window_ids: Arc<Mutex<Vec<u32>>>, window_log_request: Arc<AtomicBool>, ui_sender: flume::Sender<LiveOverlayEvent>, passthrough_window_ids: Mutex<Vec<u32>>, start_glow: AtomicBool, + private (same names as Mac) }
  pub fn new(ui_sender: flume::Sender<LiveOverlayEvent>) -> Arc<Self>; emit_capture_shortcut(&self); emit_paste_shortcut(&self); matches_shortcut(&self, keycode: u16, cmd: bool, ctrl: bool, alt: bool, shift: bool) -> bool; shortcuts(&self) -> (ShortcutConfig, ShortcutConfig); set_capture_mode(&self, mode: CaptureMode); begin_picker_session(&self); set_picker_rect(&self, rect: Option<([f32; 4], u8)>); reset_for_session(&self, frame: TargetFrame); take_escape(&self) -> bool; elapsed_ms(&self) -> u64; contains_screen_point(&self, screen_x: f64, screen_y: f64) -> bool; normalize_point(&self, screen_x: f64, screen_y: f64) -> (f32, f32, u32, u32); finish_active_laser(&self, _ax_point: Option<(f64, f64)>); finish_session(&self) -> (Vec<InteractionEvent>, Vec<Stroke>)
pub fn start_global_input_tap(telemetry: Arc<SharedCaptureTelemetry>)
pub fn current_mouse_screen() -> Option<[f32; 2]>   // GetCursorPos, physical
pub fn post_synthetic_cmd_v()                       // SendInput Ctrl down, V(layout_v_keycode) down, 12 ms, V up, Ctrl up; dwExtraInfo = SCRAWL_INJECT_MAGIC
pub fn post_synthetic_click(screen_x: f64, screen_y: f64)  // SendInput MOVE|ABSOLUTE|VIRTUALDESK, LEFTDOWN, 14 ms, LEFTUP
pub fn write_bundle_artifacts(out_dir: &Path, bundle: &crate::model::ScrawlBundle, annotated_frames: &[(crate::model::Keyframe, RgbaFrame)], contact_sheet: &RgbaFrame, summary_md: &str) -> Result<()>
// Windows-only additions (callers use them under #[cfg(windows)]):
pub const SCRAWL_INJECT_MAGIC: usize = 0x5C4A_57A1;      // our SendInput tag; the hooks ignore it
pub fn post_synthetic_shift_insert()                     // terminal paste
pub fn post_alt_mask_key()                               // VK 0xE8 down/up, tagged; sent on WM_HOTKEY and before paste so Alt release doesn't open menus
```

Platform semantics (P1):
- **Recording.** `scrawl-frame-sampler` owns all COM/D3D/MF objects. It matches the output whose `DXGI_OUTPUT_DESC.Monitor` matches `display_id`, creates the D3D11 device on that output's adapter, and calls `DuplicateOutput`. Crop (output-local px): Display = the whole output; Region = `screens::source_rect(area, display.frame)`; Window = the start rect clamped (D22). The cursor is composited via `GetCursorInfo` + `DrawIconEx` into a DIB. The MP4 uses `MFCreateSinkWriterFromURL`, H.264 at ~8-12 Mbps, at most 30 fps, written on change plus a repeat ≥1/s, with 100-ns times relative to the first sample. `video_offset_ms` = the capture time of the first sample since `started_at`. Candidates come from the latest composed frame, downscaled to `capture_size(target, 480 or 1280)`. `DXGI_ERROR_ACCESS_LOST` → recreate the duplication. `E_ACCESSDENIED` → repeat the last frame. Fallbacks: DDA → GDI BitBlt → `render_synthetic_demo_frame`. If the sink writer fails: `video: None` and 1280 candidates.
- **Input.** One `scrawl-input` thread, started by whichever of `install_global_hotkeys` / `start_global_input_tap` comes first (OnceLock). It owns a message-only HWND (`RegisterHotKey` ids unchanged: TOGGLE = 1, ALT1 + i, PASTE_BASE + i; modifiers mapped `MOD_*|MOD_NOREPEAT`), plus `WH_KEYBOARD_LL` and `WH_MOUSE_LL`. Register/unregister is marshalled via `SendMessageW(WM_APP+n)`. Hook procs only copy the event to a flume worker and return `CallNextHookEx`. The worker skips injected events: `LLKHF_INJECTED`/`LLMHF_INJECTED` with `dwExtraInfo == SCRAWL_INJECT_MAGIC`; it must also not count gpui's injected `VK_MENU` as a hold. It drops clicks on own windows via `on_scrawl_window` (`WindowFromPoint` → `GA_ROOT` → own pid, minus passthrough ids). Wheel: `dy = -(delta)/120`; horizontal wheel: `dx = +delta/120`. Autorepeat = the vk is already in the down-set. Dedup windows: 90 ms capture, 400 ms paste.
- **Keys.** Fixed-name table in VK: 0x0D ↩, 0x09 ⇥, 0x20 Space, 0x08 ⌫, 0x1B Esc, 0x2E ⌦, 0x25 ← … 0x70-0x7B F1-F12. Chord names in summary.md use plain form (D8).

### 4.2 `crate::screens` (P1)

Unchanged: `Rect`, `Screen { pub id: u32, pub frame: Rect, pub visible: Rect, pub scale: f64, pub safe_top: f64 }`, `flip`, `contains`, `index_at`, `index_for_rect`, `to_local`, `clamp_to`, `source_rect`, `MIN_AREA`, `area_on_display`, `with_scale`, `pill_origin`, `pointer`.

Windows twins: `pub fn displays() -> Vec<Screen>; pub fn current() -> Vec<Screen>`, re-exported from `crate::win::screens`.
- Source: `EnumDisplayMonitors` + `GetMonitorInfoW(MONITORINFOEXW)`.
- `frame` = `rcMonitor`, `visible` = `rcWork` (physical px).
- `scale` = `GetDpiForMonitor(MDT_EFFECTIVE_DPI)/96`; `safe_top` = 0; `id` = DISPLAYn (D4).
- Primary first; zero-size monitors filtered out. `current()` == `displays()`, callable from any thread.

### 4.3 `crate::helper` → `src/win/helper.rs` (P2)

```rust
pub fn path() -> Result<PathBuf>                     // current_exe() (UI host, D12); phone/export never reach it on Windows
#[derive(Clone, Debug, Deserialize, PartialEq)] pub struct PhoneDevice { pub id: String, pub name: String, pub model: String, pub width: u32, pub height: u32 }
pub fn devices() -> Result<Vec<PhoneDevice>>         // Ok(vec![])
pub fn permissions() -> Result<std::collections::BTreeMap<crate::onboarding::Permission, crate::onboarding::Status>>
    // ScreenRecording, Accessibility, InputMonitoring = Granted; Camera = NotDetermined; Microphone = Denied if ConsentStore\microphone (HKLM, HKCU, HKCU\NonPackaged) Value == "Deny", else Granted
pub fn transcribe(audio: &Path) -> Result<TranscriptData>   // bail!: Windows transcription lives in win/transcribe.rs (D13)
#[derive(Debug, Deserialize)] #[serde(rename_all = "camelCase")] pub struct Probe { pub duration_ms: u64, #[serde(default)] pub width: u32, #[serde(default)] pub height: u32, #[serde(default)] pub audio_tracks: u32, #[serde(default)] pub fps: f64 }
pub fn probe(movie: &Path) -> Result<Probe>          // MF SourceReader: MF_PD_DURATION/10_000; display aperture else frame size; rotation swaps w/h; fps = MF_MT_FRAME_RATE
pub fn shrink(movie: &Path, out: &Path, max_edge: u32) -> Result<crate::retention::Shrunk>  // bail!("shrinking needs macOS") (D21)
pub fn export_audio(movie: &Path, wav: &Path) -> Result<()>  // MF float decode, all audio streams mixed to mono, 16 kHz (area-average), leading zeros for first pts, audio::encode_wav_16k; no audio -> Err("Movie has no audio")
pub enum FrameTimes<'a> { Every(u64), At(&'a [u64]) }
pub fn frames(movie: &Path, times: FrameTimes, max_edge: u32) -> Result<Vec<RgbaFrame>>
pub fn qr(text: &str) -> Result<String>              // Err("QR codes need macOS")
// Windows-only addition, called by export/mod.rs cfg(windows) run_helper:
pub fn export(plan: &Path, background: bool, progress: &mut dyn FnMut(f32)) -> Result<(u64 /*duration_ms*/, u64 /*bytes*/)>
```

`frames` contract (load-bearing: `main.rs merge_full_keyframes` matches within 250 ms and silently keeps 480 px candidates on a mismatch):
- Output is RGBA8 with alpha 255, rotated per `MF_MT_VIDEO_ROTATION`, cropped to the display aperture.
- `max_edge` 0 = native. Otherwise scale to long edge ≤ `max_edge`, never upscaling.
- `Every(ms)`: grid 0, ms, 2ms … < duration. Per grid point, take the latest frame with pts ≤ g + ms/2. `t_ms` = that frame's actual pts. Skip a grid point that would repeat the previous source frame.
- `At(times)`: empty → `Ok(vec![])`. Clamp each t to [0, duration-1] and keep the request order. Take the frame on screen at t (latest pts ≤ t). `t_ms` = the requested (clamped) t. A frame that fails is skipped, not an error. Duration 0 → Err("Movie has no duration").
- Up to 6 workers, each with its own SourceReader, `ComGuard` + `MfGuard`.

`export` reads the same `PLAN.json` that `export/mod.rs` writes. It renders CFR frames composited with tiny-skia ink (strokes, pins, lasers, ripples, trails; exact formulas in the helper subsystem map, ported from helper/Export.swift). It encodes H.264 High via SinkWriter, BT.709, bitrate `min(18e6, max(1.5e6, W*H*fps*0.065))`, GOP 2 s, AAC 48 kHz mono 96 kbps. It calls `progress` every 120 ticks. It checks the written duration within 0.25 s, else deletes the file and bails. `background` → `THREAD_MODE_BACKGROUND_BEGIN`.

### 4.4 `crate::laser_overlay` → `src/win/laser_overlay.rs` (P3)

```rust
pub struct MetalLaserOverlay { .. }   // keep the name: ui.rs imports it
impl MetalLaserOverlay {
    pub fn open(telemetry: Arc<SharedCaptureTelemetry>, on: Option<[f64; 4]>) -> Result<Self, String>
    pub fn window_number(&self) -> u32        // HWND as u32
    pub fn screen(&self) -> [f64; 4]          // stored Screen.frame (physical px)
    pub fn move_to(&mut self, rect: [f64; 4]) -> bool
}
impl Drop for MetalLaserOverlay
```

Design:
- Own thread per overlay, running a `WS_EX_LAYERED|TRANSPARENT|TOPMOST|TOOLWINDOW|NOACTIVATE` popup at the monitor rect with WDA_EXCLUDEFROMCAPTURE and a 32bpp top-down DIB.
- `open` blocks until the first frame is presented.
- Loop: pump, draw, `DwmFlush`.
- Copied verbatim from laser_overlay.rs: the uniforms/segment/pin gathering and `bin_segments`.
- `laser_fragment` is ported to `fn shade(p_pt, u, segs, pins, tiles) -> [f32; 4]` (premultiplied).
- Only the dirty rect is redrawn (full screen during picker fade, intro < 0.24 s or glow < 1.1 s), rows split across threads, presented with `UpdateLayeredWindowIndirect(prcDirty)`.
- Local point = (global − frame.xy) / scale.

### 4.5 `crate::native::pill` → `src/native/pill_win/` (P3)

```rust
#[path = "../pill/model.rs"] pub mod model;   // unchanged pure model (Pres, PANEL_W/H/Y, Metrics, PillModel, FrameState, ElemId)
pub mod atlas;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] pub enum PillStyle { #[default] Black, Glass }
impl PillStyle { pub fn from_str_opt(s: &str) -> Option<Self>; pub fn as_str(self) -> &'static str; pub fn current() -> Self }   // verbatim
#[derive(Clone, Copy, Debug)] pub enum PillAction { SelectMode(SelectionMode), SwitchRecording(SelectionMode), RecordSelected, Undo, Cancel, Finish, Paste(Option<[f32; 2]>), Inspect, DismissCompleted }   // Paste point: global physical px
#[derive(Clone, Debug, PartialEq)] pub struct PillSnapshot { pub pres: Pres, pub clock: String, pub reduced_motion: bool, pub style: PillStyle }
pub struct NativePill { .. }
impl NativePill {
    pub fn open() -> Result<Self, String>          // main thread; never panics; Metrics::default() on text failure
    pub fn window_number(&self) -> u32
    pub fn place_on(&self, point: [f64; 2])        // screens::index_at, pill_origin(screen, PANEL_W*scale, PANEL_Y*scale); re-raster on a DPI change
    pub fn sync(&self, snapshot: PillSnapshot)
    pub fn drain_actions(&self) -> Vec<PillAction>
    pub fn is_fully_hidden(&self) -> bool
    pub fn pointer_on_pill(&self) -> bool
}
impl Drop for NativePill
```

Details:
- The atlas (`pill_win/atlas.rs`) keeps the Mac API except `GlyphAtlas::new(scale: f32)` (no device) and `pub pixmap: tiny_skia::Pixmap` in place of `texture`.
- Ticks: a vsync thread runs `DwmFlush` and then `PostMessageW(WM_APP_TICK)` to the pill HWND. The main thread runs `tick_main` (motion.rs thread-locals).
- Clicks: toggle `WS_EX_TRANSPARENT` while the pointer is over the island (+6 pt). `WM_MOUSEACTIVATE` → `MA_NOACTIVATE`.
- Compositing: CPU port of `pill_composite_fragment` into a premultiplied BGRA DIB, presented with `UpdateLayeredWindow`.

### 4.6 `crate::transcribe` → `src/win/transcribe.rs`, `crate::live_transcribe` → `src/win/live_transcribe.rs` (P8)

```rust
// transcribe
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)] pub enum Engine { #[default] Claude, Whisper, Hex }
impl Engine { pub fn parse(name: &str) -> Option<Self> /* "claude"|"whisper"|"hex", trim+lowercase */; pub fn name(self) -> &'static str; pub fn configured() -> Self; pub fn preferred() -> Self }
pub fn set_override(engine: Engine)
#[derive(Debug, PartialEq, Eq)] pub struct MenuState { pub apple_checked: bool, pub hex_title: &'static str, pub hex_enabled: bool, pub hex_checked: bool, pub models: Vec<(String, String, bool)>, pub engine: Engine, pub claude_ready: bool, pub whisper_ready: bool }
pub fn menu_state(preferred: Engine, status: Option<&crate::hex::Status>, configured_model: Option<&str>) -> MenuState
pub fn transcribe(wav: Option<&Path>, audio: Option<&RecordedAudio>) -> TranscriptData   // never panics; "none" without audio, "offline" when every engine failed
// live_transcribe
pub fn wanted() -> bool                                   // preferred()==Claude && session Ready; non-blocking; kicks warm() otherwise
pub struct Live { .. }                                     // Send
impl Live { pub fn start(samples: Arc<Mutex<Vec<f32>>>) -> Result<Self>; pub fn finish(mut self, wav_bytes: &[u8]) -> Result<Finished> }
impl Drop for Live                                         // cancel on a detached thread, never blocks
#[derive(Debug)] pub struct Finished { pub transcript: TranscriptData, pub tail_ms: u64, pub streamed_samples: usize }
pub fn simulate(wav_bytes: &[u8], speed: f64) -> Result<Live>   // bail!("--live-speed isn't available on Windows: Claude Code records the microphone itself")
#[derive(Clone, Default)] pub struct Slot(Arc<Mutex<Option<Live>>>); impl Slot { pub fn new(live: Option<Live>) -> Self; pub fn take(&self) -> Option<Live> }  // + impl Debug (verbatim)
pub fn warm(); pub fn restart(); pub fn shutdown()        // Windows-only additions (app init / settings change / quit)
```

### 4.7 `crate::command_line` → `src/win/command_line.rs`, `crate::sparkle` → `src/win/sparkle.rs` (P5)

```rust
#[derive(Clone, Debug)] pub struct Places { pub target: Option<PathBuf>, pub install: Vec<PathBuf>, pub also: Vec<PathBuf> }
impl Places { pub fn system() -> Self }   // target = dunce::canonicalize(current_exe) unless under target\{debug,release} or deps; install = [%LOCALAPPDATA%\scrawl\bin]; also = [~\.cargo\bin]
#[derive(Clone, Debug, PartialEq, Eq)] pub enum State { Unavailable, Installed { link: PathBuf, homebrew: bool }, Missing { link: PathBuf }, Stale { link: PathBuf }, Taken { link: PathBuf }, Nowhere }
pub fn state(places: &Places) -> State
pub fn install(places: &Places) -> Result<PathBuf>        // writes scrawl.cmd + `scrawl` sh shim (marker "scrawl-shim"), adds the dir to HKCU\Environment Path, broadcasts WM_SETTINGCHANGE
pub fn uninstall(places: &Places) -> Result<()>
pub fn tilde(path: &Path) -> String
pub fn usually_on_path(link: &Path) -> bool
// sparkle
pub fn start(); pub fn available() -> bool /* false */; pub fn check_for_updates()   // log once, never panic
```

### 4.8 Shared files with inline twins

| File | Mac items gated `#[cfg(target_os="macos")]` | Windows twin (same name/signature unless noted) | Owner |
|---|---|---|---|
| `src/ax.rs` | use core_foundation (6-7), extern block (14-34), `Owned`/`AxElement`, attribute helpers + `hit_test` (130-250) | `#[cfg(windows)] pub use crate::win::uia::hit_test;` → `pub fn hit_test(x: f64, y: f64) -> Option<Hit>` (UIA `ElementFromPoint`, `document: None`). Foundation stub returns `None`. Pure helpers get `#[cfg_attr(windows, allow(dead_code))]`. | P6 |
| `src/windows.rs` | `app_meta_of` (291-303), `dock_rect` (305-344), `snapshot` (347-365, autoreleasepool) | `pub fn raw_windows() -> Vec<RawWindow>` (from `win::winlist`, D15 layers, alpha from `GetLayeredWindowAttributes`); `fn app_meta_of(pid: i32) -> AppMeta` (bundle_id = `process::app_id`, regular = owns an app window); `pub fn dock_rect(frame: TargetFrame) -> Option<[f64; 4]>` (rcMonitor − rcWork, any side incl. top); `pub fn snapshot(..)` duplicated without the pool | P6 |
| `src/screens.rs` | `displays`, `current` (153-227) | `#[cfg(windows)] pub use crate::win::screens::{current, displays};` | P1 |
| `src/hold.rs` | — | `next()`: `#[cfg(windows)] SelectionMode::Window => SelectionMode::Screen`; gate the iPhone step of the test at 248-250 | P1 |
| `src/retention.rs` | `changed_ago` body (225-230), the QoS libc call (628-633), the `trash` body (340-352) | `changed_ago`: `metadata.modified()` of the dir. `lower_priority`: `SetThreadPriority(THREAD_MODE_BACKGROUND_BEGIN)`. `trash`: `SHFileOperationW(FO_DELETE, FOF_ALLOWUNDO\|NOCONFIRMATION\|SILENT\|NOERRORUI)`. `shrink_video`: `#[cfg(windows)] return Ok(None)` first | P2 |
| `src/export/mod.rs` | `run_helper` (342-384), `default_output` dir, the test at 456-464 | `run_helper` → `crate::helper::export` mapped into `Done{duration_ms, bytes}`; `default_output` → `dirs::video_dir()/Scrawl` | P2 |
| `src/autocrop_lab.rs` | `Command::new("open")` (253) | `crate::win::spawn::shell_open(&url)` | P2 |
| `src/render.rs` | (existing `native_text`) | `#[cfg(windows)] mod native_text { pub fn width(text: &str, size: f32, bold: bool) -> Option<f32>; pub fn mask(text: &str, size: f32, bold: bool) -> Option<(usize, usize, Vec<u8>)> }`. Mask: width = ceil(advance)+4, height = ceil(1.2*size)+4, baseline at y = 2 + 0.952*size (Helvetica Neue ascent, keeps badge centring). `text_width`/`draw_text` get `#[cfg(windows)]` blocks mirroring the mac ones | P3 |
| `src/native/pill/model.rs` | — | cfg-inline only at :614: `has_iphone \|\| (hold && cfg!(target_os = "macos"))` | P3 |
| `src/ui.rs` | the items in the list below | `src/ui/win.rs` twins | P4 |
| `src/ui/lab.rs` | backdrop block (349-357) | the HWND from the gpui window → `configure_floating_window(&hwnd, true, 4)` | P4 |
| `src/settings.rs` | login item block (387-459), mac key tables (259-289), `reserved()` (320-347), mac-bound tests | `login_item`/`set_login_item`/`open_login_items_settings` on the HKCU Run key + StartupApproved; VK tables; Windows reserved combos (any Win combo, Alt+Tab, Alt+F4, Alt+Esc, Alt+Space, Ctrl+Esc, Ctrl+Shift+Esc, Win+Shift+S, PrintScreen); messages "Hold Ctrl or Alt with the key."; `ShowCaptures` → `explorer <dir>`; spawn gets `CREATE_NO_WINDOW`; `TranscriberView.engine` = `menu.engine.name()` | P5 |
| `src/onboarding.rs` | `relaunch()` body (977-1004) | respawn `current_exe()` with `SCRAWL_RELAUNCH_AFTER_PID`; spawn gets `CREATE_NO_WINDOW`. Everything else unchanged (`Permission::REQUIRED` stays 3) | P5 |
| `src/routing.rs` | `browser_tab_url` (156-181), `listener_directory` (255-266) | → `crate::win::uia::browser_url(bundle_id)`, `crate::win::netproc::listener_dir(port)`. `browser_kind` gets a cfg arm for exe stems (chrome, msedge, brave, vivaldi, chromium, opera, thorium → Chromium; firefox, librewolf, zen → `#[cfg(windows)] Firefox`). `run_with_timeout` → `no_window`. The lsof parsers stay compiled | P6 |
| `src/routing/look.rs` | `own_names` (524-528), `tailscale` const (548), `terminal_pane` (561-575), `is_tool` (241-254), `repo_of` canonicalize (329) | `GetComputerNameExW` + `<host>.local`; `C:\Program Files\Tailscale\tailscale.exe`; `terminal_pane` → `None`; Windows tool stems; `dunce::canonicalize` | P6 |
| `src/routing/smart.rs` | — | `is_home_or_root`: also true for drive roots (`path.parent().is_none()`) | P6 |
| `src/web.rs` | — | directory check (224-230): trim `/` and `\`, accept `Path::is_absolute()` | P6 |
| `src/handoff.rs` | — | `default_captures_dir` (D7); `canon()` = dunce on Windows, used at 46, 85, 1216 + tests; clipboard/paste twins (narrow the stubs to `not(any(macos, windows))`); `random_hex_token_32` → `getrandom::fill`; `on_path`/`executable` probe PATHEXT; `file_uri` → `file:///C:/…`; `opencode_api` → `no_window` + argv-length guard; agentHost/ssh/tar → bail "agentHost isn't supported on Windows" | P7 |
| `src/inbox.rs` | — | `dunce::canonicalize` (28, 70) | P7 |
| `src/phone.rs` | — | `spawn_mirror` bails (D17); `spawn_usb_poller` no-op + log; `discover_default_ingest_urls` uses the Tailscale path + `GetAdaptersAddresses`; `serve_http` bind per D16 | P7 |
| `src/mic.rs` | — | non-mac `Watch`: 2 s polling thread comparing `inputs()`, stopped by an AtomicBool in Drop | P8 |

`ui.rs` items gated (mac) and twinned in `src/ui/win.rs` (P4); the foundation creates no-op/Err stubs:
- `use` lines 13-23 (objc2*, screencapturekit).
- `reduce_motion` → `SPI_GETCLIENTAREAANIMATION`.
- `MenuTargetIvars` + `define_class!(ScrawlMenuTarget)`, `rebuild_inbox_menu_items`, `set_status_text`, `set_status_inbox_count`, `StatusBarController`, `STATUS_BAR`, `install_status_bar` → tray (`Shell_NotifyIconW`, `TaskbarCreated`, no "Check for Updates…"). `MenuAction` and `record_menu_title` stay shared.
- `find_hud_pill_ns_window`, `sync_full_size_content_view` → dropped. `pointer_on_hud_pill` → twin.
- `find_picker_ns_window() -> Option<HWND>`, `move_window_to_screen(hwnd: &HWND, frame: [f64; 4])`, `configure_floating_window(hwnd: &HWND, ignores_mouse: bool, level_offset: isize)`.
- `OpaqueCGColor`, `CATransform3D`, `apply_pill_layer_effect`, `top_left_frame`, `trace_capsule` → dropped. `HudPillWindow` fields `ns_window`/`shell_layer` and their blocks are gated; Windows uses `trace_xf = None` and `has_shell = false`.
- `start_video_export` reveal → `crate::win::spawn::reveal_in_explorer`.
- `open_picker_windows` callback block (5682-5704) → Windows block: hwnd, `PICKER_HWND`, `configure_floating_window`, `move_window_to_screen`, excluded id, `SetForegroundWindow` unless held.
- `ensure_hud_pill_open`, `remember_front_app`, `hand_back_keys`, `PREVIOUS_FRONT_APP`, `open_review_window` (native title bar, `activate_window`), `stay_out_of_dock` (no-op), the `run_app` screenshot lookup (6092-6110), `capture_own_window_screenshot` (`PrintWindow(PW_RENDERFULLCONTENT)`), `capture_cg_window_rgba` (dropped).
- `display_at` → Windows twin matching `cx.displays()` by bounds (D4).
- Tests at 6553 and 6698 are gated to macOS.
- Shared-code cfg inserts:
  - :391 `iphone || (hold && cfg!(target_os="macos"))`
  - :2273 `#[cfg(windows)] let mods = gpui::Modifiers { platform: mods.control, control: false, ..mods };`
  - 5003-5004 and 5123: `modifiers.secondary()`
  - 5248-5257: "Show in Explorer"
  - 5139: `.pl(16)` on Windows
- `run_app` prologue (cfg windows): single-instance mutex (D18), `timeBeginPeriod(1)`, `crate::live_transcribe::warm()` in place of `crate::hex::watch()`.
- On Windows, every place ui.rs shows a shortcut renders through `crate::win::keys::display_label`: the island hint, the review "Shortcut" button, and the keycaps from `shortcut_parts` (D8).
- HWND source: first confirm `gpui::Window: raw_window_handle::HasWindowHandle` yields `RawWindowHandle::Win32`. If it doesn't, use `GetActiveWindow` inside the open callback, or `FindWindowW` by a unique title, as the HWND source.

### 4.9 `crate::win` shared helpers (fixed APIs)

The foundation implements `mod.rs`, `com.rs`, `spawn.rs` and `keys.rs` fully; they are frozen afterwards. It also implements `process.rs` and `winlist.rs` for real, and P1 owns them afterwards. The rest are stubs or empty modules owned by the listed package.

```rust
// src/win/mod.rs (F)
pub mod com; pub mod spawn; pub mod keys; pub mod process; pub mod winlist; pub mod screens;
pub mod text; pub mod layered; pub mod uia; pub mod netproc; pub mod clipboard;
pub mod conpty; pub mod claude_dictation; pub mod whisper; pub mod ui_host;
pub fn init_process()                 // SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2); ignore "already set"
pub fn launched_from_explorer() -> bool   // parent exe == explorer.exe (Toolhelp snapshot); fallback GetConsoleProcessList(..) == 1 when the parent is gone. Not count==1 alone: Git Bash/mintty children can also see 1
// src/win/com.rs (F)
pub struct ComGuard; impl ComGuard { pub fn mta() -> Self }          // CoInitializeEx(MTA); S_FALSE/RPC_E_CHANGED_MODE tolerated; CoUninitialize on drop only if it initialized
pub struct MfGuard; impl MfGuard { pub fn start() -> anyhow::Result<Self> }  // MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET); MFShutdown on drop
// src/win/spawn.rs (F)
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub fn no_window(cmd: &mut std::process::Command) -> &mut std::process::Command
pub fn shell_open(target: &str) -> std::io::Result<()>              // ShellExecuteW("open") for URLs, folders, ms-settings:
pub fn reveal_in_explorer(path: &std::path::Path) -> std::io::Result<()>  // explorer.exe /select,<path> (ignore exit code 1)
// src/win/keys.rs (F)
pub fn display_label(label: &str) -> String   // "⌥⇧S" -> "Alt+Shift+S"; order Ctrl, Alt, Shift, Win; unknown glyphs pass through
// src/win/process.rs (F real, then P1)
pub fn own_pid() -> u32
pub fn exe_path(pid: u32) -> Option<std::path::PathBuf>   // OpenProcess(QUERY_LIMITED_INFORMATION) + QueryFullProcessImageNameW
pub fn app_id(pid: u32) -> Option<String>                 // D5
pub fn app_name(pid: u32) -> Option<String>               // FileDescription (GetFileVersionInfoW) else exe stem; cached per pid
// src/win/winlist.rs (F real, then P1)
#[derive(Clone, Debug)] pub struct TopWindow { pub hwnd: isize, pub pid: u32, pub class: String, pub title: String, pub bounds: [f64; 4] /* DWMWA_EXTENDED_FRAME_BOUNDS, physical [x,y,w,h], fallback GetWindowRect */, pub style: u32, pub ex_style: u32, pub owner: isize, pub iconic: bool, pub cloaked: bool, pub alpha: f64 }
pub fn enumerate() -> Vec<TopWindow>          // EnumWindows (z-order, front to back), IsWindowVisible only
pub fn is_app_window(w: &TopWindow) -> bool   // !iconic, !cloaked, ((owner==0 && !TOOLWINDOW) || APPWINDOW), class not Progman/WorkerW/Shell_TrayWnd/Shell_SecondaryTrayWnd
pub fn is_taskbar(w: &TopWindow) -> bool
pub fn foreground() -> Option<TopWindow>      // GetForegroundWindow -> GetAncestor(GA_ROOTOWNER)
pub fn id_of(hwnd: isize) -> u32              // D24
// src/win/screens.rs (stub -> P1): pub fn displays() -> Vec<crate::screens::Screen>; pub fn current() -> Vec<crate::screens::Screen>
// src/win/ui_host.rs (stub -> P5), called from main.rs:
pub fn run_settings(pane: Option<String>, icon: Option<std::path::PathBuf>) -> anyhow::Result<()>
pub fn run_onboarding(shortcut: String, paste: String, icon: Option<std::path::PathBuf>) -> anyhow::Result<()>
// src/win/claude_dictation.rs (stub -> P8), called from main.rs:
pub fn run_editor(args: &[std::ffi::OsString]) -> anyhow::Result<()>
pub fn selftest() -> anyhow::Result<()>
// empty modules (doc comment only), API chosen by the owner: text, layered (P3); uia, netproc (P6); clipboard (P7); conpty, whisper (P8)
```

## 5. Coordinates and DPI

- **Global space G** = virtual-screen physical pixels (primary at 0,0; negative left and up). The process is PMv2-aware (D11). Everything below is in G:
  - `Screen.frame` / `visible`
  - `TargetInfo.frame`
  - `telemetry.target_frame`, `pointer_screen`, `picker_rect`
  - `current_mouse_screen`
  - `RawWindow.bounds`, `WindowEntry.bounds`
  - ax probe points
  - `post_synthetic_click`
  - `PillAction::Paste` points
  - `MetalLaserOverlay::screen`
- **Why physical.** Every OS API Scrawl consumes reports physical px under PMv2: `GetCursorPos`, `MSLLHOOKSTRUCT.pt`, `DWMWA_EXTENDED_FRAME_BOUNDS`, `DXGI_OUTPUT_DESC.DesktopCoordinates`, UIA `ElementFromPoint`/`BoundingRectangle`, `SendInput` absolute, `MONITORINFO`. Win32 has no consistent logical global space under mixed DPI: gpui's `logical = physical / scale(of that monitor)`, origin included, yields overlapping or gapped spaces. So conversion happens once, at the GPUI boundary (picker, review, GPUI pill fallback, lab), instead of at ~10 OS boundaries.
- **`TargetFrame.scale = 1.0`** always on Windows. The only consumer is `capture_size` (`native = scale.clamp(1, 2)`), which yields native px capped at the long edge and never upscales. Normalized bundle coordinates (events 0..1, strokes 0..10000) are unchanged, so bundles stay OS-independent.
- **`Screen.scale = GetDpiForMonitor(MDT_EFFECTIVE_DPI)/96`.** It is used only to size UI in points: pill `PANEL_W*scale`, overlay widths in pt × scale, `pill_origin(screen, width*scale, panel_y*scale)`, the 24 pt paste-drag threshold.
- **GPUI boundary (P4 helpers in `src/ui/win.rs`).** For monitor m with physical origin `o` and scale `s`: gpui window-local logical = `(p − o)/s`; gpui global logical = `p/s`. Inverse: `p = o + local*s`. GPUI windows are placed with `SetWindowPos` in G (`move_window_to_screen`), because gpui treats `WindowBounds.origin` as global logical.
- **Point-denominated heuristics** (`MIN_AREA` 32, autocrop thresholds, 24 pt drags) treat 1 px = 1 pt on Windows. That is exact on this PC (both monitors 100%); at 150% they are 1.5x smaller physically. Accepted for v1. **Mixed-DPI behaviour can't be verified on this PC.**
- **`WM_DPICHANGED`:** gpui resizes to the suggested rect when a window crosses monitors, so the picker re-applies `SetWindowPos` after a move (or reopens when the scale differs). Layered windows (pill, overlay) recreate their DIB at the new size.

## 6. Paths

| What | Windows path | Code |
|---|---|---|
| Config | `%USERPROFILE%\.config\scrawl\config.json` (`SCRAWL_CONFIG` override) | `handoff::default_config_path`, unchanged |
| Pill tune | `%USERPROFILE%\.config\scrawl\tune.json` | `ui/motion.rs`, unchanged |
| Captures | `%LOCALAPPDATA%\scrawl\captures` (`SCRAWL_CAPTURES_DIR` override) | `handoff::default_captures_dir` cfg (P7) |
| Inbox | `%LOCALAPPDATA%\scrawl\inbox.json` | follows captures |
| Last mode | `%LOCALAPPDATA%\scrawl\last-mode` | follows captures |
| Log | `%LOCALAPPDATA%\scrawl\logs\scrawl.log` | `win/capture.rs scrawl_log` |
| Exports | `%USERPROFILE%\Videos\Scrawl` | `export::default_output` cfg (P2) |
| CLI shims | `%LOCALAPPDATA%\scrawl\bin\{scrawl.cmd, scrawl}` | `win/command_line.rs` |
| Claude runtime | `%LOCALAPPDATA%\scrawl\claude-dictation\{cwd\, claude-settings.json, capture.txt}` | `win/claude_dictation.rs` |
| whisper | env `SCRAWL_WHISPER_CLI`/`SCRAWL_WHISPER_MODEL` → config `whisperCli`/`whisperModel` → `%LOCALAPPDATA%\scrawl\whisper\` → `~\.t3\projects\t3-voice\vendor\whisper-cpu\Release\whisper-cli.exe` + `~\.t3\projects\t3-voice\models\ggml-large-v3-turbo.bin` | `win/whisper.rs` |
| Login item | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value `Scrawl` = `"<exe>" app` | settings.rs cfg |
| Bundle-dir ACL | none (the 0700 chmod is `cfg(unix)`; `%LOCALAPPDATA%` is per-user) | — |
| Canonical paths | `dunce::canonicalize` everywhere a path reaches text (summary, clipboard, agent prompt); never `\\?\` | handoff/inbox/look |

## 7. Transcription engines (P8)

- **Chain** (`win/transcribe.rs::transcribe`):
  - No wav or no samples → `source:"none"`.
  - `Claude`: `audio.live.take()` → `Live::finish`; on Err or None → whisper.
  - `Whisper`: drop the live session (cancels) → whisper.
  - `Hex`: `try_hex` (copied) → whisper.
  - whisper Err → `source:"offline"`.
  - Import and reprocess have an empty `Slot`, so they always use whisper.
- **Claude Code dictation (live, default).** One warm hidden `claude` in a ConPTY (160x40), ported from T3 Voice `engine.py`:
  - Args: `--settings <json voice.enabled, mode:"hold", autoSubmit:false, language> --permission-mode plan --strict-mcp-config --tools ""`.
  - Env: `CLAUDE*`/`ANTHROPIC*` stripped; `EDITOR`/`VISUAL` = `"<scrawl.exe>" claude-editor`; `SCRAWL_CLAUDE_CAPTURE_FILE` set.
  - The child is in a kill-on-close job object. The screen model is `vt100` 0.16.2.
  - `Live::start` claims the session and spawns a thread that holds (writes `" "` every 30 ms), waits ≤6 s for "listening", and releases when `AudioRecorder::stop` empties the shared buffer (audio.rs:170).
  - `finish` waits for the screen to settle (≤45 s), then Ctrl+G → the editor shim copies the prompt to the capture file.
  - Words are spread over `audio::speech_islands` via `hex::plan_clips` + the new `spread_over_clips`. Result: `source:"claude-code"`, `estimated:true`. Spoken-cue keyframes are off, as for Hex.
  - Empty text with speech present → Err → whisper.
- **whisper.cpp (file).** Runs `whisper-cli -m MODEL -f audio.wav -l <lang|auto> -t max(4,cores-2) -ml 1 -sow -oj -sns -np -of <tmp>\whisper`, with `no_window` and a kill after max(60 s, 3x duration). `-of` is mandatory: without it the JSON lands as `audio.wav.json` in the bundle. JSON `transcription[].offsets{from,to}` (ms) map to one word each; `[..]`/`(..)` entries are dropped. Segments break at `. ! ? ; :` or a gap > 1 s. Result: `source:"whisper.cpp"`, `estimated:false`. Silence (no islands) → empty transcript.
- **Config keys,** read from raw config JSON by P8 (no handoff.rs edit): `transcriber` (`claude|whisper|hex`), `language` (default: two-letter `GetUserDefaultLocaleName`, else `en`; whisper default `auto`), `whisperCli`, `whisperModel`, `claudeCommand`. A language change → `live_transcribe::restart()`.
- **Hidden CLI** (foundation adds the dispatch): `scrawl claude-editor <file>` and `scrawl dictation-selftest`.

## 8. Parity table

| Feature | macOS | Windows | Status |
|---|---|---|---|
| Record / paste shortcuts (⌥⇧S, ⌥⇧V + aliases) | Carbon hotkeys | RegisterHotKey Alt+Shift+S/V + LL hook, Alt mask key | same |
| Tap/hold switcher, double press, Esc cancel | CGEventSource polling + CGEventTap | GetAsyncKeyState 120 Hz poller + WH_KEYBOARD_LL | same |
| Undo stroke | ⌘Z | Ctrl+Z | replaced |
| Display recording to H.264 MP4 | SCStream + SCRecordingOutput | DXGI Desktop Duplication + MF SinkWriter, cursor composited | replaced |
| Area mode | SCK sourceRect | DDA crop | same |
| Window mode | SCK window filter (follows, ignores occlusion) | fixed crop of the start rect | degraded |
| Multi-display, display under cursor | yes | yes | same |
| Candidate snapshots (220 ms, 384 MB budget) | SCScreenshotManager | downscaled DDA frames | same |
| Hide Scrawl windows from the recording | SCContentFilter | WDA_EXCLUDEFROMCAPTURE (spike S2) | replaced |
| Click / scroll / key events | NSEvent monitor + CGEventTap | WH_MOUSE_LL / WH_KEYBOARD_LL | same |
| ⇧ ink, laser, pins, start glow, picker dim and cutout | Metal overlay panel | layered window + CPU raster | replaced |
| Capsule pill (motion, chips, clock) | Metal NSPanel | layered window + tiny-skia, same model.rs | replaced |
| Pill Glass style | NSGlassEffectView | renders Black | degraded |
| Target picker (area/window/screen) | GPUI + NSWindow tweaks | GPUI + HWND styles | same |
| Review window | GPUI | GPUI, native title bar | same |
| Menu bar item + inbox rows | NSStatusItem | tray icon (Shell_NotifyIcon) | replaced |
| Settings / Onboarding windows | SwiftUI helper | GPUI UI-host process, same JSON protocol | replaced |
| Onboarding demo animation | 11 s animated | 4 captioned stills | degraded |
| Permission flows (Screen Recording, Accessibility, Input Monitoring) | TCC + PermissionPanel | not needed (always granted) | dropped |
| Microphone permission | TCC | privacy-settings check + ms-settings link | replaced |
| Live transcription | SpeechAnalyzer stream | Claude Code dictation; word times estimated over speech islands | replaced |
| File transcription (import / reprocess / fallback) | SpeechAnalyzer batch | whisper.cpp | replaced |
| Hex transcription | local Hex API | not available (macOS app) | dropped |
| `reprocess --live-speed` | yes | bails | dropped |
| Keyframes, contact sheet, summary.md, scrawl.json | Rust | same code | same |
| Badge / pin text in images | CoreText Helvetica Neue | DirectWrite Segoe UI | replaced |
| Clipboard copy | NSPasteboard | CF_UNICODETEXT + PNG + CF_DIBV5 | replaced |
| ⌥⇧V two-step paste | ⌘V synthetic | SendInput Ctrl+V; Shift+Insert in terminals | replaced |
| Paste chip dropped on a window | CGEvent click | SendInput click | same |
| agentContext element labels | AX API | UI Automation (no AXDocument) | replaced |
| Window log / autocrop | CGWindowList | EnumWindows + DWM | replaced |
| Frontmost app / bundle id | NSWorkspace bundle id | exe stem (D5) | replaced |
| Dev-site routing: tab URL | osascript | UIA address bar (scheme heuristic) | replaced |
| Dev-server port → project dir | lsof | GetExtendedTcpTable + PEB cwd | replaced |
| Ghostty terminal clue | osascript | none | dropped |
| sdk.js / ingest HTTP server | 0.0.0.0:47777 | 127.0.0.1:47777 (LAN opt-in) | degraded |
| OpenCode delivery + retry queue | opencode2 subprocess | opencode2 subprocess (PATHEXT, argv guard; unverified, not installed here) | same |
| agentHost ssh sync | ssh + tar | bails | dropped |
| ScrawlKit import | AVFoundation | Media Foundation (HEVC needs the Store extension) | replaced |
| Cabled iPhone mirror, USB ScrawlKit poll | CoreMediaIO, devicectl | — | dropped |
| Video export with ink | Swift AVFoundation renderer | MF + tiny-skia renderer | replaced |
| HEVC export, retention shrink | yes | H.264 only, no shrink | degraded |
| Retention trash | /usr/bin/trash | Recycle Bin | replaced |
| `scrawl frames` / `zoom` | ffmpeg or helper | ffmpeg.exe or MF | same |
| Updates | Sparkle | none | dropped |
| Launch at login | SMAppService | HKCU Run key | replaced |
| CLI on PATH | symlink | .cmd + sh shims + user PATH | replaced |
| `scrawl link` QR | CoreImage | omitted | dropped |
| Mic picker / device watch | CoreAudio | WASAPI via cpal, 2 s poll | same |
| Reduce motion | NSWorkspace | SPI_GETCLIENTAREAANIMATION | same |
| Spaces / full-screen auxiliary | collection behavior | overlays don't follow virtual desktops | degraded |
| Pill lab, tune, traces | yes | yes | same |
| `scrawl sck-probe` | SCK probe | DDA probe | replaced |

## 9. Test strategy

1. **Unit (`cargo test`).** Must pass on Windows for every pure suite: model, keyframes, sketch, zoom, autocrop, autocrop_lab, render, main, retention, agent_context, audio, delivery, inbox, page, web, export/*, hold, mic, hex, routing/*, phone, handoff, icons, ui/motion, ui (pure), native/pill/model, screens, settings (pure), onboarding.
   - Pure tests from swapped files are copied into the Windows file. From capture.rs: `screen_stop_joins_worker_before_collecting_its_final_media`, `clicks_are_recorded_from_polled_buttons_while_the_mouse_monitor_is_missing`, `the_capture_shortcut_matches_its_aliases_and_a_custom_one`, `recordings_use_the_display_under_the_cursor`, `input_on_an_external_display_normalizes_into_it`, the candidate_buffer tests, `candidates_are_small_and_recordings_keep_full_size`, `cached_window_hit_preserves_front_to_back_order_and_screen_coordinates`. The keycode-naming tests are rewritten with VKs.
   - New pure Windows tests:
     - P1: Carbon→MOD_*, wheel sign, VK chord names.
     - P2: Every/At selection on synthetic pts lists, PLAN deserialization, ink formulas.
     - P3: CPU `shade` vs `renders_a_full_segment_buffer` / `tiled_frame_matches_an_untiled_one`, atlas clock widths equal for 0:00/1:11/4:59.
     - P5: shims in a temp dir, PATH editing injectable.
     - P6: browser_kind stems, drive-root check.
     - P7: `file_uri` for `C:\`, `canon`.
     - P8: whisper JSON parse, `spread_over_clips`, env filter, cmdline quoting, screen scrapers on canned bytes, editor round trip.
2. **Platform tests** are `#[ignore]`, run as `cargo test -- --ignored <name>`:
   - LL hook installs.
   - DDA one-frame probe.
   - MF round trip: encode synthetic frames → `frames(At)` → compare.
   - WDA spike S2.
   - ConPTY echo.
   - Clipboard set/get.
   - No test installs hooks or plays audio unless ignored.
3. **CLI end-to-end** (each package's proof command, §12):
   - `scrawl sck-probe`, `scrawl windows`
   - `scrawl record --duration 5 --out %TEMP%\x.scrawl`
   - `scrawl reprocess latest --out %TEMP%\rp --transcriber whisper`
   - `scrawl export latest`, `scrawl zoom latest 0:02`
   - `scrawl lab pill`, `scrawl preview --capture-screenshot %TEMP%\r.png`
   - `scrawl dictation-selftest`
4. **Bundle inspection** (AGENTS.md principle 5, Windows paths): after any capture change, read the newest bundle in `%LOCALAPPDATA%\scrawl\captures\`: `summary.md`, `scrawl.json`, `contact-sheet.png`, `frames\*.png`.
5. **Safety:**
   - Never post to a real OpenCode session; use `IngestHooks` and `routing::Hooks`.
   - Never play audio aloud; feed WAVs to whisper.
   - Never touch `~/.t3/userdata`.
   - Tests must not register global hotkeys (they would steal Alt+Shift+S from the running app).

### 9.1 Baseline failures

The foundation fills this in after its first green `cargo test --no-run`: it runs `cargo test` and lists the tests that compile but fail on Windows, with their owning package. Each package must leave its owned files' tests green or `cfg(target_os = "macos")`-gated.

_(empty until the foundation commit)_

## 10. Build and run (Windows)

Git Bash (the Bash tool):

```sh
export CARGO_TARGET_DIR=C:/t/scrawl           # shared by all worktrees
cargo fetch                                   # once, needs network (vt100, cpal/hound/dirs-7/windows-0.62 sources are missing locally)
cargo check -j 3 --offline
cargo build -j 3 --offline
cargo test  -j 3 --offline -- --test-threads=2 [filters…]
C:/t/scrawl/debug/scrawl.exe app              # tray app (single instance)
C:/t/scrawl/debug/scrawl.exe record --duration 5 --out "$TEMP/x.scrawl"
```

PowerShell: `$env:CARGO_TARGET_DIR='C:\t\scrawl'; cargo build -j 3 --offline`.

Notes:
- The AGENTS.md rules about `taskpolicy`, `install-app.sh` and `~/Library` are macOS-only. On Windows, run the binary directly.
- Parallel worktrees serialize on cargo's build-directory lock in the shared target dir. Dependencies are shared; each worktree rebuilds only the `scrawl` crate.
- Release builds make gpui compile HLSL with `fxc.exe` (Windows SDK, or set `GPUI_FXC_PATH`). Use debug builds until that is checked.
- Kill a stray app with `taskkill /IM scrawl.exe /F`.

## 11. Foundation commit (before any package)

Step by step:
1. **Fetch.** `cargo fetch` (network, once).
2. **Cargo.toml.** Add `[target.'cfg(windows)'.dependencies]`:
   - `gpui = { version = "0.2.2", default-features = false }`
   - `win32 = { package = "windows", version = "0.61.3", features = [UNION, §11.1] }`
   - `windows-core = "0.61.2"`
   - `windows-registry = "0.5.3"`
   - `raw-window-handle = "0.6.2"`
   - `tiny-skia = "0.11.4"`
   - `ttf-parser = "0.25.1"`
   - `dunce = "1.0.5"`
   - `getrandom = "0.3.4"`
   - `vt100 = "0.16.2"`

   The mac section stays untouched. Cargo.lock updates only for vt100 and vte.
3. **build.rs.** Unchanged: it returns early off macOS, and there is no Windows build step in v1.
4. **main.rs.**
   - `mod` lines become `#[cfg_attr(windows, path = "win/<m>.rs")]` for capture, command_line, helper, laser_overlay, live_transcribe, sparkle, transcribe.
   - Add `#[cfg(windows)] mod win;`.
   - The first statement of `main()` is `#[cfg(windows)] crate::win::init_process();`.
   - The `None` arm on Windows: `launched_from_explorer()` → `FreeConsole()` + App.
   - `Record{iphone:true}`: `#[cfg(windows)] bail!` before `create_dir_all`.
   - `Devices`: Windows notice.
   - `:2063` `open` → `win::spawn::shell_open`.
   - Hidden `#[cfg(windows)]` clap variants: `Settings{--pane, --icon}`, `Onboarding{--shortcut, --paste, --icon}`, `ClaudeEditor{trailing args}` (name `claude-editor`), `DictationSelftest` (name `dictation-selftest`). They dispatch to the `win::ui_host` / `win::claude_dictation` stubs.
   - `parse_engine` error text and the `hex_status` "Apple Speech" strings get cfg(windows) wording.
5. **src/native/mod.rs.** `#[cfg_attr(windows, path = "pill_win/mod.rs")] pub mod pill;`.
6. **Stubs with the full §4 API, no panics:**
   - `src/win/capture.rs`: verbatim portable sections (§4.1) + platform stubs. `start` returns a session with no thread; `stop` returns an empty recording; `discover_target*` returns a Display target of `GetSystemMetrics` size; the lists are empty; `post_*` do nothing; `scrawl_log` is real.
   - `src/win/helper.rs`: `path` = current_exe, `devices` = `Ok(vec![])`, `permissions` = all Granted, the others `bail!`.
   - `src/win/laser_overlay.rs`: `open` → `Err("not yet on Windows")`.
   - `src/native/pill_win/{mod.rs, atlas.rs}`: verbatim `PillStyle`/`PillAction`/`PillSnapshot`; `NativePill::open` → Err; atlas consts + `measure_metrics` → `Metrics::default()`.
   - `src/win/transcribe.rs`: full enum/`MenuState`/`menu_state`; `transcribe` returns `source:"offline"`.
   - `src/win/live_transcribe.rs`: `wanted` = false, `start`/`simulate` = Err, `Slot` verbatim, `warm`/`restart`/`shutdown` no-ops.
   - `src/win/command_line.rs`: `Places::system` with `target: None`, `state` = `Unavailable`, `install`/`uninstall` = Err.
   - `src/win/sparkle.rs`: final no-op.
   - `src/win/mod.rs`, `com.rs`, `spawn.rs`, `keys.rs`, `process.rs`, `winlist.rs`: real implementations.
   - `screens.rs`, `ui_host.rs`, `claude_dictation.rs`: stubs (empty vec / Err).
   - `text`, `layered`, `uia`, `netproc`, `clipboard`, `conpty`, `whisper`: empty modules.
7. **Compile gating in shared files** (mac items gated; Windows stub twins added):
   - `ax.rs`: `hit_test` → None.
   - `windows.rs`: `raw_windows` → vec![], `app_meta_of` → `AppMeta{bundle_id: None, regular: true}`, `dock_rect` → None, `snapshot` duplicated without the pool.
   - `screens.rs`: re-export from `win::screens`.
   - `retention.rs`: `changed_ago` via `modified()`, the libc QoS call gated.
   - `settings.rs`: the login-item block gated; Windows stubs return `LoginItem::Unavailable` / Err / no-op.
   - `ui.rs` + new `src/ui/win.rs`: every item in §4.8's ui list, with no-op/None/Err twins of the same names.
     - `type Hwnd = win32::Win32::Foundation::HWND`.
     - `open_review_window` must at least open the GPUI review window (copy of the mac body minus NSWindow code, native title bar), so a capture is viewable.
     - The objc2 tests at 6553/6698 are gated.
   - `ui/lab.rs`: the backdrop block gated.
8. **Verify:**
   - `cargo check -j 3 --offline` and `cargo test -j 3 --offline --no-run` are green on Windows.
   - `scrawl --help`, `scrawl demo --out %TEMP%\d.scrawl` and `scrawl windows` run without panic.
   - Run `cargo test`, write §9.1, commit, then branch the packages.

### 11.1 `win32` features (union of all maps; names verified in windows-0.61.3)

`Win32_Foundation, Win32_Security, Win32_Security_Cryptography, Win32_Graphics_Direct3D, Win32_Graphics_Direct3D11, Win32_Graphics_Dxgi, Win32_Graphics_Dxgi_Common, Win32_Graphics_Gdi, Win32_Graphics_Dwm, Win32_Graphics_DirectWrite, Win32_Graphics_Imaging, Win32_UI_WindowsAndMessaging, Win32_UI_Input_KeyboardAndMouse, Win32_UI_HiDpi, Win32_UI_Shell, Win32_UI_Shell_PropertiesSystem, Win32_UI_Accessibility, Win32_UI_Controls, Win32_System_Threading, Win32_System_LibraryLoader, Win32_System_Com, Win32_System_Com_StructuredStorage, Win32_System_Variant, Win32_System_Ole, Win32_System_Memory, Win32_System_DataExchange, Win32_System_Console, Win32_System_Pipes, Win32_System_JobObjects, Win32_System_IO, Win32_System_Registry, Win32_System_SystemInformation, Win32_System_Diagnostics_Debug, Win32_System_Diagnostics_ToolHelp, Win32_System_ProcessStatus, Win32_System_Kernel, Win32_System_WindowsProgramming, Win32_System_Environment, Win32_System_Power, Win32_Media, Win32_Media_MediaFoundation, Win32_Media_Audio, Win32_Media_KernelStreaming, Win32_Storage_FileSystem, Win32_Storage_Xps, Win32_Storage_Packaging_Appx, Win32_NetworkManagement_IpHelper, Win32_NetworkManagement_Ndis, Win32_Networking_WinSock, Win32_Globalization, Wdk, Wdk_System, Wdk_System_Threading`


Added for WGC window capture (D22): `Foundation, Graphics, Graphics_Capture, Graphics_DirectX, Graphics_DirectX_Direct3D11, Win32_System_WinRT, Win32_System_WinRT_Direct3D11, Win32_System_WinRT_Graphics_Capture`. Also add `[[bin]] name = "scrawl-launch"` (D10).

## 12. Work packages

All packages depend only on the foundation. Each runs in its own worktree from the foundation commit and edits **only** its owned files; a needed contract change is reported, not made. Soft dependencies (needed for end-to-end checks only) are named in each brief. Answer format for every package: a commit on its branch, then a final message with: files changed; the proof command and its output summary; any deviation from §4; open issues; test baseline delta.

| Id | Title | Owned files |
|---|---|---|
| P1 | Capture, input, displays | `src/win/capture.rs`, `src/win/capture/**`, `src/win/screens.rs`, `src/win/process.rs`, `src/win/winlist.rs`, `src/screens.rs`, `src/hold.rs` |
| P2 | Media Foundation decode, export, retention | `src/win/helper.rs`, `src/win/helper/**`, `src/export/mod.rs`, `src/retention.rs`, `src/autocrop_lab.rs` |
| P3 | Overlays (pill, laser/ink), text | `src/native/pill_win/**`, `src/native/pill/model.rs` (cfg-inline only), `src/win/laser_overlay.rs`, `src/win/laser_overlay/**`, `src/win/text.rs`, `src/win/text/**`, `src/win/layered.rs`, `src/win/layered/**`, `src/render.rs` |
| P4 | GPUI app shell + tray | `src/ui.rs`, `src/ui/win.rs`, `src/ui/win/**`, `src/ui/lab.rs`, `src/ui/motion.rs`, `src/icons.rs` |
| P5 | Settings, onboarding, CLI install, login item | `src/settings.rs`, `src/onboarding.rs`, `src/win/ui_host.rs`, `src/win/ui_host/**`, `src/win/command_line.rs`, `src/win/sparkle.rs`, `scripts/install-windows.ps1` (new) |
| P6 | Context and routing | `src/ax.rs`, `src/windows.rs`, `src/win/uia.rs`, `src/win/uia/**`, `src/win/netproc.rs`, `src/win/netproc/**`, `src/routing.rs`, `src/routing/look.rs`, `src/routing/smart.rs`, `src/web.rs`, `src/autocrop.rs` |
| P7 | Handoff, clipboard, paste, delivery, ingest | `src/handoff.rs`, `src/win/clipboard.rs`, `src/win/clipboard/**`, `src/inbox.rs`, `src/phone.rs`, `src/delivery.rs` |
| P8 | Transcription (Claude dictation, whisper.cpp), mic | `src/win/transcribe.rs`, `src/win/live_transcribe.rs`, `src/win/claude_dictation.rs`, `src/win/claude_dictation/**`, `src/win/conpty.rs`, `src/win/whisper.rs`, `src/mic.rs`, `src/hex.rs`, `src/audio.rs` |

Shell for the proof commands: Git Bash, `export CARGO_TARGET_DIR=C:/t/scrawl`, `S=C:/t/scrawl/debug/scrawl.exe`.

**P1: Capture, input, displays.**
- **Goal:** native recording, hotkeys, input telemetry, target discovery and display enumeration (§4.1, §4.2). Every capture flows through `crate::capture`, which is a stub after the foundation.
- **Non-goals:**
  - overlay/pill drawing (P3)
  - GPUI picker/review windows (P4)
  - MF decode/export (P2)
  - clipboard (P7)
  - UIA (P6)
- **End state:**
  - **Recording:** DDA + MF SinkWriter (cursor composited, crop per mode, exact `video_offset_ms`, error/fallback chain).
  - **Input:** the `scrawl-input` thread with RegisterHotKey + LL hooks + mask key; the HID poller.
  - **Targets and frontmost app:** `discover_target*`, `list_selectable_windows`, `region_target`, frontmost-app tracking.
  - **Injection and probe:** the SendInput helpers (incl. `SCRAWL_INJECT_MAGIC`, `post_alt_mask_key`, `post_synthetic_shift_insert`); `screen_capture_probe` as a DDA probe.
  - **Displays:** `win/screens.rs` (DISPLAYn ids, rcWork, DPI scale).
  - **Shared helpers:** `process.rs`/`winlist.rs` refined (ApplicationFrameHost → the hosted CoreWindow pid).
  - **Misc:** hold.rs skips iPhone; pure tests copied and green; Windows tests added.
- **Ruled out:**
  - WGC (yellow border on Win10)
  - hooks on the gpui thread
  - window-follow in Window mode (D22)
  - the GPU VideoProcessor path, unless the CPU path exceeds ~25% CPU at 2560x1440@30
- **Proof:**
  - `cargo test -j 3 --offline -- --test-threads=2 capture screens hold`
  - `cargo test -j 3 --offline -- --ignored ll_hook`
  - `$S sck-probe`
  - `$S record --duration 5 --out "$TEMP/p1.scrawl"`: recording.mp4 is non-empty, the frames show the desktop with the cursor, clicks made during it appear in summary.md
  - `$S record --window Notepad --duration 3 --out "$TEMP/p1w.scrawl"`
- **Soft deps:** P2 (full-size keyframes from the mp4), P6 (window log), P3/P4 (live ink and the picker in the app).

**P2: Media Foundation decode, export, retention.**
- **Goal:** replace the Swift helper's AVFoundation work with in-process Media Foundation: `probe`, `frames`, `export_audio`, `export` (§4.3). Also the Windows bits of retention, export and autocrop_lab. Finalize (full-size keyframes), zoom, frames, import and video export all go through `crate::helper`.
- **Non-goals:**
  - the recording encoder (P1)
  - transcription (`helper::transcribe` stays a bail)
  - HEVC encode
  - shrink (D21)
  - phone mirror
- **End state:**
  - `frames` follows the §4.3 contract exactly, with pure tests on synthetic pts lists.
  - An ignored MF round-trip test encodes a synthetic MP4 with the SinkWriter (no binary fixture), then checks `probe` and `frames(At)`.
  - The export renderer ports helper/Export.swift (read it), with unit tests for the fade/pin/ripple/laser math.
  - `export/mod.rs`: `run_helper` cfg twin, `default_output` → Videos\Scrawl, the mac test gated.
  - `retention.rs`: Recycle Bin trash, `changed_ago`, background priority, `shrink_video` → `Ok(None)`.
  - `autocrop_lab.rs`: `shell_open`.
- **Ruled out:** depending on ffmpeg, spawning any scrawl-helper, HEVC encode.
- **Proof:**
  - `cargo test -j 3 --offline -- --test-threads=2 helper export retention autocrop_lab`
  - `cargo test -j 3 --offline -- --ignored mf_round_trip`
  - on a P1 bundle (or `$S import <synthetic.mp4>`): `$S zoom latest --at 0:02`, `$S frames latest --from 0:00 --to 0:02`, `$S export latest`, `$S clean --dry-run`
  - read the zoom PNG and probe the exported mp4
- **Soft deps:** P1 (real recordings).

**P3: Overlays (pill, laser/ink) and text.**
- **Goal:** the layered-window pill (§4.5), the laser/ink/picker overlay (§4.4), the shared text rasterizer `win/text.rs` (D14), and `render.rs` Windows `native_text`. A capture without visible ink and pill is unusable, and bundle badge text should match the Mac.
- **Non-goals:** GPUI picker/review (P4), DDA (P1), the Glass style, a Metal/HLSL port (unless S2 forces DirectComposition).
- **End state:**
  - Spike S2 done first; its result goes in the commit message.
  - `pill_win/{mod.rs, atlas.rs}`: same API, `model.rs` unchanged except the :614 cfg.
  - Overlay: own thread, dirty-rect CPU raster, UpdateLayeredWindowIndirect, WDA applied.
  - `win/text.rs`: DirectWrite with a ttf-parser fallback, API chosen by P3, used by the atlas and render.rs (mask contract in §4.8).
  - CPU `shade` tests replacing the Metal ones; the atlas invariants tested; no panics.
- **Ruled out:**
  - Metal-like GPU port by default
  - GPUI pill as the default
  - acrylic/SetWindowCompositionAttribute
  - full-screen redraw every frame
- **Proof:**
  - `cargo test -j 3 --offline -- --test-threads=2 laser_overlay pill render text`
  - `cargo test -j 3 --offline -- --ignored wda_hides_layered_window`
  - `$S lab pill tap-then-area`
  - `$S demo --out "$TEMP/d.scrawl"`, then read `contact-sheet.png`: the badge digits are anti-aliased text, not 5x7 blocks
- **Soft deps:** P1 (telemetry fed by real input), P4 (the picker on top).

**P4: GPUI app shell and tray.**
- **Goal:** make `scrawl app` usable: the ui.rs twins in `src/ui/win.rs` (§4.8 ui list), the tray, picker/review/pill-fallback window plumbing in G (§5), key semantics (D9), the `run_app` prologue (D18, D23, `live_transcribe::warm`), the own-window screenshot. The tray app is how the user uses Scrawl.
- **Non-goals:** settings/onboarding views (P5), overlay drawing (P3), capture internals (P1).
- **End state:**
  - Tray: icon recolored for the taskbar theme, menu built fresh on open, inbox rows, export toggle, no "Check for Updates…", `TaskbarCreated` re-add, tooltip text/count.
  - Picker: an HWND placed on the right monitor, transparent and clickable (S3).
  - Review: native title bar, Show in Explorer, Ctrl+W/Ctrl+Q/Ctrl+,.
  - `display_at` twin by bounds; GPUI-boundary helpers; `PrintWindow` screenshot.
  - lab backdrop; the objc tests gated; ui tests green.
- **Ruled out:**
  - other UI toolkits
  - focus-stealing hacks beyond SetForegroundWindow after WM_HOTKEY/tray/AllowSetForegroundWindow
  - level offsets (raise order instead)
- **Proof:**
  - `cargo test -j 3 --offline -- --test-threads=2 ui::`
  - `$S preview --capture-screenshot "$TEMP/review.png"`, then read the PNG
  - Manual `$S app` checklist:
    - the tray icon shows; the menu items work
    - Alt+Shift+S opens the picker on the display under the cursor
    - Esc cancels
    - Record Screen makes a bundle and the review window opens
    - Ctrl+W closes it
    - a second `$S app` exits
- **Soft deps:** P1, P3.

**P5: Settings, onboarding, CLI install, login item.**
- **Goal:** settings and onboarding as the GPUI UI host (D12) speaking the existing JSON protocol. Swift UIs don't exist on Windows, and both `Session`s must stay unchanged.
- **Non-goals:** the tray (P4), permissions beyond the microphone, auto-update, the animated demo.
- **End state:**
  - `scrawl settings [--pane P] [--icon F]` and `scrawl onboarding --shortcut S --paste P [--icon F]` run a GPUI window.
  - Settings protocol:
    - stdin: `{type:"state", …View camelCase}`, `{type:"show", pane}`, `{type:"request"\|"open-settings", permission}`, `{type:"close"}`
    - stdout: `Action` JSON lines; `close` on window close
  - Onboarding protocol:
    - stdin: `{type:"state", step, index, count, rows[], ready, fix, tutorial}`, request/open-settings, hide/show/close
    - stdout: `{action: continue\|back\|later\|close\|settings\|skip\|watched\|practiced}` or `{action:"grant", permission}`
    - Exact shapes come from settings.rs View/Action serde and helper/Settings.swift + Onboarding.swift (read them).
  - The host logs via `scrawl_log`; panes per the settings map, with Screen Recording/Accessibility/Camera rows hidden; Microphone opens `ms-settings:privacy-microphone`; HowItWorks = 4 stills.
  - `settings.rs` and `onboarding.rs` cfg twins (§4.8); `CREATE_NO_WINDOW` at both spawns.
  - `win/command_line.rs` (§4.7) with temp-dir tests and injectable PATH editing.
  - `scripts/install-windows.ps1`: copies the exe to `%LOCALAPPDATA%\Programs\Scrawl\scrawl.exe`, sets the Run key to `"<exe>" app`, starts it with `app`.
  - `win/keys.rs` stays frozen (report bugs).
- **Ruled out:** in-process settings windows, MSIX StartupTask, a Windows updater.
- **Proof:**
  - `cargo test -j 3 --offline -- --test-threads=2 settings onboarding command_line`
  - `echo '{"type":"close"}' | $S settings --pane general` exits 0
  - manual: tray Settings… opens; a toggle writes config.json; Agents → Install CLI, then a new Git Bash runs `scrawl --version`
- **Soft deps:** P4 (tray entry).

**P6: Context and routing.**
- **Goal:** OS context facts on Windows: UIA labels (agentContext), the window log for autocrop (D15), dev-site routing (UIA browser URL, port → project dir), look/smart/web tweaks (§4.8). Principle 2: record what the OS knows instead of making the agent guess.
- **Non-goals:** winlist/process internals (P1 owns them; consume their API), clipboard/delivery (P7), a terminal-pane clue.
- **End state:**
  - `ax::hit_test` via `win::uia` (COM per thread, 100/250 ms timeouts, own-pid fallback, never reads password/edit values).
  - `windows.rs` twins: Dock layer 20, menus 101.
  - routing twins + the cfg(windows) `Firefox` variant; `win/netproc.rs` (GetExtendedTcpTable + PEB cwd, x64).
  - look/smart/web edits; all tests green; `autocrop.rs` untouched unless D15 proves insufficient.
- **Ruled out:** MSAA/IAccessible, Chrome DevTools protocol, netstat/WMI parsing.
- **Proof:**
  - `cargo test -j 3 --offline -- --test-threads=2 ax windows routing web`
  - `$S windows`: the taskbar shows as Dock layer 20
  - `$S route --browser chrome` with Chrome on a localhost page prints its URL
  - `$S route --url http://localhost:<port>` with a local dev server running
  - with P1: `SCRAWL_CONFIG=$TEMP/cfg.json` (`{"experiments":{"agentContext":true}}`) `$S record --duration 5 --out "$TEMP/p6.scrawl"`, clicking on buttons, shows labels in summary.md
- **Soft deps:** P1.

**P7: Handoff, clipboard, paste, delivery, ingest.**
- **Goal:** the end of every capture on Windows: clipboard formats, the two-step ⌥⇧V paste, paste-at-target, captures dir, canonical paths, the ingest token RNG, opencode2 invocation, inbox, phone tweaks (§4.8).
- **Non-goals:** the SendInput primitives (P1 provides `post_*`), routing decisions (P6), agentHost.
- **End state:**
  - `win/clipboard.rs`: text, PNG + CF_DIBV5, optional CF_HDROP; one Open/Close with a ~100 ms retry.
  - Paste sequence: `post_alt_mask_key` → `wait_for_modifiers_released` → terminal ? (path text + `post_synthetic_shift_insert`) : (PNG + `post_synthetic_cmd_v`) → 160 ms → summary text → paste again.
  - `canon()` everywhere; `getrandom` token; PATHEXT; `file_uri`; argv guard; agentHost bail; ingest bind (D16); usb poller no-op; mirror bail.
  - Tests green.
- **Ruled out:** OLE data objects/delayed rendering; posting to a real OpenCode session in tests.
- **Proof:**
  - `cargo test -j 3 --offline -- --test-threads=2 handoff inbox phone delivery`
  - `cargo test -j 3 --offline -- --ignored clipboard_round_trip`
  - `$S demo --out "$TEMP/d7.scrawl" --clipboard`, then `powershell Get-Clipboard` shows the summary
  - `$S serve --port 47778 --no-opencode &` then `curl -s http://127.0.0.1:47778/sdk.js | head -c 200`
  - manual with P1+P4: Alt+Shift+V into Notepad, Windows Terminal (Git Bash) and a Chrome textarea
- **Soft deps:** P1, P4.

**P8: Transcription and mic.**
- **Goal:** Windows transcription behind the existing API (§4.6, §7): Claude Code dictation (live, default) and whisper.cpp (file, fallback); plus the polling `mic::Watch`. SpeechAnalyzer doesn't exist on Windows.
- **Non-goals:** Hex (stays compiled, "not connected"), live partial text, `--live-speed`, bundling whisper or models.
- **End state:**
  - `win/{transcribe, live_transcribe, conpty, claude_dictation, whisper}.rs`: `run_editor`, `selftest`, `spread_over_clips`.
  - Fallbacks: Claude → whisper → offline.
  - `Drop` never blocks; no panics.
  - `mic.rs` polling Watch.
  - The tests listed in §9.
- **Ruled out:**
  - Windows.Media.SpeechRecognition, cloud STT APIs, Python/pywinpty
  - feeding cpal audio into Claude
  - a warm whisper-server (later option)
- **Proof:**
  - `cargo test -j 3 --offline -- --test-threads=2 transcribe live_transcribe whisper claude_dictation conpty hex audio mic`
  - `$S dictation-selftest`
  - `$S reprocess <bundle with audio.wav> --out "$TEMP/rp" --transcriber whisper`, then read `$TEMP/rp/summary.md`. For a test WAV without speakers, render one with System.Speech `SetOutputToWaveFile`; never play it aloud.
- **Soft deps:** P1 (live captures).

## 13. Day-0 spikes (first thing in the owning package)

- **S1 (F/P4).** gpui 0.2.2 builds and opens a window on this PC: the foundation runs `cargo build`; P4 runs `scrawl preview`.
- **S2 (P3).** Does `WDA_EXCLUDEFROMCAPTURE` on an `UpdateLayeredWindow` window hide it from DDA? Test with an ignored test that draws a red layered window, grabs one DDA frame and checks the pixel. If not: switch the overlays to a D3D11/DirectComposition swap chain (`WS_EX_NOREDIRECTIONBITMAP`); otherwise ink burns into recording.mp4.
- **S3 (P4).** A full-display gpui `WindowBackgroundAppearance::Transparent` PopUp is actually transparent on 19045 and receives clicks everywhere (the picker relies on it).
- **S4 (P1/P4).** `SetForegroundWindow` succeeds after `WM_HOTKEY` and after a tray click; gpui `show` on a PopUp doesn't activate it.
- **S5 (P8).** Claude Code waits for `scrawl.exe claude-editor` under Ctrl+G, and the TUI strings ("listening", "keep holding", "Voice: processing", trust prompt, ❯) match the installed version.
- **S6 (P5).** The ConsentStore microphone keys and values on this PC; what WASAPI returns when access is denied.

## 14. Risks

1. **gpui 0.2.2 on Windows is unverified** (S1). If it fails, P4 and P5 block. Release builds need `fxc.exe`.
2. **WDA_EXCLUDEFROMCAPTURE** is unverified on `UpdateLayeredWindow` windows (S2). If it fails, ink and pill burn into recording.mp4 and keyframes; the fallback is a DirectComposition swap chain.
3. **DDA CPU cost.** The CPU path (map, crop, scale, convert) at 2560x1440@30 may be heavy and need the GPU VideoProcessor → NV12 path. DDA delivers frames only on change, so timestamps need careful pacing.
4. **LL hooks.** Windows silently removes them after callback timeouts. Callbacks must stay trivial, on the dedicated input thread.
5. **Foreground lock.** `SetForegroundWindow` can be refused, which affects the picker focus, paste handback and `ensure_external_app_active`.
6. **UIPI.** An unelevated Scrawl can't inject into, or always see input of, elevated windows.
7. **Alt+Shift hotkeys.** A lone Alt release can open the foreground app's menu bar; mitigated by the 0xE8 mask key.
8. **Claude dictation:**
   - Screen scraping of the TUI breaks on Claude Code updates.
   - Claude records the default mic, not the chosen one.
   - Speech before "listening" is missing from the text.
   - It needs a login, voice mode and the network.
   - ~330 MB resident.
   - `estimated:true` disables spoken-cue keyframes.
9. **whisper large-v3-turbo on CPU** may be slow on long recordings. Non-DTW word times are loose.
10. **Network.** vt100/vte and the Windows sources of cpal/hound/dirs-7/windows-0.62 are not in the local registry, so a one-time networked `cargo fetch` is needed.
11. **Shared target dir.** With `CARGO_TARGET_DIR` shared, the 8 parallel worktrees serialize on cargo's build lock.
12. **Mixed DPI** can't be verified on this PC (both monitors at 100%). The point-based heuristics shrink physically at >100%.
13. **HEVC.** iPhone/ScrawlKit HEVC imports need the Store "HEVC Video Extensions".
14. **opencode2.** As an npm `.cmd` shim, Rust refuses newline-bearing `--data` args (BatBadBut) and the argv limit is 8191. opencode2 isn't on PATH here.
15. **Path locks.** Renames over open files (export partial→out, retention) fail with sharing violations while a player or the review window holds them.
16. **Chromium UIA cost.** Probing switches Chromium/Electron accessibility on (CPU/RAM) while agentContext is on.
17. **Privacy incident during mapping:** one mapping subagent sent the user's email address in a User-Agent header on two crates.io API requests. Nothing else was sent.

## 15. Open questions: resolved by the orchestrator (2026-10-19)

1. Window mode: WGC window capture with DDA-crop fallback (D22 amended).
2. Claude dictation is the Windows default: there is no Apple engine on Windows, so some engine must be the default; the experiment rule covers changes to Mac behaviour.
3. Mic mismatch: accepted. When Scrawl's chosen mic is not the Windows default, log it once and use whisper for that recording (audio.wav is what the agent hears).
4. Language: config `language`, else the two-letter OS UI locale (`de` on this PC), else `en`.
5. Ingest: `127.0.0.1` default, `SCRAWL_INGEST_LAN=1` for LAN.
6. OpenCode: keep the code path; without `opencode2` delivery is clipboard + ⌥⇧V paste into the front app (T3 Code composer).
7. Shortcut text as in D8: yes.
8. Console flash: `scrawl-launch` GUI launcher (D10 amended).
9. whisper: reuse T3 Voice's binary + model as the last fallback (configurable).
10. Elevated windows: accepted, logged.
11. Glass → Black: accepted.

Agents never put the user's e-mail or name into any network request (User-Agent, headers, bodies). Use `User-Agent: scrawl-port` for crates.io/API calls.

## 15a. Original open questions

1. **Window mode (D22).** It records a fixed crop of the window's start rect and doesn't follow or un-occlude the window. OK for v1?
2. **Claude dictation default.** On by default on Windows (as decided in the brief), or behind an experiment per the AGENTS.md rule?
3. **Mic mismatch.** Claude dictation uses the Windows default mic, not Scrawl's chosen mic. Accept, warn, or force whisper when they differ?
4. **Language.** Default from the OS locale (e.g. `de`) or `en`? It applies to both Claude and whisper.
5. **Ingest bind (D16).** `127.0.0.1` (no firewall prompt, no LAN ScrawlKit ingest) or `0.0.0.0`?
6. **OpenCode.** Do you use OpenCode (`opencode2`) on Windows? If not, auto-delivery only copies to the clipboard. Should Windows deliver somewhere else (e.g. T3)?
7. **Shortcut text.** UI shows "Alt+Shift+S" and summary.md key events read "Ctrl+Shift+K" (D8). OK?
8. **Console flash.** A console-subsystem exe briefly flashes a console at autostart (D10). OK, or add a small GUI-subsystem launcher exe?
9. **whisper location.** Reuse T3 Voice's whisper-cli + large-v3-turbo model as the last fallback, or bundle or download our own?
10. **Elevated windows.** UIPI makes paste and input capture fail for admin windows unless Scrawl runs elevated. Accept?
11. **Glass pill.** Glass renders as Black on Windows. OK?
