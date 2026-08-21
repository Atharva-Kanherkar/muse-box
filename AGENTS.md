# Agent Notes for muse-box

## Project goal

Build a voice-controlled Spotify decoration: terminal-aesthetic dithered display + reactive LEDs. Web app first, ESP32 hardware second. Both clients consume the same backend-rendered document. Personal project: one user, one Spotify account, one box.

## Architecture rules

- **Fat backend, dumb clients.** Spotify auth, the GPT Realtime voice session, image decode, palette extraction, dithering, and idle-mode art all happen server-side.
- **Two public contracts only:**
  - `GET /state`: SSE stream of `RenderDoc`. Sends the current doc immediately on (re)connect; pushes only on meaningful change. No replay, no `Last-Event-ID`.
  - `POST /voice`: audio blob in (`audio/wav` or `audio/pcm` + `rate/bits/ch` params only), updated `RenderDoc` out.
- **No business logic in the client.** If you find yourself wanting to parse Spotify JSON or resize an image on the frontend, move it to the backend.
- **Version the render document.** Unknown fields must be ignored by clients; unknown major versions must be rejected.
- **Liveness rules** (why the contract looks the way it does):
  - Progress is interpolated client-side from `progress_ms` + `server_ts`. Never add per-second SSE pushes.
  - There is no `fft_bands`. Beat reactivity comes from the device's own mic (I2S FFT on ESP32, WebAudio in the browser); the server only supplies `palette`. Do not reintroduce server-side beat data (Spotify deprecated audio-analysis in Nov 2024 anyway).
  - Idle mode is a feature: the backend renders idle frames (dithered clock over Bayer drift) and pushes one doc per minute.
  - Broadcast `state: "thinking"` the moment a voice upload lands, before the model round trip.

## Technology choices

- Backend: Rust (Axum, tokio, reqwest, image crate), crate at the **repo root** (`src/`), not in a `backend/` subdir.
- Voice: **GPT Realtime over one persistent WebSocket** (lazy-opened, reused, reopened on drop). No separate STT step, no Whisper. Text-only output modality, manual buffer commit (`append` → `commit` → `response.create`), transcription events feed `voice_log`. Model from `OPENAI_REALTIME_MODEL` (default mini).
- Web client: React + Vite + Canvas in `web/` (phase 1).
- Hardware client: ESP32-S3 + LVGL + I2S mic + LED strip (phase 2).
- SSE over polling for `/state`.

## Security

- Spotify tokens and the OpenAI key never leave the backend.
- The device holds only a `DEVICE_API_TOKEN` (static env var; revoke = rotate it).
- Spotify OAuth uses a `state` parameter, verified on callback.
- Use TLS whenever the backend leaves localhost; local dev may use HTTP.

## Code style

- Keep modules small and single-purpose: `spotify.rs`, `realtime.rs`, `image.rs`, `render.rs`, `idle.rs`.
- Return `AppError` from handlers; log internal details with `tracing`.
- Prefer explicit types for the render document; it is the public API.
- Add tests for: packed-bit output length (`ceil(w/8) * h` bytes), palette length (exactly 2) and accent clamp, progress interpolation math.

## Common gotchas

- `art.bits` is packed 1-bit rows (row-major, MSB-first, rows padded to a byte, 1 = ink), **not a PNG**. Clients unpack it straight into a framebuffer/`ImageData`; nothing ever image-decodes it.
- `art` carries its own `w`/`h`/`dither`; devices request their size via query params on `/state`. Never hardcode 400x400 anywhere but the default.
- `audio/pcm` uploads are meaningless without `rate`, `bits`, `ch` query params. The backend does not sniff. Resample to 24 kHz mono PCM16 before feeding the Realtime session.
- Do not add `webm`/`ogg`/`opus` upload support; that drags in an ffmpeg-class dependency for nothing. The web client records WAV.
- Clamp `palette[1]` (HSL: L in [0.35, 0.75], S >= 0.4); k-means on dark covers returns near-black accents that look broken on LEDs.
- SSE on ESP32: one TLS handshake per uptime is the target; steady-state playback must produce zero SSE traffic between track changes.
- Spotify playback control requires Premium (the owner has it); dev-mode app on the owner's account, no quota extension needed.
- The web UI is a permanent debug view and the reference renderer, not a throwaway prototype.
