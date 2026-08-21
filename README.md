# muse-box

A voice-controlled Spotify controller with a dithered, terminal-aesthetic display and reactive LEDs.

The project is intentionally built in two phases:

1. **Web app first** — a browser-based client that exercises the same backend the hardware will use. Fast iteration, easy debugging, no serial cables.
2. **Hardware second** — an ESP32-S3 with a 1-bit-ish e-paper/IPS panel and an I2S microphone. It consumes the exact same backend endpoints as the web app.

Both clients are dumb renderers. The backend owns Spotify OAuth, voice intent parsing, the LLM tool chain, palette extraction, dithering, and beat detection.

---

## Core design decision: fat backend, dumb clients

Do not build Spotify logic, image decoding, or JPEG resize into the client. The backend emits a **render document** that is ready to blit or draw.

```json
{
  "version": 1,
  "state": "playing",
  "track": "Do I Wanna Know?",
  "artist": "Arctic Monkeys",
  "album": "AM",
  "art_1bit": "<base64, 1-bit or half-block dithered, 400x400>",
  "palette": ["#e8663a", "#2a3350"],
  "progress_ms": 84000,
  "duration_ms": 272000,
  "fft_bands": [0.0, 0.12, 0.34, 0.21, 0.08, 0.05, 0.02, 0.01],
  "voice_log": [
    {
      "transcript": "play something mellow",
      "action": "queue:search:mellow",
      "timestamp": "2026-08-21T12:00:00Z"
    }
  ]
}
```

Any client — React canvas, ESP32 LVGL, a CLI — only needs to satisfy two contracts:

- `POST /voice` — upload audio, receive the updated render document.
- `GET /state` — SSE stream of render documents, one per meaningful change.

Everything else is backend implementation detail.

---

## Repository layout (planned)

```
muse-box/
├── README.md          # this file
├── AGENTS.md          # agent-specific conventions
├── .env.example       # required environment variables
├── backend/           # Rust Axum server
│   ├── src/
│   │   ├── main.rs
│   │   ├── config.rs
│   │   ├── state.rs
│   │   ├── error.rs
│   │   ├── render.rs       # render document types
│   │   ├── image.rs        # palette + dither pipeline
│   │   ├── spotify.rs      # OAuth + API client
│   │   ├── voice.rs        # STT + LLM intent handling
│   │   └── routes/
│   │       ├── mod.rs
│   │       ├── state.rs    # SSE /state
│   │       └── voice.rs    # POST /voice
│   └── Cargo.toml
└── web/               # React/Vite client (phase 1)
    ├── src/
    ├── index.html
    └── package.json
```

---

## Public API

### 1. `GET /state` — Server-Sent Events

One long-lived connection. The backend pushes a `RenderDoc` every time playback state changes, once per second while progress advances, or immediately after a successful voice command.

**Request headers**

```http
GET /state HTTP/1.1
Authorization: Bearer <DEVICE_API_TOKEN>
Accept: text/event-stream
```

**Event format**

```text
event: state
id: <uuid>
data: {"version":1,"state":"playing",...}

```

Clients must:

- Reconnect automatically if the connection drops.
- Use `Last-Event-ID` if available, or simply call `GET /state` fresh.
- Ignore unknown fields (forwards compatibility).
- Respect `version` and refuse to render documents with a major version they do not understand.

Why SSE and not polling? On an ESP32, one TLS handshake for the entire uptime is the difference between usable and painful. In the browser, `EventSource` is free.

---

### 2. `POST /voice` — voice command

Upload a raw audio blob. The backend transcribes it, routes it through an LLM with tool calling, executes the resulting Spotify action, and returns the new render document.

**Request**

```http
POST /voice HTTP/1.1
Authorization: Bearer <DEVICE_API_TOKEN>
Content-Type: audio/webm

<binary audio bytes>
```

Accepted content types: `audio/webm`, `audio/wav`, `audio/ogg`, `audio/mpeg`, `audio/raw`.

The device must include the audio format. The backend does not sniff.

**Response — success (200 OK)**

```json
{
  "version": 1,
  "state": "playing",
  "track": "Do I Wanna Know?",
  "artist": "Arctic Monkeys",
  "album": "AM",
  "art_1bit": "...base64...",
  "palette": ["#e8663a", "#2a3350"],
  "progress_ms": 84000,
  "duration_ms": 272000,
  "fft_bands": [],
  "voice_log": [
    {
      "transcript": "play arctic monkeys",
      "action": "spotify:play:track:5FVd6KXrgO9B3JPmC8OPst",
      "timestamp": "2026-08-21T12:00:00Z"
    }
  ]
}
```

