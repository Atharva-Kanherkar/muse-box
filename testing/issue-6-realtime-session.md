# issue-6-realtime-session — Test Contract

## Functional Behavior

- `RealtimeManager` lazily opens `wss://api.openai.com/v1/realtime?model=<model>` on the first command, authenticates with a bearer key, reuses one socket for sequential commands, and never exposes the key through `Debug` or tracing.
- The initial `session.update` uses the current Realtime schema: `type: "realtime"`, text-only output, 24 kHz PCM16 input, input transcription enabled, `turn_detection: null`, automatic tool selection, and the complete Spotify command tool schema.
- Each command accepts mono PCM16 plus its source sample rate, resamples to exactly 24 kHz, encodes little-endian bytes in bounded Base64 append chunks, then sends `input_audio_buffer.commit` followed by `response.create` with current playback context.
- Supported tool calls are `play`, `pause`, `next`, `previous`, `search_and_play { query }`, `queue_search { query }`, `set_volume { percent }`, and `now_playing`. Unknown names, malformed arguments, empty search queries, and volume outside `0..=100` return `AppError::Voice`.
- A completed input-transcription event is correlated with the command and returned with the parsed tool call. Supported final function-call event shapes are handled without relying on event ordering.
- A socket drop fails only the in-flight command and discards that session; the next command reconnects and can succeed.
- The ten-second deadline covers the whole exchange — connect, the audio sends, and the wait for a reply — not just the wait. A peer that accepts a connection and never upgrades or never answers returns `AppError::Voice("model timeout")` rather than blocking, since the exchange holds the session lock and would otherwise wedge every later command. The command discards the session and leaves the manager reconnectable.
- Every failure path, connect included, logs the reset once and clears the session. Connect errors carry their underlying cause so a rejected credential is distinguishable from an unreachable endpoint, and no diagnostic surface — error `Display`, error `Debug`, or manager `Debug` — ever contains the API key.

## Unit Tests

- `realtime::tests::resampler_has_exact_lengths_for_supported_rates` — one second at 16 kHz, 44.1 kHz, and 48 kHz produces exactly 24,000 samples.
- `realtime::tests::resampler_preserves_test_tone_frequency` — a 440 Hz tone remains within 2% by positive-going zero-crossing count after each required conversion.
- `realtime::tests::tool_schema_matches_committed_snapshot` — the serialized tool array equals `testing/snapshots/issue-6-tool-schema.json`.
- `realtime::tests::tool_call_validation_rejects_bad_names_and_arguments` — invalid model output cannot escape as a Spotify action.

## Integration / Functional Tests

- `realtime::tests::mock_server_reuses_connection_and_returns_transcript_and_tool` — two sequential commands use one mock WebSocket, send append/commit/response events in order, include playback context, and return correlated transcripts and calls.
- `realtime::tests::dropped_socket_fails_current_command_and_next_reconnects` — the first mock connection drops mid-command; a second connection accepts the next command successfully.
- `realtime::tests::timeout_resets_session_and_manager_reconnects` — a shortened real deadline reaches a stalled mock, returns the exact timeout error, discards the session, and a following command opens a usable connection. Deliberately not `start_paused`: auto-advancing virtual time beside real socket I/O fires the deadline while the runtime is merely waiting on the network.
- `realtime::tests::stalled_connect_times_out_instead_of_hanging` — a peer that accepts TCP without upgrading yields the timeout error instead of blocking, guarded so a regression fails fast rather than hanging.
- `realtime::tests::diagnostics_never_contain_api_key` — a failed mock interaction leaves the secret out of the error `Display`, the error `Debug`, and the manager `Debug`, and resets the session. Asserted on values rather than captured log text, because tracing's per-callsite interest cache is process-global and a sibling test touching the same callsite silences the capture under test parallelism.
- `realtime::tests::connect_failure_is_reported_without_leaking_the_key` — an unreachable endpoint reports the underlying cause, omits the secret, and leaves no session behind.

## Smoke Tests

- `cargo fmt --all --check` succeeds.
- Both strict Clippy commands from `AGENTS.md` succeed.
- `cargo test --locked --all-features --all-targets` succeeds with nonzero tests.
- `RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps --all-features` succeeds.
- `cargo audit` succeeds in GitHub CI.

## E2E Tests

- N/A — no live OpenAI call is authorized for this change; the full documented WebSocket event flow is exercised against in-process mock servers.

## Manual / cURL Tests

- N/A — Realtime uses WebSocket frames rather than cURL in this backend. Deployment smoke testing can issue one voice command after `OPENAI_API_KEY` is provisioned and verify the resulting transcript/tool intent without logging credentials.
