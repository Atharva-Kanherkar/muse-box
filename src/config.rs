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
    /// Where the single-tenant install used to keep its one Spotify token.
    /// Read only once, at boot, to migrate that account into
    /// `accounts_root`; every account's own client uses a path under
    /// `accounts_root` instead.
    pub legacy_token_store_path: PathBuf,
    pub openai_api_key: String,
    pub openai_realtime_model: String,
    /// Model that turns a transcript into one tool call. Stateless chat
    /// completions, so this is the reliable path; Realtime is only for audio.
    pub openai_intent_model: String,
    /// Reasoning effort for the intent model. Empty disables the field.
    pub openai_reasoning_effort: String,
    /// Text-to-speech model and voice for Muse's replies.
    pub openai_speech_model: String,
    pub openai_speech_voice: String,
    pub device_api_token: String,
    /// Offset applied to the idle clock's displayed digits. Scheduling stays on
    /// UTC minute boundaries; only the rendered `HH:MM` is localized, which
    /// keeps idle goldens deterministic.
    pub idle_display_offset: FixedOffset,
    /// Browser origins allowed to call the API. Empty means any origin, which
    /// is safe here because auth is a bearer token or session cookie rather
    /// than an ambient credential another site could ride on.
    pub cors_allowed_origins: Vec<String>,
    /// Where the single-tenant install used to keep its one taste index. Read
    /// only once, at boot, alongside `legacy_token_store_path`.
    pub legacy_taste_index_path: PathBuf,
    /// Directory holding one subdirectory per account: that account's own
    /// Spotify token and taste index. The only per-account state on disk.
    pub accounts_root: PathBuf,
    /// Which account the hardware bearer token resolves to. Whoever completes
    /// OAuth first — or whoever the legacy install migrates in — keeps this
    /// permanently.
    pub owner_marker_path: PathBuf,
    /// Where browser sessions are persisted, so a redeploy does not sign anyone
    /// out. Shared across every account; a cookie's value now carries which
    /// account signed it in.
    pub session_store_path: PathBuf,
    /// Directory of cached lyrics, one file per track, shared across every
    /// account: the first person to play a song looks it up for everyone.
    pub lyrics_cache_dir: PathBuf,
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
            legacy_token_store_path: std::env::var("TOKEN_STORE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data/spotify_token.json")),
            openai_api_key: std::env::var("OPENAI_API_KEY")
                .context("OPENAI_API_KEY is required")?,
            openai_realtime_model: std::env::var("OPENAI_REALTIME_MODEL")
                .unwrap_or_else(|_| "gpt-realtime-2.1".to_string()),
            openai_intent_model: std::env::var("OPENAI_INTENT_MODEL")
                .unwrap_or_else(|_| "gpt-5.6-luna".to_string()),
            openai_reasoning_effort: std::env::var("OPENAI_REASONING_EFFORT")
                .unwrap_or_else(|_| "low".to_string()),
            openai_speech_model: std::env::var("OPENAI_SPEECH_MODEL")
                .unwrap_or_else(|_| "gpt-4o-mini-tts".to_string()),
            openai_speech_voice: std::env::var("OPENAI_SPEECH_VOICE")
                .unwrap_or_else(|_| "cedar".to_string()),
            device_api_token: std::env::var("DEVICE_API_TOKEN")
                .unwrap_or_else(|_| "dev-token-change-me".to_string()),
            idle_display_offset: idle_display_offset()?,
            legacy_taste_index_path: std::env::var("TASTE_INDEX_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data/taste_index.json")),
            accounts_root: std::env::var("ACCOUNTS_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data/accounts")),
            owner_marker_path: std::env::var("OWNER_MARKER_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data/owner_account_id.txt")),
            session_store_path: std::env::var("SESSION_STORE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data/sessions.json")),
            lyrics_cache_dir: std::env::var("LYRICS_CACHE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("./data/lyrics")),
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
