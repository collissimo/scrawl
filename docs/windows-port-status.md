# Windows: Collin's additions (orchestrator's notes)

Updated: 2026-10-20 16:45. Read this first after a context reset.

## The premise changed (2026-10-20)

Upstream `kitlangton/scrawl` main ships native Windows support. Hona's PR #3 was merged and v0.3.6 includes `Scrawl-0.3.6-windows-x64.zip`. See `docs/windows.md`.

- The from-scratch port was stopped. It is backed up as branch `backup/windows-port-from-scratch` (local and on `fork`); its design doc lives only there.
- The working branch `t3/port-scrawl-to-windows` was reset to `origin/main` (6449367). T3 renamed the branch, so don't rename it again.
- The official 0.3.6 zip, unpacked in `C:\t\scrawl-036\Scrawl`, works on this PC (Win10 19045). `record --duration 5` produced mp4, frames, a contact sheet and `summary.md`. There was no transcript: the English Parakeet model had not been downloaded.

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

## To-do

- [x] Confirm the official zip runs here.
- [ ] Build `origin/main` from source on this PC (CMake), run cargo test.
- [ ] Check transcribe.cpp for Parakeet v3 / German.
- [ ] Engine: Claude dictation (live) + German file fallback. Tests. A real check needs Collin's voice.
- [ ] Video export via Media Foundation.
- [ ] Real-use checks: app, tray, Alt+Shift+S, ink, paste, review window; fix what breaks.
- [ ] Push to `fork`, open a PR against collissimo/scrawl main, link it in T3, report in German.

## Lessons

- Collin's claim "no Windows support" was not checked against upstream branches and PRs. `git branch -a` showed `origin/feat/windows` all along, and `gh pr list` would have found PR #3. Always check upstream branches, PRs and releases before porting anything.
- Never put Collin's e-mail or name in a network request.
