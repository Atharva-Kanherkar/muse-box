use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration as StdDuration,
};

use base64::{Engine as _, engine::general_purpose};
use chrono::{DateTime, Duration, Utc};
use rand::{RngCore, rngs::OsRng};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock};

use crate::error::AppError;

const SPOTIFY_AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const SPOTIFY_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const SPOTIFY_API_BASE_URL: &str = "https://api.spotify.com/v1";
const SPOTIFY_SCOPES: &str = "user-read-playback-state user-modify-playback-state";
const REFRESH_WINDOW_SECONDS: i64 = 60;
/// Whole-request budget for every Spotify call. reqwest sets no timeout by
/// default, and an accepted-but-silent connection would otherwise hang the poll
/// loop forever: no response, no error, no backoff, and no log line.
pub(crate) const HTTP_TIMEOUT: StdDuration = StdDuration::from_secs(10);
/// Connect budget, kept well under [`HTTP_TIMEOUT`] so a dead peer is reported
/// as a connect failure rather than eating the whole request budget.
pub(crate) const HTTP_CONNECT_TIMEOUT: StdDuration = StdDuration::from_secs(5);
/// Ceiling on an honored `Retry-After`. The header is upstream input, and a
/// garbled value would otherwise park polling effectively forever.
pub(crate) const MAX_RATE_LIMIT_BACKOFF: StdDuration = StdDuration::from_secs(300);
/// How long an unconsumed OAuth state stays valid. Long enough to log in and
/// approve, short enough that abandoned flows do not accumulate.
const STATE_TTL_SECONDS: i64 = 600;
/// Hard cap on tracked states. `/auth/spotify` is unauthenticated by design, so
/// the set needs a ceiling no matter how often it is hit.
const MAX_PENDING_STATES: usize = 64;

#[derive(Clone, Debug)]
pub struct SpotifyConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub token_store_path: PathBuf,
}

#[derive(Clone, Debug)]
struct SpotifyEndpoints {
    authorize_url: String,
    token_url: String,
    api_base_url: String,
}

impl Default for SpotifyEndpoints {
    fn default() -> Self {
        Self {
            authorize_url: SPOTIFY_AUTHORIZE_URL.to_string(),
            token_url: SPOTIFY_TOKEN_URL.to_string(),
            api_base_url: SPOTIFY_API_BASE_URL.to_string(),
        }
    }
}

#[derive(Clone)]
pub struct SpotifyClient {
    http: Client,
    config: SpotifyConfig,
    endpoints: SpotifyEndpoints,
    pending_states: Arc<Mutex<HashMap<String, DateTime<Utc>>>>,
    token: Arc<RwLock<Option<StoredToken>>>,
    refresh_guard: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StoredToken {
    access_token: String,
    refresh_token: String,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: Option<String>,
}

/// Playback fields needed to decide whether clients need a new render document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackObservation {
    pub observed_at: DateTime<Utc>,
    pub track_id: Option<String>,
    pub track: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub art_url: Option<String>,
    pub is_playing: bool,
    pub progress_ms: u64,
    pub duration_ms: u64,
}

impl PlaybackObservation {
    pub fn idle(observed_at: DateTime<Utc>) -> Self {
        Self {
            observed_at,
            track_id: None,
            track: None,
            artist: None,
            album: None,
            art_url: None,
            is_playing: false,
            progress_ms: 0,
            duration_ms: 0,
        }
    }
}

