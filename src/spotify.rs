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
/// Playback control plus everything needed to learn the listener's taste.
/// Changing this requires re-authorizing at `/auth/spotify`: a stored refresh
/// token only carries the scopes it was granted.
const SPOTIFY_SCOPES: &str = "user-read-playback-state user-modify-playback-state \
user-top-read user-read-recently-played user-library-read \
playlist-read-private playlist-read-collaborative";
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
    id: Option<String>,
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

/// One track from the listener's own library, with where it was found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryTrack {
    pub id: String,
    pub name: String,
    pub artists: String,
    pub album: String,
    /// Playlist names, "saved", "top tracks", "recently played".
    pub sources: Vec<String>,
}

impl LibraryTrack {
    /// The text that gets embedded. Artists first: taste requests name artists
    /// far more often than albums.
    pub fn embedding_text(&self) -> String {
        let sources = self.sources.join(", ");
        format!(
            "{} by {} — album {} — appears in: {}",
            self.name, self.artists, self.album, sources
        )
    }
}

#[derive(Debug, Deserialize)]
struct PagedTracks {
    items: Vec<PagedTrackItem>,
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PagedTrackItem {
    track: Option<SpotifyTrack>,
}

#[derive(Debug, Deserialize)]
struct TopTracks {
    items: Vec<SpotifyTrack>,
}

#[derive(Debug, Deserialize)]
struct PlaylistPage {
    items: Vec<PlaylistSummary>,
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlaylistSummary {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct DevicesResponse {
    devices: Vec<SpotifyDevice>,
}

#[derive(Debug, Deserialize)]
struct SpotifyDevice {
    /// Null for devices Spotify will not let you address, so this cannot be a
    /// bare String: one such device in the list used to fail the whole parse and
    /// take the retry down with it.
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    is_active: bool,
    /// A restricted device refuses Web API commands, so naming it still fails.
    #[serde(default)]
    is_restricted: bool,
}

impl SpotifyDevice {
    fn usable(&self) -> Option<&str> {
        if self.is_restricted {
            return None;
        }
        self.id.as_deref()
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    tracks: SearchTracks,
}

#[derive(Debug, Deserialize)]
struct SearchTracks {
    items: Vec<SearchTrack>,
}

#[derive(Debug, Deserialize)]
struct SearchTrack {
    id: String,
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
            track_id: track.id,
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

    pub async fn resume_playback(&self) -> Result<(), AppError> {
        self.send_player_request(reqwest::Method::PUT, "me/player/play", &[])
            .await
    }

    pub async fn pause_playback(&self) -> Result<(), AppError> {
        self.send_player_request(reqwest::Method::PUT, "me/player/pause", &[])
            .await
    }

    pub async fn skip_next(&self) -> Result<(), AppError> {
        self.send_player_request(reqwest::Method::POST, "me/player/next", &[])
            .await
    }

    pub async fn skip_previous(&self) -> Result<(), AppError> {
        self.send_player_request(reqwest::Method::POST, "me/player/previous", &[])
            .await
    }

    pub async fn set_volume(&self, percent: u8) -> Result<(), AppError> {
        self.send_player_request(
            reqwest::Method::PUT,
            "me/player/volume",
            &[("volume_percent", percent.to_string())],
        )
        .await
    }

    pub async fn search_top_track(&self, query: &str) -> Result<String, AppError> {
        let access_token = self.access_token().await?;
        let url = format!(
            "{}/search",
            self.endpoints.api_base_url.trim_end_matches('/')
        );
        let response = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .query(&[("q", query), ("type", "track"), ("limit", "1")])
            .send()
            .await
            .map_err(spotify_api_error)?
            .error_for_status()
            .map_err(spotify_api_error)?
            .json::<SearchResponse>()
            .await
            .map_err(spotify_api_error)?;
        response
            .tracks
            .items
            .into_iter()
            .next()
            .map(|track| track.id)
            .ok_or_else(|| AppError::Spotify(format!("no Spotify track matched: {query}")))
    }

    /// Fetch the listener's library: what they saved, what they play most, what
    /// they played lately, and everything in their playlists.
    ///
    /// Deduplicated by track id, so a song in three playlists becomes one entry
    /// listing all three, which is exactly the signal a taste query wants.
    pub async fn library_tracks(&self, max_tracks: usize) -> Result<Vec<LibraryTrack>, AppError> {
        let mut collected: HashMap<String, LibraryTrack> = HashMap::new();

        for term in ["short_term", "medium_term", "long_term"] {
            let top: TopTracks = self
                .api_get(&format!("me/top/tracks?limit=50&time_range={term}"))
                .await?;
            for track in top.items {
                merge_track(&mut collected, track, "top tracks");
            }
        }

        let recent: PagedTracks = self.api_get("me/player/recently-played?limit=50").await?;
        for item in recent.items {
            if let Some(track) = item.track {
                merge_track(&mut collected, track, "recently played");
            }
        }

        let mut next = Some("me/tracks?limit=50".to_string());
        while let Some(path) = next.take() {
            if collected.len() >= max_tracks {
                break;
            }
            let page: PagedTracks = self.api_get(&path).await?;
            for item in page.items {
                if let Some(track) = item.track {
                    merge_track(&mut collected, track, "saved");
                }
            }
            next = page.next.and_then(absolute_to_path);
        }

        let mut next = Some("me/playlists?limit=50".to_string());
        while let Some(path) = next.take() {
            let page: PlaylistPage = self.api_get(&path).await?;
            for playlist in page.items {
                if collected.len() >= max_tracks {
                    break;
                }
                let mut tracks = Some(format!("playlists/{}/tracks?limit=100", playlist.id));
                while let Some(track_path) = tracks.take() {
                    if collected.len() >= max_tracks {
                        break;
                    }
                    let page: PagedTracks = self.api_get(&track_path).await?;
                    for item in page.items {
                        if let Some(track) = item.track {
                            merge_track(&mut collected, track, &playlist.name);
                        }
                    }
                    tracks = page.next.and_then(absolute_to_path);
                }
            }
            next = page.next.and_then(absolute_to_path);
        }

        let mut tracks: Vec<_> = collected.into_values().collect();
        // Stable order so a rebuilt index is comparable to the last one.
        tracks.sort_by(|left, right| left.id.cmp(&right.id));
        tracks.truncate(max_tracks);
        Ok(tracks)
    }

    async fn api_get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, AppError> {
        let access_token = self.access_token().await?;
        let url = format!(
            "{}/{}",
            self.endpoints.api_base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        self.http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(spotify_api_error)?
            .error_for_status()
            .map_err(spotify_api_error)?
            .json::<T>()
            .await
            .map_err(spotify_api_error)
    }

    /// Tempo and energy for a track, or `None`.
    ///
    /// Spotify deprecated audio-features for API apps created after late 2024,
    /// so a 403 here is an expected outcome, not an error: ambience then falls
    /// back to a slow default drift instead of a beat-paced pulse.
    pub async fn audio_features(&self, track_id: &str) -> Option<(f32, f32)> {
        #[derive(Deserialize)]
        struct Features {
            tempo: Option<f32>,
            energy: Option<f32>,
        }

        let access_token = self.access_token().await.ok()?;
        let url = format!(
            "{}/audio-features/{}",
            self.endpoints.api_base_url.trim_end_matches('/'),
            track_id
        );
        let response = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            tracing::debug!(status = %response.status(), "audio features unavailable");
            return None;
        }
        let features: Features = response.json().await.ok()?;
        let tempo = features.tempo.filter(|tempo| *tempo > 0.0)?;
        Some((tempo, features.energy.unwrap_or(0.5).clamp(0.0, 1.0)))
    }

    pub async fn play_track(&self, track_id: &str) -> Result<(), AppError> {
        self.send_player_command(
            reqwest::Method::PUT,
            "me/player/play",
            &[],
            Some(serde_json::json!({
                "uris": [format!("spotify:track:{track_id}")]
            })),
        )
        .await
    }

    pub async fn queue_track(&self, track_id: &str) -> Result<(), AppError> {
        self.send_player_request(
            reqwest::Method::POST,
            "me/player/queue",
            &[("uri", format!("spotify:track:{track_id}"))],
        )
        .await
    }

    async fn send_player_request(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<(), AppError> {
        self.send_player_command(method, path, query, None).await
    }

    /// Every mutating player call goes through here so they all share the
    /// no-active-device retry below.
    async fn send_player_command(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<(), AppError> {
        let response = self
            .player_request(method.clone(), path, query, None, body.as_ref())
            .await?;
        if response.status() != reqwest::StatusCode::NOT_FOUND {
            return player_response_error(response);
        }

        // Spotify answers 404 NO_ACTIVE_DEVICE when no device currently holds
        // playback, which is the normal state for a box that has been idle.
        // Naming a device explicitly transfers playback to it, so the shelf can
        // start music without someone first opening Spotify by hand.
        let Some(device_id) = self.first_available_device().await? else {
            return Err(AppError::Spotify(
                "no Spotify device is available; open Spotify on a phone, desktop or speaker once so it can be targeted".to_string(),
            ));
        };
        let response = self
            .player_request(method, path, query, Some(&device_id), body.as_ref())
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            // Named a device and Spotify still says no: it went away between
            // listing and using it, which happens when an app is backgrounded.
            return Err(AppError::Spotify(
                "the Spotify device stopped responding; open Spotify again and retry".to_string(),
            ));
        }
        player_response_error(response)
    }

    async fn player_request(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        device_id: Option<&str>,
        body: Option<&serde_json::Value>,
    ) -> Result<reqwest::Response, AppError> {
        let access_token = self.access_token().await?;
        let url = format!(
            "{}/{}",
            self.endpoints.api_base_url.trim_end_matches('/'),
            path
        );
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(access_token)
            .query(query);
        if let Some(device_id) = device_id {
            request = request.query(&[("device_id", device_id)]);
        }
        request = match body {
            Some(body) => request.json(body),
            // Spotify answers 411 Length Required for a body-less PUT/POST.
            // reqwest omits Content-Length when there is no body, and an empty
            // Vec body is not enough either, so set the header outright.
            None => request.header(reqwest::header::CONTENT_LENGTH, "0"),
        };
        request.send().await.map_err(spotify_api_error)
    }

    /// Pick a device to target, preferring one Spotify already calls active.
    async fn first_available_device(&self) -> Result<Option<String>, AppError> {
        let access_token = self.access_token().await?;
        let url = format!(
            "{}/me/player/devices",
            self.endpoints.api_base_url.trim_end_matches('/')
        );
        let devices = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(spotify_api_error)?
            .error_for_status()
            .map_err(spotify_api_error)?
            .json::<DevicesResponse>()
            .await
            .map_err(spotify_api_error)?
            .devices;
        // Prefer whatever Spotify already considers active; otherwise any device
        // that will actually accept a command.
        let chosen = devices
            .iter()
            .find(|device| device.is_active && device.usable().is_some())
            .or_else(|| devices.iter().find(|device| device.usable().is_some()));

        match chosen {
            Some(device) => {
                tracing::debug!(
                    device = device.name.as_deref().unwrap_or("unnamed"),
                    active = device.is_active,
                    "targeting a Spotify device"
                );
                Ok(device.usable().map(str::to_string))
            }
            None => {
                tracing::info!(
                    listed = devices.len(),
                    "Spotify listed no device that accepts commands"
                );
                Ok(None)
            }
        }
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

/// Turn a player response into a typed error, keeping Spotify's own reason.
fn player_response_error(response: reqwest::Response) -> Result<(), AppError> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err(AppError::Spotify(
            "Spotify refused the command; playback control requires a Premium account".to_string(),
        ));
    }
    Err(AppError::Spotify(format!(
        "Spotify Web API request failed with {status}"
    )))
}

/// Add a track to the set, or note one more place an existing one appears.
fn merge_track(collected: &mut HashMap<String, LibraryTrack>, track: SpotifyTrack, source: &str) {
    // Local files have no id, so they can never be played back by id.
    let Some(id) = track.id else {
        return;
    };
    let artists = track
        .artists
        .into_iter()
        .map(|artist| artist.name)
        .collect::<Vec<_>>()
        .join(", ");
    let entry = collected.entry(id.clone()).or_insert_with(|| LibraryTrack {
        id,
        name: track.name,
        artists,
        album: track.album.name,
        sources: Vec::new(),
    });
    if !entry.sources.iter().any(|existing| existing == source) {
        entry.sources.push(source.to_string());
    }
}

/// Spotify pages with absolute `next` URLs; keep only the part after `/v1/` so
/// the configured base URL still applies (which is what the tests point at).
fn absolute_to_path(next: String) -> Option<String> {
    next.split_once("/v1/")
        .map(|(_, path)| path.to_string())
        .or(Some(next))
}

fn spotify_api_error(error: reqwest::Error) -> AppError {
    AppError::Spotify(format!("Spotify Web API request failed: {error}"))
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
        body::to_bytes,
        extract::{Form, Request},
        http::{StatusCode, header},
        response::{IntoResponse, Response},
        routing::{any, get, post},
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
    async fn playback_controls_match_spotify_web_api_contract() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let app = Router::new().fallback(any(move |request: Request| {
            let observed = observed.clone();
            async move {
                let method = request.method().clone();
                let uri = request.uri().to_string();
                let authorization = request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let body = to_bytes(request.into_body(), 1024)
                    .await
                    .expect("request body");
                observed
                    .lock()
                    .await
                    .push((method, uri.clone(), authorization, body.to_vec()));
                if uri.starts_with("/v1/search?") {
                    Json(json!({ "tracks": { "items": [{ "id": "top-track" }] } })).into_response()
                } else {
                    StatusCode::NO_CONTENT.into_response()
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind Spotify mock");
        let address = listener.local_addr().expect("Spotify mock address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("Spotify mock");
        });
        let client = SpotifyClient::with_test_api_url(
            test_config(PathBuf::from("unused")),
            format!("http://{address}/v1"),
        );
        client.authorize_for_test().await;

        client.resume_playback().await.expect("resume");
        client.pause_playback().await.expect("pause");
        client.skip_next().await.expect("next");
        client.skip_previous().await.expect("previous");
        client.set_volume(73).await.expect("volume");
        let track_id = client.search_top_track("Teardrop").await.expect("search");
        assert_eq!(track_id, "top-track");
        client.play_track(&track_id).await.expect("play track");
        client.queue_track(&track_id).await.expect("queue track");

        let requests = requests.lock().await;
        assert_eq!(requests.len(), 8);
        assert_eq!(requests[0].0, reqwest::Method::PUT);
        assert_eq!(requests[0].1, "/v1/me/player/play");
        assert_eq!(requests[1].1, "/v1/me/player/pause");
        assert_eq!(requests[2].0, reqwest::Method::POST);
        assert_eq!(requests[2].1, "/v1/me/player/next");
        assert_eq!(requests[3].1, "/v1/me/player/previous");
        assert_eq!(requests[4].1, "/v1/me/player/volume?volume_percent=73");
        assert!(requests[5].1.contains("q=Teardrop"));
        assert!(requests[5].1.contains("type=track"));
        assert!(requests[5].1.contains("limit=1"));
        assert_eq!(requests[6].1, "/v1/me/player/play");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&requests[6].3).expect("play body"),
            json!({ "uris": ["spotify:track:top-track"] })
        );
        assert_eq!(
            requests[7].1,
            "/v1/me/player/queue?uri=spotify%3Atrack%3Atop-track"
        );
        assert!(
            requests
                .iter()
                .all(|request| request.2 == "Bearer test-access-token")
        );
    }

    #[tokio::test]
    async fn audio_features_degrade_to_none_when_spotify_refuses() {
        // The endpoint is deprecated for newer API apps, so 403 is a normal
        // answer and must cost nothing, not an error.
        let app = axum::Router::new().fallback(axum::routing::any(
            move |request: axum::extract::Request| async move {
                let uri = request.uri().to_string();
                if uri.contains("/audio-features/described") {
                    return axum::Json(serde_json::json!({
                        "tempo": 128.0, "energy": 0.82
                    }))
                    .into_response();
                }
                StatusCode::FORBIDDEN.into_response()
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = SpotifyClient::with_test_api_url(
            test_config(PathBuf::from("unused")),
            format!("http://{address}/v1"),
        );
        client.authorize_for_test().await;

        assert_eq!(
            client.audio_features("described").await,
            Some((128.0, 0.82))
        );
        assert_eq!(client.audio_features("refused").await, None);
    }

    #[tokio::test]
    async fn device_choice_skips_null_ids_and_restricted_devices() {
        // A real device list mixes these in, and either one used to break the
        // retry: a null id failed the whole parse, a restricted device accepted
        // being named and then refused the command.
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let observed = seen.clone();
        let app = axum::Router::new().fallback(axum::routing::any(
            move |request: axum::extract::Request| {
                let observed = observed.clone();
                async move {
                    let uri = request.uri().to_string();
                    observed.lock().await.push(uri.clone());
                    if uri.starts_with("/v1/me/player/devices") {
                        return axum::Json(serde_json::json!({
                            "devices": [
                                { "id": null, "name": "Unaddressable", "is_active": true },
                                { "id": "cast", "name": "Cast", "is_active": true,
                                  "is_restricted": true },
                                { "id": "phone", "name": "Phone", "is_active": false }
                            ]
                        }))
                        .into_response();
                    }
                    if uri.contains("device_id=phone") {
                        return StatusCode::NO_CONTENT.into_response();
                    }
                    StatusCode::NOT_FOUND.into_response()
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = SpotifyClient::with_test_api_url(
            test_config(PathBuf::from("unused")),
            format!("http://{address}/v1"),
        );
        client.authorize_for_test().await;
        client.resume_playback().await.expect("the phone is usable");

        let seen = seen.lock().await;
        assert!(
            seen.last()
                .is_some_and(|uri| uri.contains("device_id=phone")),
            "{seen:?}"
        );
    }

    #[tokio::test]
    async fn a_device_that_vanishes_mid_command_says_so() {
        // Listed, then gone by the time it is used: the app was backgrounded.
        let app = axum::Router::new().fallback(axum::routing::any(
            move |request: axum::extract::Request| async move {
                if request
                    .uri()
                    .to_string()
                    .starts_with("/v1/me/player/devices")
                {
                    return axum::Json(serde_json::json!({
                        "devices": [{ "id": "gone", "name": "Phone", "is_active": true }]
                    }))
                    .into_response();
                }
                StatusCode::NOT_FOUND.into_response()
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = SpotifyClient::with_test_api_url(
            test_config(PathBuf::from("unused")),
            format!("http://{address}/v1"),
        );
        client.authorize_for_test().await;
        let message = client
            .resume_playback()
            .await
            .expect_err("still 404")
            .to_string();
        assert!(
            message.contains("stopped responding"),
            "a bare 404 is not actionable: {message}"
        );
    }

    #[tokio::test]
    async fn player_command_retries_with_a_device_when_none_is_active() {
        // Spotify answers 404 NO_ACTIVE_DEVICE for an idle account, which is the
        // normal state for a shelf box. The client must find a device and retry.
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let observed = seen.clone();
        let app = axum::Router::new().fallback(axum::routing::any(
            move |request: axum::extract::Request| {
                let observed = observed.clone();
                async move {
                    let uri = request.uri().to_string();
                    observed.lock().await.push(uri.clone());
                    if uri.starts_with("/v1/me/player/devices") {
                        return axum::Json(serde_json::json!({
                            "devices": [
                                { "id": "inactive-speaker", "is_active": false },
                                { "id": "active-phone", "is_active": true }
                            ]
                        }))
                        .into_response();
                    }
                    if uri.contains("device_id=active-phone") {
                        return StatusCode::NO_CONTENT.into_response();
                    }
                    StatusCode::NOT_FOUND.into_response()
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = SpotifyClient::with_test_api_url(
            test_config(PathBuf::from("unused")),
            format!("http://{address}/v1"),
        );
        client.authorize_for_test().await;
        client.resume_playback().await.expect("retry must succeed");

        let seen = seen.lock().await;
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert!(seen[0].starts_with("/v1/me/player/play"));
        assert!(seen[1].starts_with("/v1/me/player/devices"));
        // The active device wins over the one merely listed first.
        assert!(seen[2].contains("device_id=active-phone"), "{:?}", seen[2]);
    }

    #[tokio::test]
    async fn player_command_reports_clearly_when_no_device_exists() {
        // Signed in, but nothing to play on.
        let app = axum::Router::new().fallback(axum::routing::any(
            move |request: axum::extract::Request| async move {
                if request
                    .uri()
                    .to_string()
                    .starts_with("/v1/me/player/devices")
                {
                    return axum::Json(serde_json::json!({ "devices": [] })).into_response();
                }
                StatusCode::NOT_FOUND.into_response()
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = SpotifyClient::with_test_api_url(
            test_config(PathBuf::from("unused")),
            format!("http://{address}/v1"),
        );
        client.authorize_for_test().await;
        let error = client
            .pause_playback()
            .await
            .expect_err("no device means no playback");
        let message = error.to_string();
        assert!(
            message.contains("no Spotify device is available"),
            "the caller needs the reason, not a bare 404: {message}"
        );
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
