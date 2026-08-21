use anyhow::Context;
use muse_box::{
    config::Config,
    routes,
    spotify::{SpotifyClient, SpotifyConfig},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Config::from_env()?;
    let bind_addr = config.bind_addr();
    let spotify = SpotifyClient::new(SpotifyConfig {
        client_id: config.spotify_client_id,
        client_secret: config.spotify_client_secret,
        redirect_uri: config.spotify_redirect_uri,
        token_store_path: config.spotify_token_store_path,
    });

    if spotify.initialize_from_store().await? {
        tracing::info!("refreshed persisted Spotify authorization");
    }

    let app = routes::router(spotify, config.device_api_token);
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind server to {bind_addr}"))?;
    tracing::info!(address = %bind_addr, "muse-box listening");
    axum::serve(listener, app).await.context("server failed")?;
    Ok(())
}
