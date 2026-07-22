# Handoff: SuperWhisper feature parity on the iPhone Bridge

Goal: build out enough of SuperWhisper's Windows feature set inside this project that
it can replace SuperWhisper for the owner's actual usage. This document is the brief
for the session that does that work.

Sources for every SuperWhisper claim below are `superwhisper.com/docs/*` (fetched
2026-07-21). The doc index is at `superwhisper.com/docs/llms.txt` — fetch it first,
it lists every page with real URLs. Note `docs.superwhisper.com` does **not** exist.

---

## 1. What already exists here

This is a Rust + vanilla-JS app. An iPhone acts as a microphone, keyboard and file
browser for a Windows PC over Tailscale. Four modes in the web UI: Bridge, PTT,
Dictate, Files.

**Dictation already works end to end.** Do not rebuild it.

| Piece | Where | State |
|---|---|---|
| Phone → PC mic stream | `src/net/ws.rs` (`/mic` WebSocket, f32 stereo 48kHz) | Working |
| Incremental resample to 16k mono | `src/dictate.rs` `DictationBuffer` | Working, tested |
| Local transcription | `src/dictate.rs` `transcribe_wav` → whisper.cpp | Working, tested |
| Upload path for iOS Shortcuts | `POST /api/dictate` in `src/net/server.rs` | Working, tested |
| AAC/m4a decode via ffmpeg | `src/dictate.rs` `convert_to_wav_16k` | Working, tested |
| Type into focused field + clipboard | `src/dictate.rs` `deliver` | Working |
| Dictate tab | `web/index.html` `#view-dictate`, `web/app.js` | Working |

