use anyhow::{Context, Result, anyhow};
use chrono::{FixedOffset, Offset, Utc};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub spotify_client_id: String,
    pub spotify_client_secret: String,
    pub spotify_redirect_uri: String,
    pub spotify_token_store_path: PathBuf,
    pub openai_api_key: String,
    pub openai_realtime_model: String,
    pub device_api_token: String,
    /// Offset applied to the idle clock's displayed digits. Scheduling stays on
    /// UTC minute boundaries; only the rendered `HH:MM` is localized, which
    /// keeps idle goldens deterministic.
    pub idle_display_offset: FixedOffset,
    /// Browser origins allowed to call the API. Empty means any origin, which
    /// is safe here because auth is a bearer token rather than a cookie, so
    /// there is no ambient credential for another site to ride on.
    pub cors_allowed_origins: Vec<String>,
    /// Where the embedded music-taste index lives. Belongs on the same volume
    /// as the token store so a redeploy does not re-embed the whole library.
    pub taste_index_path: PathBuf,
    /// Where browser sessions are persisted, so a redeploy does not sign anyone
    /// out. Belongs on the same volume as the token store.
    pub session_store_path: PathBuf,
    /// Cached lyrics, including known misses, so a track on repeat is looked up
    /// once. Belongs on the volume with everything else.
    pub lyrics_cache_path: PathBuf,
    /// Directory of the built web client, served from this same origin.
    pub client_root: PathBuf,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            host: std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse()
                .context("PORT must be a number")?,
            spotify_client_id: std::env::var("SPOTIFY_CLIENT_ID")
                .context("SPOTIFY_CLIENT_ID is required")?,
            spotify_client_secret: std::env::var("SPOTIFY_CLIENT_SECRET")
                .context("SPOTIFY_CLIENT_SECRET is required")?,
            spotify_redirect_uri: std::env::var("SPOTIFY_REDIRECT_URI")
                .unwrap_or_else(|_| "http://localhost:3000/auth/spotify/callback".to_string()),
            spotify_token_store_path: std::env::var("TOKEN_STORE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data/spotify_token.json")),
            openai_api_key: std::env::var("OPENAI_API_KEY")
                .context("OPENAI_API_KEY is required")?,
            openai_realtime_model: std::env::var("OPENAI_REALTIME_MODEL")
                .unwrap_or_else(|_| "gpt-realtime".to_string()),
            device_api_token: std::env::var("DEVICE_API_TOKEN")
                .unwrap_or_else(|_| "dev-token-change-me".to_string()),
            idle_display_offset: idle_display_offset()?,
            taste_index_path: std::env::var("TASTE_INDEX_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data/taste_index.json")),
            session_store_path: std::env::var("SESSION_STORE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data/sessions.json")),
            lyrics_cache_path: std::env::var("LYRICS_CACHE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data/lyrics.json")),
            client_root: std::env::var("CLIENT_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./web/dist")),
            cors_allowed_origins: std::env::var("CORS_ALLOWED_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .map(str::to_string)
                .collect(),
        })
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Parse `IDLE_UTC_OFFSET_MINUTES`, defaulting to UTC. Bounds match the real
/// range of civil offsets, so a typo fails at startup instead of quietly
/// rendering a clock hours off.
fn idle_display_offset() -> Result<FixedOffset> {
    let Ok(raw) = std::env::var("IDLE_UTC_OFFSET_MINUTES") else {
        return Ok(Utc.fix());
    };
    let minutes: i32 = raw
        .trim()
        .parse()
        .with_context(|| format!("IDLE_UTC_OFFSET_MINUTES must be a whole number: {raw}"))?;
    if !(-840..=840).contains(&minutes) {
        return Err(anyhow!(
            "IDLE_UTC_OFFSET_MINUTES must be between -840 and 840: {minutes}"
        ));
    }
    FixedOffset::east_opt(minutes * 60)
        .ok_or_else(|| anyhow!("IDLE_UTC_OFFSET_MINUTES is out of range: {minutes}"))
}