**Response — command understood but not actionable (200 OK)**

```json
{
  "version": 1,
  "state": "playing",
  "track": "Do I Wanna Know?",
  "artist": "Arctic Monkeys",
  "album": "AM",
  "art_1bit": "...base64...",
  "palette": ["#e8663a", "#2a3350"],
  "progress_ms": 84000,
  "duration_ms": 272000,
  "fft_bands": [],
  "voice_log": [
    {
      "transcript": "what's playing",
      "action": "query:now_playing",
      "timestamp": "2026-08-21T12:00:00Z"
    }
  ]
}
```

**Response — error (4xx/5xx)**

```json
{
  "error": "spotify token expired and refresh failed"
}
```

---

### 3. `GET /auth/spotify` — start Spotify OAuth

Browser-only helper. Redirects to Spotify.

### 4. `GET /auth/spotify/callback` — OAuth callback

Spotify redirects here. The backend exchanges the code for refresh/access tokens and stores them. On first auth it returns a simple HTML page that also prints the `DEVICE_API_TOKEN` for provisioning.

---

## Render document schema

| Field | Type | Description |
|-------|------|-------------|
| `version` | `u32` | Document version. Bumped on breaking schema changes. |
| `state` | `"idle" \| "playing" \| "paused"` | Playback state. |
| `track` | `string \| null` | Track title. |
| `artist` | `string \| null` | Artist name. |
| `album` | `string \| null` | Album title. |
| `art_1bit` | `string \| null` | Base64-encoded dithered album art. Format depends on `version`; v1 is a 1-bit bitmap at 400x400. |
| `palette` | `string[]` | Dominant/accent colors extracted from the cover. First entry is the primary background; second is accent. |
| `progress_ms` | `u64` | Current playback position in milliseconds. |
| `duration_ms` | `u64` | Total track length in milliseconds. |
| `fft_bands` | `f32[]` | Optional beat-reactivity data (0.0–1.0). Empty when the backend has no mic input or when the device is not the audio source. |
| `voice_log` | `VoiceLogEntry[]` | Last N voice commands. N is backend-defined (default 5). |

### Voice log entry schema

```json
{
  "transcript": "play something mellow",
  "action": "queue:search:mellow",
  "timestamp": "2026-08-21T12:00:00Z"
}
```

---

## Data flow

### Playback update loop (background)

```
┌─────────────┐     poll/notify      ┌──────────┐     ┌─────────────────┐
│   Spotify   │ ───────────────────► │ Backend  │ ──► │  RenderDoc cache │
│    API      │                      │          │     │  + broadcast SSE │
└─────────────┘                      └──────────┘     └─────────────────┘
```

The backend polls Spotify every 1–3 seconds while a track is playing. On change, it:

1. Fetches new track metadata and album art URL.
2. Downloads the art.
3. Extracts a palette.
4. Dithers the art to 1-bit.
5. Builds a new `RenderDoc`.
6. Broadcasts it over all open SSE connections.

### Voice command loop

```
Device/_browser
      │
      ▼
 POST /voice (audio blob)
      │
      ▼
┌──────────┐   STT   ┌─────────┐   intent + tools   ┌─────────┐
│ Backend  │ ───────► │  LLM    │ ─────────────────► │ Spotify │
│          │ ◄─────── │         │ ◄───────────────── │  API    │
└────┬─────┘          └─────────┘                    └────┬────┘
     │                                                    │
     │  fetch art → palette → dither → RenderDoc          │
     │◄───────────────────────────────────────────────────┘
     │
     ▼
 200 OK (RenderDoc)
```

1. Audio is uploaded as a raw blob.
2. Backend sends it to a cloud STT service (OpenAI Whisper).
3. Transcript + current playback context are sent to an LLM with a tool schema.
4. LLM emits a tool call such as `play`, `pause`, `next`, `search_and_play`, `set_volume`, or `describe`.
5. Backend executes the Spotify action.
6. Backend builds a fresh render document and returns it.
7. The same document is broadcast over SSE.

---

## Image pipeline

The backend owns every hard image operation so the clients stay cheap.

