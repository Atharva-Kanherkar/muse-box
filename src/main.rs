use std::sync::Arc;

use anyhow::Context;
use muse_box::{
    account::{self, AccountRegistry, OwnerMarker, RegistryConfig},
    config::Config,
    intent::IntentModel,
    lyrics::LyricsIndex,
    realtime::RealtimeManager,
    routes,
    session::SessionStore,
    spotify::{SpotifyClient, SpotifyConfig},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    let config = Config::from_env()?;
    let bind_addr = config.bind_addr();
    let spotify_client_id = config.spotify_client_id.clone();
    let spotify_client_secret = config.spotify_client_secret.clone();
    let spotify_redirect_uri = config.spotify_redirect_uri.clone();
    let openai_api_key = config.openai_api_key.clone();

    // Shared across every account: the first person to play a song looks it
    // up for everyone else too.
    let lyrics = Arc::new(LyricsIndex::new(config.lyrics_cache_dir.clone()));
    let cached_lyrics = lyrics.load().await;
    if cached_lyrics > 0 {
        tracing::info!(tracks = cached_lyrics, "loaded cached lyrics");
    }

    // A read failure here is fatal rather than "start unclaimed": silently
    // treating a permissions problem or a transient volume glitch as "no
    // owner yet" would let the very next login permanently take the hardware
    // bearer token away from whoever the real owner already is.
    let owner = Arc::new(
        OwnerMarker::load(config.owner_marker_path.clone())
            .await
            .context("failed to load the owner marker")?,
    );
    let legacy_spotify = SpotifyClient::new(SpotifyConfig {
        client_id: spotify_client_id.clone(),
        client_secret: spotify_client_secret.clone(),
        redirect_uri: spotify_redirect_uri.clone(),
        token_store_path: config.legacy_token_store_path.clone(),
    });
    if let Err(error) = account::migrate_legacy_install(
        &legacy_spotify,
        &config.legacy_token_store_path,
        &config.legacy_taste_index_path,
        &config.accounts_root,
        &owner,
    )
    .await
    {
        tracing::warn!(
            %error,
            "legacy install migration failed; the previous owner may need to sign in again at /auth/spotify"
        );
    }

    let registry = Arc::new(AccountRegistry::new(RegistryConfig {
        spotify_client_id: spotify_client_id.clone(),
        spotify_client_secret: spotify_client_secret.clone(),
        spotify_redirect_uri: spotify_redirect_uri.clone(),
        openai_api_key: openai_api_key.clone(),
        lyrics,
        idle_display_offset: config.idle_display_offset,
        accounts_root: config.accounts_root.clone(),
    }));

    // Warm the owner now, so the hardware's first poll never pays for a cold
    // start. Every other account resolves lazily, on its own first request.
    match owner.get().await {
        Some(owner_id) => {
            registry.get_or_create(&owner_id).await;
            tracing::info!(account = %owner_id, "warmed the owner account at boot");
        }
        None => tracing::info!("no owner yet; visit /auth/spotify to sign in"),
    }
    AccountRegistry::spawn_reaper(registry.clone());

    // The OAuth entry points' own client. It only ever runs the dance —
    // `authorization_url`, `exchange_code_for_tokens` — and, deliberately,
    // never persists a token of its own (see that method's doc comment), so
    // this path is never read.
    let oauth = SpotifyClient::new(SpotifyConfig {
        client_id: spotify_client_id,
        client_secret: spotify_client_secret,
        redirect_uri: spotify_redirect_uri,
        token_store_path: config.accounts_root.join(".oauth-unused"),
    });

    let _realtime = Arc::new(RealtimeManager::new(
        config.openai_api_key.clone(),
        config.openai_realtime_model.clone(),
    ));
    let voice_model = Arc::new(IntentModel::new(
        openai_api_key,
        config.openai_intent_model.clone(),
        config.openai_reasoning_effort.clone(),
        config.openai_speech_model.clone(),
        config.openai_speech_voice.clone(),
    ));

    let sessions = Arc::new(SessionStore::new(config.session_store_path.clone()));
    match sessions.load().await {
        Ok(0) => tracing::info!("no browser sessions yet; visit /auth/spotify to sign in"),
        Ok(count) => tracing::info!(sessions = count, "restored browser sessions"),
        Err(error) => tracing::warn!(%error, "could not restore browser sessions"),
    }

    let secure_cookies = config.spotify_redirect_uri.starts_with("https://");

    let app = routes::router(routes::RouterConfig {
        oauth,
        device_api_token: config.device_api_token,
        registry,
        owner,
        voice_model,
        sessions,
        secure_cookies,
        client_root: config.client_root.clone(),
    })
    .layer(routes::cors_layer(&config.cors_allowed_origins));
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind server to {bind_addr}"))?;
    tracing::info!(address = %bind_addr, "muse-box listening");
    axum::serve(listener, app).await.context("server failed")?;
    Ok(())
}
