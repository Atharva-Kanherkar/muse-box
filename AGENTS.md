# Agent Notes for muse-box

## Project goal

Build a voice-controlled Spotify controller with a terminal-aesthetic dithered display and reactive LEDs. Web app first, ESP32 hardware second. Both clients consume the same backend-rendered document.

## Architecture rules

- **Fat backend, dumb clients.** Spotify auth, LLM tool calling, image decode, palette extraction, and dithering all happen server-side.
- **Two public contracts only:**
  - `GET /state` — SSE stream of `RenderDoc`.
  - `POST /voice` — raw audio blob in, updated `RenderDoc` out.
- **No business logic in the client.** If you find yourself wanting to parse Spotify JSON or resize an image on the frontend, move it to the backend.
- **Version the render document.** Unknown fields must be ignored by clients; unknown versions must be rejected.

## Technology choices

- Backend: Rust (Axum, tokio, reqwest, image crate).
- Web client: React + Vite + Canvas (phase 1).
- Hardware client: ESP32-S3 + LVGL + I2S mic + LED strip (phase 2).
- SSE over polling for `/state`.

## Security

- Spotify tokens and LLM keys never leave the backend.
- The device holds only a revocable `DEVICE_API_TOKEN`.
- Use TLS in production; local dev may use HTTP only.

## Code style

- Keep modules small and single-purpose: `spotify.rs`, `voice.rs`, `image.rs`, `render.rs`.
- Return `AppError` from handlers; log internal details with `tracing`.
- Prefer explicit types for the render document; it is the public API.
- Add tests for dither output size and palette length.

## Common gotchas

- SSE requires careful error handling on ESP32; one TLS handshake per uptime is the target.
- `art_1bit` is base64-encoded 1-bit data, not a PNG. Do not decode it as an image in the backend.
- The web UI is a permanent debug view, not a throwaway prototype.
