# Purrnel

An operating system that is just an AI agent, fronted by an animated 2D cat who lives in a
lounge room. You talk to her; she listens and watches (unless you mute her). There are no
apps and no files, only **artefacts** that the AI builds for you. See [PLAN.md](PLAN.md) for
the decisions and phases.

<p align="center">
  <img src="docs/screenshots/room-day.png" alt="Purrnel the cat lounging on the sofa in the daytime" width="49%">
  <img src="docs/screenshots/room-night.png" alt="Purrnel asleep on the sofa at night, with the moon outside the window" width="49%">
</p>

Status: **phase 1, desktop prototype.** The harness runs on a normal Linux desktop and serves
the room to a browser window. Booting from USB comes in phase 2.

## Run it

One-time setup (models are gitignored, about 140 MB):

```bash
mkdir -p models && cd models
curl -LO https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en-q5_1.bin
curl -LO https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium/en_US-lessac-medium.onnx
curl -LO https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium/en_US-lessac-medium.onnx.json
```

You also need `nix` (whisper.cpp comes from nixpkgs), `pipx install piper-tts`, `ffmpeg`
(camera and the voice pitch shift) and PulseAudio/PipeWire (`parec`, `paplay`).

Put an Anthropic API key in `~/.config/purrnel/api_key` (or `ANTHROPIC_API_KEY`), then:

```bash
~/dev/purrnel/scripts/dev.sh
```

and open <http://127.0.0.1:7979>. Just talk. `M` toggles mute, `/` focuses the keyboard box.
Click the cat to pet her (she purrs). Click the TV to reopen the last artefact.

Without a key the cat still starts, tells you it has no brain, and everything local works.
While you test, watch the terminal: each turn logs how long every stage took, e.g.
`[+ 360ms] transcribed`, `[+1500ms] first token`, `[+2100ms] speaking`.

## What she can do

She has a real body in a lounge room (sofa, armchair, coffee table with a mug, TV, window,
rug). When she talks to you she turns to face you and the camera zooms in; the rest of the
time the camera pulls back and she lives her life.

- **Because you ask** (Claude calls the `act` tool): walk_to, run_to, jump_on, jump_down, sit,
  loaf, stretch, groom, scratch (the sofa or armchair), furball, knock_cup, chase_tail,
  zoomies, watch_tv. Several can be chained, and she keeps talking while she does them.
- **Feelings** (`emote`): happy, purr, curious, wave, yawn, love, hiss. Some come with a sound.
- **Sleep** (`go_to_sleep`): she walks to the sofa/armchair/rug/table/TV/window, curls up and
  stays asleep until you type to her or say her name. Room noise doesn't wake her.
- **On her own:** quiet for 60 s and she gets bored and a bit crazy (zoomies, scratching,
  knocking the mug off the table, chasing her tail, the odd furball); after 150 s she's worn
  out and yawns on the sofa; at 180 s she curls up and falls asleep.
- **The internet:** she can search the web and read pages (Anthropic's hosted `web_search` and
  `web_fetch` tools) for news, weather, sports and facts, and artefacts can fetch live data from
  public APIs when you ask for it. Web search must be enabled for your organisation in the
  Claude Console; if the API rejects it, run with `PURRNEL_WEB=0`.
- **The window:** it shows your real local time of day (dawn, day, sunset, night with moon and
  stars), the room dims and the lamp glows at night, and a tree outside gets the odd visit from
  a sparrow, blue tit or robin. When she's free (not talking, busy or asleep) she hops on the
  sill and chatters at them.
- **Camera:** `look` takes one picture when it helps. It never streams, and never while muted.
- **Artefacts:** Claude builds them as sandboxed HTML pages with versioned history. They open
  big over the room with a small live cat in the corner.

## How it fits together

```
mic (parec) -> VAD -> whisper-server -> hub -> Claude (streaming) -> tools
                                         |                          |
camera (ffmpeg, on demand) <-- look -----+       artefact store <---+
                                         |
Piper worker (pitch-shifted) + cat sounds -> paplay (+ mouth levels) -+--> websocket --> room UI
```

