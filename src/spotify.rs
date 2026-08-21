use std::{collections::HashSet, path::PathBuf, sync::Arc};

use base64::{Engine as _, engine::general_purpose};
use chrono::{DateTime, Duration, Utc};
use rand::{RngCore, rngs::OsRng};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use crate::error::AppError;

const SPOTIFY_AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const SPOTIFY_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const SPOTIFY_SCOPES: &str = "user-read-playback-state user-modify-playback-state";
const REFRESH_WINDOW_SECONDS: i64 = 60;

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
}

impl Default for SpotifyEndpoints {
    fn default() -> Self {
        Self {
            authorize_url: SPOTIFY_AUTHORIZE_URL.to_string(),
            token_url: SPOTIFY_TOKEN_URL.to_string(),
        }
    }
}

#[derive(Clone)]
pub struct SpotifyClient {
    http: Client,
    config: SpotifyConfig,
    endpoints: SpotifyEndpoints,
    pending_states: Arc<Mutex<HashSet<String>>>,
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

impl SpotifyClient {
    pub fn new(config: SpotifyConfig) -> Self {
        Self::with_endpoints(config, SpotifyEndpoints::default())
    }

    fn with_endpoints(config: SpotifyConfig, endpoints: SpotifyEndpoints) -> Self {
        Self {
            http: Client::new(),
            config,
            endpoints,
            pending_states: Arc::new(Mutex::new(HashSet::new())),
            token: Arc::new(RwLock::new(None)),
            refresh_guard: Arc::new(Mutex::new(())),
        }
    }

    pub async fn authorization_url(&self) -> Result<String, AppError> {
        let mut random = [0_u8; 32];
        OsRng.fill_bytes(&mut random);
        let state = general_purpose::URL_SAFE_NO_PAD.encode(random);

        self.pending_states.lock().await.insert(state.clone());

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

    async fn consume_state(&self, state: &str) -> bool {
        self.pending_states.lock().await.remove(state)
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

async fn persist_token(path: &PathBuf, token: &StoredToken) -> Result<(), AppError> {
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
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| AppError::Spotify(format!("failed to write token store: {error}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|error| {
                AppError::Spotify(format!("failed to secure token store permissions: {error}"))
            })?;
    }

    Ok(())
}

fn spotify_request_error(error: reqwest::Error) -> AppError {
    AppError::Spotify(format!("Spotify token request failed: {error}"))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

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
}