/// Error categories used by the poll loop to select its next delay.
#[derive(Debug, thiserror::Error)]
pub enum PlaybackFetchError {
    #[error("Spotify rate limited playback polling for {0:?}")]
    RateLimited(StdDuration),
    #[error("transient Spotify playback error: {0}")]
    Transient(String),
    #[error(transparent)]
    Spotify(#[from] AppError),
}

#[derive(Debug, Deserialize)]
struct CurrentlyPlayingResponse {
    item: Option<SpotifyTrack>,
    is_playing: bool,
    progress_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct SpotifyTrack {
    id: String,
    name: String,
    duration_ms: u64,
    artists: Vec<SpotifyArtist>,
    album: SpotifyAlbum,
}

#[derive(Debug, Deserialize)]
struct SpotifyArtist {
    name: String,
}

#[derive(Debug, Deserialize)]
struct SpotifyAlbum {
    name: String,
    images: Vec<SpotifyImage>,
}

#[derive(Debug, Deserialize)]
struct SpotifyImage {
    url: String,
}

/// Build an HTTP client that cannot hang indefinitely.
///
/// `build` only fails when the TLS backend cannot be initialized, so the
/// fallback is unreachable in practice; it exists because a panic here would
/// take down the process and strict Clippy forbids `unwrap` in production code.
pub(crate) fn http_client() -> Client {
    Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .build()
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "falling back to an HTTP client without timeouts");
            Client::new()
        })
}

impl SpotifyClient {
    pub fn new(config: SpotifyConfig) -> Self {
        Self::with_endpoints(config, SpotifyEndpoints::default())
    }

