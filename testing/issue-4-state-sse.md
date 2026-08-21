# issue-4-state-sse — Test Contract

## Functional Behavior

- A single shared `StateHub` owns the published playback snapshot, latest per-device `RenderDoc` values, one broadcast channel, and bounded per-track art/render caches.
- The background loop polls Spotify currently-playing every 2 seconds after success. HTTP 429 honors `Retry-After` with a minimum 5-second delay; 5xx/network/auth failures are logged and back off without terminating the loop.
- A new document is published only for track-ID change, play/pause transition, or observed progress differing by more than 2 seconds from progress interpolated from the last published `progress_ms` and `server_ts`.
- Normal advancing progress updates do not publish SSE events. A voice-trigger hook can force a publish for later issue #7.
- Published documents stamp the exact poll observation time into `server_ts` and use the unrounded observed `progress_ms`.
- `GET /state` is bearer-protected, immediately emits the latest complete document, then relays meaningful-change broadcasts as `event: state`, `id: <uuid>`, JSON data.
- Query defaults are `w=400`, `h=400`, `dither=bayer`; width/height must each be `16..=1024`, and dither must be `bayer` or `atkinson`. Invalid or malformed values return the standard `400` JSON error.
- Artwork is downloaded once per track and dithered once per distinct `(track_id, w, h, dither)` request. Same-parameter subscribers receive identical documents; different dimensions receive correctly sized packed art.
- Subscriber disconnect only drops its receiver/stream; it never creates or owns a polling task.

## Unit Tests

- `state::tests::render_params_apply_defaults_and_validate_bounds` — defaults and both valid modes parse; out-of-range, malformed, and unknown values fail.
- `state::tests::meaningful_change_detects_track_playback_and_seek_only` — track, play/pause, and >2-second drift publish; ordinary interpolation and the exact 2-second boundary remain silent.
- `state::tests::document_preserves_poll_timestamp_and_progress` — no rounding or restamping occurs.
- `spotify::tests::currently_playing_maps_track_and_idle_responses` — Spotify JSON and 204 map into typed playback observations.
- `spotify::tests::currently_playing_classifies_rate_limits_and_server_errors` — `Retry-After`, 5xx, and other errors are classified for loop backoff.

## Integration / Functional Tests

- `state::tests::steady_progress_is_silent_but_track_and_seek_publish_once` — a mocked observation sequence produces no steady event and exactly one event for each meaningful transition.
- `state::tests::render_cache_deduplicates_same_params_and_sizes_distinct_params` — two identical requests perform one dither/download; another size has correct packed length.
- `state::tests::broadcast_fans_out_to_two_subscribers` — two receivers observe the same generation/document.
- `state::tests::poll_loop_survives_errors_and_respects_retry_after` — paused Tokio time proves 429/5xx do not kill the loop and enforce backoff.
- `routes::state::tests::sse_is_immediate_silent_for_steady_progress_then_emits_changes` — connect through the bearer-protected router against mocked externals; assert initial event, silence, then ordered single change/seek events.
- `routes::state::tests::state_route_rejects_auth_and_parameter_matrix` — missing/wrong bearer returns 401; invalid width, height, dither, and malformed numbers return JSON 400.

## Smoke Tests

- `cargo fmt --all --check` succeeds.
- Both strict Clippy commands from `AGENTS.md` succeed.
- `cargo test --locked --all-features --all-targets` succeeds with nonzero tests.
- `RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps --all-features` succeeds.
- `cargo audit` succeeds in GitHub CI.
- With a filled `.env`, `curl -N -H 'Authorization: Bearer <DEVICE_API_TOKEN>' 'http://localhost:3000/state'` receives an `event: state` document immediately.

## E2E Tests

- N/A — real-account Spotify playback is a credentialed smoke check; the complete HTTP/SSE behavior is exercised with in-process mock servers.

## Manual / cURL Tests

- Run the authenticated `curl -N` command above and verify one immediate event.
- Leave playback steady for 10 seconds and verify no additional event.
- Seek in Spotify and verify exactly one new event with updated `progress_ms`; change track and verify exactly one event with new `track_id`, `art`, and `art_url`.
- Run `curl -i -H 'Authorization: Bearer <DEVICE_API_TOKEN>' 'http://localhost:3000/state?w=8&h=2000&dither=floyd'` and verify JSON `400`.
