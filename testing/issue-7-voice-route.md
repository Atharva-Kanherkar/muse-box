# Issue 7 test contract: `POST /voice`

## Functional Behavior

- `POST /voice` is bearer-protected and accepts only `audio/wav` and
  `audio/pcm` request bodies.
- Raw PCM requires `rate`, `bits`, and `ch`; it accepts PCM16, validates a
  positive supported sample rate, and downmixes stereo to mono before the
  Realtime call.
- WAV input parses RIFF/WAVE chunks, accepts PCM16 mono, downmixes PCM16
  stereo, and rejects malformed, truncated, compressed, or unsupported WAVs.
- Requests over 10 MiB or representing more than 30 seconds return 413.
- A valid upload broadcasts a `thinking` document before awaiting Realtime.
- The returned Realtime tool is dispatched exactly once to Spotify. Search
  commands resolve the top track; `search_and_play` starts it and
  `queue_search` queues it. `now_playing` records a query without mutation.
- Successful commands fetch fresh Spotify playback, append a voice-log entry,
  rebuild the document, return it, and broadcast the same logical document.
- Voice log entries are newest first and capped at five; the sixth evicts the
  oldest.
- Any failure after `thinking` returns the existing JSON error shape and
  publishes a corrected non-`thinking` document.
- Action strings match the documented stable formats in README.

## Unit Tests

- Parse PCM16 mono WAV data and preserve its samples and rate.
- Parse PCM16 stereo WAV data and downmix each frame with saturating-safe
  arithmetic.
- Reject non-PCM, non-16-bit, malformed, truncated, and over-duration WAVs.
- Validate raw PCM parameters, byte alignment, channel count, and duration.
- Keep only five voice-log entries in newest-first order.
- Map every Realtime tool to its documented action string.

## Integration Tests

- Exercise the content-type/parameter/size/auth rejection matrix through the
  Axum router and assert status plus `{ "error": ... }` JSON shape.
- With mocked Realtime and Spotify, assert SSE observes `thinking` before the
  final document for a successful command.
- Force a Realtime timeout/failure and a Spotify action failure independently;
  assert each caller receives an error and SSE receives a follow-up
  non-`thinking` document.
- Assert each mutating tool calls the correct Spotify endpoint and that search
  commands use the top search result.

## Smoke Tests

- Start the router with local mocked external services and upload a minimal
  valid WAV; assert a 200 render document and no real network dependency.

## E2E Tests

- Upload a known PCM16 WAV fixture, have mocked Realtime transcribe `pause` and
  return the pause tool, assert Spotify pause is called exactly once, and
  assert the response and final SSE event have `state: "paused"`.

## Manual / cURL Tests

- With the app configured against development credentials, upload a WAV with
  `curl -H 'Authorization: Bearer ...' -H 'Content-Type: audio/wav'
  --data-binary @command.wav /voice` and verify `thinking` then final state on
  a simultaneous `/state` stream. This run is optional for this PR because the
  agreed verification mode uses mocked external services only.
