# Windows port: status and to-do (orchestrator's notes)

Updated: 2026-10-19 23:30. Read this first after a context reset.

## Goal

Port Scrawl to native Windows 10 (19045, x64) with feature parity wherever Windows allows, so it runs on Collin's PC. The user asked for this in German and wants workflows used. Collin is asleep, so I work autonomously. Reply to him in German, very concisely.

## Binding design

`docs/windows-port.md` holds the decisions D1–D24, the module map, the §4 API contracts, the §8 parity table, the §11 foundation plan and the §12 work packages P1–P8. §15 holds the questions I resolved myself: WGC window mode, the `scrawl-launch` GUI launcher, language from the OS locale (`de`), whisper fallback when the mic differs from the Windows default, ingest on 127.0.0.1, clipboard plus Alt+Shift+V when opencode2 is missing.

## Environment

- Rust 1.99 MSVC at `~/.cargo/bin` (not on PATH in old shells). Before every command: `export PATH="$HOME/.cargo/bin:$PATH"; export CARGO_TARGET_DIR=C:/t/scrawl`.
- VS BuildTools 14.51 and Windows SDK 10.0.26100 are installed. Machine: 14 cores, 32 GB RAM. `git core.autocrlf=true`, `core.longpaths=true`.
- GPUI 0.2.2 builds and opens a window on Windows. Verified with the probe crate `C:\t\probe-gpui`, target dir `C:\t\tgt-probe`.
- Git: branch `t3/2c2a4260`. Remote `fork` = https://github.com/collissimo/scrawl (the user's fork, created with gh). `origin` = kitlangton/scrawl (upstream; never push there).
- The PR goes to `collissimo/scrawl` main. Merge only after the user approves it. Link the PR with t3 `link_pull_request`.
- Transcription source:
  - T3 Voice = the user's own tool at `~/.t3/projects/t3-voice`. `t3voice/engine.py` drives a hidden `claude` session in a PTY: hold space, then Ctrl+G with an editor hook.
  - whisper fallback: `~/.t3/projects/t3-voice/vendor/whisper-cpu` (zip/Release) + `models/ggml-large-v3-turbo.bin`.
  - Never read `~/.t3/userdata` or `secrets` (the classifier blocks it).
- Never put the user's e-mail or name in a network request. A mapping agent once sent the e-mail in a crates.io User-Agent; the user has been told.

## Workflow runs

| Run | What | State |
|---|---|---|
| wf_b4a49843-562 | Understand: 8 maps + design | done. Maps saved in `C:\t\understand.json`; design committed (af826a6, fcf2ce0) |
| wf_323d1e89-092 | Foundation (implement → 2 verifiers → fix loop ×3) | **stopped after ~3 min when the session restarted.** Partial, uncommitted: Cargo.toml windows deps, Cargo.lock, `src/bin/scrawl-launch.rs`. Resume with `Workflow({scriptPath: <session>/workflows/scripts/scrawl-windows-foundation-wf_323d1e89-092.js, resumeFromRunId: "wf_323d1e89-092"})` |

## To-do

- [x] Toolchain, GPUI probe, fork
- [x] Understand workflow + design doc (+ my §15 decisions)
- [ ] Foundation commit: crate compiles, tests build, CLI smoke, §9.1 baseline failures. **← now**
- [ ] Package workflow: P1–P8 in parallel, each in its own worktree with exclusive files (§12). Each package returns `{branch, sha, proof}`.
- [ ] Merge the package branches into `t3/2c2a4260` (I merge), then a full build and tests.
- [ ] Integration workflow:
  - [ ] Run `scrawl app` (tray, hotkeys, pill, overlay).
  - [ ] Real Screen/Area/Window captures via `scrawl record --duration N`.
  - [ ] Inspect the bundle (`summary.md`, `contact-sheet.png`, frames).
- [ ] whisper test: make a speech WAV with SAPI `SetOutputToWaveFile` (never play it aloud), then `scrawl reprocess`/`import`.
- [ ] Claude dictation: `scrawl dictation-selftest` (session start, typed text through the editor hook). Speech needs the user.
- [ ] Adversarial review workflow on the full diff, then fixes.
- [ ] `scripts/install-windows.ps1`: build release, copy to `%LOCALAPPDATA%\scrawl`, Start-menu + Run key via `scrawl-launch`. README Windows section; SKILL.md paths.
- [ ] Push the branch to `fork`, open a PR against collissimo/scrawl main, link it in T3.
- [ ] Morning report to the user, in German and short: what works, what doesn't, what needs their voice test.

## Rules learned

- If the same fix fails twice: stop and write a sharper brief.
- Mac code can't be compiled here, so only add cfg gates and Windows twins; leave the mac bytes unchanged.
- Workflow agents: give each brief its goal, non-goals, end state, files, what's ruled out, proof and answer format; tell them never to commit unless asked; give them the shared `CARGO_TARGET_DIR`.