    fn with_endpoints(config: SpotifyConfig, endpoints: SpotifyEndpoints) -> Self {
        Self {
            http: http_client(),
            config,
            endpoints,
            pending_states: Arc::new(Mutex::new(HashMap::new())),
            token: Arc::new(RwLock::new(None)),
            refresh_guard: Arc::new(Mutex::new(())),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_endpoints(
        config: SpotifyConfig,
        authorize_url: String,
        token_url: String,
    ) -> Self {
        Self::with_endpoints(
            config,
            SpotifyEndpoints {
                authorize_url,
                token_url,
                api_base_url: SPOTIFY_API_BASE_URL.to_string(),
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn with_test_api_url(config: SpotifyConfig, api_base_url: String) -> Self {
        Self::with_endpoints(
            config,
            SpotifyEndpoints {
                authorize_url: SPOTIFY_AUTHORIZE_URL.to_string(),
                token_url: SPOTIFY_TOKEN_URL.to_string(),
                api_base_url,
            },
        )
    }

    #[cfg(test)]
    pub(crate) async fn authorize_for_test(&self) {
        *self.token.write().await = Some(StoredToken {
            access_token: "test-access-token".to_string(),
            refresh_token: "test-refresh-token".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        });
    }

    pub async fn authorization_url(&self) -> Result<String, AppError> {
        let mut random = [0_u8; 32];
        OsRng.fill_bytes(&mut random);
        let state = general_purpose::URL_SAFE_NO_PAD.encode(random);

        {
            let mut pending = self.pending_states.lock().await;
            let now = Utc::now();
            pending.retain(|_, issued| now - *issued < Duration::seconds(STATE_TTL_SECONDS));
            // Still full of live states? Drop the oldest to make room, so a
            // flood of unfinished flows cannot grow this without bound.
            while pending.len() >= MAX_PENDING_STATES {
                let oldest = pending
                    .iter()
                    .min_by_key(|(_, issued)| **issued)
                    .map(|(key, _)| key.clone());
                match oldest {
                    Some(key) => {
                        pending.remove(&key);
                    }
                    None => break,
                }
            }
            pending.insert(state.clone(), now);
        }

        let mut url = Url::parse(&self.endpoints.authorize_url)
            .map_err(|error| AppError::Spotify(format!("invalid authorize URL: {error}")))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("scope", SPOTIFY_SCOPES)
            .append_pair("redirect_uri", &self.config.redirect_uri)
            .append_pair("state", &state);

        Ok(url.into())
    }

    pub async fn exchange_authorization_code(
        &self,
        code: &str,
        state: &str,
    ) -> Result<(), AppError> {
        if !self.consume_state(state).await {
            return Err(AppError::BadRequest("invalid OAuth state".to_string()));
        }

        let response = self
            .http
            .post(&self.endpoints.token_url)
            .basic_auth(&self.config.client_id, Some(&self.config.client_secret))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", self.config.redirect_uri.as_str()),
            ])
            .send()
            .await
            .map_err(spotify_request_error)?
            .error_for_status()
            .map_err(spotify_request_error)?
            .json::<TokenResponse>()
            .await
            .map_err(spotify_request_error)?;

        let refresh_token = response.refresh_token.ok_or_else(|| {
            AppError::Spotify("authorization response omitted refresh token".to_string())
        })?;
        let token = StoredToken {
            access_token: response.access_token,
            refresh_token,
            expires_at: Utc::now() + Duration::seconds(response.expires_in),
        };

        self.persist_and_set(token).await
    }

    pub async fn initialize_from_store(&self) -> Result<bool, AppError> {
        let bytes = match tokio::fs::read(&self.config.token_store_path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(AppError::Spotify(format!(
                    "failed to read Spotify token store: {error}"
                )));
            }
        };
        let token: StoredToken = serde_json::from_slice(&bytes).map_err(|error| {
            AppError::Spotify(format!("failed to parse Spotify token store: {error}"))
        })?;
        *self.token.write().await = Some(token);
        self.refresh_access_token(true).await?;
        Ok(true)
    }

    pub async fn access_token(&self) -> Result<String, AppError> {
        self.refresh_access_token(false).await?;
        self.token
            .read()
            .await
            .as_ref()
            .map(|token| token.access_token.clone())
            .ok_or_else(|| AppError::Spotify("Spotify authorization is required".to_string()))
    }

    pub async fn currently_playing(&self) -> Result<PlaybackObservation, PlaybackFetchError> {
        let access_token = self.access_token().await?;
        let url = format!(
            "{}/me/player/currently-playing",
            self.endpoints.api_base_url.trim_end_matches('/')
        );
        let response = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|error| PlaybackFetchError::Transient(error.to_string()))?;
        let status = response.status();
        let observed_at = Utc::now();

        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(PlaybackObservation::idle(observed_at));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map_or(StdDuration::from_secs(5), StdDuration::from_secs)
                .clamp(StdDuration::from_secs(5), MAX_RATE_LIMIT_BACKOFF);
            return Err(PlaybackFetchError::RateLimited(retry_after));
        }
        if status.is_server_error() {
            return Err(PlaybackFetchError::Transient(format!(
                "Spotify currently-playing returned {status}"
            )));
        }
        if !status.is_success() {
            return Err(PlaybackFetchError::Spotify(AppError::Spotify(format!(
                "currently-playing returned {status}"
            ))));
        }

        let payload = response
            .json::<CurrentlyPlayingResponse>()
            .await
            .map_err(|error| PlaybackFetchError::Transient(error.to_string()))?;
        let Some(track) = payload.item else {
            return Ok(PlaybackObservation::idle(observed_at));
        };
        Ok(PlaybackObservation {
            observed_at,
            track_id: Some(track.id),
            track: Some(track.name),
            artist: Some(
                track
                    .artists
                    .into_iter()
                    .map(|artist| artist.name)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            album: Some(track.album.name),
            art_url: track.album.images.into_iter().next().map(|image| image.url),
            is_playing: payload.is_playing,
            progress_ms: payload.progress_ms.unwrap_or_default(),
            duration_ms: track.duration_ms,
        })
    }

    async fn consume_state(&self, state: &str) -> bool {
        let issued = self.pending_states.lock().await.remove(state);
        match issued {
            Some(issued) => Utc::now() - issued < Duration::seconds(STATE_TTL_SECONDS),
            None => false,
        }
    }

    async fn refresh_access_token(&self, force: bool) -> Result<(), AppError> {
        let _guard = self.refresh_guard.lock().await;
        let current =
            self.token.read().await.clone().ok_or_else(|| {
                AppError::Spotify("Spotify authorization is required".to_string())
            })?;

        if !force && !token_needs_refresh(&current, Utc::now()) {
            return Ok(());
        }

        let response = self
            .http
            .post(&self.endpoints.token_url)
            .basic_auth(&self.config.client_id, Some(&self.config.client_secret))
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", current.refresh_token.as_str()),
            ])
            .send()
            .await
            .map_err(spotify_request_error)?
            .error_for_status()
            .map_err(spotify_request_error)?
            .json::<TokenResponse>()
            .await
            .map_err(spotify_request_error)?;

