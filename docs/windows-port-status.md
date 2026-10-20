# Windows: Collin's additions (orchestrator's notes)

Updated: 2026-10-20 16:45. Read this first after a context reset.

## The premise changed (2026-10-20)

Upstream `kitlangton/scrawl` main ships native Windows support. Hona's PR #3 was merged and v0.3.6 includes `Scrawl-0.3.6-windows-x64.zip`. See `docs/windows.md`.

- The from-scratch port was stopped. It is backed up as branch `backup/windows-port-from-scratch` (local and on `fork`); its design doc lives only there.
- The working branch `t3/port-scrawl-to-windows` was reset to `origin/main` (6449367). T3 renamed the branch, so don't rename it again.
- The official 0.3.6 zip, unpacked in `C:\t\scrawl-036\Scrawl`, works on this PC (Win10 19045). `record --duration 5` produced mp4, frames, a contact sheet and `summary.md`. There was no transcript: the English Parakeet model had not been downloaded.

## Scope decision (Collin, 2026-10-20)

macOS support does NOT need to be preserved in this fork: Collin has no Mac. Windows correctness comes first. Still keep Windows code in `src/win/` and behind cfg where it's cheap, so upstream merges stay easy, but never trade Windows quality for the Mac. Review R1 (macOS) was cancelled for this reason.

## Goal now: what upstream lacks for Collin

1. **Transcription like T3 Voice.** Upstream Windows uses transcribe.cpp with Parakeet Unified **English**, but Collin speaks German.
   - Add a Claude Code dictation engine: a hidden `claude` in a ConPTY, hold space, Ctrl+G editor hook. Port it from `~/.t3/projects/t3-voice/t3voice/engine.py`.
   - Language comes from config `language`, else the OS locale (`de`).
   - Keep the file-based path for import, reprocess and retranscribe. Check whether transcribe.cpp runs Parakeet v3 (multilingual); otherwise use whisper (`~/.t3/projects/t3-voice/vendor/whisper-cpu` + `models/ggml-large-v3-turbo.bin`, verified working: 10 s for 9 s of audio).
2. **Video export on Windows.** Upstream marks it "Not on Windows". Use Media Foundation, reusing upstream `src/win/media.rs`.
3. **Fix bugs** found by real use on Win10.
4. Possibly the browser tab URL through UIA (upstream: not on Windows). Low priority.

## Environment

- Rust 1.99 MSVC at `~/.cargo/bin`. Before commands: `export PATH="$HOME/.cargo/bin:$PATH"`. Build the upstream source with `CARGO_TARGET_DIR=C:/t/scrawl-up`.
- transcribe-cpp needs CMake (+ Ninja), installed via winget on 2026-10-20.
- VS BuildTools 14.51 and Windows SDK 10.0.26100. 14 cores, 32 GB RAM.
- Remote `fork` = github.com/collissimo/scrawl. The PR goes there against main. Never push to `origin` (kitlangton). Merge only after Collin approves.
- Fixtures: `C:/t/fixtures/speech-de.wav` and `speech-en.wav` (SAPI, 16 kHz mono, never played aloud).
- Subagents: T3 `delegate_task`, `claudeAgent` / `claude-opus-5-5`, options `{fastMode:true}`. Keep tasks small and commit often: one 15-minute monolithic agent lost the night to a DNS outage.

## Active subagents (T3 delegate_task, fastMode, isolation worktree), started 2026-10-20 ~17:00

Each brief is `C:/t/briefs/common.md` plus its task file. Each task has its own target dir `C:/t/scrawl-<id>`.