whisper.cpp lives at `%LOCALAPPDATA%\iphone-bridge\whisper\` — CUDA build plus
`ggml-large-v3-turbo-q5_0.bin` (hash-verified against the official HuggingFace
release). An 11-second clip transcribes in ~2.1s including model load on the
owner's RTX 5070 Ti.

Tests: `cargo test` (121), plus `node tests/*.test.mjs` (three suites, 121 total).
The JS tests extract marked pure regions out of `app.js` and run them in a `vm` —
follow that pattern, it needs no DOM or bundler.

---

## 2. SuperWhisper's feature surface

### Not available on SuperWhisper for Windows anyway
Don't build parity with things the Windows build doesn't have. Per
`docs/get-started/windows`: FileSync, custom app folder location, Hold-Shift-to-
auto-send, **Simulate Keypresses**, and agentic coding integrations (Claude Code,
OpenCode, Pi, Codex) are all unsupported on Windows.

That last one matters: the owner drives Claude Code from their phone. SuperWhisper
*cannot* integrate with Claude Code on Windows. This project already can — it owns
the keyboard. That's the strongest argument for switching and the thing to lean into.

### Modes — the core feature, highest value to copy
`docs/modes/modes.md`. A mode bundles:
- a voice model + language (+ optional translate-to-English)
- an AI model and a processing prompt
- audio handling (mute while recording, pause media, record system audio, speaker ID)
- output/formatting instructions
- whether context awareness is on

Built-ins: **Voice to Text** (raw transcription, no AI), **Message / Email / Note**
(prompt-tuned formatting), **Meeting** (multi-speaker), **Super** (context-aware),
**Custom** (user prompt). Modes can auto-activate per application or website —
though once auto-activated the user cannot manually override, which is a known
annoyance worth *not* copying.

### Context awareness
`docs/common-issues/context.md`. Three sources:
1. **Selected text** — highlighted text in the active window when recording starts
2. **Application context** — text from active input fields, plus the window name/title,
   captured *after* voice processing completes
3. **Clipboard** — most recent copy within a 3-second window before/during recording

Only Super Mode enables all three by default.

### Vocabulary and replacements
`docs/get-started/interface-vocabulary.md`. Two distinct mechanisms:
- **Vocabulary** — hint terms fed to the model to bias recognition (names, acronyms,
  jargon). Docs warn that too many entries degrade results.
- **Replacements** — post-transcription, programmatic, **not** AI-mediated, and
  case-insensitive. Deterministic find/replace. Recommended for symbols
  ("at sign" → `@`) and canned strings like email addresses.

This distinction is important and cheap to implement. Replacements are pure string
work — a perfect first unit of parity.

### History
`docs/get-started/interface-history.md`. Per recording it stores the raw
transcription, the AI-processed result, speaker segments, metadata, the mode used,
and **the full prompt + context sent to the AI**. Users can search (raw text only,
not processed output), reprocess an old recording with the current mode, copy either
version, and delete. No automatic retention — it grows forever.

### Voice models
`docs/models/voice.md`. Local options are whisper.cpp tiers (Ultra ≈3GB down to Fast
≈75MB) and NVIDIA Parakeet via WhisperKit. Cloud options are SuperWhisper's own
S1/Ultra and Deepgram Nova. Since this project already runs whisper.cpp locally, the
parity gap is model *selection*, not capability. Note the whisper.cpp build already
downloaded ships `parakeet-cli.exe` too.

### Other
File transcription of arbitrary audio/video files; reprocess-from-history; realtime
transcription; language auto-detection.

---

## 3. Recommended build order

Ordered by value-to-effort for this specific owner, who dictates into Claude Code
and other apps on a Windows PC from an iPhone.

**Tier 1 — do these first**
1. **Replacements.** Pure, deterministic, trivially testable, immediately useful.
   Config in `%LOCALAPPDATA%\iphone-bridge\config.json` alongside the existing PIN.
2. **Modes as prompts.** A mode = name + optional post-processing prompt + settings.
   Run the transcript through a language model when a prompt is set, otherwise pass
   through. Wire mode selection into the Dictate tab.
3. **History.** Store transcripts with timestamp, mode, duration, raw and processed
   text. The Files tab already has list/search UI conventions to copy.

**Tier 2**
4. **Vocabulary hints.** whisper.cpp accepts an initial prompt (`--prompt`) which is
   exactly the mechanism for biasing recognition. Small change to `transcribe_wav`.
5. **Context capture.** Window title and foreground process are easy on Windows
   (`GetForegroundWindow` + `GetWindowTextW`). Selected text and input-field text
   need UI Automation and are substantially harder — treat as a stretch goal.
6. **Model selection.** Let the user pick a whisper model; download on demand.

**Tier 3**
7. Auto-activation per app (depends on 5).
8. File transcription — mostly plumbing over `convert_to_wav_16k`.
9. Speaker separation — genuinely hard, and likely not needed.

---

## 4. Gotchas already paid for

Do not rediscover these.

- **whisper-cli exits 0 when it cannot decode the input.** It prints an error to
  stderr and returns an empty transcript with a success status. Assert on the text,
  never on exit status.
- **whisper.cpp cannot decode AAC/m4a**, which is what an iPhone records. Its
  bundled miniaudio decoder handles WAV/MP3/FLAC only. `convert_to_wav_16k` shells
  out to ffmpeg (already installed on this machine and on PATH).
- **`eprintln!` goes nowhere in the release build** — `windows_subsystem = "windows"`
  means no stderr. Use `crate::logging::log_both`, which writes to
  `%LOCALAPPDATA%\iphone-bridge\bridge.log`. Anything logged with `eprintln!` is
  invisible in production; much of the existing `ws.rs` still has this problem.
- **Spawn console processes with `CREATE_NO_WINDOW`** (`0x0800_0000`) or a black
  window flashes on every dictation.
- **iOS Shortcuts has no "stop recording" action.** Record Audio offers a fixed
  duration or "On Tap" (tap to start, tap to stop). Any design assuming
  press-and-hold from a Shortcut is impossible.
- **iOS drops pointer events** when it steals a gesture. The press reducer in
  `app.js` had a deadlock from exactly this — see the test
  `a lost pointerup does not deadlock the button forever`.
- **Tailscale on this PC wedges in `NoState`.** Restarting the service does not fix
  it; launching `C:\Program Files\Tailscale\tailscale-ipn.exe` does. Then flush DNS.
- **Rebuild sequence is kill → build → verify → relaunch.** The running exe locks
  the binary and `cargo build --release` fails otherwise. The owner does not want
  automatic rebuilds after every edit.

---

## 5. Working agreements for the next session

- **TDD is mandatory here.** Write the failing test, run it, confirm it fails for the
  right reason, then implement. The existing suites are the model to follow.
- **Do not give research subagents write or execute tools.** A general-purpose agent
  in an earlier session downloaded and ran binaries unprompted, which tripped
  Defender's behavioural detection and badly damaged trust. Use read-only web tools
  for research, and ask before anything downloads or executes.
- **Explanations in plain English**, technical detail in parentheses at the end. The
  owner is not a developer.
- **Verify before claiming done.** Run the tests and quote the output. For UI work,
  the owner tests on a real iPhone — nothing is "working" until they say so.
- Ask before rebuilding and relaunching the bridge.

## 6. Open questions for the owner

- Which language model should post-processing use, and should it be local or an API?
  This is the one place a cloud call may be worth it, and it changes the architecture.
- Should modes be selectable from the phone, auto-selected by the focused Windows
  app, or both?
- Is history worth persisting to disk, given the Files tab can already browse the PC?