        let token = StoredToken {
            access_token: response.access_token,
            refresh_token: response.refresh_token.unwrap_or(current.refresh_token),
            expires_at: Utc::now() + Duration::seconds(response.expires_in),
        };
        self.persist_and_set(token).await
    }

    async fn persist_and_set(&self, token: StoredToken) -> Result<(), AppError> {
        persist_token(&self.config.token_store_path, &token).await?;
        *self.token.write().await = Some(token);
        Ok(())
    }
}

fn token_needs_refresh(token: &StoredToken, now: DateTime<Utc>) -> bool {
    token.expires_at <= now + Duration::seconds(REFRESH_WINDOW_SECONDS)
}

/// Writes the token store atomically: a fresh sibling temp file is written,
/// synced, and renamed over the destination. Truncating the real file in place
/// would leave an empty or partial store if the process died mid-write, and a
/// lost refresh token means the box needs a browser to come back.
async fn persist_token(path: &Path, token: &StoredToken) -> Result<(), AppError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            AppError::Spotify(format!("failed to create token store directory: {error}"))
        })?;
    }

    let bytes = serde_json::to_vec_pretty(token).map_err(|error| {
        AppError::Spotify(format!("failed to serialize Spotify token: {error}"))
    })?;

    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(".tmp");
    let temp_path = path.with_file_name(temp_name);

    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(&temp_path).await.map_err(|error| {
        AppError::Spotify(format!("failed to open token store temporary: {error}"))
    })?;

    #[cfg(unix)]
    {
        // mode() only applies when the file is created, so an inherited temp
        // file keeps its old permissions without this.
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|error| {
                AppError::Spotify(format!("failed to secure token store permissions: {error}"))
            })?;
    }

    file.write_all(&bytes)
        .await
        .map_err(|error| AppError::Spotify(format!("failed to write token store: {error}")))?;
    file.sync_all()
        .await
        .map_err(|error| AppError::Spotify(format!("failed to sync token store: {error}")))?;
    drop(file);

    tokio::fs::rename(&temp_path, path)
        .await
        .map_err(|error| AppError::Spotify(format!("failed to commit token store: {error}")))?;

    Ok(())
}

