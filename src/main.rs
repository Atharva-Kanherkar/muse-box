use anyhow::Result;

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = muse_box::config::Config::from_env()?;
    tracing::info!(
        addr = %config.bind_addr(),
        "config loaded; server wiring lands with the SSE /state issue"
    );
    Ok(())
}