```
┌────────────────┐    ┌─────────────┐    ┌──────────────┐    ┌─────────────┐
│  Spotify art   │───►│ resize 400  │───►│   palette    │───►│  1-bit      │
│  JPEG/PNG      │    │ x 400       │    │ extraction   │    │  dither     │
└────────────────┘    └─────────────┘    └──────────────┘    └──────┬──────┘
                                                                    │
                                                                    ▼
                                                          ┌──────────────────┐
                                                          │ base64 RenderDoc │
                                                          │ art_1bit field   │
                                                          └──────────────────┘
```

### Dither strategy

V1 uses **Bayer-ordered dither** to a 1-bit image. Future versions may add:

- Atkinson error diffusion (more texture).
- Half-block mode (`▀` with independent fg/bg colors) for higher effective resolution in terminal-like clients.
- Full-color fallback for the web UI.

The client does not decide the dither mode; it is set per-device by a backend config or query parameter on `/state`.

### Palette extraction

Use a small k-means or median-cut over the resized cover. The backend returns exactly two colors:

1. `palette[0]` — dominant background.
2. `palette[1]` — accent (used for LED wash and UI highlights).

---

## Client consumption guide

### Web client (phase 1)

- `GET /state` via `EventSource`.
- `POST /voice` via `fetch()` with `getUserMedia()` audio.
- Render `art_1bit` to a `<canvas>` or draw it as an `<img>` if the backend provides a PNG.
- Use `palette` for CSS theming and FFT bars.

### ESP32 client (phase 2)

- One TLS connection to `/state`.
- Parse the SSE stream incrementally; JSON objects are small.
- Decode `art_1bit` base64 into a framebuffer.
- Blit the 1-bit buffer to the panel via DMA.
- Drive the LED strip from `palette[1]` and/or `fft_bands`.
- Voice: stream I2S microphone audio to `POST /voice`.

### Why the UI does not "transfer"

There is no browser on the ESP32. LVGL is a C toolkit with a scene graph, not a DOM. The web client is therefore not a throwaway; it remains the debug view and the reference renderer. Because both clients consume the same render document, a rendering bug on hardware can be reproduced instantly in a browser tab.

---

## Security model

| Secret | Location | Reason |
|--------|----------|--------|
| Spotify refresh token | Backend database/env | Never on device. ESP32 flash is dumpable. |
| LLM API key | Backend env | Same reason. |
| `DEVICE_API_TOKEN` | One per device, revocable | The device holds only this token. All other auth is server-side. |

The device authenticates every request with `Authorization: Bearer <DEVICE_API_TOKEN>`.

For development, `DEVICE_API_TOKEN` can be a static env var. In production it should be provisioned per-device and rotatable.

---

## Environment variables

See `.env.example` for the full list. Required for first boot:

- `SPOTIFY_CLIENT_ID`
- `SPOTIFY_CLIENT_SECRET`
- `SPOTIFY_REDIRECT_URI`
- `TOKEN_STORE_PATH` — persisted Spotify token file; defaults to `./data/spotify_token.json`. Mount its parent directory as a Railway volume so authorization survives redeploys.
- `OPENAI_API_KEY`
- `DEVICE_API_TOKEN`

Optional:

- `HOST` (default `0.0.0.0`)
- `PORT` (default `3000`)
- `OPENAI_MODEL` (default `gpt-4o-mini`)

---

## Development plan

### Phase 1 — web app

1. Rust backend scaffold (Axum, SSE, config).
2. Spotify OAuth + token refresh.
3. `GET /state` SSE endpoint.
4. `POST /voice` stub that returns the current state.
5. Image pipeline: download art, resize, palette, dither.
6. LLM tool schema + intent execution.
7. React client: canvas renderer, `getUserMedia` voice button.

### Phase 2 — hardware

1. ESP32-S3 firmware skeleton.
2. Connect to same `/state` SSE stream.
3. Decode and blit `art_1bit`.
4. Add I2S microphone and `POST /voice` upload.
5. Add LED strip output from `palette`/`fft_bands`.
6. Iterate on physical case and panel choice.

---

## Open questions

- Should the web UI support full-color album art as a toggle, or stay strictly 1-bit to match hardware?
- Should beat data (`fft_bands`) be computed server-side from a room microphone stream, or omitted entirely when the device is the only mic source?
- Should we support multiple Spotify accounts / multiple devices per account?

These decisions live in the backend; clients are unaffected.
