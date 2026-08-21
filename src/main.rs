use std::sync::Arc;

use anyhow::Context;
use muse_box::{
    config::Config,
    routes,
    spotify::{SpotifyClient, SpotifyConfig},
    state::{StateHub, run_poll_loop},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    // from_default_env() would default to ERROR, silencing every info! line
    // when RUST_LOG is unset (the normal case on Railway, where there is no
    // .env for dotenvy to load).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    let config = Config::from_env()?;
    let bind_addr = config.bind_addr();
    let spotify = SpotifyClient::new(SpotifyConfig {
        client_id: config.spotify_client_id,
        client_secret: config.spotify_client_secret,
        redirect_uri: config.spotify_redirect_uri,
        token_store_path: config.spotify_token_store_path,
    });

    // Never fatal: a revoked token or a Spotify blip during a redeploy must not
    // stop the server from binding, or /auth/spotify would be unreachable and
    // the box could never be re-authorized.
    match spotify.initialize_from_store().await {
        Ok(true) => tracing::info!("refreshed persisted Spotify authorization"),
        Ok(false) => tracing::info!("no persisted Spotify authorization; visit /auth/spotify"),
        Err(error) => tracing::warn!(
            %error,
            "could not restore Spotify authorization; visit /auth/spotify to re-authorize"
        ),
    }

    let state_hub = Arc::new(StateHub::new());
    let (_poll_shutdown, poll_shutdown_rx) = tokio::sync::watch::channel(false);
    let poll_spotify = spotify.clone();
    let poll_hub = state_hub.clone();
    let _poll_task = tokio::spawn(run_poll_loop(
        poll_hub,
        move || {
            let spotify = poll_spotify.clone();
            async move { spotify.currently_playing().await }
        },
        poll_shutdown_rx,
    ));

    let app = routes::router(spotify, config.device_api_token, state_hub);
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind server to {bind_addr}"))?;
    tracing::info!(address = %bind_addr, "muse-box listening");
    axum::serve(listener, app).await.context("server failed")?;
    Ok(())
}
