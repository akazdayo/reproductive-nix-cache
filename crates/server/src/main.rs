mod api;
mod binary_cache;
mod cache_location;
mod cli;
mod config;
mod entity;
mod store;
use anyhow::bail;
use chrono::Duration;
use clap::Parser;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    let config = config::Config::default();
    let listen = cli.listen.unwrap_or(config.listen_addr);
    let database = cli.database.unwrap_or(config.database_path);
    let store = store::EvidenceStore::open(&database).await?;
    if cli.cache_min_builders == 0 || cli.commit_min_builders == 0 {
        bail!("builder thresholds must be at least 1");
    }
    if cli.commit_window_seconds <= 0 || cli.reveal_window_seconds <= 0 {
        bail!("commit and reveal windows must be at least 1 second");
    }
    if cli.upstream_cache.is_some() && cli.commit_min_builders < cli.cache_min_builders {
        bail!("--commit-min-builders must be at least --cache-min-builders");
    }
    let round_config = store::RoundConfig {
        minimum_builders: cli.commit_min_builders,
        commit_window: Duration::seconds(cli.commit_window_seconds),
        reveal_window: Duration::seconds(cli.reveal_window_seconds),
    };
    let mut state = api::AppState::new(store).with_round_config(round_config);
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
