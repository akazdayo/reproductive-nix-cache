mod api;
mod binary_cache;
mod cli;
mod config;
mod entity;
mod store;
use anyhow::bail;
use clap::Parser;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    let config = config::Config::default();
    let listen = cli.listen.unwrap_or(config.listen_addr);
    let database = cli.database.unwrap_or(config.database_path);
    let store = store::EvidenceStore::open(&database).await?;
    if cli.cache_min_builders == 0 {
        bail!("--cache-min-builders must be at least 1");
    }
    let mut state = api::AppState::new(store);
    if let Some(upstream_url) = &cli.upstream_cache {
        let cache =
            binary_cache::BinaryCache::from_upstream_url(upstream_url, cli.cache_min_builders)?;
        state = state.with_binary_cache(cache);
    }
    let app = api::router(state);
    let listener = TcpListener::bind(listen).await?;
    eprintln!(
        "reproductive-nix-cache server listening on {listen}; database: {}; upstream cache: {}",
        database.display(),
        cli.upstream_cache.as_deref().unwrap_or("disabled")
    );
    axum::serve(listener, app).await?;

    Ok(())
}
