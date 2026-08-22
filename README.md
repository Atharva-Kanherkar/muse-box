# muse-box

A voice-controlled Spotify decoration: a dithered, terminal-aesthetic display with reactive LEDs. It sits on a shelf, looks beautiful whether or not music is playing, and does what you tell it.

This is a personal project for one person, one Spotify account, one box. It is deliberately not designed to scale, multi-tenant, or monetize.

The project is intentionally built in two phases:

1. **Web app first**: a browser-based client that exercises the same backend the hardware will use. Fast iteration, easy debugging, no serial cables.
2. **Hardware second**: an ESP32-S3 with a 1-bit-ish e-paper/IPS panel and an I2S microphone. It consumes the exact same backend endpoints as the web app.

Both clients are dumb renderers. The backend owns Spotify OAuth, the GPT Realtime voice session, tool execution, palette extraction, dithering, and the idle-mode art.

---

## Core design decision: fat backend, dumb clients

Do not build Spotify logic, image decoding, or JPEG resize into the client. The backend emits a **render document** that is ready to blit or draw.

```json
{
  "version": 1,
  "state": "playing",
  "server_ts": "2026-08-21T12:00:00.000Z",
  "track_id": "5FVd6KXrgO9B3JPmC8OPst",
  "track": "Do I Wanna Know?",
  "artist": "Arctic Monkeys",
  "album": "AM",
  "art": {
    "w": 400,
    "h": 400,
    "dither": "bayer",
    "bits": "<base64, packed 1-bit, row-major, MSB first>"
  },
  "art_url": "https://i.scdn.co/image/ab67616d0000b273...",
  "palette": ["#e8663a", "#e8a63a"],
  "progress_ms": 84000,
  "duration_ms": 272000,
  "voice_log": [
    {
      "transcript": "play something mellow",
      "action": "queue:search:mellow",
      "timestamp": "2026-08-21T12:00:00Z"
    }
  ]
}
```

Any client (React canvas, ESP32 LVGL, a CLI) only needs to satisfy two contracts:

- `POST /voice`: upload audio, receive the updated render document.
- `GET /state`: SSE stream of render documents, one per meaningful change.

Everything else is backend implementation detail.

---

## Making it feel alive

This box is a decoration first. Three rules keep it from feeling like a status dashboard:

### 1. Progress is interpolated, never streamed

The backend does **not** push a document every second while a track plays. It pushes one document per meaningful change (track change, play/pause, seek, voice command) and stamps it with `server_ts` and `progress_ms`. Clients animate locally:

```
rendered_progress = progress_ms + (now - server_ts)   // while state == "playing"
```

That is how a 1-per-track update becomes a 60 fps progress bar. The backend still polls Spotify every 1-3 seconds internally; if the observed position drifts more than ~2 s from the expected position (someone seeked from their phone), it broadcasts a fresh document. Nobody re-blits a 20 KB album cover because a second elapsed.

### 2. Beat reactivity is device-local