- A: German file transcription (Parakeet TDT v3). **Done and merged** (b614a2c). 316 tests pass. Follow-up: Settings must call `speech_model::ensure()` after the language changes (B owns settings.rs, or do it at integration).
- B: Claude Code dictation engine. **Done and merged** (cfc45d8); A+B+C together: 341 tests pass. Not yet tested with real speech (needs Collin). Risks: it hears only the Windows default mic; a cold start begins listening 2-3 s late; npm `claude.cmd` isn't found.
- Reviews done: R2 robustness and R3 product (R1 macOS was cancelled; macOS is out of scope). Verbatim findings: `C:/t/briefs/review-findings.md`.
- Fix tasks (started 2026-10-20 ~17:45, fastMode, worktree):
  - F1 Claude-dictation robustness: **done and merged** (43e707f).
    - It produced the first real Claude-dictated bundle: `source: claude-code`, `estimated: true`, and the line in `summary.md`. The test bundles held screen and room audio and are deleted.
    - Open item: a recording started within ~1 s of a no-speech finish falls back with "busy".
    - Open item: a recording that ends before Claude listens restarts the session, so the next one starts cold.
  - F2 language picker + model watcher + export retry: **done and merged** (4c55926, plus d86a9d0); 351 tests pass. Open items:
    - The Settings Language menu (31 entries, scrolling) has never been seen on screen.
    - The 5 s watcher loop has only unit tests for its decision.
  - Decision: the default language is `en` unless config `language` is set (no locale following; experiments rule). Collin picks Deutsch in Settings.
  - Merge order: F1 first, then F2, then a full `cargo test`.
  - Language gate for Claude: done (afb45c5, in `claude_dictation::dictate`). F1's doc notes are in `docs/windows.md` (7843d4c). 363 tests pass.
  - R4 review of the fix round: no blockers and no majors; I fixed its 5 minor findings (9ae53f5). 363 tests pass, and `dictation-selftest` passes (the footer error now reads through: "No speech detected.").
  - Collin's test build is `C:/t/scrawl-test/scrawl.exe`, a release build of 9ae53f5. No `~/.config/scrawl/config.json` exists yet, so he chooses Language and Transcription in Settings. Write F1's doc notes into `docs/windows.md`.
  - Then Collin's spoken test.
- C: Windows video export. **Done and merged** (ec4b14f), plus my fix for sheared decoded frames (513473d). 327 tests pass. Follow-ups: the AMD encoder runs near-constant bitrate, so static screens produce big files (switch to VBR via CODECAPI_AVEncCommonRateControlMode); no fast-start; `media::audio_mono` duplicates `export_audio`.

Stopped agents from the old plan are obsolete and must not be resumed: T3 "Scrawl Windows: foundation commit" and the workflow "foundation".

Build: `cmd //c 'C:/t/cargo-msvc.cmd' <args>`. It runs vcvars64 with CMake and Ninja, because the VS 18 generator isn't found. If cmake complains "generator does not match", delete `<target>/debug/build/transcribe-cpp-sys-*/out/build`.

## To-do

- [x] Confirm the official zip runs here.
- [x] Build `origin/main` from source (1m48s): 312 tests pass, 0 fail.
- [x] `language` config + `handoff::transcription_language()` (commit 5162612).
- [x] transcribe.cpp supports Parakeet/Whisper; HF has `handy-computer/parakeet-tdt-0.6b-v3-gguf` (multilingual) → task A
- [x] Engine: Claude dictation (live) + German file fallback (tasks A+B). [x] Collin's real voice test (2026-10-20): 25 s of German, text correct, 5 keyframes, ink fine. Drift: "Follow-Knopf" landed under #4, though it was circled in #3, because word times are estimated.
- [x] Video export via Media Foundation (task C).
- [ ] Real-use checks: app, tray, Alt+Shift+S, ink, paste, review window; fix what breaks.
- [x] PR https://github.com/collissimo/scrawl/pull/1, linked in T3. Merge only after Collin approves.
- [x] The fork's `main` is synced with upstream (6449367), with Collin's OK.
- [x] Alt+Shift+V works (Collin). [ ] It's still unclear whether the Settings Language menu scrolls.
- [ ] F3 (running; `C:/t/briefs/F3-full.md`, target scrawl-b, clientRequestId `scrawl-win-F3-align-1`): run Parakeet alongside Claude and align the word times. Collin approved it.
- [ ] F4 (running; `C:/t/briefs/F4-full.md`, target scrawl-c, clientRequestId `scrawl-win-F4-overlay-1`): BUG from Collin. Ink stops showing on screen after a click on the other monitor, though it is still recorded. Evidence: bundle `scrawl-1792514420118-44d4-3`, log `[1792514413614]`.
- [x] Tests no longer write to the real `scrawl.log` (`cfg!(test)` in `scrawl_log`).
- Collin's config has `transcriber: apple` again (he switched back for a Parakeet test, or something else reset it; ask him).

## Lessons

- Collin's claim "no Windows support" was not checked against upstream branches and PRs. `git branch -a` showed `origin/feat/windows` all along, and `gh pr list` would have found PR #3. Always check upstream branches, PRs and releases before porting anything.
- Never put Collin's e-mail or name in a network request.
