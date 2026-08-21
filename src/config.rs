use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub spotify_client_id: String,
    pub spotify_client_secret: String,
    pub spotify_redirect_uri: String,
    pub openai_api_key: String,
    pub openai_model: String,
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
            openai_api_key: std::env::var("OPENAI_API_KEY")
                .context("OPENAI_API_KEY is required")?,
            openai_model: std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string()),
            device_api_token: std::env::var("DEVICE_API_TOKEN")
                .unwrap_or_else(|_| "dev-token-change-me".to_string()),
        })
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