There is no `fft_bands` field. Beat data over a 1 Hz SSE stream can never look alive (LEDs need 30-60 Hz), and Spotify [deprecated the audio-features and audio-analysis endpoints in November 2024](https://developer.spotify.com/blog/2024-11-27-changes-to-the-web-api), so there is no server-side beat grid to lean on anyway.

Instead, the device already has ears:

- **ESP32**: the I2S microphone hears the room. Run a small FFT on-device at full frame rate and drive the LED strip from local audio energy.
- **Web client**: same idea with a WebAudio `AnalyserNode`.

The server supplies the *color* (`palette`), the device supplies the *motion* (its own mic). This is the classic music-visualizer split and it is the only version that actually pulses on the beat.

### 3. Idle mode is a first-class feature

A shelf decoration is idle most of the day, so `state: "idle"` renders something worth looking at, not a blank panel. The backend renders idle frames server-side (same fat-backend rule) and pushes one document per minute:

- **v1**: a large dithered clock over a slowly evolving Bayer pattern field.
- Later modes: the last album cover slowly decaying/eroding, generative dither drift.

One 20 KB frame per minute costs nothing and keeps every client dumb.

---

## Repository layout

The backend crate lives at the repo root. The web client is a subdirectory.

```
muse-box/
├── README.md
├── AGENTS.md          # agent-specific conventions
├── .env.example       # required environment variables
├── Cargo.toml         # Rust Axum backend (root crate)
├── src/
│   ├── main.rs
│   ├── config.rs
│   ├── account.rs     # per-account isolation + the account registry
│   ├── state.rs
│   ├── error.rs
│   ├── render.rs      # render document types
│   ├── image.rs       # palette + dither pipeline
│   ├── spotify.rs     # OAuth + API client
│   ├── realtime.rs    # GPT Realtime session (voice)
│   ├── idle.rs        # idle-mode frame generator
│   └── routes/
│       ├── mod.rs
│       ├── state.rs   # SSE /state
│       └── voice.rs   # POST /voice
└── web/               # React/Vite client (phase 1)
    ├── README.md       # client-specific constraints
    ├── src/
    ├── index.html
    └── package.json
```

Run the client with `npm install && npm run dev` in `web/`, then set the backend
URL and `DEVICE_API_TOKEN` in its Connection panel. Two constraints are
documented in `web/README.md` and are easy to trip over: `EventSource` cannot
send the bearer header, so `/state` is read with `fetch` and a `ReadableStream`;
and `MediaRecorder` only produces webm/opus, which `/voice` rejects, so audio is
captured through an `AudioWorklet` as PCM16.

---

## Public API

### 1. `GET /state`: Server-Sent Events

One long-lived connection. On connect (and reconnect), the backend **immediately sends the current document**, then pushes a new one on every meaningful change: track change, play/pause, seek detection, voice command, idle-frame tick.

**Request**

```http
GET /state?w=400&h=400&dither=bayer HTTP/1.1
Authorization: Bearer <DEVICE_API_TOKEN>
Accept: text/event-stream
```

**Per-device render parameters** (query string):

| Param | Default | Notes |
|-------|---------|-------|
| `w` | `400` | Art width in pixels. The panel is not chosen yet; the device asks for its own size instead of the contract baking one in. |
| `h` | `400` | Art height in pixels. |
| `dither` | `bayer` | `bayer` or `atkinson`. |

**Event format**

```text
event: state
id: <uuid>
data: {"version":1,"state":"playing",...}

```

Clients must:

- Reconnect automatically if the connection drops. The fresh document on connect is the whole recovery story; there is no replay and no `Last-Event-ID` handling.
- Ignore unknown fields (forwards compatibility).
- Refuse to render documents with a major `version` they do not understand.

Why SSE and not polling? On an ESP32, one TLS handshake for the entire uptime is the difference between usable and painful. In the browser, `EventSource` is free.

---

### 2. `POST /voice`: voice command

Upload an audio blob. The backend feeds it into a persistent **GPT Realtime** session (audio in, tool calls out; no separate STT step), executes the resulting Spotify action, and returns the new render document.

**Request**

```http
POST /voice?rate=16000&bits=16&ch=1 HTTP/1.1
Authorization: Bearer <DEVICE_API_TOKEN>
Content-Type: audio/pcm

<binary audio bytes>
```

Accepted content types, deliberately short:

| Content-Type | Notes |
|--------------|-------|
| `audio/wav` | Self-describing header. Web client default. |
| `audio/pcm` | Raw samples. **Requires** `rate`, `bits`, `ch` query params (raw PCM carries no format info; the backend does not sniff). ESP32 default. |

`webm`/`ogg`/`opus` are intentionally not accepted: decoding them server-side pulls in an ffmpeg-class dependency for zero benefit. The web client records WAV (or raw PCM via an AudioWorklet); the ESP32 sends raw PCM straight off the I2S bus. The backend resamples to 24 kHz mono PCM16 for the Realtime API.

**Processing states**: the moment the upload lands, the backend broadcasts `state: "thinking"` over SSE so the box visibly reacts before the 2-5 s of model + Spotify round trips finish. (`"listening"` is reserved for the future streaming-voice mode; in blob mode the client knows it is recording.)

**Response, success (200 OK)**: the full updated render document, same shape as SSE. The `voice_log` gains an entry with the Realtime transcript and the executed action.

**Response, understood but not actionable (200 OK)**: same document; the `voice_log` entry records the query action (e.g. `query:now_playing`).

**Response, error (4xx/5xx)**

```json
{
  "error": "spotify token expired and refresh failed"
}
```

---

### 3. `GET /auth/spotify`: start Spotify OAuth

Browser-only helper. Redirects to Spotify. Includes a random `state` parameter, verified on callback (standard CSRF protection, cheap even for a one-user box).

### 4. `GET /auth/spotify/callback`: OAuth callback

Spotify redirects here. The backend verifies `state`, exchanges the code for refresh/access tokens, and calls `GET /v1/me` to learn whose account this is — that Spotify user id is the account's whole identity. It gets (or lazily creates) that account's own isolated runtime, persists the tokens there, and signs the browser in with an `HttpOnly` session cookie mapped to that account before redirecting to `/`. Whoever completes this first on a fresh install becomes the **owner** account, which is the only thing `DEVICE_API_TOKEN` (hardware) ever resolves to; everyone else is a cookie-only browser account.

---

## Voice pipeline: GPT Realtime

One model call does the whole job: audio goes in, a tool call comes out. There is no Whisper step and no second LLM.

```
Device/browser
      │
      ▼
 POST /voice (audio blob)
      │ broadcast state:"thinking" over SSE
      ▼
┌──────────┐  input_audio_buffer.append   ┌──────────────┐
│ Backend  │ ───────────────────────────► │ GPT Realtime │
│          │      + commit + response     │  (WebSocket) │
│          │ ◄─────────────────────────── │              │
└────┬─────┘   transcript + tool call     └──────────────┘
     │
     │ execute tool ──► Spotify API
     │ fetch art → palette → dither → RenderDoc
     ▼
 200 OK (RenderDoc)  +  same doc broadcast over SSE
```

Session details:

- The backend keeps **one lazy, persistent WebSocket** to the Realtime API: opened on the first voice command, reused across commands, reopened on drop. The device never talks to OpenAI.
- Model comes from `OPENAI_REALTIME_MODEL`. Default is the mini realtime model (roughly a third of the flagship price per audio minute, and a shelf decoration does not need flagship reasoning). Swap to the full model with one env var if command understanding disappoints.
- Session config: text-only output modality (the box does not talk back, for now), input audio transcription enabled (that transcript is what lands in `voice_log`), server-side turn detection **disabled**. Blob mode commits the buffer manually: `append` → `commit` → `response.create`.
- Tools exposed to the model: `play`, `pause`, `next`, `previous`, `search_and_play`, `queue_search`, `set_volume`, `now_playing`. Current playback context is injected into the session so "play the acoustic version of this" resolves.

---

## Render document schema

| Field | Type | Description |
|-------|------|-------------|
| `version` | `u32` | Document version. Bumped on breaking schema changes. |
| `state` | `"idle" \| "playing" \| "paused" \| "thinking" \| "listening"` | Playback/interaction state. `thinking` = a voice command is being processed. `listening` = reserved for streaming voice. |
| `server_ts` | `string` (RFC 3339) | When this document was built. Clients interpolate progress from it. |
| `track_id` | `string \| null` | Spotify track ID. Clients may use it to cache decoded art. |
| `track` | `string \| null` | Track title. |
| `artist` | `string \| null` | Artist name. |
| `album` | `string \| null` | Album title. |
| `art` | `Art \| null` | Dithered artwork (or idle frame). See below. |
| `art_url` | `string \| null` | Full-color album art URL (Spotify CDN). The web client uses this for its polished UI; the ESP32 ignores it. |
| `palette` | `string[]` | Exactly two colors extracted from the cover. `palette[0]` = dominant background, `palette[1]` = accent (LED wash + UI highlights). |
| `progress_ms` | `u64` | Playback position at `server_ts`. |
| `duration_ms` | `u64` | Total track length. |
| `voice_log` | `VoiceLogEntry[]` | Last N voice commands. N is backend-defined (default 5). |

### Art object

| Field | Type | Description |
|-------|------|-------------|
| `w` | `u32` | Width in pixels (matches the device's requested `w`). |
| `h` | `u32` | Height in pixels. |
| `dither` | `"bayer" \| "atkinson"` | Algorithm used. |
| `bits` | `string` | Base64 of packed 1-bit data. **Row-major, MSB-first within each byte, each row padded to a whole byte. `1` = foreground (ink), `0` = background.** Not a PNG; clients unpack it straight into a framebuffer or `ImageData`. |

There is exactly one **packed-bit** encoding in v1, and the ESP32 consumes only that. The web client is free to render a genuinely nice full-color UI from `art_url`; it also keeps a 1-bit "panel preview" toggle that renders the exact packed bits the hardware will blit. That toggle is what makes the web client the reference renderer.

### Voice log entry

```json
{
  "transcript": "play something mellow",
  "action": "queue:search:mellow",
  "timestamp": "2026-08-21T12:00:00Z"
}
```

Voice actions are stable, machine-readable strings:

| Realtime tool | `action` format |
|---------------|-----------------|
| `play` | `spotify:play` |
| `pause` | `spotify:pause` |
| `next` | `spotify:next` |
| `previous` | `spotify:previous` |
| `search_and_play` | `spotify:play:track:<spotify-track-id>` |
| `queue_search` | `queue:search:<query>` |
| `set_volume` | `spotify:volume:<0-100>` |
| `now_playing` | `query:now_playing` |

`now_playing` is observational and does not mutate Spotify. Search actions use
Spotify's top track result; the resolved track ID is recorded for immediate
playback, while queued searches retain the spoken query for display.

---

## Playback update loop (background)

```
┌─────────────┐   poll every 1-3 s   ┌──────────┐     ┌──────────────────┐
│   Spotify   │ ───────────────────► │ Backend  │ ──► │  RenderDoc cache │
│    API      │                      │          │     │  + broadcast SSE │
└─────────────┘                      └──────────┘     └──────────────────┘
```

On a **meaningful change only** (new track, play/pause flip, seek drift > ~2 s), the backend:

1. Fetches new track metadata and album art URL.
2. Downloads the art.
3. Extracts a palette.
4. Dithers the art per connected device's requested size/algorithm.
5. Builds a new `RenderDoc` and broadcasts it over all open SSE connections.

Steady-state playback produces **zero** SSE traffic between track changes. Clients animate progress themselves.

---

## Image pipeline

The backend owns every hard image operation so the clients stay cheap.

```
┌────────────────┐    ┌─────────────┐    ┌──────────────┐    ┌─────────────┐
│  Spotify art   │───►│ resize to   │───►│   palette    │───►│  1-bit      │
│  JPEG/PNG      │    │ device w×h  │    │ extraction   │    │  dither     │
└────────────────┘    └─────────────┘    └──────────────┘    └──────┬──────┘
                                                                    │
                                                                    ▼
                                                          ┌──────────────────┐
                                                          │ base64 RenderDoc │
                                                          │ art.bits field   │
                                                          └──────────────────┘
```

### Dither strategy

V1 ships **Bayer-ordered** and **Atkinson** (selected per device via the `dither` query param on `/state`). Future candidates:

- Half-block mode (`▀` with independent fg/bg colors) for higher effective resolution in terminal-like clients.

The client never decides the dither mode; it only asks for one.

### Palette extraction

Small k-means or median-cut over the resized cover. Exactly two colors:

1. `palette[0]`: dominant background.
2. `palette[1]`: accent, **clamped for display**. Dark album covers happily hand k-means a near-black "accent," and an LED wash of near-black looks broken. Clamp the accent in HSL to roughly `L ∈ [0.35, 0.75]`, `S ≥ 0.4` before emitting it.

---

## Client consumption guide

### Web client (phase 1)

- `GET /state` via `EventSource`; reconnect = just reconnect, the first event restores everything.
- Interpolate the progress bar locally from `progress_ms` + `server_ts`.
- Render the polished UI from `art_url` (full color, nice typography, the works). Keep a "panel preview" toggle that unpacks `art.bits` into `ImageData` on a `<canvas>`, pixel-for-pixel what the hardware will show.
- `POST /voice` with WAV recorded via `getUserMedia` + an AudioWorklet.
- Drive FFT bars/LED-preview from a WebAudio `AnalyserNode`, colored by `palette[1]`.

### ESP32 client (phase 2)

- One TLS connection to `/state?w=<panel_w>&h=<panel_h>&dither=atkinson`.
- Parse the SSE stream incrementally; documents are small and infrequent.
- Base64-decode `art.bits` directly into the framebuffer, blit via DMA.
- Drive the LED strip: hue from `palette[1]`, motion from an on-device FFT of the I2S mic at 30-60 Hz.
- Voice: record from the I2S mic, `POST /voice` as raw PCM with `rate/bits/ch` params.

### Why the UI does not "transfer"

There is no browser on the ESP32. LVGL is a C toolkit with a scene graph, not a DOM. The web client is therefore not a throwaway; it remains the debug view and the reference renderer. Because both clients consume the same render document, a rendering bug on hardware can be reproduced instantly in a browser tab.

---

## Security model

Open to the public, but not a SaaS with a spend cap: every account shares one `OPENAI_API_KEY`, uncapped, watched manually via the per-account usage log line rather than enforced against. Spotify's own Development Mode ceiling (25 accounts) is the only hard limit right now.

| Secret | Location | Reason |
|--------|----------|--------|
| Spotify refresh token | One per account, under `ACCOUNTS_ROOT` | Never on device. ESP32 flash is dumpable. Never shared between accounts. |
| OpenAI API key | Backend env | Same reason, shared: it is the one thing every account's requests spend against. |
| `DEVICE_API_TOKEN` | One static token, env var | The device holds only this, and it always resolves to the owner account. Revoke = change the env var. |
| Session cookie | `HttpOnly`, per browser | Maps to one account id, never the device token. Page JavaScript never sees it. |

Hardware carries `Authorization: Bearer <DEVICE_API_TOKEN>`, which always resolves to the owner account. A browser carries the session cookie instead, which resolves to whichever account signed it in — same-origin and `SameSite=Lax`, so it never rides along on a cross-origin request. OAuth uses a `state` parameter. TLS in "production" (i.e., whenever the backend leaves localhost); local dev may use HTTP.

---

## Spotify API reality check

- **Premium is required** for the playback-control endpoints (play/pause/skip/volume). Covered.
- A **development-mode app** on your own account is all this needs; no quota extension, no review.
- The `audio-features`, `audio-analysis`, and `recommendations` endpoints were [deprecated for new apps in November 2024](https://developer.spotify.com/blog/2024-11-27-changes-to-the-web-api). This is why beat reactivity is device-local and why "play something mellow" is resolved by the model + `search_and_play`, not by Spotify's recommendation engine.

---

## Environment variables

See `.env.example` for the full list. Required for first boot:

- `SPOTIFY_CLIENT_ID`
- `SPOTIFY_CLIENT_SECRET`
- `SPOTIFY_REDIRECT_URI`
- `OPENAI_API_KEY`
- `DEVICE_API_TOKEN`

Optional:

- `HOST` (default `0.0.0.0`)
- `PORT` (default `3000`)
- `OPENAI_REALTIME_MODEL` (default: the mini realtime model, e.g. `gpt-realtime-mini`; set to `gpt-realtime` for the flagship)
- `ACCOUNTS_ROOT` (default `./data/accounts`) — one subdirectory per account that has completed OAuth, each holding that account's own Spotify token and taste index. Mount its parent as a Railway volume so every account's authorization survives a redeploy.
- `OWNER_MARKER_PATH` (default `./data/owner_account_id.txt`) — which account the hardware `DEVICE_API_TOKEN` resolves to. Set once, by whoever completes OAuth first, and never overwritten by a later login.
- `TOKEN_STORE_PATH` / `TASTE_INDEX_PATH` (defaults `./data/spotify_token.json` / `./data/taste_index.json`) — read only once, at boot, to migrate a pre-multi-tenant install into `ACCOUNTS_ROOT`. Irrelevant on a fresh install.

---

## CI and contribution rules

Every PR runs the full gauntlet in `.github/workflows/ci.yml` and all jobs must pass:

1. `cargo fmt --all --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. Strict clippy on production code (`--lib --bins`): `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, `dbg!`, `println!`/`eprintln!` are all **denied**. Return `AppError`/`anyhow::Error` instead; log with `tracing`. Tests may unwrap.
4. `cargo test --all-targets` with `RUSTFLAGS=-D warnings`, plus a zero-tests guard: a run where no tests execute fails CI.
5. `cargo doc` with `RUSTDOCFLAGS=-D warnings`.
6. `cargo audit` (RustSec advisories).

`Cargo.lock` is committed and CI runs `--locked`; update the lockfile in the same PR as the dependency change. Each issue lists its own acceptance criteria; a PR is done when the criteria boxes are checked, the tests specified in the issue exist, and CI is green.

## Deployment

The backend runs on **Railway**: one service built from this repo, secrets set as Railway variables (`SPOTIFY_*`, `OPENAI_API_KEY`, `DEVICE_API_TOKEN`), TLS terminated by Railway's domain. The Spotify refresh token is persisted to a path on a small Railway volume so redeploys do not require re-auth. `SPOTIFY_REDIRECT_URI` must point at the Railway domain's `/auth/spotify/callback`.

---

## Development plan

### Phase 1: web app

1. Rust backend scaffold (Axum, SSE, config). ✅
2. Spotify OAuth (`state` param) + token refresh.
3. `GET /state` SSE endpoint: current-doc-on-connect, broadcast-on-change, per-device render params.
4. Image pipeline: download art, resize, palette (+ clamp), Bayer + Atkinson dither, packed 1-bit output.
5. Idle mode: dithered clock frame generator, one doc per minute.
6. GPT Realtime session manager: persistent WS, tool schema, blob → PCM16/24k → append/commit/response.
7. `POST /voice`: tool execution against Spotify, `thinking` broadcast, voice_log.
8. React client: canvas renderer (packed-bit unpack), interpolated progress, WAV voice button, WebAudio bars.

### Phase 2: hardware

1. ESP32-S3 firmware skeleton.
2. Connect to the same `/state` SSE stream at panel-native size.
3. Decode and blit `art.bits`.
4. I2S microphone: local FFT → LED strip (hue from `palette`).
5. `POST /voice` raw-PCM upload.
6. Iterate on physical case and panel choice.

### Phase 2.5 (optional): streaming voice

Replace the blob upload with a WebSocket that relays mic audio to the Realtime session continuously, with server-side turn detection. That is when `state: "listening"` comes alive and the box becomes a true walkie-talkie. Blob mode stays as the fallback.

---

## Decisions log

Settled (so future sessions do not relitigate):

- **GPT Realtime, not STT + LLM.** One session does transcription + intent + tool calls.
- **No `fft_bands` in the contract.** Beat reactivity is device-local from the device's own mic; the server only supplies the palette.
- **Progress is interpolated client-side.** No per-second SSE pushes.
- **One packed-bit art encoding for hardware; the web UI renders a nice full-color UI** from `art_url`, with a 1-bit panel-preview toggle as the reference renderer.
- **Multi-tenant: any Spotify account can sign in, up to Spotify's own 25-account Development Mode ceiling.** `SpotifyClient`, `StateHub` and `TasteIndex` are per-account, built lazily behind an `AccountRegistry` and reaped after 10 minutes of no SSE subscriber; `LyricsIndex` and the tempo cache stay shared, since a track's lyrics do not depend on who is listening. `DEVICE_API_TOKEN` (hardware) always resolves to whichever account completed OAuth first — the "owner" — recorded in `OWNER_MARKER_PATH`; a static device token was never going to be multi-tenant. No cost cap: the shared `OPENAI_API_KEY` is watched manually via a per-account log line, not enforced against, and nothing here works toward Spotify's Extended Quota Mode — that is filed only once the 25-account ceiling is actually hit.
- **Backend hosted on Railway.** TLS terminated by Railway; secrets live in Railway service variables; the refresh-token store path points at a Railway volume.
- **Backend is built first.** All open issues are backend-only until phase 1 item 8 (web client).

Still open:

- Panel choice (drives the real `w`/`h` and whether Atkinson beats Bayer in the flesh).
- Spoken replies: Realtime can talk back if the box ever gets a speaker. Off for now.
- Idle mode variants beyond the clock.