- `crates/purrnel-core`: cat states and timeline, the single mute control, risk levels, and
  the versioned content-addressed **artefact store**.
- `crates/purrnel-harness`: the daemon (`audio`, `stt`, `tts`, `camera`, `brain`, `hub`, `server`).
- `ui/index.html`: the whole UI: the room, the front-facing and side-on cat rigs, the curled-up
  sleeper, the behaviour engine and the camera. Behaviours are plain `async` functions in `ACTS`.
- `tools/tts_worker.py`: persistent Piper worker (loads the voice once, pitch-shifts with ffmpeg).
- `tools/make_sfx.py`: synthesises the cat noises into `assets/sfx/`.
- `tools/mock_anthropic.py`: a fake streaming API for testing without a key.

Cat noises written as words ("mrow", "mew", "mrrp", "purr") are played as sounds instead of
being spoken. **The sounds are synthetic and a first attempt**: replace them with real
recordings by dropping `meow1.wav`, `mew1.wav`, `mrrp1.wav`, `purr1.wav`... into `assets/sfx/`.

## Configuration (environment variables)

| Variable | Default |
|---|---|
| `ANTHROPIC_API_KEY` | falls back to `~/.config/purrnel/api_key` |
| `PURRNEL_MODEL` | `claude-sonnet-5-5` (chosen for speed; try `claude-opus-5-5` for more depth) |
| `PURRNEL_FALLBACKS` | on; `0` disables the server-side refusal fallback beta |
| `PURRNEL_WEB` | on; `0` turns off web search and page reading |
| `PURRNEL_PITCH` | `1.3` voice pitch multiplier (1.0 = as recorded) |
| `PURRNEL_SPEED` | `0.92` Piper length scale (lower is faster) |
| `PURRNEL_PORT` | `7979` (bound to localhost only) |
| `PURRNEL_DATA` | `~/.local/share/purrnel` |
| `PURRNEL_MIC` | default PulseAudio source |
| `PURRNEL_CAMERA` | `/dev/video0` |
| `PURRNEL_WHISPER_BIN`, `PURRNEL_WHISPER_MODEL`, `PURRNEL_VOICE`, `PURRNEL_PIPER_PYTHON`, `PURRNEL_SFX` | see `config.rs` |
| `PURRNEL_API_URL` | the real API; point at the mock for tests |

## Tests

```bash
cargo test                              # 40 unit tests
cargo test -- --ignored camera          # needs a webcam
```

The UI has no automated tests; it was checked by driving it in a browser. From the browser
console, `room.ACTS`, `room.Cat`, `room.setState('sleeping')`, `onAct([{do:'scratch'}])`,
`room.setHour(22)` (fake the time of day) and `room.spawnBirds()` poke the room directly.

## Known gaps in phase 1

- **Never run against the real Anthropic API yet.** The streaming parser, tool loop and
  request shape were tested only against `tools/mock_anthropic.py`. The first real run may need
  small fixes. If the request is rejected, try `PURRNEL_FALLBACKS=0`.
- Half-duplex only: the mic is ignored while she talks, so you can't interrupt her.
- Always-on listening hears every sentence in the room; the model is told to answer `[silent]`
  to speech not meant for it, which costs a call each time. The VAD is energy-based.
- Latency is dominated by the cloud call plus first-sentence speech synthesis; local speech
  recognition is about 0.4 s. The GPU build of whisper.cpp wasn't usable here (no Vulkan GPU).
- Conversation history is append-only and unbounded (no compaction yet).
- Artefacts can call the network (the sandbox only blocks same-origin access, not `fetch`), so a
  badly written one could send its own stored data somewhere. A content-security policy for
  artefacts is the obvious next hardening step.
- The room, furniture and her moves are hand-drawn SVG; some poses and transitions will need polish.
- First-run setup (wifi, key by QR or text prompt) is not built; the key comes from a file.

## License

Purrnel is released under the [MIT License](LICENSE).
