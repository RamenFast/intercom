# 🎙 intercom — the station's two-way voice

*Local TTS and STT for Ben's machine under one convention CLI. `say` speaks through
[piper](https://github.com/rhasspy/piper); `hear` listens through
[whisper.cpp](https://github.com/ggerganov/whisper.cpp) — the GGML sibling of the
llama.cpp / stable-diffusion.cpp engines this estate already trusts. Both are one-shot
CPU subprocesses: free, private, and exempt from the sequential-VRAM law, so the mouth
can speak while the card paints.*

Built 2026-07-07 by Claude Fable 5 with Ben. Rust. GPLv3. A station node:
probed by [concourse](https://github.com/RamenFast/concourse), governed by
`~/Dev/ClaudeWorkspace/AGENT-CLI-STANDARD.md`.

## Verbs

```bash
intercom say "text" [--voice lessac] [--out f.wav] [--play]   # '-' reads stdin
intercom hear f.wav [--ear base-en]        # any rate/channels — ffmpeg resamples
intercom hear --mic 6                      # record the mic, then transcribe
intercom voices · ears · status · schema
```

Envelope `{status, tool, version, ts}` on every one-shot; errors carry `fix`;
exit `0/2/3/4`; isatty auto-switch; every wav gets a `.wav.json` sidecar.

## The pipelines (why it exists)

```bash
wisp --json "…" | jq -r .answer | intercom say - --play        # local LLM → mouth
intercom hear --mic 6 --json | jq -r .text | wisp              # ears → local LLM
intercom say "check" --json | jq -r .wav | xargs intercom hear # round-trip self-test
```

## The parts (all data, no code)

| part | where |
|---|---|
| registry | `~/Nexus/💻HomePC/🧰LocalModels/voice-models.json` (`INTERCOM_REGISTRY` overrides) |
| voices / ear models | `~/Nexus/💻HomePC/🧰LocalModels/models-voice/` |
| piper engine | `~/Nexus/🛠️TheWorkshop/📁Projects/piper-current/` (prebuilt release 2023.11.14-2) |
| whisper engine | `~/Nexus/🛠️TheWorkshop/📁Projects/whisper.cpp-current/` (CPU build; `-current` symlink pattern) |
| spoken outputs | `~/.local/share/intercom/said/` (wav + sidecar) |

Installed: `cargo build --release && install -Dm755 target/release/intercom ~/.local/bin/intercom`
(already done on Ben's machine).

## First-day receipts

`say` synthesized 4.45 s of speech in 467 ms; `hear` transcribed it back **verbatim**
at rtf 0.245 (4× faster than realtime, 12 CPU cores); the first announcement played on
the desk speakers while phosphor traced it. Voice ladder (more voices, `small.en` ear)
lives in `voice-models.json §ladder_next` — next rungs are a decision, not a download.
