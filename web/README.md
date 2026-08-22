# muse-box web client

React + Vite client for the render-document contract. It satisfies the same two
contracts as the ESP32 — `GET /state` and `POST /voice` — and adds nothing the
backend does not already publish.

```bash
npm install
npm run dev        # http://localhost:5173
npm run build      # static bundle in dist/
```

Nothing to configure here — see "No setup, and no token in the browser" below.
In dev, Vite proxies `/state`, `/voice`, `/control`, `/command`, `/auth`, and
`/healthz` to a backend on `http://127.0.0.1:3000` (`vite.config.ts`); run
`cargo run` in the repo root alongside `npm run dev`.

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

## Design

One scene: the cover is the interface. Transport keys and the progress needle
live on the cover itself, the track title sits under it in Instrument Serif,
and Muse is a single pill dock at the bottom. When nothing is playing, the
box's own 1-bit dithered clock becomes the cover, which keeps the retro
identity without a separate panel. The album palette drives the accent and the
background wash; the ground carries a halftone dot screen from the same 1-bit
world. Setup hides behind the gear until it is needed.

## What is where

| File | Role |
|---|---|
| `lib/types.ts` | Mirror of `src/render.rs`, plus progress interpolation |
| `lib/stateStream.ts` | Authenticated SSE over `fetch`, with reconnect and backoff |
| `lib/voice.ts` | Mic capture → PCM16 → `POST /voice` |
| `lib/art.ts` | Unpacks the 1-bit device frame for the preview canvas |

## Deploying

There is no separate frontend deployment. The Rust backend serves `dist/` from
its own origin, which is what makes a plain `SameSite=Lax` session cookie work —
a separate origin would be cross-site, and browsers are dropping those cookies.
The root `Dockerfile` builds this client and the server into one image.

`server.mjs` remains for local static previews and is not used in production.

## No setup, and no token in the browser

There is nothing to configure. The client calls its own origin with relative
URLs, the session cookie rides along automatically, and a `401` sends the
browser into `/auth/spotify` — the Spotify authorization you already have to do
once is the login. The device token never reaches page JavaScript; it exists for
hardware, which carries it in an `Authorization` header because it cannot do
OAuth or hold a cookie.

## Reconnect is still handled

The backend now holds the stream open with SSE comments, so a healthy connection
stays up instead of being reaped by a proxy every minute. Reconnect logic
remains for genuine drops — laptop sleep, network changes, a backend redeploy —
with jittered backoff, and the first event after connect is always a complete
document, so reconnecting is lossless. The UI says nothing while the stream is
healthy; only trouble gets words.

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
