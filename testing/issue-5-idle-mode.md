# issue-5-idle-mode — Test Contract

## Functional Behavior

- A deterministic server-side idle renderer produces a large `HH:MM` seven-segment clock over a slowly evolving ordered-pattern field for a fixed UTC minute, display offset, width, height, and dither mode.
- The pattern field keys off the absolute UTC minute, while the displayed digits are localized by a configured offset (`IDLE_UTC_OFFSET_MINUTES`, minutes east of UTC, `-840..=840`, default UTC). Scheduling stays on UTC minute boundaries, so rendering remains byte-deterministic for a fixed offset. An offset outside the range, or one that is not a whole number, fails at startup rather than silently displaying the wrong hour.
- Frames too small to fit any clock layout render the pattern field alone rather than a clipped one-pixel smear. Every accepted `(w, h)` in `16..=1024` renders without panicking.
- With no subscribers connected, an idle tick stays in clock mode and keeps the scheduler ticking but skips the render and the broadcast, and it drops cached documents so a later subscriber builds a frame for the minute current at connect time.
- Idle art uses the same packed 1-bit `Art` contract and per-device `(w, h, dither)` parameters as album artwork.
- Clock glyphs occupy at least 40% of the frame height at both required reference sizes: `400x400` and `296x128`.
- After playback becomes empty, idle art waits 30 seconds and then broadcasts only on the next wall-clock minute boundary. Later frames are emitted at most once per minute and carry the boundary timestamp, so the displayed minute is never stale.
- Any observation with a track cancels a pending idle tick immediately. An idle publication rechecks current playback under the hub's publication guard, preventing an idle frame from overtaking an already-processed play event.
- Idle frames are rendered only for the bounded set of registered device variants. Existing subscriber/variant pruning remains in force, so no collection grows once its configured limit is reached.
- The idle palette is `#1a1a1a` / `#e0e0e0` until album art yields a palette. Later idle documents retain `#1a1a1a` as the background and the most recently rendered album-art accent as the second color.

## Unit Tests

- `idle::tests::golden_bayer_clock_at_400_square` — fixed timestamp, `400x400`, Bayer mode produces the reviewed golden hash and a clock at least 40% of frame height.
- `idle::tests::golden_atkinson_clock_at_296_wide` — a second fixed timestamp, `296x128`, Atkinson mode produces the reviewed golden hash and a clock at least 40% of frame height.
- `idle::tests::fixed_inputs_are_byte_deterministic` — two renders with identical timestamp, size, and mode yield identical packed bytes.
- `idle::tests::minute_changes_pattern_and_clock` — adjacent minute timestamps produce distinct art.
- `idle::tests::offset_localizes_displayed_digits_across_midnight` — the offset shifts the displayed digits, wraps in both directions across midnight, reaches the rendered frame, and stays deterministic.
- `idle::tests::frames_too_small_for_a_clock_render_pattern_only` — `16x16` yields no layout, and with no clock drawn the display offset cannot change a single bit.
- `state::tests::idle_publish_without_subscribers_skips_broadcast_but_keeps_ticking` — a tick with nobody listening returns `true` so ticking continues, and a later subscriber gets the current minute rather than the skipped one.

## Integration / Functional Tests

- `state::tests::idle_scheduler_emits_once_per_minute_then_stops_on_play` — paused Tokio time proves the 30-second threshold, minute alignment, exactly one event per simulated minute, and zero idle events after a play observation.
- `state::tests::idle_frames_follow_registered_device_params` — registered reference variants receive art with their own dimensions and dither modes.
- `state::tests::idle_palette_keeps_last_rendered_accent` — a successfully rendered track palette is retained for the next idle clock while the default background remains stable.

## Smoke Tests

- `cargo fmt --all --check` succeeds.
- Both strict Clippy commands from `AGENTS.md` succeed.
- `cargo test --locked --all-features --all-targets` succeeds with nonzero tests.
- `RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps --all-features` succeeds.
- `cargo audit` succeeds in GitHub CI.

## E2E Tests

- N/A — the renderer and scheduler use deterministic inputs and mocked/paused time; credentialed Spotify behavior remains outside this issue.

## Manual Verification

- Connect two authenticated `/state` clients with `w=400&h=400&dither=bayer` and `w=296&h=128&dither=atkinson`, stop playback, and verify both receive a legible clock on minute boundaries after the idle threshold.
- Start playback just before a boundary and verify the next event is playback state, with no later idle clock while the track remains present.
