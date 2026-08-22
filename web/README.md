# muse-box web client

React + Vite client for the render-document contract. It satisfies the same two
contracts as the ESP32 — `GET /state` and `POST /voice` — and adds nothing the
backend does not already publish.

```bash
npm install
npm run dev        # http://localhost:5173
npm run build      # static bundle in dist/
```

Set the backend URL and `DEVICE_API_TOKEN` in the UI; both persist to
`localStorage`. `VITE_API_BASE_URL` pre-fills the URL field.

## Two constraints worth knowing before you change this

**`EventSource` is unusable here.** `/state` is bearer-protected and the browser
`EventSource` API cannot set request headers, so the stream is read with `fetch`
plus a `ReadableStream` and the SSE frames are parsed by hand
(`src/lib/stateStream.ts`). Do not "simplify" it back to `EventSource`. Putting
the token in a query string instead would leak it into proxy logs and history.

**`MediaRecorder` is unusable here.** The backend accepts `audio/wav` and
`audio/pcm` only — webm/opus is refused on purpose, to keep an ffmpeg-class
dependency out of the server. So audio is captured through an `AudioWorklet`,
converted to PCM16, and posted as `audio/pcm` with the microphone's own sample
rate in the query string (`src/lib/voice.ts`). Raw PCM accepts any positive
rate, so there is no client-side resampling; the backend resamples to 24 kHz.

## What is where

| File | Role |
|---|---|
| `lib/types.ts` | Mirror of `src/render.rs`, plus progress interpolation |
| `lib/stateStream.ts` | Authenticated SSE over `fetch`, with reconnect and backoff |
| `lib/voice.ts` | Mic capture → PCM16 → `POST /voice` |
| `lib/art.ts` | Unpacks the 1-bit device frame for the preview canvas |

## Reconnect is not optional

The stream is deliberately silent between meaningful changes, so an idle
connection carries no traffic and proxies will reap it with no error the server
knows about. The client reconnects with jittered backoff; the first event after
connect is always a complete document, so reconnecting is lossless.

## Device frame preview

The preview decodes `art.bits` and paints it at native resolution, scaled up
with `image-rendering: pixelated`. It is the packed frame the ESP32 receives —
row-major, MSB first, rows padded to whole bytes — so a bit-order regression in
the backend shows up here as visible noise rather than passing silently.

A set bit means ink, but whether ink is the lit pixel or the dark one depends on
the panel, so the **Ink** control flips the polarity: `lit` for an emissive
display, `dark` for e-paper.

The full-color UI uses `art_url` and ignores `art`, exactly as the render
document intends.
