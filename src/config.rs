use anyhow::{Context, Result};
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
                .unwrap_or_else(|_| "gpt-realtime-mini".to_string()),
            device_api_token: std::env::var("DEVICE_API_TOKEN")
                .unwrap_or_else(|_| "dev-token-change-me".to_string()),
        })
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