fn spotify_request_error(error: reqwest::Error) -> AppError {
    AppError::Spotify(format!("Spotify token request failed: {error}"))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        collections::VecDeque,
        os::unix::fs::PermissionsExt,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use axum::{
        Json, Router,
        extract::Form,
        http::{StatusCode, header},
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use serde_json::json;

    use super::*;

    fn test_config(path: PathBuf) -> SpotifyConfig {
        SpotifyConfig {
            client_id: "client-id".to_string(),
            client_secret: "client-secret".to_string(),
            redirect_uri: "http://localhost/callback".to_string(),
            token_store_path: path,
        }
    }

    #[tokio::test]
    async fn rejects_unknown_or_consumed_state() {
        let client = SpotifyClient::new(test_config(PathBuf::from("unused")));
        assert!(!client.consume_state("unknown").await);

        let url = client.authorization_url().await.expect("authorization URL");
        let parsed = Url::parse(&url).expect("valid URL");
        let state = parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
            .expect("state query parameter");

        assert!(client.consume_state(&state).await);
        assert!(!client.consume_state(&state).await);
    }

    #[test]
    fn refresh_decision_uses_sixty_second_window() {
        let now = Utc::now();
        let mut token = StoredToken {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: now + Duration::seconds(61),
        };
        assert!(!token_needs_refresh(&token, now));

        token.expires_at = now + Duration::seconds(60);
        assert!(token_needs_refresh(&token, now));
    }

    #[tokio::test]
    async fn token_store_round_trip_uses_private_permissions() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("tokens/spotify.json");
        let token = StoredToken {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        };

        persist_token(&path, &token).await.expect("persist token");
        let reloaded: StoredToken =
            serde_json::from_slice(&tokio::fs::read(&path).await.expect("read token"))
                .expect("parse token");

        assert_eq!(reloaded, token);
        assert_eq!(
            std::fs::metadata(path)
                .expect("token metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn pending_states_are_capped_and_expired_states_rejected() {
        let client = SpotifyClient::new(test_config(PathBuf::from("unused")));

        // An abandoned flow must not pin memory: issuing more than the cap
        // leaves the set at the ceiling rather than growing forever.
        for _ in 0..(MAX_PENDING_STATES * 2) {
            client.authorization_url().await.expect("authorization URL");
        }
        assert_eq!(client.pending_states.lock().await.len(), MAX_PENDING_STATES);

        // A state older than the TTL is refused even though it was issued here.
        let stale = "stale-state".to_string();
        client.pending_states.lock().await.insert(
            stale.clone(),
            Utc::now() - Duration::seconds(STATE_TTL_SECONDS + 1),
        );
        assert!(!client.consume_state(&stale).await);
    }

    #[tokio::test]
    async fn persist_token_replaces_atomically_and_leaves_no_temp_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("spotify.json");

        // A store left world-readable by an earlier run must end up private,
        // and the rename must not leave its temp file behind.
        tokio::fs::write(&path, b"{}").await.expect("seed store");
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .await
            .expect("loosen permissions");

        let token = StoredToken {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        };
        persist_token(&path, &token).await.expect("persist token");

        let reloaded: StoredToken =
            serde_json::from_slice(&tokio::fs::read(&path).await.expect("read token"))
                .expect("parse token");
        assert_eq!(reloaded, token);
        assert_eq!(
            std::fs::metadata(&path)
                .expect("token metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(!directory.path().join("spotify.json.tmp").exists());
    }

    #[tokio::test]
    async fn authorization_url_contains_required_parameters() {
        let client = SpotifyClient::new(test_config(PathBuf::from("unused")));
        let url = Url::parse(&client.authorization_url().await.expect("authorization URL"))
            .expect("valid authorization URL");
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();

        assert_eq!(
            query.get("client_id").map(String::as_str),
            Some("client-id")
        );
        assert_eq!(
            query.get("redirect_uri").map(String::as_str),
            Some("http://localhost/callback")
        );
        assert_eq!(query.get("scope").map(String::as_str), Some(SPOTIFY_SCOPES));
        let state = query.get("state").expect("state query parameter");
        let decoded = general_purpose::URL_SAFE_NO_PAD
            .decode(state)
            .expect("base64url state");
        assert!(decoded.len() >= 16);
    }

    #[tokio::test]
    async fn callback_exchange_persists_and_refreshes_token() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_grants = Arc::new(Mutex::new(Vec::new()));
        let token_url = spawn_token_server(calls.clone(), observed_grants.clone()).await;
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("spotify.json");
        let config = test_config(path.clone());
        let client = SpotifyClient::with_test_endpoints(
            config.clone(),
            "https://accounts.spotify.com/authorize".to_string(),
            token_url.clone(),
        );
        let authorization_url = client.authorization_url().await.expect("authorization URL");
        let state = Url::parse(&authorization_url)
            .expect("valid authorization URL")
            .query_pairs()
            .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
            .expect("state query parameter");

        client
            .exchange_authorization_code("authorization-code", &state)
            .await
            .expect("exchange authorization code");
        assert!(path.exists());

        let restarted = SpotifyClient::with_test_endpoints(
            config,
            "https://accounts.spotify.com/authorize".to_string(),
            token_url,
        );
        assert!(
            restarted
                .initialize_from_store()
                .await
                .expect("load and refresh stored token")
        );
        assert_eq!(
            restarted.access_token().await.expect("access token"),
            "refreshed-access"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            *observed_grants.lock().await,
            vec!["authorization_code", "refresh_token"]
        );
    }

    #[tokio::test]
    async fn currently_playing_maps_track_and_idle_responses() {
        let playback_url = spawn_playback_server(vec![
            (
                StatusCode::OK,
                Json(json!({
                    "is_playing": true,
                    "progress_ms": 12345,
                    "item": {
                        "id": "track-id",
                        "name": "Track Name",
                        "duration_ms": 234567,
                        "artists": [{"name": "First"}, {"name": "Second"}],
                        "album": {
                            "name": "Album Name",
                            "images": [{"url": "http://art.test/cover.png"}]
                        }
                    }
                })),
            )
                .into_response(),
            StatusCode::NO_CONTENT.into_response(),
        ])
        .await;
        let client =
            SpotifyClient::with_test_api_url(test_config(PathBuf::from("unused")), playback_url);
        client.authorize_for_test().await;

        let playing = client.currently_playing().await.expect("playing response");
        assert_eq!(playing.track_id.as_deref(), Some("track-id"));
        assert_eq!(playing.artist.as_deref(), Some("First, Second"));
        assert_eq!(playing.album.as_deref(), Some("Album Name"));
        assert_eq!(
            playing.art_url.as_deref(),
            Some("http://art.test/cover.png")
        );
        assert!(playing.is_playing);
        assert_eq!(playing.progress_ms, 12_345);
        assert_eq!(playing.duration_ms, 234_567);

        let idle = client.currently_playing().await.expect("idle response");
        assert_eq!(idle, PlaybackObservation::idle(idle.observed_at));
    }

    #[tokio::test]
    async fn currently_playing_classifies_rate_limits_and_server_errors() {
        let playback_url = spawn_playback_server(vec![
            (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "7")]).into_response(),
            StatusCode::SERVICE_UNAVAILABLE.into_response(),
            StatusCode::UNAUTHORIZED.into_response(),
        ])
        .await;
        let client =
            SpotifyClient::with_test_api_url(test_config(PathBuf::from("unused")), playback_url);
        client.authorize_for_test().await;

        assert!(matches!(
            client.currently_playing().await,
            Err(PlaybackFetchError::RateLimited(delay)) if delay == StdDuration::from_secs(7)
        ));
        assert!(matches!(
            client.currently_playing().await,
            Err(PlaybackFetchError::Transient(_))
        ));
        assert!(matches!(
            client.currently_playing().await,
            Err(PlaybackFetchError::Spotify(AppError::Spotify(_)))
        ));
    }

    #[tokio::test]
    async fn retry_after_is_floored_and_capped() {
        let playback_url = spawn_playback_server(vec![
            (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "1")]).into_response(),
            (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, "999999999")],
            )
                .into_response(),
            (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, "not-a-number")],
            )
                .into_response(),
        ])
        .await;
        let client =
            SpotifyClient::with_test_api_url(test_config(PathBuf::from("unused")), playback_url);
        client.authorize_for_test().await;

        // Floored to 5s, capped at the ceiling so a garbled header cannot park
        // polling forever, and a non-numeric header falls back to the default.
        for expected in [
            StdDuration::from_secs(5),
            MAX_RATE_LIMIT_BACKOFF,
            StdDuration::from_secs(5),
        ] {
            assert!(matches!(
                client.currently_playing().await,
                Err(PlaybackFetchError::RateLimited(delay)) if delay == expected
            ));
        }
    }

    async fn spawn_playback_server(responses: Vec<Response>) -> String {
        let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
        let app = Router::new().route(
            "/v1/me/player/currently-playing",
            get(move || {
                let responses = responses.clone();
                async move {
                    responses
                        .lock()
                        .await
                        .pop_front()
                        .unwrap_or_else(|| StatusCode::NO_CONTENT.into_response())
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind playback server");
        let address = listener.local_addr().expect("playback server address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("playback server");
        });
        format!("http://{address}/v1")
    }

    async fn spawn_token_server(
        calls: Arc<AtomicUsize>,
        observed_grants: Arc<Mutex<Vec<String>>>,
    ) -> String {
        let app = Router::new().route(
            "/token",
            post(move |Form(form): Form<HashMap<String, String>>| {
                let calls = calls.clone();
                let observed_grants = observed_grants.clone();
                async move {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    observed_grants
                        .lock()
                        .await
                        .push(form.get("grant_type").cloned().unwrap_or_default());
                    if call == 0 {
                        Json(json!({
                            "access_token": "initial-access",
                            "refresh_token": "persisted-refresh",
                            "expires_in": 30
                        }))
                    } else {
                        Json(json!({
                            "access_token": "refreshed-access",
                            "expires_in": 3600
                        }))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock token server");
        let address = listener.local_addr().expect("mock server address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock token server");
        });
        format!("http://{address}/token")
    }
}
