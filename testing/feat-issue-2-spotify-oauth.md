# feat-issue-2-spotify-oauth — Test Contract

## Functional Behavior

- `GET /auth/spotify` returns a `302` redirect to Spotify Accounts authorization with the configured client ID, redirect URI, the scopes `user-read-playback-state user-modify-playback-state`, and a single-use state value containing at least 128 bits of cryptographic randomness.
- `GET /auth/spotify/callback` rejects missing, unknown, or already-consumed state with a `400` JSON error and does not exchange or persist a token.
- A valid callback exchanges the authorization code, stores the access and refresh token, writes the token file with mode `0600`, and returns an HTML provisioning page containing the configured device API token.
- The token store path comes from `TOKEN_STORE_PATH` and defaults to `./data/spotify_token.json`.
- A stored refresh token is loaded and refreshed during startup so Spotify API calls do not require another browser authorization.
- An access token that expires within 60 seconds is refreshed before use; refresh failures return `AppError::Spotify`.
- Protected non-auth routes require `Authorization: Bearer <DEVICE_API_TOKEN>` and return the standard `{ "error": "..." }` JSON shape on failure. The auth scheme is matched case-insensitively (RFC 7235) and the credential is compared in constant time.
- Production code introduced by this change contains no `unwrap`, `expect`, or `panic!`.

### Failure-mode behavior (added during review of PR #8)

- Startup never aborts because of Spotify authorization: a missing, unparsable, or unrefreshable token store logs a warning and the server still binds, so `/auth/spotify` stays reachable for re-authorization.
- Logging defaults to `info` when `RUST_LOG` is unset, so startup lines are visible on Railway without extra configuration.
- The token store is replaced atomically (temp file, fsync, rename) so an interrupted write cannot destroy the stored refresh token, and the committed file is `0600` even when it replaces a looser one.
- Unconsumed OAuth states expire after 10 minutes and are capped at 64, so the unauthenticated `/auth/spotify` route cannot grow memory without bound.

## Unit Tests

- `spotify::tests::rejects_unknown_or_consumed_state` — an invalid state is rejected and a valid state can only be consumed once.
- `spotify::tests::refresh_decision_uses_sixty_second_window` — tokens expiring within 60 seconds refresh; longer-lived tokens do not.
- `spotify::tests::token_store_round_trip_uses_private_permissions` — persisted token data reloads unchanged and has mode `0600` on Unix.
- `routes::tests::bearer_middleware_rejects_missing_and_wrong_tokens` — missing and incorrect bearer credentials return `401`; the correct credential reaches the handler.
- `spotify::tests::pending_states_are_capped_and_expired_states_rejected` — the pending-state set stays at its cap under repeated authorization starts, and a state older than the TTL is refused.
- `spotify::tests::persist_token_replaces_atomically_and_leaves_no_temp_file` — replacing an existing `0644` store yields `0600`, reloads unchanged, and leaves no `.tmp` sibling.
- `routes::tests::bearer_scheme_is_case_insensitive_and_credential_must_match_exactly` — `bearer`/`BEARER` are accepted, other schemes are not, and the comparison rejects wrong-length and wrong-byte credentials.

## Integration / Functional Tests

- `spotify::tests::authorization_url_contains_required_parameters` — the redirect URL contains the configured client ID, redirect URI, exact required scopes, and a state value backed by at least 128 random bits.
- `spotify::tests::callback_exchange_persists_and_refreshes_token` — a hand-rolled mock Spotify server receives the authorization-code exchange, persisted credentials reload, and an expiring token triggers the refresh-token exchange.
- `routes::tests::callback_rejects_bad_state_without_token_request` — invalid callback state returns `400` and the token endpoint is not called.
- `routes::tests::callback_success_returns_provisioning_page` — a valid callback returns HTML containing the device token after persistence.

## Smoke Tests

- `cargo check --all-targets` succeeds.
- `cargo test --all-targets` succeeds.
- `cargo clippy --all-targets --all-features -- -D warnings` succeeds.
- `cargo fmt --all -- --check` succeeds.
- With required environment variables filled, `cargo run` binds the configured address and exposes the OAuth start route plus a bearer-protected health route.

## E2E Tests

- N/A — live Spotify authorization requires user interaction and real credentials; the complete OAuth flow is covered against an in-process mock server, with a manual live flow below.

## Manual / cURL Tests

- Start the server with a filled `.env`, then run `curl -i http://localhost:3000/auth/spotify`; verify a `302` to `accounts.spotify.com` containing both scopes and a non-empty state.
- Run `curl -i http://localhost:3000/health`; verify `401` and `{"error":"unauthorized"}`.
- Run `curl -i -H 'Authorization: Bearer <DEVICE_API_TOKEN>' http://localhost:3000/health`; verify `200` and `{"status":"ok"}`.
- Complete the Spotify browser flow; verify the callback page displays the device token and `TOKEN_STORE_PATH` exists with private permissions (`stat -f '%Lp' <path>` prints `600` on macOS).
- Restart the server and verify startup refreshes the stored token without opening a browser.
