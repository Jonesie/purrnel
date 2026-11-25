# Purrnel

An operating system that is just an AI agent, fronted by an animated 2D cat.
Boots from a USB stick. You talk to it; it listens and watches (unless you mute
it). There are no apps and no files, only **artefacts** that the AI creates.

"Purrnel" = purr + kernel. Working name: rename freely (alternatives: Catnix,
Cheshire, Familiar).

## Decisions (agreed)

| Area | Decision |
|---|---|
| Brain | Claude via API (cloud), Sonnet 5.5 by default for speed. Offline = the cat stops and waits for the network to return. No local-LLM fallback. |
| Harness | Rust daemon. |
| Base OS | NixOS (reproducible, declarative USB image). Slim, with extra downloads allowed at runtime. |
| Display | No desktop. Wayland kiosk compositor (e.g. cage) running one fullscreen web view. |
| Cat | 2D skeletal-animated figure with a front-facing rig (talking to you) and a side-on rig (walking, jumping, scratching), living in a lounge room: sofa, armchair, table, TV, window, rug. States: idle, listening, thinking, talking, bored/crazy, tired, sleeping, waiting. |
| Voice | Local STT (whisper.cpp base.en) and local TTS (Piper, pitch-shifted to sound smaller) first, plus synthetic cat sounds. Cloud TTS (e.g. ElevenLabs) is an optional upgrade later. |
| Listening | Always listening by default. |
| Camera | On by default. |
| Mute | **One control mutes mic and camera together.** Visible indicator whenever either is live. |
| Input | Voice and camera primary, keyboard secondary. |
| First-run setup | Text prompts or QR / phone pairing for wifi + API key. |
| User data | Everything is an artefact. No filesystem exposed to the user. |
| Apps | None. The AI generates artefacts (sandboxed web pages) on demand. |
| Personality | Friendly and cuddly; a little crazy when bored, then tired, then asleep on the sofa. |
| Safety | Ask (spoken confirmation) only for genuinely risky actions. Otherwise just act. |

## Architecture

```
 mic ─┐                                   ┌─► speaker (TTS)
 cam ─┤  purrnel-harness (Rust daemon)    │
      ├─► privacy gate ─► VAD/STT ─► Claude agent loop ─► tool calls
      │                   presence det.       │              │
      │                                       ▼              ▼
      │                               artefact store   risk gate
      │                               (versioned,      (confirm if
      │                               content-addr.)    risky)
      └─────────────── state ──ws──► kiosk web UI: cat + artefact viewport
```

- **Privacy gate** sits in front of every sensor. When muted, audio/video are
  not captured at all (not just ignored).
- **Artefact store**: content-addressed and versioned (git-like) so undo and
  history are free. Lives on the USB persistence partition.
- **Artefacts** run in a sandboxed browser context with per-artefact
  capability grants (mic, network, storage). They never get a shell. Only the
  harness has system powers.
- **Risk gate**: tool calls carry a risk level. Low/Medium run freely; High
  needs spoken confirmation.

## Phases

1. **Desktop prototype** *(built, see README)*: harness runs on a dev machine with a
   web UI in a window: cat, voice in/out, Claude loop, artefacts. Goal: does it *feel*
   good? Verified against a mock API; still needs a first run with a real key.
2. **Image**: NixOS config booting the same thing in a kiosk; test in QEMU.
3. **USB hardening**: persistence, first-run wifi/API-key setup, firmware,
   image size.
4. **Extras**: camera understanding, more characters, sync.

## Open questions

- ~~Wake word vs. always-on~~ Decided: always-on. Open: camera-assisted "is the user
  talking to me" to cut false triggers.
- Target hardware beyond a generic x86_64 UEFI laptop.
- Cat art: drawn by us (SVG rig) or commissioned/found.
- Where API key and wifi credentials are stored on the stick (encryption).
